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
    /// 是否创建 TUN 虚拟网卡（参与 MC 局域网发现等需要虚拟 IP 的场景）。
    #[serde(default = "default_enable_tun")]
    pub enable_tun: bool,
    /// 启用 TUN 时可选的静态虚拟 IP（CIDR，如 "10.126.0.1/24"）；为空则走 DHCP。
    #[serde(default)]
    pub static_ipv4: String,
    /// 守护进程：服务启动后自动加入预设房间。
    #[serde(default)]
    pub auto_join_enabled: bool,
    /// 自动加入使用的邀请：短码 / 完整链接 / AG1. 离线串。
    #[serde(default)]
    pub auto_join_code: String,
    /// 守护进程：服务启动后自动创建预设房间（与自动加入二选一）。
    #[serde(default)]
    pub auto_create_enabled: bool,
    /// 自动创建使用的预设游戏 ID。
    #[serde(default)]
    pub auto_create_game_id: String,
    /// 自动创建使用的预设游戏名称。
    #[serde(default)]
    pub auto_create_game_name: String,
    /// SMTP 邮件通知配置（服务器参数写配置文件，收件人可在面板维护）。
    #[serde(default)]
    pub smtp: SmtpConfig,
    /// WebSocket 第三方服务连接密钥（留空则启动时自动生成随机密钥）。
    #[serde(default)]
    pub ws_key: String,
}

/// SMTP 发信配置。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SmtpConfig {
    /// 总开关（关闭则不发信）。
    #[serde(default)]
    pub enabled: bool,
    /// SMTP 服务器主机，如 "smtp.qq.com"。
    #[serde(default)]
    pub host: String,
    /// SMTP 端口（TLS 一般 465，STARTTLS 一般 587）。
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    /// 登录用户名（一般与发件邮箱一致）。
    #[serde(default)]
    pub username: String,
    /// 登录密码 / 授权码。
    #[serde(default)]
    pub password: String,
    /// 发件人显示名称。
    #[serde(default = "default_from_name")]
    pub from_name: String,
    /// 发件邮箱地址。
    #[serde(default)]
    pub from_email: String,
    /// 加密方式："tls"（隐式 TLS/465）/ "starttls"（587）/ "none"（不加密，25）。
    #[serde(default = "default_smtp_encryption")]
    pub encryption: String,
    /// 收件人邮箱列表（面板可维护，写回配置文件）。
    #[serde(default)]
    pub recipients: Vec<String>,
}

impl Default for SmtpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: default_smtp_port(),
            username: String::new(),
            password: String::new(),
            from_name: default_from_name(),
            from_email: String::new(),
            encryption: default_smtp_encryption(),
            recipients: Vec::new(),
        }
    }
}

fn default_smtp_port() -> u16 {
    465
}

fn default_from_name() -> String {
    "Astral Server".into()
}

fn default_smtp_encryption() -> String {
    "tls".into()
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
            enable_tun: default_enable_tun(),
            static_ipv4: String::new(),
            auto_join_enabled: false,
            auto_join_code: String::new(),
            auto_create_enabled: false,
            auto_create_game_id: String::new(),
            auto_create_game_name: String::new(),
            smtp: SmtpConfig::default(),
            ws_key: String::new(),
        }
    }
}

/// 生成 WebSocket 第三方服务连接密钥（32 位十六进制随机串）。
pub fn generate_ws_key() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn default_enable_tun() -> bool {
    true
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
# 是否创建 TUN 虚拟网卡（参与 MC 局域网发现等需要虚拟 IP 的场景）
enable_tun = true
# 启用 TUN 时的静态虚拟 IP（CIDR 格式，如 "10.126.0.1/24"）；留空则走 DHCP
static_ipv4 = ""
# 守护进程：系统启动（服务启动）后自动加入下面预设的房间
auto_join_enabled = false
# 自动加入的邀请：短码 / 完整邀请链接 / AG1. 离线串
auto_join_code = ""

# 守护进程（二选一）：系统启动后自动创建下面预设的游戏房间。
# 若设置了 auto_join_enabled = true 则优先执行"自动加入"，此项忽略。
auto_create_enabled = false
# 自动创建使用的预设游戏 ID（如我的世界 minecraft，与面板创建房间所选 ID 一致）
auto_create_game_id = ""
# 自动创建使用的预设游戏名称（面板展示用，与游戏 ID 对应）
auto_create_game_name = ""

# -----------------------------------------------------------------------------
# SMTP 邮件通知：服务器重启或房间码变动时，自动把房间内容与房间码发给收件人。
# 收件人列表也可以在 Web 面板「设置 → 邮件通知」里维护。
# -----------------------------------------------------------------------------
[astral_server.smtp]
enabled = false                  # 总开关
host = "smtp.qq.com"             # SMTP 服务器（QQ: smtp.qq.com / 163: smtp.163.com / Gmail: smtp.gmail.com）
port = 465                       # 隐式 TLS 用 465；STARTTLS 用 587
username = "your_mail@qq.com"    # 登录账号（一般为发件邮箱）
password = "授权码"               # 注意是 SMTP 授权码，不是邮箱登录密码
from_name = "Astral Server"      # 发件人显示名称
from_email = "your_mail@qq.com"  # 发件邮箱
encryption = "tls"               # 加密方式：tls / starttls / none
recipients = ["a@example.com"]   # 收件人列表（可在面板编辑）

# -----------------------------------------------------------------------------
# WebSocket 第三方服务连接密钥
# 第三方软件（如 AstrBOT 插件）通过 /api/ws/service?key=...&name=...&type=... 连接，
# 连接成功后会出现在面板「设置 → 第三方服务」列表中。留空则启动时自动生成随机密钥。
# -----------------------------------------------------------------------------
# ws_key = ""

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