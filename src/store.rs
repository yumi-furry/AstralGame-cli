//! JSON 文件持久化：服务器列表。
//! 数据文件与可执行文件放在同一目录，便于管理。

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::models::ServerEntry;

/// 获取可执行文件所在目录。
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn servers_path() -> PathBuf {
    exe_dir().join("servers.json")
}

pub fn load_servers() -> Vec<ServerEntry> {
    let path = servers_path();
    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => default_servers(),
    }
}

pub fn save_servers(list: &[ServerEntry]) -> Result<()> {
    let path = servers_path();
    let json = serde_json::to_string_pretty(list).context("序列化服务器列表失败")?;
    fs::write(&path, json).context("写入 servers.json 失败")?;
    Ok(())
}

fn default_servers() -> Vec<ServerEntry> {
    vec![ServerEntry {
        id: 1,
        name: "公共中继".into(),
        uri: "tcp://public.easytier.cn:11010".into(),
        enabled: true,
        sort_order: 0,
    }]
}

fn avatar_path() -> PathBuf {
    exe_dir().join("avatar.bin")
}

/// 读取头像原始字节；未设置/为空时返回 None。
pub fn load_avatar() -> Option<Vec<u8>> {
    fs::read(avatar_path()).ok().filter(|b| !b.is_empty())
}

/// 写入头像原始字节。
pub fn save_avatar(bytes: &[u8]) -> Result<()> {
    fs::write(avatar_path(), bytes).context("写入头像 avatar.bin 失败")?;
    Ok(())
}

/// 删除头像文件。
pub fn delete_avatar() -> Result<()> {
    let p = avatar_path();
    if p.exists() {
        fs::remove_file(&p).context("删除头像 avatar.bin 失败")?;
    }
    Ok(())
}
