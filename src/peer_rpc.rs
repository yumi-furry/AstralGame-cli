//! peer-RPC 入站处理器：响应客户端 `user.getInfo` 调用，广播本机昵称与头像。
//!
//! 协议与 AstralGame 客户端对齐：
//! - 请求 params 为 JSON（可为空），可携带 `avatarHash` 表示对端已知的头像 hash；
//! - 响应为 JSON 对象，包含 `name` / `avatarHash` / `avatar`（base64）及环境字段；
//! - 对端已知 hash 与当前一致时不回传整图（仅返回 `avatarHash`）。

use std::sync::{Arc, RwLock};

use easytier::peers::astral_app_rpc::AstralAppRpcService;

use crate::config::ServerConfig;

/// 处理一条入站 Call。channel 为 `user.getInfo` 时返回本机资料，其余返回 `-32601`。
pub async fn handle_call(
    service: &Arc<AstralAppRpcService>,
    channel: String,
    token: u64,
    payload: Vec<u8>,
    profile: &Arc<RwLock<ServerConfig>>,
) {
    if channel != "user.getInfo" {
        service.reply_call(token, -32601, "Method not found".to_string(), Vec::new());
        return;
    }

    let params: serde_json::Value = if payload.is_empty() {
        serde_json::Value::Null
    } else {
        match serde_json::from_slice(&payload) {
            Ok(v) => v,
            Err(_) => {
                service.reply_call(token, -32700, "Parse error".to_string(), Vec::new());
                return;
            }
        }
    };

    let known_hash = params
        .get("avatarHash")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let name = {
        let cfg = profile.read().unwrap();
        cfg.nickname.clone()
    };

    // 头像：读取 avatar.bin，计算 sha256；与对端已知 hash 一致时跳过整图。
    let (avatar_b64, avatar_hash) = match crate::store::load_avatar() {
        Some(bytes) => {
            use sha2::{Digest, Sha256};
            let hash = hex_string(&Sha256::digest(&bytes));
            let send = known_hash.as_deref() != Some(hash.as_str());
            let b64 = if send {
                use base64::Engine;
                Some(base64::engine::general_purpose::STANDARD.encode(&bytes))
            } else {
                None
            };
            (b64, hash)
        }
        None => (None, String::new()),
    };

    let resp = serde_json::json!({
        "name": name,
        "avatarHash": if avatar_hash.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(avatar_hash)
        },
        "avatar": avatar_b64
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
        "os": std::env::consts::OS,
        "osVersion": "Linux",
        "appName": "AstralServer",
        "appVersion": env!("CARGO_PKG_VERSION"),
        "network": "ethernet",
        "firewall": "unsupported",
        "isp": "",
    });

    let resp_payload = serde_json::to_vec(&resp).unwrap_or_default();
    service.reply_call(token, 0, String::new(), resp_payload);
}

fn hex_string(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}