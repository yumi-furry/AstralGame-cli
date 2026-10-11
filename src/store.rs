//! ==== 本地文件持久化模块 ====
//! JSON 文件持久化：服务器列表 servers.json 与头像 avatar.bin。
//! 所有数据文件与可执行文件放在同一目录，单目录部署、便于备份与打包。

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::models::ServerEntry;

/// 获取可执行文件所在目录；取不到（极少数环境）时回退为当前目录 "."。
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// servers.json 的完整路径（exe 同目录）。
fn servers_path() -> PathBuf {
    exe_dir().join("servers.json")
}

/// 读取服务器列表。
/// 文件不存在（首次运行）→ 返回内置默认列表；
/// 文件存在但内容损坏 → serde 反序列化失败时返回空列表（unwrap_or_default），
/// 避免坏数据导致整个面板不可用。
pub fn load_servers() -> Vec<ServerEntry> {
    let path = servers_path();
    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => default_servers(),
    }
}

/// 全量覆盖写入服务器列表（pretty JSON）。
pub fn save_servers(list: &[ServerEntry]) -> Result<()> {
    let path = servers_path();
    let json = serde_json::to_string_pretty(list).context("序列化服务器列表失败")?;
    fs::write(&path, json).context("写入 servers.json 失败")?;
    Ok(())
}

/// 首次运行的内置服务器：EasyTier 官方公共中继。
fn default_servers() -> Vec<ServerEntry> {
    vec![ServerEntry {
        id: 1,
        name: "公共中继".into(),
        uri: "tcp://public.easytier.cn:11010".into(),
        enabled: true,
        sort_order: 0,
    }]
}

/// avatar.bin 的完整路径（原始图片字节，≤512KB，格式不限由前端识别）。
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
