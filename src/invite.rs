//! 短码与离线邀请串的生成/解析。

use anyhow::{Context, Result};
use base64::Engine;

use crate::models::RoomInvitePayload;

const OFFLINE_INVITE_PREFIX: &str = "AG1.";

/// 生成离线邀请串：AG1. + Base64urlNoPad(UTF8(JSON(payload)))
pub fn encode_offline_invite(payload: &RoomInvitePayload) -> Result<String> {
    let json = serde_json::to_string(payload).context("序列化邀请载荷失败")?;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes());
    Ok(format!("{OFFLINE_INVITE_PREFIX}{b64}"))
}

/// 解析离线邀请串。
pub fn decode_offline_invite(raw: &str) -> Result<RoomInvitePayload> {
    let raw = raw.trim();
    let b64 = raw
        .strip_prefix(OFFLINE_INVITE_PREFIX)
        .context("缺少 AG1. 前缀")?;
    let json_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(b64)
        .context("Base64url 解码失败")?;
    let payload: RoomInvitePayload =
        serde_json::from_slice(&json_bytes).context("JSON 解析失败")?;
    if payload.network_name.is_empty() || payload.network_secret.is_empty() {
        anyhow::bail!("network_name 或 network_secret 为空");
    }
    Ok(payload)
}

/// 生成随机 network_name（ag_ + 10位小写字母数字）。
pub fn generate_network_name() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let suffix: String = (0..10)
        .map(|_| {
            let idx = rng.gen_range(0..36);
            (b'a'..=b'z')
                .chain(b'0'..=b'9')
                .nth(idx)
                .unwrap() as char
        })
        .collect();
    format!("ag_{suffix}")
}

/// 生成随机 network_secret（16位，排除易混淆字符）。
pub fn generate_network_secret() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] =
        b"23456789ABCDEFGHJKMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz";
    let mut rng = rand::thread_rng();
    (0..16)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

/// 规范化短码：去空白/连字符、大写，I/L→1，O→0。
pub fn normalize_share_code(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| !matches!(c, '-' | ' ' | '_'))
        .map(|c| match c {
            'i' | 'I' | 'l' | 'L' => '1',
            'o' | 'O' => '0',
            _ => c.to_ascii_uppercase(),
        })
        .collect()
}

/// 构建分享链接。
pub fn build_join_url(short_code: Option<&str>, offline_invite: Option<&str>) -> String {
    let token = short_code
        .map(|s| s.to_string())
        .or_else(|| offline_invite.map(|s| s.to_string()));
    match token {
        Some(t) => format!("https://next.astral.fan/j?c={t}"),
        None => String::new(),
    }
}
