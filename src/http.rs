//! Axum 路由与鉴权中间件（Cookie 会话 + Basic Auth 兼容）。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    http::{header, HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Json, Response},
    routing::{delete, get, post, put},
    Router,
};
use serde_json::json;

use crate::config::ServerConfig;
use crate::models::ActiveRoom;
use crate::node::NodeRuntime;

#[derive(Clone)]
pub struct AppState {
    pub node: Arc<NodeRuntime>,
    pub config_path: PathBuf,
    pub server_config: Arc<std::sync::RwLock<ServerConfig>>,
    pub current_room: Arc<std::sync::Mutex<Option<ActiveRoom>>>,
    /// 已登录会话 token 集合（Cookie 值），重启后失效
    pub sessions: Arc<std::sync::RwLock<HashSet<String>>>,
}

pub fn build_router(state: AppState) -> Router {
    let api = Router::new()
        // 节点状态 / 日志 / 配置
        .route("/status", get(crate::api::status))
        .route("/logs", get(crate::api::logs))
        .route("/profile", get(crate::api::get_profile).put(crate::api::put_profile))
        .route("/config", get(crate::api::get_config).put(crate::api::put_config))
        .route("/start", post(crate::api::start))
        .route("/stop", post(crate::api::stop))
        .route("/restart", post(crate::api::restart))
        // 凭据
        .route(
            "/credentials",
            get(crate::api::list_credentials).post(crate::api::create_credential),
        )
        .route("/credentials/:id", delete(crate::api::revoke_credential))
        // 房间
        .route("/room/create", post(crate::api::create_room))
        .route("/room/join", post(crate::api::join_room))
        .route("/room/leave", post(crate::api::leave_room))
        .route("/room/current", get(crate::api::current_room))
        // 服务器列表
        .route("/servers", get(crate::api::list_servers).post(crate::api::add_server))
        .route("/servers/ping", get(crate::api::ping_servers))
        .route("/servers/:id", put(crate::api::update_server).delete(crate::api::delete_server))
        // 游戏列表
        .route("/games", get(crate::api::list_games))
        .route("/update/check", get(crate::api::check_update))
        .route("/update/apply", post(crate::api::apply_update));

    // 登录/登出接口不鉴权（route_layer 只作用于已注册路由，merge 进来的不受影响）
    let auth_routes = Router::new()
        .route("/auth/login", post(crate::api::login))
        .route("/auth/logout", post(crate::api::logout));

    let mw_state = state.clone();
    let protected_api = api.route_layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: Next| {
            let s = mw_state.clone();
            async move { auth_middleware(s, request, next).await }
        },
    ));
    let combined_api = protected_api.merge(auth_routes);

    // `/` 与 fallback 直接返回前端页面（不鉴权），登录验证由前端 login 视图 + /api 鉴权完成。
    Router::new()
        .route("/", get(serve_index))
        .fallback(fallback_handler)
        .nest("/api", combined_api)
        .with_state(state)
}

async fn serve_index() -> impl IntoResponse {
    Html(crate::web::INDEX_HTML)
}

/// Fallback：GET 请求返回首页（SPA 前端路由），其它方法返回 404 JSON。
async fn fallback_handler(method: Method, uri: axum::http::Uri) -> Response {
    if method == Method::GET {
        Html(crate::web::INDEX_HTML).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("Not found: {} {}", method.as_str(), uri.path()) })),
        )
            .into_response()
    }
}

fn auth_required(cfg: &ServerConfig) -> bool {
    !cfg.username.is_empty()
}

/// 从 Cookie 头提取 session token。
pub fn session_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        if let Some(v) = part.trim().strip_prefix("astral_session=") {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn basic_auth_ok(cfg: &ServerConfig, headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let Some(encoded) = value.strip_prefix("Basic ") else {
        return false;
    };
    use base64::Engine;
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return false;
    };
    let Ok(decoded) = String::from_utf8(decoded) else {
        return false;
    };
    let Some((user, pass)) = decoded.split_once(':') else {
        return false;
    };
    user == cfg.username && pass == cfg.password
}

fn auth_ok(state: &AppState, cfg: &ServerConfig, headers: &HeaderMap) -> bool {
    // Cookie 会话（Web 前端）
    if let Some(token) = session_token(headers) {
        if state.sessions.read().unwrap().contains(&token) {
            return true;
        }
    }
    // Basic Auth（curl / 外部工具兼容）
    basic_auth_ok(cfg, headers)
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized" }))).into_response()
}

async fn auth_middleware(
    state: AppState,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    // 在 await 之前释放读锁（RwLockReadGuard 非 Send，不能跨 await 存活）。
    let allowed = {
        let cfg = state.server_config.read().unwrap();
        !auth_required(&cfg) || auth_ok(&state, &cfg, request.headers())
    };
    if allowed {
        next.run(request).await
    } else {
        unauthorized()
    }
}
