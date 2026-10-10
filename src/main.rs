mod api;
mod config;
mod http;
mod invite;
mod mailer;
mod models;
mod node;
mod notify;
mod peer_rpc;
mod status;
mod store;
mod update;
mod web;
mod ws;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "astral-server",
    version = "1.4.2_3",
    about = "Astral Game P2P server (headless) with embedded web management"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 运行服务：启动 EasyTier 节点 + Web 管理端。
    Run {
        /// config.toml 路径（默认与可执行文件同目录）
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// 生成一份示例配置文件（默认生成在可执行文件同目录）。
    Init {
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// 打印内嵌 Web 管理端（可选：导出 index.html 供外部托管）。
    ExportWeb {
        #[arg(long, default_value = "web-export/index.html")]
        output: PathBuf,
    },
}

/// 获取可执行文件所在目录。
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 解析配置路径：未指定时使用可执行文件同目录的 config.toml。
fn resolve_config_path(config: Option<PathBuf>) -> PathBuf {
    config.unwrap_or_else(|| exe_dir().join("config.toml"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { config } => {
            let path = resolve_config_path(config);
            config::write_example_config(&path)?;
            println!("✅ 已生成示例配置: {}", path.display());
            println!("   编辑后直接运行: astral-server run");
            Ok(())
        }
        Command::ExportWeb { output } => {
            if let Some(parent) = output.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            std::fs::write(&output, web::INDEX_HTML)?;
            println!("✅ 已导出 Web 管理端: {}", output.display());
            Ok(())
        }
        Command::Run { config } => {
            let path = resolve_config_path(config);
            run_server(&path).await
        }
    }
}

async fn run_server(config_path: &PathBuf) -> anyhow::Result<()> {
    let mut loaded = config::load(config_path)?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new(&loaded.server.log_level)
                }),
        )
        .with_target(false)
        .try_init()
        .ok();

    // WebSocket 第三方服务密钥：未配置则自动生成随机密钥并写回配置。
    if loaded.server.ws_key.is_empty() {
        loaded.server.ws_key = config::generate_ws_key();
        if let Err(e) = config::save(config_path, &loaded.server) {
            tracing::warn!("自动生成 WebSocket 服务密钥后保存配置失败: {e}");
        } else {
            tracing::info!("已自动生成 WebSocket 第三方服务密钥（可在面板设置中查看/刷新）");
        }
    }

    // 邮件模板：首次运行生成到 Email/ 目录，用户可自由编辑。
    mailer::init_templates();

    let server_config = Arc::new(std::sync::RwLock::new(loaded.server.clone()));
    let node = node::NodeRuntime::new(server_config.clone());
    let hub = Arc::new(notify::EventHub::new());

    let app_state = http::AppState {
        node: node.clone(),
        config_path: config_path.clone(),
        server_config,
        current_room: Arc::new(std::sync::Mutex::new(None)),
        sessions: Arc::new(std::sync::RwLock::new(std::collections::HashSet::new())),
        hub: hub.clone(),
        ws_registry: Arc::new(ws::WsRegistry::new()),
    };

    // 启动联网：优先执行守护进程自动创建预设房间；
    // 其次自动加入预设房间；均未启用时沿用旧逻辑——配置含 EasyTier 实例节则自动联网。
    let auto_create = {
        let cfg = app_state.server_config.read().unwrap();
        if cfg.auto_create_enabled && !cfg.auto_create_game_id.is_empty() {
            Some((cfg.auto_create_game_id.clone(), cfg.auto_create_game_name.clone()))
        } else {
            None
        }
    };
    let auto_join = {
        let cfg = app_state.server_config.read().unwrap();
        if !auto_create.is_some()
            && cfg.auto_join_enabled
            && !cfg.auto_join_code.is_empty()
        {
            Some(cfg.auto_join_code.clone())
        } else {
            None
        }
    };
    if let Some((game_id, game_name)) = auto_create {
        match api::create_room_impl(&app_state, game_id, game_name, None).await {
            Ok((room, _share_url, id)) => {
                tracing::info!(
                    "守护进程：已自动创建房间「{}」instance_id={id}",
                    room.game_name
                );
                let st = app_state.clone();
                tokio::spawn(async move {
                    crate::notify::EventHub::publish(&st, "room.created").await;
                });
            }
            Err((status, msg)) => {
                tracing::warn!("守护进程自动创建失败（可稍后在 Web 端重试）: {status} {msg}")
            }
        }
    } else if let Some(code) = auto_join {
        match api::join_from_input(&app_state, code).await {
            Ok((room, _share_url, id)) => tracing::info!(
                "守护进程：已自动加入房间「{}」instance_id={id}",
                room.game_name
            ),
            Err((status, msg)) => {
                tracing::warn!("守护进程自动加入失败（可稍后在 Web 端重试）: {status} {msg}")
            }
        }
    } else if !loaded.instance_toml.trim().is_empty() && has_instance_config(&loaded.instance_toml)
    {
        match node.start(loaded.instance_toml.clone()).await {
            Ok(id) => tracing::info!("实例已启动 instance_id={}", id),
            Err(e) => tracing::warn!("实例自动启动失败（可稍后在 Web 端重试）: {e}"),
        }
    }

    let router = http::build_router(app_state.clone());

    let addr = format!("{}:{}", loaded.server.web_bind, loaded.server.web_port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("无法监听 {}", addr))?;
    tracing::info!("Web 管理端已监听: http://{}", addr);

    // 服务启动事件：稍等节点快照稳定后，广播房间状态并按需发送通知邮件。
    let st = app_state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        notify::EventHub::publish(&st, "server.started").await;
    });

    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("http server error")?;

    node.stop().await;
    tracing::info!("已停止");
    Ok(())
}

fn has_instance_config(toml_str: &str) -> bool {
    toml_str.contains("[network_identity]")
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install ctrl+c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("收到退出信号，正在关闭…");
}