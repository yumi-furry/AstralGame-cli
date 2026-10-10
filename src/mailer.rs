//! SMTP 发信：房间变动通知邮件。HTML 模板与面板同风格（晴空蓝主色、圆角卡片）。
//! 模板文件外置：首次运行时生成到可执行文件同目录的 Email/ 文件夹，用户可自由编辑。

use std::path::PathBuf;
use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::config::SmtpConfig;
use crate::models::ActiveRoom;
use crate::node::Snapshot;

/// 可执行文件所在目录。
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Email 模板目录：可执行文件同目录下的 Email/ 文件夹。
fn email_dir() -> PathBuf {
    exe_dir().join("Email")
}

/// 首次运行时生成邮件模板文件（已存在则跳过）。
pub fn init_templates() {
    let dir = email_dir();
    let _ = std::fs::create_dir_all(&dir);
    let html_path = dir.join("room_notification.html");
    let txt_path = dir.join("room_notification.txt");
    if !html_path.exists() {
        let _ = std::fs::write(&html_path, DEFAULT_ROOM_HTML);
    }
    if !txt_path.exists() {
        let _ = std::fs::write(&txt_path, DEFAULT_ROOM_TXT);
    }
}

/// 读取模板文件；文件不存在时返回内嵌默认值。
fn load_template(filename: &str, fallback: &str) -> String {
    let path = email_dir().join(filename);
    std::fs::read_to_string(&path).unwrap_or_else(|_| fallback.to_string())
}

/// 模板变量替换：将 {{VAR}} 替换为实际值（已 HTML 转义）。
fn render_template(tpl: &str, vars: &[(&str, String)]) -> String {
    let mut out = tpl.to_string();
    for (key, val) in vars {
        out = out.replace(&format!("{{{{{key}}}}}"), val);
    }
    out
}

/// HTML 转义。
fn h(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 发送房间变动通知邮件。
pub async fn send_room_notification(
    smtp: &SmtpConfig,
    event_title: &str,
    event_desc: &str,
    room: Option<&ActiveRoom>,
    share_url: &str,
    snap: &Snapshot,
) -> Result<(), String> {
    let (code, code_hint) = match room {
        Some(r) => match &r.short_code {
            Some(c) => (h(c), "房间码（短码）".to_string()),
            None => (
                h(&r.offline_invite.clone().unwrap_or_default()),
                "离线邀请串（短码服务暂不可用）".to_string(),
            ),
        },
        None => (String::new(), String::new()),
    };
    let (game_id, game_name) = match room {
        Some(r) => (h(&r.game_id), h(&r.game_name)),
        None => ("-".to_string(), "无房间".to_string()),
    };
    let vars: [(&str, String); 10] = [
        ("EVENT_TITLE", h(event_title)),
        ("EVENT_DESC", h(event_desc)),
        ("GAME_NAME", game_name),
        ("GAME_ID", game_id),
        ("SHORT_CODE", code),
        ("CODE_HINT", code_hint),
        ("SHARE_URL", h(share_url)),
        ("RUNNING", if snap.running { "运行中" } else { "未运行" }.to_string()),
        ("TOTAL_NODES", snap.total_nodes.to_string()),
        ("SERVER_VERSION", h(&snap.server_version)),
    ];

    let html_tpl = load_template("room_notification.html", DEFAULT_ROOM_HTML);
    let txt_tpl = load_template("room_notification.txt", DEFAULT_ROOM_TXT);
    let html = render_template(&html_tpl, &vars);
    let plain = render_template(&txt_tpl, &vars);

    let subject = match room {
        Some(r) => format!("[Astral Server] {} · {}", event_title, r.game_name),
        None => format!("[Astral Server] {}", event_title),
    };
    let message = build_message(smtp, &subject, plain, html)?;
    let transport = build_transport(smtp)?;
    transport.send(message).await.map_err(|e| format!("发送失败: {e}"))?;
    Ok(())
}

/// 发送测试邮件（验证 SMTP 配置）。
pub async fn send_test(smtp: &SmtpConfig) -> Result<(), String> {
    let html = TEST_HTML.to_string();
    let plain = "这是一封来自 Astral Server 的测试邮件，收到说明 SMTP 配置正确。".to_string();
    let message =
        build_message(smtp, "[Astral Server] SMTP 测试邮件", plain, html)?;
    let transport = build_transport(smtp)?;
    transport.send(message).await.map_err(|e| format!("发送失败: {e}"))?;
    Ok(())
}

fn build_message(
    smtp: &SmtpConfig,
    subject: &str,
    plain: String,
    html: String,
) -> Result<Message, String> {
    let from: Mailbox = format!("{} <{}>", smtp.from_name, smtp.from_email)
        .parse()
        .map_err(|e| format!("发件人地址无效: {e}"))?;

    let mut builder = Message::builder().from(from);
    for addr in &smtp.recipients {
        let mbox: Mailbox = addr.parse().map_err(|e| format!("收件人地址 {addr} 无效: {e}"))?;
        builder = builder.to(mbox);
    }

    builder
        .subject(subject)
        .multipart(
            MultiPart::alternative()
                .singlepart(SinglePart::plain(plain))
                .singlepart(SinglePart::html(html)),
        )
        .map_err(|e| format!("构建邮件失败: {e}"))
}

fn build_transport(smtp: &SmtpConfig) -> Result<AsyncSmtpTransport<Tokio1Executor>, String> {
    let mut builder = match smtp.encryption.as_str() {
        "starttls" => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp.host)
            .map_err(|e| e.to_string())?
            .port(smtp.port),
        "none" => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&smtp.host)
            .port(smtp.port),
        _ => AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp.host)
            .map_err(|e| e.to_string())?
            .port(smtp.port),
    };

    if !smtp.username.is_empty() {
        builder =
            builder.credentials(Credentials::new(smtp.username.clone(), smtp.password.clone()));
    }
    Ok(builder.build())
}

/* ============================== 内嵌默认模板 ============================== */

const DEFAULT_ROOM_TXT: &str = r#"Astral Server · {{EVENT_TITLE}}
{{EVENT_DESC}}
游戏：{{GAME_NAME}}
{{CODE_HINT}}：{{SHORT_CODE}}
邀请链接：{{SHARE_URL}}
运行状态：{{RUNNING}} · 节点 {{TOTAL_NODES}} 个 · 服务器版本 {{SERVER_VERSION}}

原版仓库：https://github.com/AstralNext/AstralGame
二开仓库：https://github.com/yumi-furry/AstralGame-cli
问题反馈 QQ：3783260249
"#;

const DEFAULT_ROOM_HTML: &str = r#"<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"></head>
<body style="margin:0;padding:0;background:#F5F8FC;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI','PingFang SC','Microsoft YaHei',sans-serif;">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="background:#F5F8FC;padding:32px 12px;">
<tr><td align="center">
  <table role="presentation" width="560" cellpadding="0" cellspacing="0" border="0" style="max-width:560px;width:100%;">
    <tr><td style="background:linear-gradient(135deg,#3B82F6 0%,#6366F1 100%);border-radius:18px 18px 0 0;padding:26px 32px;">
      <div style="font-size:22px;color:#ffffff;font-weight:700;">✦ Astral Server</div>
      <div style="font-size:13px;color:#DBEAFE;margin-top:4px;">无头服务器 · 房间状态自动通知</div>
    </td></tr>
    <tr><td style="background:#ffffff;border-radius:0 0 18px 18px;padding:28px 32px 24px;box-shadow:0 8px 30px rgba(59,130,246,.08);">
      <div style="font-size:19px;font-weight:700;color:#1F2937;">{{EVENT_TITLE}}</div>
      <div style="font-size:13px;color:#6B7280;margin-top:6px;line-height:1.6;">{{EVENT_DESC}}</div>
      <table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="margin-top:20px;border:1px solid #E5E7EB;border-radius:12px;">
        <tr>
          <td width="50%" style="padding:14px 16px;border-right:1px solid #E5E7EB;border-bottom:1px solid #E5E7EB;">
            <div style="font-size:11px;color:#6B7280;">游戏</div>
            <div style="font-size:14px;font-weight:600;color:#1F2937;margin-top:3px;">{{GAME_NAME}}</div>
          </td>
          <td style="padding:14px 16px;border-bottom:1px solid #E5E7EB;">
            <div style="font-size:11px;color:#6B7280;">运行状态</div>
            <div style="font-size:14px;font-weight:600;color:#10B981;margin-top:3px;">{{RUNNING}}</div>
          </td>
        </tr>
        <tr>
          <td style="padding:14px 16px;border-right:1px solid #E5E7EB;">
            <div style="font-size:11px;color:#6B7280;">在线节点</div>
            <div style="font-size:14px;font-weight:600;color:#1F2937;margin-top:3px;">{{TOTAL_NODES}} 个</div>
          </td>
          <td style="padding:14px 16px;">
            <div style="font-size:11px;color:#6B7280;">游戏标识</div>
            <div style="font-size:14px;font-weight:600;color:#1F2937;margin-top:3px;">{{GAME_ID}}</div>
          </td>
        </tr>
      </table>
      <div style="margin-top:20px;background:#EFF6FF;border:1px solid #BFDBFE;border-radius:12px;padding:16px 18px;">
        <div style="font-size:11px;color:#2563EB;font-weight:600;">{{CODE_HINT}}</div>
        <div style="font-size:24px;font-weight:700;color:#1D4ED8;letter-spacing:2px;margin-top:6px;word-break:break-all;line-height:1.35;">{{SHORT_CODE}}</div>
      </div>
      <table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="margin-top:20px;">
        <tr><td style="padding:6px 0 0 0;">
          <table role="presentation" cellpadding="0" cellspacing="0" border="0"><tr>
            <td style="border-radius:10px;background:#3B82F6;">
              <a href="{{SHARE_URL}}" target="_blank" style="display:inline-block;padding:11px 26px;font-size:14px;font-weight:600;color:#ffffff;text-decoration:none;border-radius:10px;">打开邀请链接 →</a>
            </td>
          </tr></table>
          <div style="font-size:12px;color:#6B7280;margin-top:8px;word-break:break-all;">{{SHARE_URL}}</div>
        </td></tr>
      </table>
    </td></tr>
    <tr><td style="padding:20px 8px 0;">
      <div style="font-size:12px;color:#9CA3AF;line-height:1.8;text-align:center;">
        原版仓库 <a href="https://github.com/AstralNext/AstralGame" style="color:#3B82F6;text-decoration:none;">AstralNext/AstralGame</a>
        · 二开仓库 <a href="https://github.com/yumi-furry/AstralGame-cli" style="color:#3B82F6;text-decoration:none;">yumi-furry/AstralGame-cli</a><br>
        服务器版本 {{SERVER_VERSION}} · 问题反馈 QQ 3783260249
      </div>
    </td></tr>
  </table>
</td></tr>
</table>
</body>
</html>"#;

const TEST_HTML: &str = r#"<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"></head>
<body style="margin:0;padding:0;background:#F5F8FC;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI','PingFang SC','Microsoft YaHei',sans-serif;">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="background:#F5F8FC;padding:32px 12px;">
<tr><td align="center">
  <table role="presentation" width="560" cellpadding="0" cellspacing="0" border="0" style="max-width:560px;width:100%;">
    <tr><td style="background:linear-gradient(135deg,#3B82F6 0%,#6366F1 100%);border-radius:18px 18px 0 0;padding:26px 32px;">
      <div style="font-size:22px;color:#ffffff;font-weight:700;">✦ Astral Server</div>
      <div style="font-size:13px;color:#DBEAFE;margin-top:4px;">SMTP 配置测试</div>
    </td></tr>
    <tr><td style="background:#ffffff;border-radius:0 0 18px 18px;padding:32px;text-align:center;box-shadow:0 8px 30px rgba(59,130,246,.08);">
      <div style="font-size:40px;">✅</div>
      <div style="font-size:18px;font-weight:700;color:#1F2937;margin-top:10px;">SMTP 配置正确</div>
      <div style="font-size:13px;color:#6B7280;margin-top:8px;">当服务器重启或房间码变动时，通知邮件将自动发送到本邮箱列表。</div>
    </td></tr>
    <tr><td style="padding:20px 8px 0;">
      <div style="font-size:12px;color:#9CA3AF;text-align:center;">
        <a href="https://github.com/yumi-furry/AstralGame-cli" style="color:#3B82F6;text-decoration:none;">yumi-furry/AstralGame-cli</a>
        · QQ 3783260249
      </div>
    </td></tr>
  </table>
</td></tr>
</table>
</body>
</html>"#;
