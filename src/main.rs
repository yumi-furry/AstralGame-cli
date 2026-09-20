mod api;
mod config;
mod http;
mod invite;
mod models;
mod node;
mod status;
mod store;
mod web;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "astral-server",
    version,
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
    let loaded = config::load(config_path)?;

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

    let node = node::NodeRuntime::new();

    // 若配置里包含 EasyTier 实例节（至少 network_identity），启动时自动联网。
    if !loaded.instance_toml.trim().is_empty() && has_instance_config(&loaded.instance_toml) {
        match node.start(loaded.instance_toml.clone()).await {
            Ok(id) => tracing::info!("实例已启动 instance_id={}", id),
            Err(e) => tracing::warn!("实例自动启动失败（可稍后在 Web 端重试）: {e}"),
        }
    }

    let app_state = Arc::new(http::AppState {
        node: node.clone(),
        config_path: config_path.clone(),
        server_config: Arc::new(loaded.server.clone()),
        current_room: Arc::new(std::sync::Mutex::new(None)),
    });
    let router = http::build_router(app_state);

    let addr = format!("{}:{}", loaded.server.web_bind, loaded.server.web_port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("无法监听 {}", addr))?;
    tracing::info!("Web 管理端已监听: http://{}", addr);

    axum::serve(listener, router)
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