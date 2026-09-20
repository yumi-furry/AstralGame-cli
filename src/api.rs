//! REST API 处理器：房间、服务器、节点状态。

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;

use crate::http::AppState;
use crate::invite;
use crate::models::{PeerEndpoint, RoomInvitePayload, ServerEntry};
use crate::store;

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

// ---------- 节点状态 / 日志 ----------

pub async fn status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.node.snapshot())
}

#[derive(Deserialize)]
pub struct LogsQuery {
    tail: Option<usize>,
}

pub async fn logs(
    State(state): State<Arc<AppState>>,
    Query(q): Query<LogsQuery>,
) -> impl IntoResponse {
    let tail = q.tail.unwrap_or(200).min(800);
    Json(json!({ "logs": state.node.logs(tail) }))
}

pub async fn get_config(State(state): State<Arc<AppState>>) -> Response {
    match std::fs::read_to_string(&state.config_path) {
        Ok(raw) => Json(json!({ "config": raw, "path": state.config_path.to_string_lossy() }))
            .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, format!("读取配置失败: {e}")),
    }
}

#[derive(Deserialize)]
pub struct StartRequest {
    config: Option<String>,
}

pub async fn start(State(state): State<Arc<AppState>>, Json(req): Json<StartRequest>) -> Response {
    let toml = match req.config {
        Some(cfg) => match crate::config::parse(&cfg) {
            Ok(p) => p.instance_toml,
            Err(e) => return err(StatusCode::BAD_REQUEST, format!("配置无效: {e}")),
        },
        None => match std::fs::read_to_string(&state.config_path) {
            Ok(raw) => match crate::config::parse(&raw) {
                Ok(p) => p.instance_toml,
                Err(e) => return err(StatusCode::BAD_REQUEST, format!("配置无效: {e}")),
            },
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("读取配置失败: {e}")),
        },
    };

    match state.node.start(toml).await {
        Ok(id) => Json(json!({ "ok": true, "instance_id": id })).into_response(),
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response()
        }
    }
}

pub async fn stop(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    state.node.stop().await;
    Json(json!({ "ok": true }))
}

pub async fn restart(State(state): State<Arc<AppState>>) -> Response {
    match state.node.restart().await {
        Ok(id) => Json(json!({ "ok": true, "instance_id": id })).into_response(),
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct PutConfigRequest {
    config: String,
    restart: Option<bool>,
}

pub async fn put_config(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PutConfigRequest>,
) -> Response {
    let parsed = match crate::config::parse(&req.config) {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_REQUEST, format!("配置无效: {e}")),
    };

    if let Err(e) = std::fs::write(&state.config_path, &req.config) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, format!("写入配置失败: {e}"));
    }

    if req.restart.unwrap_or(false) {
        match state.node.start(parsed.instance_toml).await {
            Ok(id) => Json(json!({ "ok": true, "restarted": true, "instance_id": id }))
                .into_response(),
            Err(e) => {
                (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response()
            }
        }
    } else {
        Json(json!({ "ok": true, "restarted": false })).into_response()
    }
}

// ---------- 凭据 ----------

#[derive(Deserialize)]
pub struct CreateCredentialRequest {
    ttl_seconds: i64,
    #[serde(default = "default_reusable")]
    reusable: bool,
}

fn default_reusable() -> bool {
    true
}

pub async fn create_credential(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateCredentialRequest>,
) -> Response {
    match state.node.generate_credential(req.ttl_seconds, req.reusable).await {
        Ok(c) => Json(c).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

pub async fn list_credentials(State(state): State<Arc<AppState>>) -> Response {
    match state.node.list_credentials().await {
        Ok(c) => Json(json!({ "credentials": c })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

pub async fn revoke_credential(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.node.revoke_credential(id).await {
        Ok(success) => Json(json!({ "ok": success })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

// ---------- 房间 ----------

#[derive(Deserialize)]
pub struct CreateRoomRequest {
    game_id: String,
    game_name: String,
    #[serde(default)]
    display_name: Option<String>,
}

pub async fn create_room(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateRoomRequest>,
) -> Response {
    let servers = store::load_servers();
    let enabled: Vec<PeerEndpoint> = servers
        .iter()
        .filter(|s| s.enabled)
        .map(|s| PeerEndpoint { uri: s.uri.clone() })
        .collect();
    if enabled.is_empty() {
        return err(StatusCode::BAD_REQUEST, "请先在「服务器」页启用至少一个服务器");
    }

    let payload = RoomInvitePayload {
        v: 1,
        game_id: req.game_id,
        game_name: req.game_name,
        network_name: invite::generate_network_name(),
        network_secret: invite::generate_network_secret(),
        peers: enabled,
        display_name: req.display_name,
    };

    let offline_invite = match invite::encode_offline_invite(&payload) {
        Ok(s) => s,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("生成邀请串失败: {e}")),
    };

    let short_code = try_create_short_code(&payload).await.ok();

    let room = crate::models::ActiveRoom {
        is_host: true,
        game_id: payload.game_id.clone(),
        game_name: payload.game_name.clone(),
        network_name: payload.network_name.clone(),
        network_secret: payload.network_secret.clone(),
        display_name: payload
            .display_name
            .clone()
            .unwrap_or_else(|| payload.game_name.clone()),
        short_code: short_code.clone(),
        offline_invite: Some(offline_invite.clone()),
        peers: payload.peers.clone(),
    };

    let toml = build_room_toml(&room);
    match state.node.start(toml).await {
        Ok(id) => {
            *state.current_room.lock().unwrap() = Some(room.clone());
            Json(json!({
                "ok": true,
                "instance_id": id,
                "room": room,
                "share_url": invite::build_join_url(short_code.as_deref(), Some(&offline_invite)),
            }))
            .into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, format!("启动实例失败: {e}")),
    }
}

#[derive(Deserialize)]
pub struct JoinRoomRequest {
    input: String,
}

pub async fn join_room(
    State(state): State<Arc<AppState>>,
    Json(req): Json<JoinRoomRequest>,
) -> Response {
    let input = req.input.trim();
    let payload = match resolve_invite(input).await {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_REQUEST, format!("解析邀请失败: {e}")),
    };

    // 判断输入是否为短码，若是则保存以便前端显示
    let short_code = extract_short_code(input);

    let room = crate::models::ActiveRoom {
        is_host: false,
        game_id: payload.game_id.clone(),
        game_name: payload.game_name.clone(),
        network_name: payload.network_name.clone(),
        network_secret: payload.network_secret.clone(),
        display_name: payload
            .display_name
            .clone()
            .unwrap_or_else(|| payload.game_name.clone()),
        short_code,
        offline_invite: invite::encode_offline_invite(&payload).ok(),
        peers: payload.peers.clone(),
    };

    let toml = build_room_toml(&room);
    match state.node.start(toml).await {
        Ok(id) => {
            *state.current_room.lock().unwrap() = Some(room.clone());
            Json(json!({
                "ok": true,
                "instance_id": id,
                "room": room,
            }))
            .into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, format!("启动实例失败: {e}")),
    }
}

pub async fn leave_room(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    state.node.stop().await;
    *state.current_room.lock().unwrap() = None;
    Json(json!({ "ok": true }))
}

pub async fn current_room(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let room = state.current_room.lock().unwrap().clone();
    match room {
        Some(r) => {
            let share_url = invite::build_join_url(r.short_code.as_deref(), r.offline_invite.as_deref());
            Json(json!({ "room": r, "share_url": share_url }))
        }
        None => Json(json!({ "room": null })),
    }
}

fn build_room_toml(room: &crate::models::ActiveRoom) -> String {
    let mut s = String::new();
    s.push_str("instance_name = \"astral-server\"\n");
    s.push_str("hostname = \"astral-server\"\n");
    s.push_str("dhcp = true\n");
    s.push_str("listeners = [\n");
    s.push_str("    \"tcp://0.0.0.0:11010\",\n");
    s.push_str("    \"udp://0.0.0.0:11010\",\n");
    s.push_str("]\n\n");
    s.push_str("[network_identity]\n");
    s.push_str(&format!("network_name = \"{}\"\n", room.network_name));
    s.push_str(&format!("network_secret = \"{}\"\n", room.network_secret));
    s.push('\n');
    for p in &room.peers {
        s.push_str("[[peer]]\n");
        s.push_str(&format!("uri = \"{}\"\n", p.uri));
    }
    s.push_str("\n[flags]\n");
    s.push_str("default_protocol = \"tcp\"\n");
    s.push_str("dev_name = \"astral0\"\n");
    s
}

async fn try_create_short_code(payload: &RoomInvitePayload) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let resp = client
        .post("http://103.194.107.25:8080/v1/codes")
        .json(payload)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("短码服务返回 {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    let code = body["code"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("短码服务响应缺少 code"))?;
    Ok(code.to_string())
}

/// 从用户输入中提取短码（如果输入是短码或含短码的链接）。
fn extract_short_code(input: &str) -> Option<String> {
    let input = input.trim();

    // 离线邀请串 → 不是短码
    if input.starts_with("AG1.") {
        return None;
    }

    // 从 URL 中提取 token
    let token = if input.starts_with("http") {
        if let Ok(url) = url::Url::parse(input) {
            url.query_pairs()
                .find(|(k, _)| k == "c")
                .map(|(_, v)| v.to_string())
        } else {
            None
        }
    } else {
        None
    };
    let token = token.unwrap_or_else(|| input.to_string());

    // token 是离线串 → 不是短码
    if token.starts_with("AG1.") {
        return None;
    }

    // 判断是否为短码格式
    let normalized = invite::normalize_share_code(&token);
    let is_valid_code = normalized.len() >= 4
        && normalized.len() <= 12
        && normalized
            .chars()
            .all(|c| "0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(c));

    if is_valid_code {
        Some(normalized)
    } else {
        None
    }
}

/// 解析邀请输入：支持 AG1. 离线串、短码、完整链接。
async fn resolve_invite(input: &str) -> anyhow::Result<RoomInvitePayload> {
    // 1. 离线邀请串
    if input.starts_with("AG1.") {
        return invite::decode_offline_invite(input);
    }

    // 2. 从 URL 中提取 token
    let token = if input.starts_with("http") {
        if let Ok(url) = url::Url::parse(input) {
            url.query_pairs()
                .find(|(k, _)| k == "c")
                .map(|(_, v)| v.to_string())
        } else {
            None
        }
    } else {
        None
    };
    let token = token.unwrap_or_else(|| input.to_string());

    // 3. 离线串
    if token.starts_with("AG1.") {
        return invite::decode_offline_invite(&token);
    }

    // 4. 短码（4-12 位 Crockford Base32 字符）
    let normalized = invite::normalize_share_code(&token);
    let is_valid_code = normalized.len() >= 4
        && normalized.len() <= 12
        && normalized
            .chars()
            .all(|c| "0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(c));

    if is_valid_code {
        return fetch_short_code(&normalized).await;
    }

    anyhow::bail!(
        "无法识别的邀请格式。请输入：\n\
         1. 6 位短码（如 ABC123）\n\
         2. AG1. 开头的离线邀请串\n\
         3. 完整邀请链接 https://next.astral.fan/j?c=..."
    )
}

async fn fetch_short_code(code: &str) -> anyhow::Result<RoomInvitePayload> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let resp = client
        .get(format!("http://103.194.107.25:8080/v1/codes/{code}"))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("短码服务不可达: {e}"))?;

    if !resp.status().is_success() {
        anyhow::bail!("短码服务返回 {}，短码可能已过期或不存在", resp.status());
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("短码服务响应解析失败: {e}"))?;

    // 尝试直接解析为 RoomInvitePayload
    if let Ok(p) = serde_json::from_value::<RoomInvitePayload>(body.clone()) {
        return Ok(p);
    }

    // 如果直接解析失败，尝试从嵌套字段提取
    if let Some(data) = body.get("data") {
        if let Ok(p) = serde_json::from_value::<RoomInvitePayload>(data.clone()) {
            return Ok(p);
        }
    }

    anyhow::bail!(
        "短码服务返回的数据格式不匹配。原始响应: {}",
        serde_json::to_string_pretty(&body).unwrap_or_default()
    )
}

// ---------- 服务器列表 ----------

pub async fn list_servers() -> impl IntoResponse {
    Json(json!({ "servers": store::load_servers() }))
}

/// 并发 ping 所有服务器，返回延迟（TCP 连接耗时）。
pub async fn ping_servers() -> impl IntoResponse {
    let servers = store::load_servers();
    let mut handles = Vec::new();
    for s in &servers {
        let id = s.id;
        let uri = s.uri.clone();
        handles.push(tokio::spawn(async move {
            let latency = tcp_ping(&uri).await;
            json!({ "id": id, "latency_ms": latency })
        }));
    }
    let mut results = Vec::new();
    for h in handles {
        match h.await {
            Ok(r) => results.push(r),
            Err(e) => results.push(json!({ "error": e.to_string() })),
        }
    }
    Json(json!({ "results": results }))
}

/// TCP 连接测延迟：从 URI 中解析 host:port，测量 TCP 握手耗时。
async fn tcp_ping(uri: &str) -> i64 {
    let (_scheme, rest) = uri.split_once("://").unwrap_or(("tcp", uri));
    let target = if rest.contains(':') {
        rest.to_string()
    } else {
        format!("{rest}:11010")
    };
    let start = std::time::Instant::now();
    match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(&target),
    )
    .await
    {
        Ok(Ok(_)) => start.elapsed().as_millis() as i64,
        Ok(Err(_)) | Err(_) => -1,
    }
}

#[derive(Deserialize)]
pub struct AddServerRequest {
    name: String,
    uri: String,
}

pub async fn add_server(Json(req): Json<AddServerRequest>) -> Response {
    let mut list = store::load_servers();
    let id = list.iter().map(|s| s.id).max().unwrap_or(0) + 1;
    let sort_order = list.iter().map(|s| s.sort_order).max().unwrap_or(0) + 1;
    list.push(ServerEntry {
        id,
        name: req.name,
        uri: req.uri,
        enabled: true,
        sort_order,
    });
    match store::save_servers(&list) {
        Ok(_) => Json(json!({ "ok": true, "id": id })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, format!("保存失败: {e}")),
    }
}

#[derive(Deserialize)]
pub struct UpdateServerRequest {
    name: Option<String>,
    uri: Option<String>,
    enabled: Option<bool>,
}

pub async fn update_server(
    Path(id): Path<u64>,
    Json(req): Json<UpdateServerRequest>,
) -> Response {
    let mut list = store::load_servers();
    let Some(s) = list.iter_mut().find(|s| s.id == id) else {
        return err(StatusCode::NOT_FOUND, "服务器不存在");
    };
    if let Some(n) = req.name { s.name = n; }
    if let Some(u) = req.uri { s.uri = u; }
    if let Some(e) = req.enabled { s.enabled = e; }
    match store::save_servers(&list) {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, format!("保存失败: {e}")),
    }
}

pub async fn delete_server(Path(id): Path<u64>) -> Response {
    let mut list = store::load_servers();
    let len_before = list.len();
    list.retain(|s| s.id != id);
    if list.len() == len_before {
        return err(StatusCode::NOT_FOUND, "服务器不存在");
    }
    match store::save_servers(&list) {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, format!("保存失败: {e}")),
    }
}

// ---------- 游戏列表 ----------

pub async fn list_games() -> impl IntoResponse {
    Json(json!({
        "games": [
            { "id": "minecraft", "name": "我的世界", "description": "开局域网世界，客人在本机游戏列表加入" },
            { "id": "gta5", "name": "侠盗猎车手5", "description": "同时支持传承版和增强版以及跨平台联机" },
            { "id": "mindustry", "name": "Mindustry", "description": "塔防+工厂建设沙盒" },
            { "id": "raft", "name": "木筏求生", "description": "海上生存建造" },
            { "id": "supreme-commander", "name": "最高指挥官：钢铁联盟", "description": "RTS 经典" },
            { "id": "custom", "name": "自定义", "description": "手动填写网络名与密钥" },
        ]
    }))
}
