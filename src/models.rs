//! 数据模型：房间、服务器、邀请载荷。

use serde::{Deserialize, Serialize};

/// 房间邀请载荷（对应 AstralGame 的 RoomInvitePayload）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomInvitePayload {
    pub v: u32,
    pub game_id: String,
    pub game_name: String,
    pub network_name: String,
    pub network_secret: String,
    pub peers: Vec<PeerEndpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// 服务器条目（对应 AstralGame 的 PeerEndpoint / ServerMod）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerEndpoint {
    pub uri: String,
}

/// 服务器列表条目（含元数据）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEntry {
    pub id: u64,
    pub name: String,
    pub uri: String,
    pub enabled: bool,
    pub sort_order: u32,
}

/// 当前房间会话（内存态，不落盘）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveRoom {
    pub is_host: bool,
    pub game_id: String,
    pub game_name: String,
    pub network_name: String,
    pub network_secret: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline_invite: Option<String>,
    pub peers: Vec<PeerEndpoint>,
}

/// 已连接 WebSocket 的第三方服务（如 AstrBOT 插件）。
#[derive(Debug, Clone, Serialize)]
pub struct WsServiceInfo {
    /// 连接唯一 ID。
    pub id: String,
    /// 服务名称（连接时上报）。
    pub name: String,
    /// 服务类型（连接时上报，如 "AstrBOT"）。
    pub service_type: String,
    /// 初次连接时间（unix 秒）。
    pub first_connected_at: u64,
    /// 本次连接时间（unix 秒）。
    pub connected_at: u64,
    /// 远程地址。
    pub remote_addr: String,
}
