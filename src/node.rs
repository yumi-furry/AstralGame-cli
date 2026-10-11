//! =============================================================================
//! 无头节点运行时：封装 EasyTier NetworkInstanceManager。
//!
//! 对外能力：
//! * 实例生命周期：start / stop / restart（start 内部会先 stop，保证"重入即重启"）；
//! * 事件日志：订阅内核 EventBus，转成环形缓冲（上限 800 条）供面板拉取；
//! * 状态快照：后台任务每 2 秒把网络信息转换成前端 DTO 写入 RwLock；
//! * 进网凭据：通过实例 RPC 生成 / 列出 / 撤销凭据；
//! * peer-RPC：订阅应用层入站调用，向房间客户端回应昵称与头像；
//! * Linux 网卡修复：等待 TUN 出现后把组播/广播路由指过去并压低 metric，
//!   保证我的世界等游戏的局域网发现可用（对齐上游 v1.4.2 的 Windows 跃点修复）。
//! =============================================================================

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use uuid::Uuid;

use easytier::common::config::{
    process_secure_mode_cfg, ConfigFileControl, ConfigLoader, TomlConfigLoader,
};
use easytier::common::global_ctx::{EventBusSubscriber, GlobalCtxEvent};
use easytier::instance_manager::NetworkInstanceManager;
use easytier::proto::api::instance::{
    GenerateCredentialRequest, ListCredentialsRequest, RevokeCredentialRequest,
};
use easytier::proto::rpc_types::controller::BaseController;
use easytier::rpc_service::InstanceRpcService;

use crate::config::ServerConfig;
use crate::status::{self, NetworkStatus};

/// 内存日志环形缓冲的最大条数（超出后最旧日志被挤出）。
const LOG_CAP: usize = 800;

/// 单条节点日志（面板 /api/logs 返回的元素）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogEntry {
    /// 时间戳（unix 秒）。
    pub ts: u64,
    /// 级别：info / warn / error。
    pub level: String,
    /// 日志正文。
    pub msg: String,
}

/// 节点状态快照：由后台轮询任务持续刷新，/api/status 直接序列化返回。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Snapshot {
    /// 内核实例是否在运行。
    pub running: bool,
    /// 当前实例 UUID（字符串化），无实例时为 None。
    pub instance_id: Option<String>,
    /// EasyTier 内核版本号。
    pub easytier_version: String,
    /// astral-server 自身版本号。
    pub server_version: String,
    /// 本机虚拟 IPv4（去掉 CIDR 后缀），未分配为 None。
    pub my_ipv4: Option<String>,
    /// 网络内节点总数（含本机合成节点）。
    pub total_nodes: usize,
    /// 节点详情列表。
    pub nodes: Vec<status::NodeInfo>,
    /// 内核上报的最近错误信息（无错误为 None）。
    pub error: Option<String>,
}

impl Snapshot {
    /// 构造"未运行"状态的空快照（版本号照常填充，便于首屏展示）。
    fn idle() -> Self {
        Self {
            running: false,
            instance_id: None,
            easytier_version: easytier_version(),
            server_version: crate::update::current_version(),
            my_ipv4: None,
            total_nodes: 0,
            nodes: Vec::new(),
            error: None,
        }
    }
}

/// 新生成的进网凭据（含可由私钥推导出的 x25519 公钥，便于展示/分发）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct GeneratedCredential {
    /// 凭据 ID。
    pub credential_id: String,
    /// 凭据密文（base64 的 32 字节 x25519 私钥，客户端入网时使用）。
    pub credential_secret: String,
    /// 由密文推导出的公钥（base64）。
    pub pubkey_b64: String,
}

/// 已存在凭据的展示信息。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CredentialInfo {
    /// 凭据 ID。
    pub credential_id: String,
    /// 所属分组。
    pub groups: Vec<String>,
    /// 是否允许作为中继。
    pub allow_relay: bool,
    /// 过期时间（unix 秒）。
    pub expiry_unix: i64,
    /// 是否可复用（false 为单次凭据）。
    pub reusable: bool,
}

/// 节点运行时（整个服务期间单例，Arc 共享）。
pub struct NodeRuntime {
    /// EasyTier 实例管理器（可同时管多个实例，本服务同一时刻只跑一个）。
    manager: Arc<NetworkInstanceManager>,
    /// 当前运行中的实例 ID（None 表示未运行）。
    current_id: Mutex<Option<Uuid>>,
    /// 最近一次启动使用的 TOML（restart 时原样重放）。
    toml: Mutex<Option<String>>,
    /// 节点事件日志环形缓冲。
    logs: Arc<Mutex<VecDeque<LogEntry>>>,
    /// 最近一次状态快照（轮询任务写，API 读）。
    snapshot: Arc<RwLock<Snapshot>>,
    /// 服务配置（peer-RPC 回应昵称时读，与 AppState 共享同一份）。
    profile: Arc<RwLock<ServerConfig>>,
    /// peer-RPC 入站处理任务句柄（stop 时 abort）。
    rpc_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl NodeRuntime {
    /// 创建运行时单例（不启动任何网络实例）。
    pub fn new(profile: Arc<RwLock<ServerConfig>>) -> Arc<Self> {
        Arc::new(Self {
            manager: Arc::new(NetworkInstanceManager::new()),
            current_id: Mutex::new(None),
            toml: Mutex::new(None),
            logs: Arc::new(Mutex::new(VecDeque::new())),
            snapshot: Arc::new(RwLock::new(Snapshot::idle())),
            profile,
            rpc_task: Mutex::new(None),
        })
    }

    /// 启动（或重启）实例。传入剥离 [astral_server] 后的 EasyTier 配置 TOML。
    /// 成功返回实例 ID 字符串；任何阶段失败都以 Err(String) 返回给调用方（API 层转 400/500）。
    pub async fn start(&self, config_toml: String) -> Result<String, String> {
        // 先清理旧实例：重复调用 start 等价于"用新配置重启"。
        self.stop().await;

        // 解析 TOML 为内核配置加载器。
        let cfg = TomlConfigLoader::new_from_str(&config_toml).map_err(|e| e.to_string())?;
        // secure_mode：启用时由内核生成本地密钥对（凭据组网相关），需先处理再运行实例。
        if let Some(sm) = cfg.get_secure_mode() {
            if sm.enabled {
                let processed = process_secure_mode_cfg(sm).map_err(|e| e.to_string())?;
                cfg.set_secure_mode(Some(processed));
                self.push_log("info", "secure_mode: 本地密钥已生成/加载");
            }
        }

        // dev_name 要在 run 之前取出（cfg 随后被管理器接管），供网卡路由修复任务使用。
        let dev_name = cfg.get_flags().dev_name.clone();
        // 创建并运行网络实例；第二个参数表示阻塞等待部分初始化，STATIC_CONFIG 表示配置不再热加载。
        let id = self
            .manager
            .run_network_instance(cfg, true, ConfigFileControl::STATIC_CONFIG)
            .map_err(|e| format!("启动实例失败: {e}"))?;

        // 对齐上游 v1.4.2 的网卡跃点修复（Linux 等价：组播/广播路由指向 TUN 并压低 metric）。
        #[cfg(target_os = "linux")]
        self.spawn_nic_route_fix_task(dev_name);
        // 非 Linux 平台避免 dev_name 未使用告警。
        #[cfg(not(target_os = "linux"))]
        let _ = dev_name;

        // 记录当前实例与配置（stop/restart 依赖它们）。
        *self.current_id.lock().unwrap() = Some(id);
        *self.toml.lock().unwrap() = Some(config_toml);
        self.push_log("info", format!("实例已创建 instance_id={id}"));

        // 订阅该实例的内核事件总线，转发到内存日志缓冲。
        if let Some(sub) = self
            .manager
            .iter()
            .find(|it| *it.key() == id)
            .and_then(|it| it.value().subscribe_event())
        {
            let logs = self.logs.clone();
            tokio::spawn(async move {
                forward_events(sub, logs).await;
            });
        }

        // 启动应用层 peer-RPC 监听与 2 秒一次的状态轮询。
        self.spawn_app_rpc(id).await;
        self.spawn_poll(id).await;
        Ok(id.to_string())
    }

    /// 上游 v1.4.2 修复了 Windows wintun 跃点偏高导致 MC 局域网发现失败的问题。
    /// Linux 上等价问题：组播（MC 发现用 224.0.2.60）与受限广播默认走默认路由接口，
    /// 不会进入 TUN 虚拟网卡，房间内其他成员收不到。
    /// 这里等虚拟网卡出现后，把组播/广播路由指向它并压 metric=1（合法最小值）。
    /// 网卡由 EasyTier 异步创建，run_network_instance 返回时可能尚未出现，需轮询等待；
    /// 设置失败只记日志，不影响启动流程。
    #[cfg(target_os = "linux")]
    fn spawn_nic_route_fix_task(&self, dev_name: String) {
        // dev_name 为空（no_tun 场景）时无事可做。
        if dev_name.is_empty() {
            return;
        }
        let logs = self.logs.clone();
        tokio::spawn(async move {
            // 最多等 15 秒（30 次 × 500ms）。
            let max_attempts = 30u32;
            for attempt in 1..=max_attempts {
                // spawn_blocking：ip 命令是阻塞子进程调用，不能堵异步运行时线程。
                let dev = dev_name.clone();
                let exists = tokio::task::spawn_blocking(move || nic_exists(&dev))
                    .await
                    .unwrap_or(false);
                if exists {
                    // 网卡已出现：写入组播/广播路由。
                    let dev = dev_name.clone();
                    let ok = tokio::task::spawn_blocking(move || apply_mcast_routes(&dev))
                        .await
                        .unwrap_or(false);
                    if ok {
                        push_log_into(
                            &logs,
                            "info",
                            format!("nic {dev_name}: 组播/广播路由已指向虚拟网卡 (metric 1)"),
                        );
                    } else {
                        push_log_into(
                            &logs,
                            "warn",
                            format!("nic {dev_name}: 组播路由设置失败，MC 局域网发现可能不可用"),
                        );
                    }
                    return;
                }
                if attempt < max_attempts {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
            push_log_into(
                &logs,
                "warn",
                format!("nic {dev_name}: 等待网卡出现超时，跳过组播路由设置"),
            );
        });
    }

    /// 后台状态轮询任务：实例被删除时退出；否则每 2 秒刷新一次 [`Snapshot`]。
    async fn spawn_poll(&self, id: Uuid) {
        let manager = self.manager.clone();
        let snapshot = self.snapshot.clone();
        tokio::spawn(async move {
            loop {
                // 实例已从管理器消失（被 stop/异常退出）：标记未运行并结束轮询。
                if !manager.list_network_instance_ids().contains(&id) {
                    if let Ok(mut s) = snapshot.write() {
                        s.running = false;
                    }
                    break;
                }
                let running = manager.iter().any(|it| it.value().is_easytier_running());
                // 读旧快照做底（保留版本号等不变字段），再覆盖运行态字段。
                let mut snap = snapshot.read().map(|s| s.clone()).unwrap_or_else(|_| Snapshot::idle());
                snap.running = running;
                snap.instance_id = Some(id.to_string());
                // 拉取网络信息并转换成前端节点列表 DTO。
                if let Some(info) = manager.get_network_info(&id).await {
                    snap.error = info.error_msg.clone();
                    let st = status::network_status_from_info(info);
                    snap.my_ipv4 = node_ipv4(&st);
                    snap.total_nodes = st.total_nodes;
                    snap.nodes = st.nodes;
                }
                if let Ok(mut s) = snapshot.write() {
                    *s = snap;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
    }

    /// 订阅应用层 peer-RPC 入站事件，响应客户端 `user.getInfo`（广播昵称/头像）。
    async fn spawn_app_rpc(&self, id: Uuid) {
        // app-rpc 服务随实例异步注册，这里最多等 5 秒；等不到就跳过（不影响组网本体）。
        let service = {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                match easytier::peers::astral_app_rpc::get_service(&id) {
                    Some(s) => break s,
                    None if std::time::Instant::now() >= deadline => {
                        self.push_log("warn", "app rpc 服务未就绪，头像广播暂不可用");
                        return;
                    }
                    None => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        };

        let mut rx = service.subscribe_inbound();
        let profile = self.profile.clone();
        // 长驻任务：逐条处理入站事件，直到通道关闭。
        let handle = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    // 对端发起 RPC 调用：交给 peer_rpc 模块应答。
                    Ok(easytier::peers::astral_app_rpc::AppInboundEvent::Call {
                        channel,
                        token,
                        payload,
                        ..
                    }) => {
                        crate::peer_rpc::handle_call(&service, channel, token, payload, &profile)
                            .await;
                    }
                    Ok(_) => {} // Notify 类事件：本服务不需要
                    // 广播通道关闭：实例销毁，任务退出。
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    // 处理速度跟不上时丢掉积压消息，保持存活。
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                }
            }
        });
        *self.rpc_task.lock().unwrap() = Some(handle);
    }

    /// 停止当前实例并清空运行态（幂等：未运行时直接复位快照）。
    pub async fn stop(&self) {
        // 先终止 peer-RPC 任务，避免实例删除后还在引用服务。
        if let Some(h) = self.rpc_task.lock().unwrap().take() {
            h.abort();
        }
        // 取出并清空当前实例 ID，再请求内核删除实例（停监听、删 TUN、断连接）。
        let id = { self.current_id.lock().unwrap().take() };
        if let Some(id) = id {
            if let Err(e) = self.manager.delete_network_instance(vec![id]) {
                self.push_log("error", format!("停止实例失败: {e}"));
            } else {
                self.push_log("info", format!("实例已停止 instance_id={id}"));
            }
        }
        // 快照复位为空态（版本号字段保留）。
        if let Ok(mut s) = self.snapshot.write() {
            s.running = false;
            s.instance_id = None;
            s.my_ipv4 = None;
            s.total_nodes = 0;
            s.nodes.clear();
            s.error = None;
        }
    }

    /// 用最近一次的配置重启实例；从未启动过时返回错误。
    pub async fn restart(&self) -> Result<String, String> {
        let toml = self.toml.lock().unwrap().clone();
        match toml {
            Some(t) => self.start(t).await,
            None => Err("没有已加载的实例配置".into()),
        }
    }

    /// 取当前状态快照的克隆（API 每 2 秒轮询一次，代价很小）。
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.read().map(|s| s.clone()).unwrap_or_else(|_| Snapshot::idle())
    }

    /// 取最近 tail 条日志（环形缓冲里不足时全量返回）。
    pub fn logs(&self, tail: usize) -> Vec<LogEntry> {
        let logs = self.logs.lock().unwrap();
        let skip = logs.len().saturating_sub(tail);
        logs.iter().skip(skip).cloned().collect()
    }

    /// 写一条节点日志到环形缓冲。
    fn push_log(&self, level: &str, msg: impl Into<String>) {
        push_log_into(&self.logs, level, msg);
    }

    /// 获取当前实例的 RPC 服务句柄（凭据管理走它）。
    /// 实例刚启动时 RPC 可能尚未注册，因此以 50ms 间隔轮询，最多等 5 秒。
    async fn instance_rpc(&self) -> Result<Arc<dyn InstanceRpcService>, String> {
        let id = { *self.current_id.lock().unwrap() };
        let id = id.ok_or("实例未运行")?;
        let step = Duration::from_millis(50);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(svc) = self.manager.get_instance_service(&id) {
                return Ok(svc);
            }
            if std::time::Instant::now() >= deadline {
                return Err("实例 RPC 服务尚未就绪".into());
            }
            tokio::time::sleep(step).await;
        }
    }

    /// 生成一张进网凭据。ttl_seconds 为有效期秒数；reusable=false 表示单次凭据。
    pub async fn generate_credential(
        &self,
        ttl_seconds: i64,
        reusable: bool,
    ) -> Result<GeneratedCredential, String> {
        if ttl_seconds <= 0 {
            return Err("ttl_seconds 必须大于 0".into());
        }
        let svc = self.instance_rpc().await?;
        // groups / proxy_cidrs 留空：不做分组与代播网段限制；allow_relay 允许凭据节点走中继。
        let resp = svc
            .get_credential_manage_service()
            .generate_credential(
                BaseController::default(),
                GenerateCredentialRequest {
                    groups: vec![],
                    allow_relay: true,
                    allowed_proxy_cidrs: vec![],
                    ttl_seconds,
                    credential_id: None,
                    instance: None,
                    reusable: Some(reusable),
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        // 额外推导公钥返回，方便面板直接展示。
        let pubkey_b64 = pubkey_b64_from_credential_secret(&resp.credential_secret)?;
        Ok(GeneratedCredential {
            credential_id: resp.credential_id,
            credential_secret: resp.credential_secret,
            pubkey_b64,
        })
    }

    /// 按 ID 撤销凭据（已分发的凭据立即失效），返回内核是否报告成功。
    pub async fn revoke_credential(&self, credential_id: String) -> Result<bool, String> {
        let svc = self.instance_rpc().await?;
        svc.get_credential_manage_service()
            .revoke_credential(
                BaseController::default(),
                RevokeCredentialRequest {
                    credential_id,
                    instance: None,
                },
            )
            .await
            .map(|r| r.success)
            .map_err(|e| e.to_string())
    }

    /// 列出实例上全部凭据（面板"进网凭据"卡片使用）。
    pub async fn list_credentials(&self) -> Result<Vec<CredentialInfo>, String> {
        let svc = self.instance_rpc().await?;
        let resp = svc
            .get_credential_manage_service()
            .list_credentials(BaseController::default(), ListCredentialsRequest { instance: None })
            .await
            .map_err(|e| e.to_string())?;
        // 内核类型 → 前端 DTO；reusable 缺省按可复用处理（兼容旧内核）。
        Ok(resp
            .credentials
            .into_iter()
            .map(|c| CredentialInfo {
                credential_id: c.credential_id,
                groups: c.groups,
                allow_relay: c.allow_relay,
                expiry_unix: c.expiry_unix,
                reusable: c.reusable.unwrap_or(true),
            })
            .collect())
    }
}

/// 检查指定名称的网卡是否已存在（`ip link show dev <name>`）。
#[cfg(target_os = "linux")]
fn nic_exists(dev: &str) -> bool {
    std::process::Command::new("ip")
        .args(["link", "show", "dev", dev])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 写入组播/广播路由（`ip route replace ... dev <tun> metric 1`）。
/// 组播是关键路径（MC 局域网发现走 224.0.2.60）；受限广播尽力而为，失败不影响结果判定。
/// 返回值只取决于组播路由是否成功。
#[cfg(target_os = "linux")]
fn apply_mcast_routes(dev: &str) -> bool {
    // replace：路由已存在则覆盖，避免重复添加报错；metric 1 是合法最小值，优先走 TUN。
    let replace = |dst: &str| -> bool {
        std::process::Command::new("ip")
            .args(["route", "replace", dst, "dev", dev, "metric", "1"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    // 224.0.0.0/4 覆盖全部组播（含 224.0.2.60）；255.255.255.255/32 为受限广播。
    let mcast = replace("224.0.0.0/4");
    let _bcast = replace("255.255.255.255/32");
    mcast
}

/// 从网络状态中找本机合成节点（peer_id == 0）的虚拟 IPv4，去掉 "/24" 后缀。
fn node_ipv4(st: &NetworkStatus) -> Option<String> {
    st.nodes
        .iter()
        .find(|n| n.peer_id == status::LOCAL_SYNTHETIC_PEER_ID)
        .map(|n| n.ipv4.split('/').next().unwrap_or("").to_string())
        .filter(|s| !s.is_empty())
}

/// EasyTier 内核版本号（编译期常量）。
fn easytier_version() -> String {
    easytier::VERSION.to_string()
}

/// 向环形缓冲追加一条日志并在超限时丢弃最旧条目（自由函数，便于任务线程共享调用）。
fn push_log_into(logs: &Mutex<VecDeque<LogEntry>>, level: &str, msg: impl Into<String>) {
    let mut logs = logs.lock().unwrap();
    logs.push_back(LogEntry {
        ts: now_unix(),
        level: level.into(),
        msg: msg.into(),
    });
    while logs.len() > LOG_CAP {
        logs.pop_front();
    }
}

/// 当前 unix 秒时间戳（系统时钟异常时退化为 0）。
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 内核事件总线转发任务：把 GlobalCtxEvent 翻译成中文/英文日志行写入缓冲，直到订阅关闭。
async fn forward_events(mut events: EventBusSubscriber, logs: Arc<Mutex<VecDeque<LogEntry>>>) {
    loop {
        match events.recv().await {
            Ok(e) => {
                // None 表示该事件被刻意忽略（如内部 ring listener、ConfigPatched 噪音）。
                if let Some(msg) = event_to_message(e) {
                    push_log_into(&logs, "info", msg);
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                push_log_into(&logs, "warn", format!("事件滞后，丢弃 {n} 条"));
            }
        }
    }
}

/// 凭据密文 → x25519 公钥的 base64。
/// 密文是 32 字节私钥的 base64；用 x25519-dalek 由私钥推导公钥供面板展示。
fn pubkey_b64_from_credential_secret(secret_b64: &str) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(secret_b64.trim())
        .map_err(|e| format!("无效的 credential_secret base64: {e}"))?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| "credential_secret 必须为 32 字节".to_string())?;
    let private = x25519_dalek::StaticSecret::from(arr);
    let public = x25519_dalek::PublicKey::from(&private);
    Ok(base64::engine::general_purpose::STANDARD.encode(public.as_bytes()))
}

/// 把一条连接信息格式化为日志字符串（peer 连接增删事件复用）。
fn peer_conn_info_to_string(p: easytier::proto::api::instance::PeerConnInfo) -> String {
    format!(
        "my_peer_id: {}, dst_peer_id: {}, tunnel_info: {:?}",
        p.my_peer_id, p.peer_id, p.tunnel
    )
}

/// 将内核事件翻译为可展示日志；返回 None 表示该事件忽略不入日志。
fn event_to_message(e: GlobalCtxEvent) -> Option<String> {
    match e {
        // ---- 节点与连接拓扑变化 ----
        GlobalCtxEvent::PeerAdded(p) => Some(format!("peer added. peer_id: {p}")),
        GlobalCtxEvent::PeerRemoved(p) => Some(format!("peer removed. peer_id: {p}")),
        GlobalCtxEvent::PeerConnAdded(p) => {
            Some(format!("peer connection added. conn_info: {}", peer_conn_info_to_string(p)))
        }
        GlobalCtxEvent::PeerConnRemoved(p) => {
            Some(format!("peer connection removed. conn_info: {}", peer_conn_info_to_string(p)))
        }
        // ---- 本地监听器 ----
        GlobalCtxEvent::ListenerAddFailed(p, msg) => {
            Some(format!("listener add failed. listener: {p}, msg: {msg}"))
        }
        GlobalCtxEvent::ListenerAcceptFailed(p, msg) => {
            Some(format!("listener accept failed. listener: {p}, msg: {msg}"))
        }
        GlobalCtxEvent::ListenerAdded(p) => {
            // ring 通道是内核内部 IPC，对用户无意义，忽略。
            if p.scheme() == "ring" {
                None
            } else {
                Some(format!("listener added. listener: {p}"))
            }
        }
        // ---- 入站连接 ----
        GlobalCtxEvent::ConnectionAccepted(local, remote) => {
            Some(format!("connection accepted. local: {local}, remote: {remote}"))
        }
        GlobalCtxEvent::ConnectionError(local, remote, err) => {
            Some(format!("connection error. local: {local}, remote: {remote}, err: {err}"))
        }
        // ---- TUN 虚拟网卡 ----
        GlobalCtxEvent::TunDeviceReady(dev) => Some(format!("tun device ready. dev: {dev}")),
        GlobalCtxEvent::TunDeviceError(err) => Some(format!("tun device error. err: {err}")),
        // ---- 主动拨号对端 ----
        GlobalCtxEvent::Connecting(dst) => Some(format!("connecting to peer. dst: {dst}")),
        GlobalCtxEvent::ConnectError(dst, ip_version, err) => {
            Some(format!("connect error. dst: {dst}, ip_version: {ip_version}, err: {err}"))
        }
        // ---- VPN Portal（DHCP 网关模式）----
        GlobalCtxEvent::VpnPortalStarted(portal) => Some(format!("vpn portal started. portal: {portal}")),
        GlobalCtxEvent::VpnPortalClientConnected(portal, client_addr) => {
            Some(format!("vpn portal client connected. portal: {portal}, client_addr: {client_addr}"))
        }
        GlobalCtxEvent::VpnPortalClientDisconnected(portal, client_addr) => {
            Some(format!("vpn portal client disconnected. portal: {portal}, client_addr: {client_addr}"))
        }
        // ---- DHCP 分配与冲突 ----
        GlobalCtxEvent::DhcpIpv4Changed(old, new) => {
            Some(format!("dhcp ip changed. old: {old:?}, new: {new:?}"))
        }
        GlobalCtxEvent::DhcpIpv4Conflicted(ip) => Some(format!("dhcp ip conflict. ip: {ip:?}")),
        // ---- 端口转发 / NAT 映射 ----
        GlobalCtxEvent::PortForwardAdded(cfg) => Some(format!("port forward added. cfg: {cfg:?}")),
        GlobalCtxEvent::ListenerPortMappingEstablished {
            local_listener,
            mapped_listener,
            backend,
        } => Some(format!(
            "listener port mapping established. local: {local_listener}, mapped: {mapped_listener}, backend: {backend}"
        )),
        // ---- 公网 IPv6 与 UDP 广播中继 ----
        GlobalCtxEvent::PublicIpv6Changed(old, new) => {
            Some(format!("public ipv6 changed. old: {old:?}, new: {new:?}"))
        }
        GlobalCtxEvent::PublicIpv6RoutesUpdated(added, removed) => {
            Some(format!("public ipv6 routes updated. added: {added:?}, removed: {removed:?}"))
        }
        GlobalCtxEvent::UdpBroadcastRelayStartResult {
            capture_backend,
            error,
        } => Some(format!(
            "udp broadcast relay start result. backend: {capture_backend:?}, error: {error:?}"
        )),
        // ---- 凭据热更新：记录但无需展示细节 ----
        GlobalCtxEvent::CredentialChanged => Some("credential changed".to_string()),
        // ---- 纯内部事件，不进用户日志 ----
        GlobalCtxEvent::ConfigPatched(_) => None,
        GlobalCtxEvent::ProxyCidrsUpdated(_, _) => None,
    }
}