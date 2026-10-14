//! =============================================================================
//! REST API 处理器集合（全部挂在 /api 下）。
//!
//! 分区：
//! * 登录/登出（Cookie 会话，路由层免鉴权）
//! * 节点状态 / 日志 / 资料 / 原始配置 / 启停
//! * 进网凭据（生成 / 列表 / 撤销）
//! * 房间（创建 / 加入 / 离开 / 当前房间，含邀请解析与短码服务交互）
//! * 中继服务器列表（CRUD + TCP 测速）与内置游戏列表
//! * SMTP 邮件通知（查询 / 收件人维护 / 测试 / 一键发信）
//! * 第三方服务 ws_key（查看 / 轮换）
//! * 在线更新（检查 GitHub Release / 下载应用）
//!
//! 房间的两个核心函数 `create_room_impl` / `join_from_input` 同时供 HTTP
//! 与 main.rs 的守护进程自动联网逻辑复用，避免行为分叉。
//! =============================================================================

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;

use crate::http::AppState;
use crate::invite;
use crate::models::{PeerEndpoint, RoomInvitePayload, ServerEntry};
use crate::store;

/// 统一错误响应：`{"error": "..."}` + 指定 HTTP 状态码。
fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

// ---------- 登录 / 登出（Cookie 会话） ----------

/// 登录请求体。
#[derive(Deserialize)]
pub struct LoginRequest {
    /// 配置中的用户名。
    username: String,
    /// 配置中的密码（明文走内网/HTTPS，不落盘到前端）。
    password: String,
}

/// POST /api/auth/login：校验账密，通过后生成随机会话 token 并经 HttpOnly Cookie 下发。
/// 硬约束：不使用 localStorage 存凭据；Cookie 带 HttpOnly + SameSite=Lax。
pub async fn login(State(state): State<AppState>, Json(body): Json<LoginRequest>) -> Response {
    // 读锁内完成比较后立即释放，避免跨 await。
    let ok = {
        let cfg = state.server_config.read().unwrap();
        !cfg.username.is_empty() && body.username == cfg.username && body.password == cfg.password
    };
    if !ok {
        return err(StatusCode::UNAUTHORIZED, "用户名或密码错误");
    }
    // 会话 token 仅存内存：服务重启后所有面板需重新登录。
    let token = uuid::Uuid::new_v4().to_string();
    state.sessions.write().unwrap().insert(token.clone());
    (
        [(
            header::SET_COOKIE,
            format!("astral_session={token}; Path=/; HttpOnly; SameSite=Lax"),
        )],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

/// POST /api/auth/logout：删除内存会话，并用 Max-Age=0 让浏览器立即过期 Cookie。
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = crate::http::session_token(&headers) {
        state.sessions.write().unwrap().remove(&token);
    }
    (
        [(
            header::SET_COOKIE,
            "astral_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0".to_string(),
        )],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

// ---------- 节点状态 / 日志 ----------

/// GET /api/status：返回节点状态快照（前端 3 秒轮询一次）。
pub async fn status(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.node.snapshot())
}

/// /api/logs 的查询参数：tail=返回最近多少条。
#[derive(Deserialize)]
pub struct LogsQuery {
    tail: Option<usize>,
}

/// GET /api/logs?tail=N：读取节点环形日志，默认 200 条、上限 800 条。
pub async fn logs(
    State(state): State<AppState>,
    Query(q): Query<LogsQuery>,
) -> impl IntoResponse {
    let tail = q.tail.unwrap_or(200).min(800);
    Json(json!({ "logs": state.node.logs(tail) }))
}

/// GET /api/config：返回 config.toml 原文及路径（供面板"配置文件"编辑器）。
pub async fn get_config(State(state): State<AppState>) -> Response {
    match std::fs::read_to_string(&state.config_path) {
        Ok(raw) => Json(json!({ "config": raw, "path": state.config_path.to_string_lossy() }))
            .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, format!("读取配置失败: {e}")),
    }
}

/// PUT /api/profile 请求体：所有字段可选，仅更新出现的字段（PATCH 语义）。
#[derive(Deserialize)]
pub struct PutProfileRequest {
    /// 房间昵称（写入 hostname）。
    nickname: Option<String>,
    /// 房间 emoji 图标。
    icon: Option<String>,
    /// 是否禁用 P2P 打洞（仅中继）。
    disable_p2p: Option<bool>,
    /// 是否创建 TUN 虚拟网卡
    enable_tun: Option<bool>,
    /// 静态虚拟 IP（CIDR；空串=DHCP）
    static_ipv4: Option<String>,
    /// 头像图片 base64（None=不改；空串=清除；否则更新为图片）
    avatar: Option<String>,
    /// 守护进程：开机自动加入
    auto_join_enabled: Option<bool>,
    /// 自动加入邀请（短码/链接/AG1.离线串）
    auto_join_code: Option<String>,
    /// 守护进程：开机自动创建预设房间（与自动加入二选一）
    auto_create_enabled: Option<bool>,
    /// 自动创建预设游戏 ID
    auto_create_game_id: Option<String>,
    /// 自动创建预设游戏名称
    auto_create_game_name: Option<String>,
    /// 是否允许第三方服务发送控制指令（重启/关闭/开启/加入房间）
    service_control_enabled: Option<bool>,
}

/// GET /api/profile：返回面板"资料/网络/守护进程"相关设置；头像读 avatar.bin 并 base64 内联。
pub async fn get_profile(State(state): State<AppState>) -> impl IntoResponse {
    let cfg = state.server_config.read().unwrap();
    // 头像以 data URI 所需的纯 base64 返回（前端自行拼前缀）；无头像时字段为 null。
    let avatar = crate::store::load_avatar().map(|bytes| {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    });
    Json(json!({
        "nickname": cfg.nickname,
        "icon": cfg.icon,
        "disable_p2p": cfg.disable_p2p,
        "enable_tun": cfg.enable_tun,
        "static_ipv4": cfg.static_ipv4,
        "avatar": avatar,
        "auto_join_enabled": cfg.auto_join_enabled,
        "auto_join_code": cfg.auto_join_code,
        "auto_create_enabled": cfg.auto_create_enabled,
        "auto_create_game_id": cfg.auto_create_game_id,
        "auto_create_game_name": cfg.auto_create_game_name,
        "service_control_enabled": cfg.service_control_enabled,
    }))
}

/// PUT /api/profile：更新资料/网络/守护进程设置并写回 config.toml。
/// 头像单独存 avatar.bin；自动加入与自动创建强制互斥；更新后返回最新完整资料。
pub async fn put_profile(
    State(state): State<AppState>,
    Json(req): Json<PutProfileRequest>,
) -> Response {
    // 第一段：更新内存中的 ServerConfig 并做合法性校验（整个块持有写锁）。
    {
        let mut cfg = state.server_config.write().unwrap();
        if let Some(n) = req.nickname {
            let n = n.trim().to_string();
            if n.is_empty() {
                return err(StatusCode::BAD_REQUEST, "昵称不能为空");
            }
            cfg.nickname = n;
        }
        if let Some(i) = req.icon {
            let i = i.trim().to_string();
            if !i.is_empty() {
                cfg.icon = i;
            }
        }
        if let Some(d) = req.disable_p2p {
            cfg.disable_p2p = d;
        }
        if let Some(t) = req.enable_tun {
            cfg.enable_tun = t;
        }
        if let Some(ip) = req.static_ipv4 {
            let ip = ip.trim().to_string();
            if !ip.is_empty() && ip.parse::<cidr::Ipv4Inet>().is_err() {
                return err(StatusCode::BAD_REQUEST, "静态 IP 必须是合法 CIDR（如 10.126.0.1/24）");
            }
            cfg.static_ipv4 = ip;
        }
        if let Some(aj) = req.auto_join_enabled {
            cfg.auto_join_enabled = aj;
        }
        if let Some(code) = req.auto_join_code {
            cfg.auto_join_code = code.trim().to_string();
        }
        if let Some(ac) = req.auto_create_enabled {
            cfg.auto_create_enabled = ac;
        }
        if let Some(gid) = req.auto_create_game_id {
            cfg.auto_create_game_id = gid.trim().to_string();
        }
        if let Some(gname) = req.auto_create_game_name {
            cfg.auto_create_game_name = gname.trim().to_string();
        }
        if let Some(sc) = req.service_control_enabled {
            cfg.service_control_enabled = sc;
        }
        // 二选一：启用自动加入则关闭自动创建，反之亦然
        if cfg.auto_join_enabled {
            cfg.auto_create_enabled = false;
        }
        if cfg.auto_create_enabled {
            cfg.auto_join_enabled = false;
        }
        // 启用自动加入但邀请为空 → 校验失败
        if cfg.auto_join_enabled && cfg.auto_join_code.is_empty() {
            return err(StatusCode::BAD_REQUEST, "启用自动加入时，房间邀请不能为空");
        }
        // 启用自动创建但未选游戏 → 校验失败
        if cfg.auto_create_enabled && cfg.auto_create_game_id.is_empty() {
            return err(StatusCode::BAD_REQUEST, "启用自动创建时，请先选择预设游戏");
        }
    }

    // 第二段：头像独立于 TOML，以原始字节存 avatar.bin。
    // None=不变；空串=清除；非空 base64=解码并限 512KB 后覆盖写入。
    if let Some(avatar_b64) = req.avatar {
        let b64 = avatar_b64.trim().to_string();
        if b64.is_empty() {
            if let Err(e) = crate::store::delete_avatar() {
                return err(StatusCode::INTERNAL_SERVER_ERROR, format!("清除头像失败: {e}"));
            }
        } else {
            use base64::Engine;
            let bytes = match base64::engine::general_purpose::STANDARD.decode(&b64) {
                Ok(b) => b,
                Err(_) => return err(StatusCode::BAD_REQUEST, "头像 base64 无效"),
            };
            if bytes.len() > 512 * 1024 {
                return err(StatusCode::BAD_REQUEST, "头像图片不能超过 512KB");
            }
            if let Err(e) = crate::store::save_avatar(&bytes) {
                return err(StatusCode::INTERNAL_SERVER_ERROR, format!("保存头像失败: {e}"));
            }
        }
    }

    // 第三段：把更新后的 [astral_server] 持久化到 config.toml（EasyTier 部分原样保留）。
    let cfg = state.server_config.read().unwrap();
    if let Err(e) = crate::config::save(&state.config_path, &cfg) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, format!("保存配置失败: {e}"));
    }
    Json(json!({
        "ok": true,
        "nickname": cfg.nickname,
        "icon": cfg.icon,
        "disable_p2p": cfg.disable_p2p,
        "enable_tun": cfg.enable_tun,
        "static_ipv4": cfg.static_ipv4,
        "auto_join_enabled": cfg.auto_join_enabled,
        "auto_join_code": cfg.auto_join_code,
        "auto_create_enabled": cfg.auto_create_enabled,
        "auto_create_game_id": cfg.auto_create_game_id,
        "auto_create_game_name": cfg.auto_create_game_name,
        "service_control_enabled": cfg.service_control_enabled,
    }))
    .into_response()
}

/// POST /api/start 请求体：可直接带完整 TOML 文本；不带则读 config.toml。
#[derive(Deserialize)]
pub struct StartRequest {
    config: Option<String>,
}

/// POST /api/start：用请求体配置或磁盘配置启动（重启）EasyTier 实例。
pub async fn start(State(state): State<AppState>, Json(req): Json<StartRequest>) -> Response {
    // 无论配置来自请求体还是文件，都先经 parse 剥离 [astral_server]，只把内核配置交给节点。
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

/// POST /api/stop：停止当前实例（不断开 HTTP 服务本身）。
pub async fn stop(State(state): State<AppState>) -> impl IntoResponse {
    state.node.stop().await;
    Json(json!({ "ok": true }))
}

/// POST /api/restart：用最近一次配置重启实例。
pub async fn restart(State(state): State<AppState>) -> Response {
    match state.node.restart().await {
        Ok(id) => Json(json!({ "ok": true, "instance_id": id })).into_response(),
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response()
        }
    }
}

/// PUT /api/config 请求体：config 为完整 TOML 原文；restart=true 时保存后立即重启实例。
#[derive(Deserialize)]
pub struct PutConfigRequest {
    config: String,
    restart: Option<bool>,
}

/// PUT /api/config：面板"配置文件"编辑器保存接口（整文件覆盖写）。
pub async fn put_config(
    State(state): State<AppState>,
    Json(req): Json<PutConfigRequest>,
) -> Response {
    // 先解析校验，避免把坏配置写进文件。
    let parsed = match crate::config::parse(&req.config) {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_REQUEST, format!("配置无效: {e}")),
    };

    if let Err(e) = std::fs::write(&state.config_path, &req.config) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, format!("写入配置失败: {e}"));
    }

    // 要求重启时用剥离后的内核配置重启；否则只落盘不影响运行中的实例。
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

/// POST /api/credentials 请求体：有效期秒数 + 是否可复用（缺省可复用）。
#[derive(Deserialize)]
pub struct CreateCredentialRequest {
    ttl_seconds: i64,
    #[serde(default = "default_reusable")]
    reusable: bool,
}

/// serde 缺省值：reusable 字段缺省时按可复用凭据生成。
fn default_reusable() -> bool {
    true
}

/// POST /api/credentials：生成一张进网凭据。
pub async fn create_credential(
    State(state): State<AppState>,
    Json(req): Json<CreateCredentialRequest>,
) -> Response {
    match state.node.generate_credential(req.ttl_seconds, req.reusable).await {
        Ok(c) => Json(c).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

/// GET /api/credentials：列出全部凭据。
pub async fn list_credentials(State(state): State<AppState>) -> Response {
    match state.node.list_credentials().await {
        Ok(c) => Json(json!({ "credentials": c })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

/// DELETE /api/credentials/:id：撤销指定凭据。
pub async fn revoke_credential(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    match state.node.revoke_credential(id).await {
        Ok(success) => Json(json!({ "ok": success })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

// ---------- 房间 ----------

/// POST /api/room/create 请求体。
#[derive(Deserialize)]
pub struct CreateRoomRequest {
    /// 游戏 ID（取自 /api/games）。
    game_id: String,
    /// 游戏名称。
    game_name: String,
    /// 可选的自定义展示名。
    #[serde(default)]
    display_name: Option<String>,
}

/// POST /api/room/create：REST 包装；成功后异步广播 room.created 事件（WS + 邮件）。
pub async fn create_room(
    State(state): State<AppState>,
    Json(req): Json<CreateRoomRequest>,
) -> Response {
    match create_room_impl(&state, req.game_id, req.game_name, req.display_name).await {
        Ok((room, share_url, instance_id)) => {
            // 广播房间变动（WebSocket + 邮件）
            let st = state.clone();
            tokio::spawn(async move {
                crate::notify::EventHub::publish(&st, "room.created").await;
            });
            Json(json!({
                "ok": true,
                "instance_id": instance_id,
                "room": room,
                "share_url": share_url,
            }))
            .into_response()
        }
        Err((status, reason)) => err(status, reason),
    }
}

/// 创建房间核心实现：供 REST 处理器与守护进程自动创建复用。
///
/// 流程：
/// 1. 读取已启用的中继服务器（没有则报错，房间必须至少有一个 peer）；
/// 2. 随机生成网络名/密钥，组装邀请载荷；
/// 3. 编码 AG1. 离线串，并尝试向短码服务注册短码（失败不阻断）；
/// 4. 组装房间 TOML 启动 EasyTier 实例；
/// 5. 写入 current_room 并生成分享链接。
///
/// 返回 (房间会话, 分享 URL, 实例 ID)。
pub async fn create_room_impl(
    state: &AppState,
    game_id: String,
    game_name: String,
    display_name: Option<String>,
) -> Result<(crate::models::ActiveRoom, String, String), (StatusCode, String)> {
    // 仅启用状态的服务器进入邀请载荷。
    let servers = store::load_servers();
    let enabled: Vec<PeerEndpoint> = servers
        .iter()
        .filter(|s| s.enabled)
        .map(|s| PeerEndpoint { uri: s.uri.clone() })
        .collect();
    if enabled.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "请先在「服务器」页启用至少一个服务器".to_string(),
        ));
    }

    // 组装邀请载荷：网络名/密钥每次建房随机生成，房间之间天然隔离。
    let payload = RoomInvitePayload {
        v: 1,
        game_id: game_id.clone(),
        game_name: game_name.clone(),
        network_name: invite::generate_network_name(),
        network_secret: invite::generate_network_secret(),
        peers: enabled,
        display_name: display_name.clone(),
    };

    // AG1. 离线串是"保底邀请方式"，生成失败属于内部错误。
    let offline_invite = match invite::encode_offline_invite(&payload) {
        Ok(s) => s,
        Err(e) => {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("生成邀请串失败: {e}")));
        }
    };

    // 短码是"便捷邀请方式"：依赖外部短码服务，超时/异常时降级为仅离线串。
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

    // 用服务端资料（昵称/TUN/P2P/静态IP）+ 房间网络身份拼出 EasyTier TOML 并启动。
    let (hostname, disable_p2p, enable_tun, static_ipv4) = room_template_params(state);
    let toml = build_room_toml(&room, &hostname, disable_p2p, enable_tun, &static_ipv4);
    let instance_id = state.node.start(toml).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("启动实例失败: {e}"),
        )
    })?;
    // 启动成功后才把房间写入内存会话。
    *state.current_room.lock().unwrap() = Some(room.clone());
    let share_url = invite::build_join_url(short_code.as_deref(), Some(&offline_invite));
    Ok((room, share_url, instance_id))
}

/// POST /api/room/join 请求体：邀请短码 / 完整链接 / AG1. 离线串（三选一原文）。
#[derive(Deserialize)]
pub struct JoinRoomRequest {
    input: String,
}

/// POST /api/room/join：REST 包装；成功后异步广播 room.joined 事件。
pub async fn join_room(
    State(state): State<AppState>,
    Json(req): Json<JoinRoomRequest>,
) -> Response {
    match join_from_input(&state, req.input).await {
        Ok((room, share_url, instance_id)) => {
            // 广播房间变动（WebSocket + 邮件）
            let st = state.clone();
            tokio::spawn(async move {
                crate::notify::EventHub::publish(&st, "room.joined").await;
            });
            Json(json!({
                "ok": true,
                "instance_id": instance_id,
                "room": room,
                "share_url": share_url,
            }))
            .into_response()
        }
        Err((status, msg)) => err(status, msg),
    }
}

/// 可复用的「加入房间」核心逻辑：解析邀请 → 构建房间 → 启动节点 → 写入当前房间。
/// 同时供 HTTP 处理器与启动时守护进程自动加入调用。
pub async fn join_from_input(
    state: &AppState,
    input: String,
) -> Result<(crate::models::ActiveRoom, String, String), (StatusCode, String)> {
    let input = input.trim();
    if input.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "房间邀请不能为空".into()));
    }

    // 解析邀请（自动识别三种格式），失败统一转 400 并带上可读原因。
    let payload = resolve_invite(input)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("解析邀请失败: {e}")))?;

    // 判断输入是否为短码，若是则保存以便前端显示
    // （离线串加入时 short_code 为 None，房间码区域降级显示离线串前缀）。
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
        // 加入时也重新编码一份离线串存内存，保证离开前分享链接始终可生成。
        offline_invite: invite::encode_offline_invite(&payload).ok(),
        peers: payload.peers.clone(),
    };

    // 与建房走同一套 TOML 模板，确保本机 TUN/P2P/静态 IP 设置一致生效。
    let (hostname, disable_p2p, enable_tun, static_ipv4) = room_template_params(state);
    let toml = build_room_toml(&room, &hostname, disable_p2p, enable_tun, &static_ipv4);
    let id = state
        .node
        .start(toml)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("启动实例失败: {e}")))?;

    let share_url = invite::build_join_url(
        room.short_code.as_deref(),
        room.offline_invite.as_deref(),
    );
    // 注意：先算 share_url 再锁存房间（share_url 借用 room 字段）。
    *state.current_room.lock().unwrap() = Some(room.clone());
    Ok((room, share_url, id))
}

/// POST /api/room/leave：停止实例、清空当前房间，并广播 room.left（不发邮件）。
pub async fn leave_room(State(state): State<AppState>) -> impl IntoResponse {
    close_room_impl(&state).await;
    Json(json!({ "ok": true }))
}

/// 关闭房间核心实现：停止实例、清空当前房间、异步广播 room.left。
/// 同时供 REST 处理器与第三方服务 WebSocket 控制指令复用，避免行为分叉。
pub async fn close_room_impl(state: &AppState) {
    state.node.stop().await;
    *state.current_room.lock().unwrap() = None;
    // 广播房间已离开（room=null，不发邮件）
    let st = state.clone();
    tokio::spawn(async move {
        crate::notify::EventHub::publish(&st, "room.left").await;
    });
}

/// GET /api/room/current：返回内存中的当前房间与分享链接（无房间时 room=null）。
pub async fn current_room(State(state): State<AppState>) -> impl IntoResponse {
    let room = state.current_room.lock().unwrap().clone();
    match room {
        Some(r) => {
            let share_url = invite::build_join_url(r.short_code.as_deref(), r.offline_invite.as_deref());
            Json(json!({ "room": r, "share_url": share_url }))
        }
        None => Json(json!({ "room": null })),
    }
}

/// 按房间会话与本机资料拼装 EasyTier 实例 TOML 文本。
/// 建房与加入共用，确保 [network_identity]、listeners、[[peer]]、[flags] 写法一致。
fn build_room_toml(
    room: &crate::models::ActiveRoom,
    hostname: &str,
    disable_p2p: bool,
    enable_tun: bool,
    static_ipv4: &str,
) -> String {
    let mut s = String::new();
    // instance_name 固定；hostname 用面板昵称（对端客户端可见），写入前做 TOML 转义。
    s.push_str("instance_name = \"astral-server\"\n");
    s.push_str(&format!("hostname = \"{}\"\n", tml_escape(hostname)));

    // TUN 开关：关闭时不创建虚拟网卡（服务器仅作为网络协调节点）。
    if !enable_tun {
        s.push_str("no_tun = true\n");
        s.push_str("dhcp = false\n");
        s.push_str("dhcpv6 = false\n");
    } else {
        // 静态 IP 模式：写入顶层 ipv4 键并关闭 DHCP。
        // 与上游客户端修复对齐：必须写顶层，不能写 [flags] 里。
        let static_ip = static_ipv4.trim();
        if !static_ip.is_empty() {
            s.push_str(&format!("ipv4 = \"{}\"\n", tml_escape(static_ip)));
            s.push_str("dhcp = false\n");
        } else {
            // 未指定静态 IP：交由 EasyTier 内置 DHCP 分配。
            // 对齐上游 v1.5.1：显式给初始网段而不是裸 true，避开默认 10.126.126.0/24，
            // 减少与家用 10.x / 192.168.x 网段撞车（尤其移动 CGNAT 环境）。
            s.push_str("dhcp = \"100.100.100.0/24\"\n");
        }
        // 对齐上游 v1.5.1 默认：DHCPv6 自动分配虚拟 IPv6（ULA，fc00::/7 内）。
        s.push_str("dhcpv6 = \"fdae::/120\"\n");
    }

    // 监听 tcp/udp 随机端口（0=由操作系统分配），对所有网卡开放。
    s.push_str("listeners = [\n");
    s.push_str("    \"tcp://0.0.0.0:0\",\n");
    s.push_str("    \"udp://0.0.0.0:0\",\n");
    s.push_str("]\n\n");
    // 网络身份：同房间成员必须完全一致，否则无法互通。
    s.push_str("[network_identity]\n");
    s.push_str(&format!("network_name = \"{}\"\n", room.network_name));
    s.push_str(&format!("network_secret = \"{}\"\n", room.network_secret));
    s.push('\n');
    // 每个中继/对等节点一个 [[peer]] 表（uri 来自邀请载荷，视为受控数据直接写入）。
    for p in &room.peers {
        s.push_str("[[peer]]\n");
        s.push_str(&format!("uri = \"{}\"\n", p.uri));
    }
    // flags 与原版客户端对齐：关 KCP/QUIC 入站与中继，默认 TCP，压缩算法 2。
    s.push_str("\n[flags]\n");
    s.push_str(&format!("disable_p2p = {disable_p2p}\n"));
    // 与原版客户端对齐：压缩算法、KCP/QUIC 入站与中继开关。
    s.push_str("data_compress_algo = 2\n");
    s.push_str("default_protocol = \"tcp\"\n");
    s.push_str("dev_name = \"astral0\"\n");
    s.push_str("disable_kcp_input = true\n");
    s.push_str("disable_relay_kcp = true\n");
    s.push_str("enable_relay_foreign_network_kcp = false\n");
    s.push_str("disable_quic_input = true\n");
    s.push_str("disable_relay_quic = true\n");
    s.push_str("enable_relay_foreign_network_quic = false\n");
    s
}

/// 最小 TOML 字符串转义：转义反斜杠与双引号（昵称/IP 均为单行 Basic String）。
fn tml_escape(s: &str) -> String {
    s.replace('\\', r"\\").replace('"', r#"\""#)
}

/// 读取构建房间 TOML 所需的服务端配置。
/// 返回 (昵称 hostname, 是否禁用P2P, 是否启用TUN, 静态IPv4)。
fn room_template_params(state: &AppState) -> (String, bool, bool, String) {
    let cfg = state.server_config.read().unwrap();
    (
        cfg.nickname.clone(),
        cfg.disable_p2p,
        cfg.enable_tun,
        cfg.static_ipv4.clone(),
    )
}

/// 向短码服务注册邀请载荷，返回短码字符串。
/// POST {短码服务}/v1/codes，body 为 RoomInvitePayload，15 秒超时。
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

/// 用短码向短码服务换回邀请载荷；兼容载荷裸返回与 {data: ...} 包裹两种响应。
async fn fetch_short_code(code: &str) -> anyhow::Result<RoomInvitePayload> {
    // GET {短码服务}/v1/codes/{code}，15 秒超时。
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

    // 如果直接解析失败，尝试从嵌套字段提取（兼容 {data: payload} 包裹格式）。
    if let Some(data) = body.get("data") {
        if let Ok(p) = serde_json::from_value::<RoomInvitePayload>(data.clone()) {
            return Ok(p);
        }
    }

    // 两种结构都对不上：把原始响应带回错误信息，便于排查服务端版本差异。
    anyhow::bail!(
        "短码服务返回的数据格式不匹配。原始响应: {}",
        serde_json::to_string_pretty(&body).unwrap_or_default()
    )
}

// ---------- 服务器列表 ----------

/// GET /api/servers：返回 servers.json 中的全部中继服务器（含启用状态、排序）。
pub async fn list_servers() -> impl IntoResponse {
    Json(json!({ "servers": store::load_servers() }))
}

/// 并发 ping 所有服务器，返回延迟（TCP 连接耗时）。
pub async fn ping_servers() -> impl IntoResponse {
    let servers = store::load_servers();
    let mut handles = Vec::new();
    // 每个服务器一个 tokio 任务并发探测，避免串行等待超时。
    for s in &servers {
        let id = s.id;
        let uri = s.uri.clone();
        handles.push(tokio::spawn(async move {
            let latency = tcp_ping(&uri).await;
            json!({ "id": id, "latency_ms": latency })
        }));
    }
    let mut results = Vec::new();
    // 按任务发起顺序收集；单个任务 panic 时回传 error 而不是整体失败。
    for h in handles {
        match h.await {
            Ok(r) => results.push(r),
            Err(e) => results.push(json!({ "error": e.to_string() })),
        }
    }
    Json(json!({ "results": results }))
}

/// TCP 连接测延迟：从 URI 中解析 host:port，测量 TCP 握手耗时。
/// 成功返回毫秒数；连接失败或 5 秒超时返回 -1（前端显示为"超时"）。
async fn tcp_ping(uri: &str) -> i64 {
    // 剥掉 scheme 前缀（tcp://、udp:// 等）；无 scheme 时整体当作 host。
    let (_scheme, rest) = uri.split_once("://").unwrap_or(("tcp", uri));
    // 缺省端口按 EasyTier 默认 11010。
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
        // Ok(Err) = 连接被拒/不可达；Err = 超时。
        Ok(Err(_)) | Err(_) => -1,
    }
}

/// POST /api/servers 请求体：服务器名与 URI（如 tcp://1.2.3.4:11010）。
#[derive(Deserialize)]
pub struct AddServerRequest {
    name: String,
    uri: String,
}

/// POST /api/servers：新增服务器，默认启用，id 与排序号取当前最大值 +1。
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

/// PUT /api/servers/:id 请求体：三个字段均可选，只更新提交的字段。
#[derive(Deserialize)]
pub struct UpdateServerRequest {
    name: Option<String>,
    uri: Option<String>,
    enabled: Option<bool>,
}

/// PUT /api/servers/:id：按 id 修改服务器条目（局部更新）。
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

/// DELETE /api/servers/:id：按 id 删除服务器；条目不存在返回 404。
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

/// GET /api/games：返回内置游戏目录（硬编码）。
/// id 用于建房请求；"custom" 表示自定义网络（由客户端自行处理）；
/// icon 为空串表示无图标，前端用默认占位图。
pub async fn list_games() -> impl IntoResponse {
    Json(json!({
        "games": [
            { "id": "minecraft", "name": "我的世界", "description": "开局域网世界，客人在本机游戏列表加入", "icon": "https://next.astral.fan/games/minecraft/icon.png" },
            { "id": "grand_theft_auto_v", "name": "侠盗猎车手5", "description": "同时支持传承版和增强版以及跨平台联机", "icon": "https://next.astral.fan/games/grand_theft_auto_v/icon.png" },
            { "id": "mindustry", "name": "Mindustry", "description": "塔防 + 工厂建设沙盒", "icon": "https://next.astral.fan/games/mindustry/icon.png" },
            { "id": "raft", "name": "木筏求生", "description": "海上生存建造", "icon": "https://next.astral.fan/games/raft/icon.png" },
            { "id": "forged_alliance", "name": "最高指挥官：钢铁联盟", "description": "RTS 经典", "icon": "https://next.astral.fan/games/forged_alliance/icon.png" },
            { "id": "valheim", "name": "英灵神殿", "description": "维京生存合作", "icon": "https://next.astral.fan/games/valheim/icon.png" },
            { "id": "ark", "name": "方舟：生存进化", "description": "恐龙生存沙盒", "icon": "https://next.astral.fan/games/ark/icon.png" },
            { "id": "dont_starve_together", "name": "饥荒联机版", "description": "荒野生存合作", "icon": "https://next.astral.fan/games/dont_starve_together/icon.png" },
            { "id": "factorio", "name": "异星工厂", "description": "自动化工厂建设", "icon": "https://next.astral.fan/games/factorio/icon.png" },
            { "id": "left_4_dead_2", "name": "求生之路2", "description": "合作求生射击", "icon": "https://next.astral.fan/games/left_4_dead_2/icon.png" },
            { "id": "palworld", "name": "幻兽帕鲁", "description": "帕鲁收集与生存", "icon": "https://next.astral.fan/games/palworld/icon.png" },
            { "id": "project_zomboid", "name": "僵尸毁灭工程", "description": "僵尸生存沙盒", "icon": "https://next.astral.fan/games/project_zomboid/icon.png" },
            { "id": "rust", "name": "腐蚀", "description": "多人生存建造", "icon": "https://next.astral.fan/games/rust/icon.png" },
            { "id": "satisfactory", "name": "幸福工厂", "description": "第一人称工厂建设", "icon": "https://next.astral.fan/games/satisfactory/icon.png" },
            { "id": "seven_days_to_die", "name": "七日杀", "description": "僵尸生存建造", "icon": "https://next.astral.fan/games/seven_days_to_die/icon.png" },
            { "id": "stardew_valley", "name": "星露谷物语", "description": "农场经营合作", "icon": "https://next.astral.fan/games/stardew_valley/icon.png" },
            { "id": "terraria", "name": "泰拉瑞亚", "description": "2D 沙盒冒险", "icon": "https://next.astral.fan/games/terraria/icon.png" },
            { "id": "v_rising", "name": "夜族崛起", "description": "吸血鬼生存建造", "icon": "https://next.astral.fan/games/v_rising/icon.png" },
            { "id": "custom", "name": "自定义", "description": "手动填写网络名与密钥", "icon": "" },
        ]
    }))
}

// ---------- SMTP / 邮件通知 ----------

/// 获取 SMTP 配置（密码不回传，仅返回是否已设置）。
pub async fn get_smtp(State(state): State<AppState>) -> impl IntoResponse {
    let cfg = state.server_config.read().unwrap();
    let s = &cfg.smtp;
    Json(json!({
        "enabled": s.enabled,
        "host": s.host,
        "port": s.port,
        "username": s.username,
        "password_set": !s.password.is_empty(),
        "from_name": s.from_name,
        "from_email": s.from_email,
        "encryption": s.encryption,
        "recipients": s.recipients,
    }))
}

/// PUT /api/smtp/recipients 请求体：收件人邮箱地址列表（全量覆盖）。
#[derive(Deserialize)]
pub struct RecipientsRequest {
    recipients: Vec<String>,
}

/// 更新收件人邮箱列表（面板维护，写回 config.toml）。
pub async fn put_recipients(
    State(state): State<AppState>,
    Json(req): Json<RecipientsRequest>,
) -> Response {
    // trim → 去重 → 基本格式校验，保持顺序
    let mut recipients: Vec<String> = Vec::new();
    for addr in req.recipients {
        let addr = addr.trim().to_string();
        if addr.is_empty() || recipients.contains(&addr) {
            continue;
        }
        if !is_valid_email(&addr) {
            return err(StatusCode::BAD_REQUEST, format!("邮箱地址格式不正确：{addr}"));
        }
        recipients.push(addr);
    }

    {
        let mut cfg = state.server_config.write().unwrap();
        cfg.smtp.recipients = recipients.clone();
        if let Err(e) = crate::config::save(&state.config_path, &cfg) {
            return err(StatusCode::INTERNAL_SERVER_ERROR, format!("保存配置失败: {e}"));
        }
    }
    Json(json!({ "ok": true, "recipients": recipients })).into_response()
}

/// 发送测试邮件验证 SMTP 配置。
pub async fn test_smtp(State(state): State<AppState>) -> Response {
    let smtp = { state.server_config.read().unwrap().smtp.clone() };
    if !smtp.enabled {
        return err(StatusCode::BAD_REQUEST, "SMTP 未启用（请在 config.toml 中开启并填写服务器参数）");
    }
    if smtp.host.is_empty() || smtp.from_email.is_empty() {
        return err(StatusCode::BAD_REQUEST, "SMTP 主机或发件邮箱未配置");
    }
    if !is_valid_email(&smtp.from_email) {
        return err(StatusCode::BAD_REQUEST, "发件邮箱格式不正确");
    }
    if smtp.recipients.is_empty() {
        return err(StatusCode::BAD_REQUEST, "收件人列表为空，请先添加收件邮箱");
    }
    // 发信耗时可能较长，最多等 30 秒
    match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        crate::mailer::send_test(&smtp),
    )
    .await
    {
        Ok(Ok(())) => Json(json!({ "ok": true, "message": "测试邮件已发送" })).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_GATEWAY, e),
        Err(_) => err(StatusCode::GATEWAY_TIMEOUT, "SMTP 连接超时，请检查主机/端口/加密方式"),
    }
}

/// 一键发送当前房间详情邮件给所有收件人。
pub async fn send_now_smtp(State(state): State<AppState>) -> Response {
    // 克隆 SMTP 配置，避免持锁进行耗时网络发送。
    let smtp = { state.server_config.read().unwrap().smtp.clone() };
    if !smtp.enabled {
        return err(StatusCode::BAD_REQUEST, "SMTP 未启用（请在 config.toml 中开启并填写服务器参数）");
    }
    if smtp.recipients.is_empty() {
        return err(StatusCode::BAD_REQUEST, "收件人列表为空，请先添加收件邮箱");
    }
    // 克隆房间快照（同样为了缩短锁持有时间）。
    let room = { state.current_room.lock().unwrap().clone() };
    let Some(room) = room else {
        return err(StatusCode::BAD_REQUEST, "当前无房间，请先创建或加入房间");
    };
    let share_url = crate::invite::build_join_url(
        room.short_code.as_deref(),
        room.offline_invite.as_deref(),
    );
    // 快照中含本机虚拟 IP 与在线成员，随邮件一起发送。
    let snap = state.node.snapshot();
    match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        crate::mailer::send_room_notification(
            &smtp, "房间详情速递", "您请求的当前房间详情如下", Some(&room), &share_url, &snap,
        ),
    ).await {
        Ok(Ok(())) => Json(json!({ "ok": true, "message": format!("房间详情邮件已发送给 {} 位收件人", smtp.recipients.len()) })).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_GATEWAY, e),
        Err(_) => err(StatusCode::GATEWAY_TIMEOUT, "SMTP 连接超时，请检查主机/端口/加密方式"),
    }
}

// ---------- WebSocket 第三方服务 ----------

/// 获取当前第三方服务连接密钥。
pub async fn get_ws_key(State(state): State<AppState>) -> impl IntoResponse {
    let key = state.server_config.read().unwrap().ws_key.clone();
    Json(json!({ "key": key }))
}

/// 刷新第三方服务连接密钥：生成新密钥并写回配置，旧密钥立即失效（所有已连接服务被踢下线）。
pub async fn rotate_ws_key(State(state): State<AppState>) -> Response {
    let new_key = crate::config::generate_ws_key();
    {
        let mut cfg = state.server_config.write().unwrap();
        cfg.ws_key = new_key.clone();
        if let Err(e) = crate::config::save(&state.config_path, &cfg) {
            return err(StatusCode::INTERNAL_SERVER_ERROR, format!("保存配置失败: {e}"));
        }
    }
    let kicked = state.ws_registry.kick_all();
    Json(json!({ "ok": true, "key": new_key, "disconnected": kicked })).into_response()
}

/// 当前在线第三方服务列表。
pub async fn list_ws_services(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({ "services": state.ws_registry.list() }))
}

/// 简单的邮箱格式校验：含 @，@ 前后有内容，域名含点。
fn is_valid_email(addr: &str) -> bool {
    let Some((user, domain)) = addr.split_once('@') else {
        return false;
    };
    !user.is_empty()
        && !domain.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
}

// ---------- 在线更新 ----------

/// POST /api/update/apply 请求体：GitHub 发行版资产直链（需通过域名白名单校验）。
#[derive(Deserialize)]
pub struct ApplyUpdateRequest {
    download_url: String,
}

/// 检查 GitHub 最新发行版与当前版本是否一致。
pub async fn check_update() -> Response {
    let current = crate::update::current_version();
    match crate::update::fetch_latest_release().await {
        Ok(info) => {
            // 语义版本比较；download_url 已在 update 模块按 linux+x86_64 资产过滤。
            let update_available = crate::update::is_newer(&info.tag_name, &current);
            Json(json!({
                "current_version": current,
                "latest_version": info.tag_name,
                "latest_name": info.name,
                "update_available": update_available,
                "release_notes": info.body,
                "release_url": info.html_url,
                "download_url": info.download_url,
            }))
            .into_response()
        }
        // 网络/解析失败不抛 500：前端仍需展示 current_version。
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "current_version": current, "error": e })),
        )
            .into_response(),
    }
}

/// 下载指定版本并替换自身二进制，然后重启服务。
pub async fn apply_update(Json(body): Json<ApplyUpdateRequest>) -> Response {
    if body.download_url.is_empty() {
        return err(StatusCode::BAD_REQUEST, "download_url 不能为空");
    }
    // 安全白名单：只允许从 GitHub 官方域名下载，防止服务端被灌入任意二进制。
    if !body.download_url.starts_with("https://github.com/")
        && !body.download_url.starts_with("https://objects.githubusercontent.com/")
        && !body.download_url.starts_with("https://release-assets.githubusercontent.com/")
    {
        return err(StatusCode::BAD_REQUEST, "下载地址必须来自 GitHub");
    }
    // 下载→chmod→备份→原子替换→systemctl restart 均在 update 模块内完成。
    match crate::update::apply_update(&body.download_url).await {
        Ok(()) => Json(json!({ "ok": true, "message": "更新已应用，服务即将重启" })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, format!("更新失败: {e}")),
    }
}
