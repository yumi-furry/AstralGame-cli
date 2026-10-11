//! WebSocket 事件推送：
//! ① `/api/ws`：面板页内推送（Cookie 会话鉴权）。
//! ② `/api/ws/service?key=...&name=...&type=...`：第三方服务连接（系统随机密钥鉴权，
//!    连接成功后登记到在线服务列表，面板可查看；密钥刷新后旧密钥立即失效）。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;

use crate::http::AppState;
use crate::models::WsServiceInfo;

/// GET /api/ws 升级处理（鉴权由路由层中间件保证，进入此处即已登录）。
pub async fn ws_upgrade(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// 面板 WebSocket 连接生命周期：首包推快照，之后只做事件转发。
async fn handle_socket(socket: WebSocket, state: AppState) {
    let mut rx = state.hub.subscribe();

    // 连接建立后立即推送当前快照，第三方无需等待下一次变动。
    let initial = crate::notify::EventHub::build_payload(&state, "snapshot").to_string();

    let (mut tx, mut rx_ws) = socket.split();
    if tx.send(Message::Text(initial)).await.is_err() {
        return;
    }

    // 广播 → 客户端
    let send_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(text) => {
                    if tx.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    });

    // 读侧仅用于检测客户端断开（未来可扩展为处理客户端指令）。
    while let Some(Ok(msg)) = rx_ws.next().await {
        if let Message::Close(_) = msg {
            break;
        }
    }
    send_task.abort();
}

// ============================ 第三方服务端点 ============================

#[derive(Deserialize)]
pub struct ServiceWsQuery {
    /// 系统随机密钥（必须与服务端配置一致）。
    key: String,
    /// 服务名称（上报）。
    #[serde(default)]
    name: Option<String>,
    /// 服务类型（上报，如 "AstrBOT"）。
    #[serde(default, rename = "type")]
    service_type: Option<String>,
}

/// GET /api/ws/service 升级处理：query 携带 key/name/type，
/// 密钥校验失败在 HTTP 握手阶段以 401 拒绝（不升级协议）。
pub async fn ws_service_upgrade(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(q): Query<ServiceWsQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    // 密钥校验（握手前完成，失败直接拒绝升级）。
    let key_ok = {
        let cfg = state.server_config.read().unwrap();
        !cfg.ws_key.is_empty() && cfg.ws_key == q.key
    };
    if !key_ok {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "invalid ws service key" })),
        )
            .into_response();
    }

    let name = q.name.unwrap_or_default();
    let service_type = q.service_type.unwrap_or_default();
    ws.on_upgrade(move |socket| handle_service_socket(socket, state, name, service_type, addr))
}

/// 第三方服务连接生命周期：登记 → hello → 快照 → 事件转发；
/// 三路 select：被踢（密钥轮换）、广播消息、客户端断开，任一退出即注销登记。
async fn handle_service_socket(
    socket: WebSocket,
    state: AppState,
    name: String,
    service_type: String,
    addr: SocketAddr,
) {
    let now = now_unix();
    // 每条连接分配 UUID 作为注册表键。
    let id = uuid::Uuid::new_v4().to_string();
    // 容量 1 的踢下线信令通道；注册表持有发送端，本任务持有接收端。
    let (kick_tx, mut kick_rx) = tokio::sync::mpsc::channel::<()>(1);
    let info = WsServiceInfo {
        id: id.clone(),
        // 缺省上报名统一显示 unknown。
        name: if name.is_empty() { "unknown".to_string() } else { name },
        service_type: if service_type.is_empty() {
            "unknown".to_string()
        } else {
            service_type
        },
        first_connected_at: now,
        connected_at: now,
        remote_addr: addr.to_string(),
    };

    state.ws_registry.add(info.clone(), kick_tx);

    let (mut tx, mut rx_ws) = socket.split();
    let mut rx = state.hub.subscribe();

    // ① hello 确认：返回连接 ID / 名称 / 类型 / 初次连接时间。
    let hello = json!({
        "event": "hello",
        "id": info.id,
        "name": info.name,
        "service_type": info.service_type,
        "first_connected_at": info.first_connected_at,
    });
    if tx.send(Message::Text(hello.to_string())).await.is_err() {
        state.ws_registry.remove(&info.id);
        return;
    }

    // ② 立即推送当前快照。
    let initial = crate::notify::EventHub::build_payload(&state, "snapshot").to_string();
    if tx.send(Message::Text(initial)).await.is_err() {
        state.ws_registry.remove(&info.id);
        return;
    }

    loop {
        tokio::select! {
            // 密钥被刷新 → 踢下线
            _ = kick_rx.recv() => {
                let _ = tx.send(Message::Close(Some(axum::extract::ws::CloseFrame {
                    code: 4001,
                    reason: "ws key rotated".into(),
                }))).await;
                break;
            }
            res = rx.recv() => {
                match res {
                    Ok(text) => { if tx.send(Message::Text(text)).await.is_err() { break; } }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
            msg = rx_ws.next() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    state.ws_registry.remove(&info.id);
}

// ============================ 在线服务注册表 ============================

/// 注册表条目：展示信息 + 踢下线信令发送端。
struct Entry {
    info: WsServiceInfo,
    /// 踢下线信号（密钥刷新时触发）。
    kick: tokio::sync::mpsc::Sender<()>,
}

/// 在线第三方服务注册表：登记/查询/踢下线。
pub struct WsRegistry {
    inner: Arc<RwLock<HashMap<String, Entry>>>,
}

impl WsRegistry {
    /// 创建空注册表。
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 登记一条新连接（同 UUID 覆盖，正常不会发生）。
    pub fn add(&self, info: WsServiceInfo, kick: tokio::sync::mpsc::Sender<()>) {
        self.inner.write().unwrap().insert(info.id.clone(), Entry { info, kick });
    }

    /// 连接结束时注销（重复移除无害）。
    pub fn remove(&self, id: &str) {
        self.inner.write().unwrap().remove(id);
    }

    /// 当前在线服务快照（按初次连接时间升序）。
    pub fn list(&self) -> Vec<WsServiceInfo> {
        let mut v: Vec<WsServiceInfo> = self
            .inner
            .read()
            .unwrap()
            .values()
            .map(|e| e.info.clone())
            .collect();
        v.sort_by_key(|i| i.first_connected_at);
        v
    }

    /// 踢掉所有在线服务（密钥刷新后旧密钥立即失效），返回被踢数量。
    pub fn kick_all(&self) -> usize {
        let mut guard = self.inner.write().unwrap();
        let n = guard.len();
        for e in guard.values() {
            // 通道满/关闭都忽略：连接任务退出时会自行 remove。
            let _ = e.kick.try_send(());
        }
        // 立即清空登记，使在线列表即时归零（连接关闭帧异步发出）。
        guard.clear();
        n
    }
}

impl Default for WsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 当前 UNIX 时间戳（秒）。
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
