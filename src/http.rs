//! Axum 路由与 Basic Auth 中间件。

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::State,
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

pub struct AppState {
    pub node: Arc<NodeRuntime>,
    pub config_path: PathBuf,
    pub server_config: Arc<ServerConfig>,
    pub current_room: Arc<std::sync::Mutex<Option<ActiveRoom>>>,
}

pub fn build_router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        // 节点状态 / 日志 / 配置
        .route("/status", get(crate::api::status))
        .route("/logs", get(crate::api::logs))
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
        .route("/games", get(crate::api::list_games));

    Router::new()
        .route("/", get(serve_index))
        .nest("/api", api)
        .fallback(fallback_handler)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
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

fn auth_ok(cfg: &ServerConfig, headers: &HeaderMap) -> bool {
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

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"astral-server\"")],
        "Unauthorized",
    )
        .into_response()
}

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if !auth_required(&state.server_config) || auth_ok(&state.server_config, request.headers()) {
        return next.run(request).await;
    }
    unauthorized()
}
