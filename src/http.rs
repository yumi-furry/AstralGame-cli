//! =============================================================================
//! HTTP 层：Axum 路由装配与鉴权中间件。
//!
//! 路由结构：
//! * `/`               —— 内嵌单页前端（GET，不鉴权；fallback 同样回退到前端以支持 SPA）
//! * `/api/auth/*`     —— 登录/登出（不鉴权）
//! * `/api/ws/service` —— 第三方服务 WebSocket（不走 Cookie，改用系统随机 ws_key 鉴权）
//! * `/api/**`         —— 其余所有业务接口，统一经过 auth_middleware
//!
//! 鉴权策略（硬约束）：优先校验 HttpOnly Cookie 会话（浏览器面板使用）；
//! 同时保留 HTTP Basic Auth 兼容 curl / 外部脚本；登录登出路由不鉴权。
//! =============================================================================

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

/// 全局共享状态（Clone 廉价，内部全部是 Arc）。注入到每个 Handler 与中间件。
#[derive(Clone)]
pub struct AppState {
    /// EasyTier 节点运行时（实例控制、日志、快照、凭据）。
    pub node: Arc<NodeRuntime>,
    /// config.toml 的绝对路径（读原文/保存设置时使用）。
    pub config_path: PathBuf,
    /// 可热更新的服务配置（面板保存后立即生效并写回文件）。
    pub server_config: Arc<std::sync::RwLock<ServerConfig>>,
    /// 当前房间内存会话（创建/加入后置入，离开或重启后为 None）。
    pub current_room: Arc<std::sync::Mutex<Option<ActiveRoom>>>,
    /// 已登录会话 token 集合（Cookie 值），重启后失效
    pub sessions: Arc<std::sync::RwLock<HashSet<String>>>,
    /// 房间变动事件中心（WebSocket 广播 + 邮件触发）
    pub hub: Arc<crate::notify::EventHub>,
    /// 在线第三方服务注册表（WebSocket 密钥认证连接）
    pub ws_registry: Arc<crate::ws::WsRegistry>,
}

/// 组装完整路由树：受保护的 `/api` 业务路由 + 不鉴权的登录/第三方 WS 路由 + 前端页面。
pub fn build_router(state: AppState) -> Router {
    // 受保护的业务 API：后续统一挂 auth_middleware。
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
        // WebSocket 事件推送（房间内容/房间码/房间状态）
        .route("/ws", get(crate::ws::ws_upgrade))
        // 第三方服务连接管理
        .route("/ws/key", get(crate::api::get_ws_key))
        .route("/ws/key/rotate", post(crate::api::rotate_ws_key))
        .route("/ws/services", get(crate::api::list_ws_services))
        // SMTP / 邮件通知
        .route("/smtp", get(crate::api::get_smtp))
        .route("/smtp/recipients", put(crate::api::put_recipients))
        .route("/smtp/test", post(crate::api::test_smtp))
        .route("/smtp/send-now", post(crate::api::send_now_smtp))
        .route("/update/check", get(crate::api::check_update))
        .route("/update/apply", post(crate::api::apply_update));

    // 登录/登出接口不鉴权（route_layer 只作用于已注册路由，merge 进来的不受影响）
    let auth_routes = Router::new()
        .route("/auth/login", post(crate::api::login))
        .route("/auth/logout", post(crate::api::logout))
        // 第三方服务连接也不走面板 Cookie 鉴权，改用系统随机密钥在握手时校验
        .route("/ws/service", get(crate::ws::ws_service_upgrade));

    // 用闭包把 state 克隆进 axum 中间件（from_fn 本身拿不到 State）。
    let mw_state = state.clone();
    // route_layer 只作用于 api 路由器自身已注册的路由，
    // 之后 merge 进来的 auth_routes 不会被该层覆盖——这正是登录接口免鉴权的关键。
    let protected_api = api.route_layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: Next| {
            let s = mw_state.clone();
            async move { auth_middleware(s, request, next).await }
        },
    ));
    // 合并受保护与免鉴权两组 /api 路由。
    let combined_api = protected_api.merge(auth_routes);

    // `/` 与 fallback 直接返回前端页面（不鉴权），登录验证由前端 login 视图 + /api 鉴权完成。
    Router::new()
        .route("/", get(serve_index))
        .fallback(fallback_handler)
        .nest("/api", combined_api)
        .with_state(state)
}

/// 根路径：返回内嵌的单页管理端 HTML（无需读盘，编译时已 include 进二进制）。
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

/// 是否需要鉴权：只要配置了用户名即启用（密码可能为空的情况也照配，由部署者负责）。
fn auth_required(cfg: &ServerConfig) -> bool {
    !cfg.username.is_empty()
}

/// 从 Cookie 头提取 session token。
/// Cookie 头形如 "k1=v1; astral_session=<uuid>; k3=v3"，需逐段匹配。
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

/// 校验 HTTP Basic Auth：Authorization: Basic base64(username:password)。
/// 供 curl 等不维护 Cookie 的外部工具使用；任一步骤解析失败都视为不通过。
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
    // 用户名不允许含 ':'，split_once 取第一个冒号（密码中可以含冒号）。
    let Some((user, pass)) = decoded.split_once(':') else {
        return false;
    };
    user == cfg.username && pass == cfg.password
}

/// 综合鉴权判定：Cookie 会话优先，未命中再尝试 Basic Auth。
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

/// 统一的 401 JSON 响应。
fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized" }))).into_response()
}

/// 鉴权中间件：未配置用户名时直通；否则要求有效 Cookie 会话或 Basic Auth。
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
        // 前端 fetch 收到 401 后会切换到登录视图。
        unauthorized()
    }
}
