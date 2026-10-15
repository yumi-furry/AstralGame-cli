//! =============================================================================
//! 操作审计日志：登录 / 登出 / 修改类操作的持久化审计。
//!
//! 设计要点：
//! * 条目类型 kind：login（登录成功）/ login_failed（登录失败）/ logout（登出）/ modify（修改操作）；
//! * 内存 VecDeque（按时间升序）+ exe 同目录 audit_log.json 全量落盘（沿用 store.rs 模式）；
//! * 保留策略：14 天过期 + 最多 2000 条，超出自动丢弃最旧条目；
//! * 新记录通过 EventHub 以 `audit` 事件即时广播给面板 WebSocket，实现前端动态更新。
//! =============================================================================

use std::collections::VecDeque;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::http::AppState;

/// 审计记录保留时长：14 天。
const RETENTION_SECS: u64 = 14 * 24 * 60 * 60;
/// 内存与文件中最多保留的条目数（超出丢弃最旧）。
const MAX_ENTRIES: usize = 2000;

/// 单条审计记录。
#[derive(Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// 事件时间（UNIX 秒）。
    pub ts: u64,
    /// login | login_failed | logout | modify
    pub kind: String,
    /// 人类可读的操作描述（中文）。
    pub action: String,
    /// 操作者 IP（优先反代头，否则 TCP 连接源地址）。
    pub ip: String,
}

/// 审计日志存储（以 Arc<AuditLog> 放入 AppState 共享）。
pub struct AuditLog {
    entries: Mutex<VecDeque<AuditEntry>>,
}

/// audit_log.json 的完整路径（exe 同目录）。
fn audit_path() -> PathBuf {
    crate::store::exe_dir().join("audit_log.json")
}

impl AuditLog {
    /// 启动时加载历史审计记录：文件缺失/损坏视为空；加载后立即执行一次过期清理。
    pub fn load() -> Self {
        let raw = fs::read_to_string(audit_path()).unwrap_or_default();
        let parsed: Vec<AuditEntry> = serde_json::from_str(&raw).unwrap_or_default();
        let this = Self { entries: Mutex::new(VecDeque::from(parsed)) };
        this.prune();
        this
    }

    /// 追加一条记录：过期清理 + 容量裁剪 + 落盘，返回条目副本（供事件广播）。
    pub fn record(&self, kind: &str, action: String, ip: String) -> AuditEntry {
        let entry = AuditEntry { ts: now_unix(), kind: kind.to_string(), action, ip };
        {
            let mut q = self.entries.lock().unwrap();
            q.push_back(entry.clone());
            Self::trim(&mut q);
        }
        self.save();
        entry
    }

    /// 返回最近 tail 条（新→旧）。
    pub fn list(&self, tail: usize) -> Vec<AuditEntry> {
        let q = self.entries.lock().unwrap();
        let start = q.len().saturating_sub(tail);
        q.iter().skip(start).rev().cloned().collect()
    }

    /// 清理过期条目；有移除时重写文件（供每日定时任务调用）。
    pub fn prune(&self) {
        let removed = { Self::trim(&mut self.entries.lock().unwrap()) };
        if removed {
            self.save();
        }
    }

    /// 丢弃超过保留期与超出容量的条目；返回是否发生了移除。
    fn trim(q: &mut VecDeque<AuditEntry>) -> bool {
        let cutoff = now_unix().saturating_sub(RETENTION_SECS);
        let before = q.len();
        while q.front().map(|e| e.ts < cutoff).unwrap_or(false) {
            q.pop_front();
        }
        while q.len() > MAX_ENTRIES {
            q.pop_front();
        }
        before != q.len()
    }

    /// 全量写盘（pretty 便于直接查看；失败只告警，不影响业务路径）。
    fn save(&self) {
        let serialized = {
            let q = self.entries.lock().unwrap();
            serde_json::to_string_pretty(&q.iter().collect::<Vec<_>>())
        };
        match serialized {
            Ok(text) => {
                if let Err(e) = fs::write(audit_path(), text) {
                    tracing::warn!("写入 audit_log.json 失败: {e}");
                }
            }
            Err(e) => tracing::warn!("序列化审计日志失败: {e}"),
        }
    }
}

/// 记录一条审计并通过 WebSocket 广播 `audit` 事件（面板动态更新）。
pub fn log(state: &AppState, kind: &str, action: impl Into<String>, ip: &str) {
    let entry = state.audit.record(kind, action.into(), ip.to_string());
    let payload = json!({ "event": "audit", "timestamp": entry.ts, "entry": entry });
    crate::notify::EventHub::publish_raw(state, payload);
}

/// 解析操作者 IP：反代头（X-Forwarded-For 首个 / X-Real-IP）优先，
/// 否则用 TCP 连接源地址（只取 IP，不带端口）。
pub fn client_ip(headers: &HeaderMap, addr: &SocketAddr) -> String {
    if let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = v.split(',').next().map(|s| s.trim()) {
            if !first.is_empty() {
                return first.to_string();
            }
        }
    }
    if let Some(v) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        if !v.is_empty() {
            return v.to_string();
        }
    }
    addr.ip().to_string()
}

/// Axum 提取器：任意 Handler 加一个 `ClientIp(ip)` 参数即可获得操作者 IP。
/// 依赖 main.rs 的 into_make_service_with_connect_info（ConnectInfo 在请求扩展中）。
pub struct ClientIp(pub String);

#[axum::async_trait]
impl<S: Send + Sync> axum::extract::FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let addr = parts
            .extensions
            .get::<axum::extract::ConnectInfo<SocketAddr>>()
            .map(|c| c.0)
            .unwrap_or(([0, 0, 0, 0], 0).into());
        Ok(ClientIp(client_ip(&parts.headers, &addr)))
    }
}

/// 当前 UNIX 时间戳（秒）；时钟异常回退为 0。
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
