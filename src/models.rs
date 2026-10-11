//! =============================================================================
//! 数据模型：房间、服务器条目、邀请载荷、第三方服务信息。
//!
//! 这些结构承担三类用途：
//! 1. 作为 REST API 的 JSON 响应/请求体（Serialize/Deserialize）；
//! 2. 作为 AG1. 离线邀请串的编解码内容（[`RoomInvitePayload`]）；
//! 3. 作为内存中当前房间会话（[`ActiveRoom`]，不落盘）。
//! =============================================================================

use serde::{Deserialize, Serialize};

/// 房间邀请载荷（对应 AstralGame 的 RoomInvitePayload）。
/// 创建房间时生成并编码为 AG1. 离线串 / 提交给短码服务；加入房间时由其还原。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomInvitePayload {
    /// 载荷格式版本（当前固定为 1，供客户端做兼容判断）。
    pub v: u32,
    /// 游戏 ID（如 minecraft / grand_theft_auto_v / custom）。
    pub game_id: String,
    /// 游戏显示名称。
    pub game_name: String,
    /// EasyTier 网络名（同网络节点必须一致）。
    pub network_name: String,
    /// EasyTier 网络密钥（同网络节点必须一致）。
    pub network_secret: String,
    /// 入网用的中继/对等节点端点列表。
    pub peers: Vec<PeerEndpoint>,
    /// 可选的房主展示名（缺省时客户端用游戏名兜底）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// 服务器条目（对应 AstralGame 的 PeerEndpoint / ServerMod）。
/// 序列化为邀请载荷中的一个 `{ uri }` 对象。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerEndpoint {
    /// EasyTier 监听 URI，如 "tcp://1.2.3.4:11010"。
    pub uri: String,
}

/// 服务器列表条目（含本地管理用的元数据，持久化在 servers.json）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEntry {
    /// 本地自增主键。
    pub id: u64,
    /// 用户可读名称。
    pub name: String,
    /// EasyTier 监听 URI（含协议前缀）。
    pub uri: String,
    /// 是否启用：只有启用的服务器才会被写入新建房间的 peers。
    pub enabled: bool,
    /// 排序序号（当前 UI 按添加顺序使用）。
    pub sort_order: u32,
}

/// 当前房间会话（内存态，不落盘）。
/// 服务重启后清空，由守护进程的自动创建/加入策略重建。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveRoom {
    /// 本机是否为房主（创建房间为 true，加入房间为 false）。
    pub is_host: bool,
    /// 游戏 ID。
    pub game_id: String,
    /// 游戏名称。
    pub game_name: String,
    /// EasyTier 网络名。
    pub network_name: String,
    /// EasyTier 网络密钥。
    pub network_secret: String,
    /// 房间展示名（payload.display_name 缺省时取游戏名）。
    pub display_name: String,
    /// 短码服务返回的短码；服务不可用时为 None（前端降级显示离线串）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_code: Option<String>,
    /// AG1. 开头的离线邀请串（始终可用，不依赖短码服务在线）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline_invite: Option<String>,
    /// 本房间使用的中继/对等节点端点。
    pub peers: Vec<PeerEndpoint>,
}

/// 已连接 WebSocket 的第三方服务（如 AstrBOT 插件）。
/// 由 `/api/ws/service` 端点在握手成功后登记，面板"第三方服务"列表展示。
#[derive(Debug, Clone, Serialize)]
pub struct WsServiceInfo {
    /// 连接唯一 ID（每次连接新生成的 UUID v4）。
    pub id: String,
    /// 服务名称（连接时通过 query 参数上报）。
    pub name: String,
    /// 服务类型（连接时通过 query 参数上报，如 "AstrBOT"）。
    pub service_type: String,
    /// 首次连接时间（unix 秒；重连时保留最早值由注册表维护，当前实现取本次值）。
    pub first_connected_at: u64,
    /// 本次连接时间（unix 秒）。
    pub connected_at: u64,
    /// 远程 TCP 地址（ip:port）。
    pub remote_addr: String,
}
