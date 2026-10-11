//! ==== 网络状态 DTO 转换模块 ====
//! EasyTier 运行状态（NetworkInstanceRunningInfo）→ 可序列化的 JSON DTO，
//! 供 /api/status 返回给面板，字段口径与 Astral Game 官方客户端一致。
//! 移植自 astral_rust_core 的 `network_status_from_info` / `peer_route_pairs_from_info`。

use easytier::launcher::NetworkInstanceRunningInfo;
use easytier::proto::api::instance::{list_peer_route_pair, PeerRoutePair, Route};
use serde::Serialize;

/// 本机在节点列表中的合成 peer_id（内核真实 peer_id 不为 0，
/// 列表中用 0 这条"虚拟路由"代表本机节点）。
pub const LOCAL_SYNTHETIC_PEER_ID: u32 = 0;

/// 中继路径上的一跳信息（经哪些节点转发到目标）。
#[derive(Debug, Clone, Serialize)]
pub struct NodeHopStats {
    /// 该跳节点的 peer_id。
    pub peer_id: u32,
    /// 该跳节点的虚拟 IPv4。
    pub target_ip: String,
    /// 到该跳的延迟（当前路径探测未细分时为 0）。
    pub latency_ms: f64,
    /// 到该跳的丢包率（0.0~100.0，同上可能为 0）。
    pub packet_loss: f32,
    /// 该跳节点主机名。
    pub node_name: String,
}

/// 单条物理连接（一个节点可能同时有多条连接）的流量统计。
#[derive(Debug, Clone, Serialize)]
pub struct NodeConnectionStats {
    /// 隧道类型（tcp/udp/ws/wg 等）。
    pub conn_type: String,
    /// 累计接收字节。
    pub rx_bytes: u64,
    /// 累计发送字节。
    pub tx_bytes: u64,
    /// 累计接收包数。
    pub rx_packets: u64,
    /// 累计发送包数。
    pub tx_packets: u64,
}

/// 网络中的一个节点（含本机合成节点）。
#[derive(Debug, Clone, Serialize)]
pub struct NodeInfo {
    /// EasyTier peer_id。
    pub peer_id: u32,
    /// 对端主机名（昵称）。
    pub hostname: String,
    /// 虚拟网 IPv4（可能为空字符串）。
    pub ipv4: String,
    /// 虚拟网 IPv6（可能为空字符串）。
    pub ipv6: String,
    /// 直连延迟毫秒；多跳节点取路径首跳延迟。
    pub latency_ms: f64,
    /// UDP NAT 类型描述串（如 FullCone / Symmetric）。
    pub nat: String,
    /// 中继路径各跳；直连/本机为空。
    pub hops: Vec<NodeHopStats>,
    /// 丢包率百分比（0.0~100.0）。
    pub loss_rate: f32,
    /// 该节点各条物理连接的流量明细。
    pub connections: Vec<NodeConnectionStats>,
    /// 所有连接协议拼串（逗号分隔）。
    pub tunnel_proto: String,
    /// 主连接协议；取不到协议时回退显示 NAT 类型。
    pub conn_type: String,
    /// 节点级累计接收字节。
    pub rx_bytes: u64,
    /// 节点级累计发送字节。
    pub tx_bytes: u64,
    /// 对端 EasyTier 版本（为空时上层填 "unknown"）。
    pub version: String,
    /// 路由代价：0=本机，1=直连，>1=经过中继的跳数。
    pub cost: i32,
    /// 对端 Noise 静态公钥（Base64），用于指纹展示。
    pub remote_static_pubkey_b64: String,
    /// 是否为凭据准入的对端（feature_flag 或连接身份任一命中即为 true）。
    pub is_credential_peer: bool,
}

/// /api/status 节点区的顶层结构。
#[derive(Debug, Clone, Serialize)]
pub struct NetworkStatus {
    /// 节点总数（含本机）。
    pub total_nodes: usize,
    /// 节点列表，按 peer_id 升序并去重。
    pub nodes: Vec<NodeInfo>,
}

impl Default for NetworkStatus {
    fn default() -> Self {
        Self {
            total_nodes: 0,
            nodes: Vec::new(),
        }
    }
}

/// 从运行信息快照构建与 Astral Game 一致的节点列表。
pub fn network_status_from_info(info: NetworkInstanceRunningInfo) -> NetworkStatus {
    // 组装 (peer, route) 配对，并补齐孤立 peer 与本机合成节点。
    let pairs = peer_route_pairs_from_info(info);

    // 以 peer_id 索引路由表，供后续沿 next_hop_peer_id 追中继跳路径。
    let routes_by_peer: std::collections::HashMap<u32, Route> = pairs
        .iter()
        .filter_map(|p| p.route.clone())
        .map(|r| (r.peer_id, r))
        .collect();

    let mut nodes: Vec<NodeInfo> = Vec::new();
    for p in &pairs {
        // 没有路由的孤立 peer（见下方 pairs 补齐）不展示为节点。
        let Some(route) = p.route.clone() else {
            continue;
        };

        // cost==1 直连用实测延迟；多跳节点用路由表里的路径首跳延迟。
        let lat_ms = if route.cost == 1 {
            p.get_latency_ms().unwrap_or(0.0)
        } else {
            route.path_latency_latency_first() as f64
        };

        // get_loss_rate 返回 0~1 的比例，转百分比。
        let loss_percent = p.get_loss_rate().unwrap_or(0.0) * 100.0;
        let ipv4 = route
            .ipv4_addr
            .as_ref()
            .and_then(|ip| ip.address.clone())
            .map(|ip| ip.to_string())
            .unwrap_or_default();

        let ipv6 = route
            .ipv6_addr
            .as_ref()
            .map(|addr| addr.to_string())
            .unwrap_or_default();

        let tunnel_proto = p.get_conn_protos().unwrap_or_default().join(",");
        let conn_type = p
            .get_conn_protos()
            .unwrap_or_default()
            .into_iter()
            .next()
            .unwrap_or_else(|| p.get_udp_nat_type());

        // 只有中继节点（cost>1）才需要展开 hops；本机/直连为空数组。
        let hops = if route.cost <= 1
            || route.inst_id == "local"
            || route.peer_id == LOCAL_SYNTHETIC_PEER_ID
        {
            Vec::new()
        } else {
            collect_relay_hops(&routes_by_peer, route.next_hop_peer_id, route.peer_id)
        };

        // 凭据节点标记来源之一：路由 feature_flag。
        let flag_credential = route
            .feature_flag
            .as_ref()
            .map(|f| f.is_credential_peer)
            .unwrap_or(false);

        // 从各条连接里顺带提取：首个可用的 Noise 静态公钥，
        // 以及"任一连接以 Credential 身份鉴权"的标记。
        let mut remote_static_pubkey_b64 = String::new();
        let mut identity_credential = false;
        if let Some(peer) = &p.peer {
            for conn in &peer.conns {
                if remote_static_pubkey_b64.is_empty() && !conn.noise_remote_static_pubkey.is_empty()
                {
                    use base64::Engine;
                    remote_static_pubkey_b64 =
                        base64::engine::general_purpose::STANDARD.encode(&conn.noise_remote_static_pubkey);
                }
                if conn.peer_identity_type
                    == easytier::proto::peer_rpc::PeerIdentityType::Credential as i32
                {
                    identity_credential = true;
                }
            }
        }

        let mut node_info = NodeInfo {
            peer_id: route.peer_id,
            hostname: route.hostname.clone(),
            ipv4,
            ipv6,
            latency_ms: lat_ms,
            nat: p.get_udp_nat_type(),
            hops,
            loss_rate: loss_percent as f32,
            connections: vec![],
            tunnel_proto,
            conn_type,
            rx_bytes: p.get_rx_bytes().unwrap_or(0),
            tx_bytes: p.get_tx_bytes().unwrap_or(0),
            version: if route.version.is_empty() {
                "unknown".to_string()
            } else {
                route.version
            },
            cost: route.cost,
            remote_static_pubkey_b64,
            is_credential_peer: flag_credential || identity_credential,
        };

        // 本机合成节点：连接类型固定显示 Local，无协议时显示 "-"。
        if route.inst_id == "local" || route.peer_id == LOCAL_SYNTHETIC_PEER_ID {
            node_info.conn_type = "Local".to_string();
            if node_info.tunnel_proto.is_empty() {
                node_info.tunnel_proto = "-".to_string();
            }
        }

        // 展开每条物理连接的收发统计（仅内核上报了 stats 的连接）。
        if let Some(peer) = &p.peer {
            for conn in &peer.conns {
                if let Some(stats) = &conn.stats {
                    let conn_type = conn
                        .tunnel
                        .as_ref()
                        .map(|t| t.tunnel_type.clone())
                        .unwrap_or_else(|| "unknown".to_string());
                    node_info.connections.push(NodeConnectionStats {
                        conn_type,
                        rx_bytes: stats.rx_bytes,
                        tx_bytes: stats.tx_bytes,
                        rx_packets: stats.rx_packets,
                        tx_packets: stats.tx_packets,
                    });
                }
            }
        }

        nodes.push(node_info);
    }

    // 按 peer_id 排序后按相邻去重（防御内核重复上报同一路由）。
    nodes.sort_by(|a, b| a.peer_id.cmp(&b.peer_id));
    nodes.dedup_by(|a, b| a.peer_id == b.peer_id);

    NetworkStatus {
        total_nodes: nodes.len(),
        nodes,
    }
}

/// 构造 (peer, route) 配对列表，并做两项补齐：
/// 1. 有 peer 连接但暂无路由的节点，补 route=None 的占位；
/// 2. 根据 my_node_info 手工合成 peer_id=0 的本机路由（inst_id="local"）。
fn peer_route_pairs_from_info(info: NetworkInstanceRunningInfo) -> Vec<PeerRoutePair> {
    // 内核新版本直接给配对；旧版本/空配对时用 peers+routes 现场笛卡尔关联。
    let mut pairs = if info.peer_route_pairs.is_empty() {
        list_peer_route_pair(info.peers.clone(), info.routes.clone())
    } else {
        info.peer_route_pairs
    };

    let mut route_peer_ids: std::collections::HashSet<u32> = pairs
        .iter()
        .filter_map(|p| p.route.as_ref().map(|r| r.peer_id))
        .collect();

    // 已连接但路由尚未收敛的 peer：补空路由占位，避免节点在列表里消失。
    for peer in &info.peers {
        if !route_peer_ids.contains(&peer.peer_id) {
            pairs.push(PeerRoutePair {
                route: None,
                peer: Some(peer.clone()),
            });
            route_peer_ids.insert(peer.peer_id);
        }
    }

    // 合成本机节点：IPv4 取 my_node_info.virtual_ipv4，IPv6 从 routes 反查，
    // cost/延迟均为 0；next_hop 指向自己。
    if let Some(my_node_info) = &info.my_node_info {
        let my_peer_id = LOCAL_SYNTHETIC_PEER_ID;

        let my_ipv6_from_routes = info
            .routes
            .iter()
            .find(|r| r.peer_id == my_node_info.peer_id)
            .and_then(|r| r.ipv6_addr.clone());

        let my_route = Route {
            peer_id: my_peer_id,
            ipv4_addr: my_node_info.virtual_ipv4.clone(),
            ipv6_addr: my_ipv6_from_routes,
            next_hop_peer_id: my_peer_id,
            cost: 0,
            path_latency: 0,
            proxy_cidrs: vec![],
            hostname: my_node_info.hostname.clone(),
            stun_info: my_node_info.stun_info.clone(),
            inst_id: "local".to_string(),
            version: my_node_info.version.clone(),
            feature_flag: None,
            next_hop_peer_id_latency_first: None,
            cost_latency_first: None,
            path_latency_latency_first: None,
            public_ipv6_addr: None,
            ipv6_public_addr_prefix: None,
        };

        pairs.push(PeerRoutePair {
            route: Some(my_route),
            peer: None,
        });
    }

    pairs
}

/// 沿路由表的 next_hop 链收集中继路径上的各跳。
/// 参数：全量路由索引、起始下一跳 peer_id、最终目标 peer_id。
/// visited 集合用于防止路由环导致死循环；next==0 表示路径到头。
fn collect_relay_hops(
    routes_by_peer: &std::collections::HashMap<u32, Route>,
    mut next: u32,
    target: u32,
) -> Vec<NodeHopStats> {
    let mut hops = Vec::new();
    let mut visited = std::collections::HashSet::new();
    while next != 0 && next != target && visited.insert(next) {
        let Some(route) = routes_by_peer.get(&next) else {
            break;
        };
        let ip = route
            .ipv4_addr
            .as_ref()
            .and_then(|a| a.address.as_ref())
            .map(|a| a.to_string())
            .unwrap_or_default();
        hops.push(NodeHopStats {
            peer_id: next,
            target_ip: ip,
            latency_ms: 0.0,
            packet_loss: 0.0,
            node_name: route.hostname.clone(),
        });
        let step = route.next_hop_peer_id;
        // 自环防护：下一跳指向自己时立即停止。
        if step == next {
            break;
        }
        next = step;
    }
    hops
}