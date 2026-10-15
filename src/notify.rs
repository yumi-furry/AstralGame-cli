//! ==== 事件中心 ====
//! 服务器重启或房间码变动时，
//! ① 通过 WebSocket 向所有已连接服务广播房间内容/房间码/房间状态；
//! ② 在存在房间且 SMTP 已配置时，自动发送通知邮件。
//!
//! 广播通道容量 64：突发事件堆积时丢弃最旧消息（tokio broadcast 语义），
//! 保证慢消费者不会阻塞发布方。

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::http::AppState;

/// 进程内事件总线：所有 WebSocket 连接共享同一个 broadcast::Sender。
pub struct EventHub {
    tx: broadcast::Sender<String>,
}

impl EventHub {
    /// 创建事件总线（容量 64；接收端 lagged 时会丢失旧消息，属可接受取舍）。
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(64);
        Self { tx }
    }

    /// 订阅事件流（每个 WebSocket 连接持有一个 Receiver）。
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    /// 构建事件负载：房间内容、房间码、房间状态 + 节点快照。
    pub fn build_payload(state: &AppState, event: &str) -> Value {
        let room = state.current_room.lock().unwrap().clone();
        let share_url = room
            .as_ref()
            .map(|r| {
                crate::invite::build_join_url(r.short_code.as_deref(), r.offline_invite.as_deref())
            })
            .unwrap_or_default();
        let snap = state.node.snapshot();
        json!({
            "event": event,
            "timestamp": now_unix(),
            "running": snap.running,
            "room": room,
            "share_url": share_url,
            "node": {
                "my_ipv4": snap.my_ipv4,
                "total_nodes": snap.total_nodes,
                "error": snap.error,
                "server_version": snap.server_version,
                "easytier_version": snap.easytier_version,
            }
        })
    }

    /// 广播一条已构建好的原始事件负载（不走房间邮件逻辑）。
    /// 供审计日志等与房间无关的事件使用；无订阅者时 send 返回 Err，属正常情况。
    pub fn publish_raw(state: &AppState, payload: Value) {
        let _ = state.hub.tx.send(payload.to_string());
    }

    /// 发布事件：先广播，再按需发邮件（邮件只在存在房间时发送）。
    pub async fn publish(state: &AppState, event: &str) {
        let payload = Self::build_payload(state, event);

        // 没有 WebSocket 订阅者时 send 返回 Err，属正常情况。
        let _ = state.hub.tx.send(payload.to_string());

        let room = { state.current_room.lock().unwrap().clone() };
        // room.left 时房间已清空：只广播不发邮件。
        let Some(room) = room else { return };
        let smtp = { state.server_config.read().unwrap().smtp.clone() };
        if !smtp.enabled || smtp.recipients.is_empty() {
            return;
        }

        let share_url = crate::invite::build_join_url(
            room.short_code.as_deref(),
            room.offline_invite.as_deref(),
        );
        let snap = state.node.snapshot();
        let (title, desc) = event_text(event);
        match crate::mailer::send_room_notification(
            &smtp, title, desc, Some(&room), &share_url, &snap,
        )
        .await
        {
            Ok(()) => tracing::info!(
                "房间变动通知邮件已发送给 {} 位收件人（event={event}）",
                smtp.recipients.len()
            ),
            Err(e) => tracing::warn!("通知邮件发送失败: {e}"),
        }
    }
}

/// 事件名 → 邮件标题与描述（中文）。
fn event_text(event: &str) -> (&'static str, &'static str) {
    match event {
        "server.started" => (
            "服务器已启动",
            "服务重启完成，房间已自动恢复，房间内容与房间码如下。",
        ),
        "room.created" => ("房间已创建", "新房间已就绪，房间内容与房间码如下。"),
        "room.joined" => (
            "已加入房间",
            "服务器已加入目标房间，房间内容与房间码如下。",
        ),
        _ => (
            "房间状态已更新",
            "服务器检测到房间状态变动，最新房间内容与房间码如下。",
        ),
    }
}

/// 当前 UNIX 时间戳（秒）；时钟早于 1970 等异常情况回退为 0。
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
