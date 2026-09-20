//! 无头节点的生命周期管理：启动/停止 EasyTier 实例、事件日志、状态快照、凭据管理。

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

use crate::status::{self, NetworkStatus};

const LOG_CAP: usize = 800;

#[derive(Debug, Clone, serde::Serialize)]
pub struct LogEntry {
    pub ts: u64,
    pub level: String,
    pub msg: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Snapshot {
    pub running: bool,
    pub instance_id: Option<String>,
    pub easytier_version: String,
    pub server_version: String,
    pub my_ipv4: Option<String>,
    pub total_nodes: usize,
    pub nodes: Vec<status::NodeInfo>,
    pub error: Option<String>,
}

impl Snapshot {
    fn idle() -> Self {
        Self {
            running: false,
            instance_id: None,
            easytier_version: easytier_version(),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            my_ipv4: None,
            total_nodes: 0,
            nodes: Vec::new(),
            error: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GeneratedCredential {
    pub credential_id: String,
    pub credential_secret: String,
    pub pubkey_b64: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CredentialInfo {
    pub credential_id: String,
    pub groups: Vec<String>,
    pub allow_relay: bool,
    pub expiry_unix: i64,
    pub reusable: bool,
}

pub struct NodeRuntime {
    manager: Arc<NetworkInstanceManager>,
    current_id: Mutex<Option<Uuid>>,
    toml: Mutex<Option<String>>,
    logs: Arc<Mutex<VecDeque<LogEntry>>>,
    snapshot: Arc<RwLock<Snapshot>>,
}

impl NodeRuntime {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            manager: Arc::new(NetworkInstanceManager::new()),
            current_id: Mutex::new(None),
            toml: Mutex::new(None),
            logs: Arc::new(Mutex::new(VecDeque::new())),
            snapshot: Arc::new(RwLock::new(Snapshot::idle())),
        })
    }

    /// 启动（或重启）实例。传入剥离 [astral_server] 后的 EasyTier 配置 TOML。
    pub async fn start(&self, config_toml: String) -> Result<String, String> {
        self.stop().await;

        let cfg = TomlConfigLoader::new_from_str(&config_toml).map_err(|e| e.to_string())?;
        if let Some(sm) = cfg.get_secure_mode() {
            if sm.enabled {
                let processed = process_secure_mode_cfg(sm).map_err(|e| e.to_string())?;
                cfg.set_secure_mode(Some(processed));
                self.push_log("info", "secure_mode: 本地密钥已生成/加载");
            }
        }

        let id = self
            .manager
            .run_network_instance(cfg, true, ConfigFileControl::STATIC_CONFIG)
            .map_err(|e| format!("启动实例失败: {e}"))?;

        *self.current_id.lock().unwrap() = Some(id);
        *self.toml.lock().unwrap() = Some(config_toml);
        self.push_log("info", format!("实例已创建 instance_id={id}"));

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

        self.spawn_poll(id).await;
        Ok(id.to_string())
    }

    async fn spawn_poll(&self, id: Uuid) {
        let manager = self.manager.clone();
        let snapshot = self.snapshot.clone();
        tokio::spawn(async move {
            loop {
                if !manager.list_network_instance_ids().contains(&id) {
                    if let Ok(mut s) = snapshot.write() {
                        s.running = false;
                    }
                    break;
                }
                let running = manager.iter().any(|it| it.value().is_easytier_running());
                let mut snap = snapshot.read().map(|s| s.clone()).unwrap_or_else(|_| Snapshot::idle());
                snap.running = running;
                snap.instance_id = Some(id.to_string());
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

    pub async fn stop(&self) {
        let id = { self.current_id.lock().unwrap().take() };
        if let Some(id) = id {
            if let Err(e) = self.manager.delete_network_instance(vec![id]) {
                self.push_log("error", format!("停止实例失败: {e}"));
            } else {
                self.push_log("info", format!("实例已停止 instance_id={id}"));
            }
        }
        if let Ok(mut s) = self.snapshot.write() {
            s.running = false;
            s.instance_id = None;
            s.my_ipv4 = None;
            s.total_nodes = 0;
            s.nodes.clear();
            s.error = None;
        }
    }

    pub async fn restart(&self) -> Result<String, String> {
        let toml = self.toml.lock().unwrap().clone();
        match toml {
            Some(t) => self.start(t).await,
            None => Err("没有已加载的实例配置".into()),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.read().map(|s| s.clone()).unwrap_or_else(|_| Snapshot::idle())
    }

    pub fn logs(&self, tail: usize) -> Vec<LogEntry> {
        let logs = self.logs.lock().unwrap();
        let skip = logs.len().saturating_sub(tail);
        logs.iter().skip(skip).cloned().collect()
    }

    fn push_log(&self, level: &str, msg: impl Into<String>) {
        push_log_into(&self.logs, level, msg);
    }

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

    pub async fn generate_credential(
        &self,
        ttl_seconds: i64,
        reusable: bool,
    ) -> Result<GeneratedCredential, String> {
        if ttl_seconds <= 0 {
            return Err("ttl_seconds 必须大于 0".into());
        }
        let svc = self.instance_rpc().await?;
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
        let pubkey_b64 = pubkey_b64_from_credential_secret(&resp.credential_secret)?;
        Ok(GeneratedCredential {
            credential_id: resp.credential_id,
            credential_secret: resp.credential_secret,
            pubkey_b64,
        })
    }

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

    pub async fn list_credentials(&self) -> Result<Vec<CredentialInfo>, String> {
        let svc = self.instance_rpc().await?;
        let resp = svc
            .get_credential_manage_service()
            .list_credentials(BaseController::default(), ListCredentialsRequest { instance: None })
            .await
            .map_err(|e| e.to_string())?;
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

fn node_ipv4(st: &NetworkStatus) -> Option<String> {
    st.nodes
        .iter()
        .find(|n| n.peer_id == status::LOCAL_SYNTHETIC_PEER_ID)
        .map(|n| n.ipv4.split('/').next().unwrap_or("").to_string())
        .filter(|s| !s.is_empty())
}

fn easytier_version() -> String {
    easytier::VERSION.to_string()
}

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

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn forward_events(mut events: EventBusSubscriber, logs: Arc<Mutex<VecDeque<LogEntry>>>) {
    loop {
        match events.recv().await {
            Ok(e) => {
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

fn peer_conn_info_to_string(p: easytier::proto::api::instance::PeerConnInfo) -> String {
    format!(
        "my_peer_id: {}, dst_peer_id: {}, tunnel_info: {:?}",
        p.my_peer_id, p.peer_id, p.tunnel
    )
}

fn event_to_message(e: GlobalCtxEvent) -> Option<String> {
    match e {
        GlobalCtxEvent::PeerAdded(p) => Some(format!("peer added. peer_id: {p}")),
        GlobalCtxEvent::PeerRemoved(p) => Some(format!("peer removed. peer_id: {p}")),
        GlobalCtxEvent::PeerConnAdded(p) => {
            Some(format!("peer connection added. conn_info: {}", peer_conn_info_to_string(p)))
        }
        GlobalCtxEvent::PeerConnRemoved(p) => {
            Some(format!("peer connection removed. conn_info: {}", peer_conn_info_to_string(p)))
        }
        GlobalCtxEvent::ListenerAddFailed(p, msg) => {
            Some(format!("listener add failed. listener: {p}, msg: {msg}"))
        }
        GlobalCtxEvent::ListenerAcceptFailed(p, msg) => {
            Some(format!("listener accept failed. listener: {p}, msg: {msg}"))
        }
        GlobalCtxEvent::ListenerAdded(p) => {
            if p.scheme() == "ring" {
                None
            } else {
                Some(format!("listener added. listener: {p}"))
            }
        }
        GlobalCtxEvent::ConnectionAccepted(local, remote) => {
            Some(format!("connection accepted. local: {local}, remote: {remote}"))
        }
        GlobalCtxEvent::ConnectionError(local, remote, err) => {
            Some(format!("connection error. local: {local}, remote: {remote}, err: {err}"))
        }
        GlobalCtxEvent::TunDeviceReady(dev) => Some(format!("tun device ready. dev: {dev}")),
        GlobalCtxEvent::TunDeviceError(err) => Some(format!("tun device error. err: {err}")),
        GlobalCtxEvent::Connecting(dst) => Some(format!("connecting to peer. dst: {dst}")),
        GlobalCtxEvent::ConnectError(dst, ip_version, err) => {
            Some(format!("connect error. dst: {dst}, ip_version: {ip_version}, err: {err}"))
        }
        GlobalCtxEvent::VpnPortalStarted(portal) => Some(format!("vpn portal started. portal: {portal}")),
        GlobalCtxEvent::VpnPortalClientConnected(portal, client_addr) => {
            Some(format!("vpn portal client connected. portal: {portal}, client_addr: {client_addr}"))
        }
        GlobalCtxEvent::VpnPortalClientDisconnected(portal, client_addr) => {
            Some(format!("vpn portal client disconnected. portal: {portal}, client_addr: {client_addr}"))
        }
        GlobalCtxEvent::DhcpIpv4Changed(old, new) => {
            Some(format!("dhcp ip changed. old: {old:?}, new: {new:?}"))
        }
        GlobalCtxEvent::DhcpIpv4Conflicted(ip) => Some(format!("dhcp ip conflict. ip: {ip:?}")),
        GlobalCtxEvent::PortForwardAdded(cfg) => Some(format!("port forward added. cfg: {cfg:?}")),
        GlobalCtxEvent::ListenerPortMappingEstablished {
            local_listener,
            mapped_listener,
            backend,
        } => Some(format!(
            "listener port mapping established. local: {local_listener}, mapped: {mapped_listener}, backend: {backend}"
        )),
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
        GlobalCtxEvent::CredentialChanged => Some("credential changed".to_string()),
        GlobalCtxEvent::ConfigPatched(_) => None,
        GlobalCtxEvent::ProxyCidrsUpdated(_, _) => None,
    }
}