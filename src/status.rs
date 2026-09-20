//! EasyTier 运行状态 → 可序列化的 JSON DTO。
//! 移植自 astral_rust_core 的 `network_status_from_info` / `peer_route_pairs_from_info`。

use easytier::launcher::NetworkInstanceRunningInfo;
use easytier::proto::api::instance::{list_peer_route_pair, PeerRoutePair, Route};
use serde::Serialize;

pub const LOCAL_SYNTHETIC_PEER_ID: u32 = 0;

#[derive(Debug, Clone, Serialize)]
pub struct NodeHopStats {
    pub peer_id: u32,
    pub target_ip: String,
    pub latency_ms: f64,
    pub packet_loss: f32,
    pub node_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeConnectionStats {
    pub conn_type: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeInfo {
    pub peer_id: u32,
    pub hostname: String,
    pub ipv4: String,
    pub ipv6: String,
    pub latency_ms: f64,
    pub nat: String,
    pub hops: Vec<NodeHopStats>,
    pub loss_rate: f32,
    pub connections: Vec<NodeConnectionStats>,
    pub tunnel_proto: String,
    pub conn_type: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub version: String,
    pub cost: i32,
    pub remote_static_pubkey_b64: String,
    pub is_credential_peer: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkStatus {
    pub total_nodes: usize,
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
    let pairs = peer_route_pairs_from_info(info);

    let routes_by_peer: std::collections::HashMap<u32, Route> = pairs
        .iter()
        .filter_map(|p| p.route.clone())
        .map(|r| (r.peer_id, r))
        .collect();

    let mut nodes: Vec<NodeInfo> = Vec::new();
    for p in &pairs {
        let Some(route) = p.route.clone() else {
            continue;
        };

        let lat_ms = if route.cost == 1 {
            p.get_latency_ms().unwrap_or(0.0)
        } else {
            route.path_latency_latency_first() as f64
        };

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

        let hops = if route.cost <= 1
            || route.inst_id == "local"
            || route.peer_id == LOCAL_SYNTHETIC_PEER_ID
        {
            Vec::new()
        } else {
            collect_relay_hops(&routes_by_peer, route.next_hop_peer_id, route.peer_id)
        };

        let flag_credential = route
            .feature_flag
            .as_ref()
            .map(|f| f.is_credential_peer)
            .unwrap_or(false);

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

        if route.inst_id == "local" || route.peer_id == LOCAL_SYNTHETIC_PEER_ID {
            node_info.conn_type = "Local".to_string();
            if node_info.tunnel_proto.is_empty() {
                node_info.tunnel_proto = "-".to_string();
            }
        }

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

    nodes.sort_by(|a, b| a.peer_id.cmp(&b.peer_id));
    nodes.dedup_by(|a, b| a.peer_id == b.peer_id);

    NetworkStatus {
        total_nodes: nodes.len(),
        nodes,
    }
}

fn peer_route_pairs_from_info(info: NetworkInstanceRunningInfo) -> Vec<PeerRoutePair> {
    let mut pairs = if info.peer_route_pairs.is_empty() {
        list_peer_route_pair(info.peers.clone(), info.routes.clone())
    } else {
        info.peer_route_pairs
    };

    let mut route_peer_ids: std::collections::HashSet<u32> = pairs
        .iter()
        .filter_map(|p| p.route.as_ref().map(|r| r.peer_id))
        .collect();

    for peer in &info.peers {
        if !route_peer_ids.contains(&peer.peer_id) {
            pairs.push(PeerRoutePair {
                route: None,
                peer: Some(peer.clone()),
            });
            route_peer_ids.insert(peer.peer_id);
        }
    }

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
        if step == next {
            break;
        }
        next = step;
    }
    hops
}