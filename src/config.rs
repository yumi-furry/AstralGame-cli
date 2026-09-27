use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Web 管理端 / 服务自身的配置，放在顶层 `[astral_server]` 表里。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    #[serde(default = "default_web_bind")]
    pub web_bind: String,
    #[serde(default = "default_web_port")]
    pub web_port: u16,
    /// 为空则关闭 Basic Auth。
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// tracing 日志级别（trace/debug/info/warn/error）。
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// 房间内展示的昵称（写入 EasyTier hostname，对端可见）。
    #[serde(default = "default_nickname")]
    pub nickname: String,
    /// 房间内展示的本机图标（emoji，Web 端显示）。
    #[serde(default = "default_icon")]
    pub icon: String,
    /// 是否禁用 P2P 打洞（仅走中继）。
    #[serde(default)]
    pub disable_p2p: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            web_bind: default_web_bind(),
            web_port: default_web_port(),
            username: String::new(),
            password: String::new(),
            log_level: default_log_level(),
            nickname: default_nickname(),
            icon: default_icon(),
            disable_p2p: false,
        }
    }
}

fn default_nickname() -> String {
    "astral-server".into()
}

fn default_icon() -> String {
    "🎮".into()
}

fn default_web_bind() -> String {
    "0.0.0.0".into()
}

fn default_web_port() -> u16 {
    8080
}

fn default_log_level() -> String {
    "info".into()
}

/// 从磁盘加载并解析后的配置。
pub struct LoadedConfig {
    pub server: ServerConfig,
    /// 剥离 `[astral_server]` 后、交给 EasyTier 的实例 TOML 文本。
    pub instance_toml: String,
}

pub fn load(path: &PathBuf) -> Result<LoadedConfig> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("无法读取配置文件 {}", path.display()))?;
    parse(&raw)
}

/// 解析 TOML 文本：抽出 `[astral_server]`，其余原样作为 EasyTier 实例配置。
pub fn parse(raw: &str) -> Result<LoadedConfig> {
    let value: toml::Value = toml::from_str(raw).context("配置文件不是合法 TOML")?;

    let server = match value.get("astral_server") {
        Some(s) => s.clone().try_into::<ServerConfig>().context("解析 [astral_server] 失败")?,
        None => ServerConfig::default(),
    };

    let mut easytier_value = value;
    if let toml::Value::Table(t) = &mut easytier_value {
        t.remove("astral_server");
    }
    let instance_toml = toml::to_string_pretty(&easytier_value)?;

    Ok(LoadedConfig {
        server,
        instance_toml,
    })
}

/// 将 [ServerConfig] 写回 config.toml 的 `[astral_server]` 节，其余 EasyTier 内容原样保留。
pub fn save(path: &Path, server: &ServerConfig) -> Result<()> {
    let raw = fs::read_to_string(path).unwrap_or_default();
    let mut value: toml::Value = if raw.trim().is_empty() {
        toml::Value::Table(Default::default())
    } else {
        toml::from_str(&raw).context("配置文件不是合法 TOML")?
    };
    let table = value
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("配置根节点不是表"))?;
    let server_value: toml::Value =
        toml::Value::try_from(server).context("序列化 ServerConfig 失败")?;
    table.insert("astral_server".to_string(), server_value);
    let out = toml::to_string_pretty(&value).context("序列化配置失败")?;
    fs::write(path, out).context("写入配置文件失败")?;
    Ok(())
}

pub fn write_example_config(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(path, EXAMPLE_CONFIG)?;
    Ok(())
}

const EXAMPLE_CONFIG: &str = r#"# =============================================================================
# Astral Game 服务器（CLI + Web 管理端）配置
# 除 [astral_server] 之外的所有内容都是标准 EasyTier 配置，会原样传给内核。
# =============================================================================

[astral_server]
# Web 管理端监听地址与端口（端口在此处本地配置文件中修改即可）
web_bind = "0.0.0.0"
web_port = 8080
# Basic Auth（留空则不启用鉴权，请务必在生产环境设置）
username = "admin"
password = "change-me-please"
# 日志级别：trace / debug / info / warn / error
log_level = "info"
# 房间内展示的昵称（写入 EasyTier hostname，对端客户端可见）
nickname = "astral-server"
# 房间内展示的本机图标（emoji，Web 端显示）
icon = "🎮"
# 是否禁用 P2P 打洞（仅走中继，谨慎开启）
disable_p2p = false

# -----------------------------------------------------------------------------
# 以下是 EasyTier 节点配置
# -----------------------------------------------------------------------------
instance_name = "astral-server"
hostname = "astral-server"

# dhcp = true 时由网络自动分配虚拟 IP；false 则使用下方固定 ipv4
dhcp = true
# ipv4 = "10.10.10.1"

# 本机监听地址（0.0.0.0 对外开放；换成 127.0.0.1 仅本机；端口 0 表示随机）
listeners = [
    "tcp://0.0.0.0:0",
    "udp://0.0.0.0:0",
]

# 组网身份：同网络的节点 network_name / network_secret 必须一致
[network_identity]
network_name = "astral-net"
network_secret = "please-change-this-secret"

# 可选的公共服务器（中继 / 打洞）。客户端与服务器共享这些 peer。
# [[peer]]
# uri = "tcp://public.easytier.cn:11010"

# EasyTier 内核开关（与原版客户端对齐）
[flags]
data_compress_algo = 2
default_protocol = "tcp"
dev_name = "astral0"
disable_kcp_input = true
disable_relay_kcp = true
enable_relay_foreign_network_kcp = false
disable_quic_input = true
disable_relay_quic = true
enable_relay_foreign_network_quic = false
# 若使用固定 IP 且关闭 DHCP，请把 dhcp 设为 false 并填写 ipv4
"#;