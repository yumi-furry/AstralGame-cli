//! =============================================================================
//! Astral Server —— AstralGame 无头（headless）组网服务器入口。
//!
//! 职责：
//! 1. 解析命令行子命令（run / init / export-web）；
//! 2. 加载 config.toml（[astral_server] 为本服务配置，其余原样交给 EasyTier 内核）；
//! 3. 拉起 EasyTier 网络实例（守护进程可按预设自动建房/加入）；
//! 4. 启动内嵌 Axum Web 管理端（REST API + WebSocket + 静态单页前端）；
//! 5. 监听 Ctrl-C / SIGTERM，优雅停止节点。
//!
//! 模块划分见各文件头部的 `//!` 说明。
//! =============================================================================

// ---------- 业务模块（每个模块对应 src/ 下一个同名 .rs 文件） ----------
mod api; // REST API 处理器（登录、房间、服务器、凭据、SMTP、更新等）
mod config; // config.toml 的结构定义、解析、写回与示例配置
mod http; // Axum 路由表与鉴权中间件（Cookie 会话优先，Basic Auth 兼容）
mod invite; // AG1. 离线邀请串 / 短码 / 分享链接的生成与解析
mod mailer; // SMTP 发信（房间变动通知、测试邮件，模板可外置编辑）
mod models; // 跨模块共用的可序列化数据模型
mod node; // EasyTier 实例生命周期：启动/停止/重启、事件日志、状态快照、凭据
mod notify; // 事件中心：一处发布，同时驱动 WebSocket 广播与通知邮件
mod peer_rpc; // EasyTier 应用层 peer-RPC 入站处理（向客户端广播昵称/头像）
mod status; // EasyTier 运行信息 → 前端节点列表 DTO 的转换
mod store; // 本地文件持久化（servers.json 服务器列表、avatar.bin 头像）
mod update; // 在线更新：检查 GitHub Releases、下载替换二进制、systemctl 重启
mod web; // 通过 include_str! 内嵌的单文件 Web 管理端
mod ws; // WebSocket：面板事件推送 + 第三方服务密钥接入 + 在线服务注册表

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};

/// 命令行根参数（clap derive）。
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

/// 支持的三个子命令。
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
/// 所有运行期数据（config.toml、servers.json、avatar.bin、Email/ 模板）都放在这里，
/// 符合"单目录即可维护整套服务"的部署约定；取不到时回退到当前工作目录。
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

/// 程序入口：异步运行时由 #[tokio::main] 提供。
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        // 子命令 init：仅落地示例配置后退出，不启动服务。
        Command::Init { config } => {
            let path = resolve_config_path(config);
            config::write_example_config(&path)?;
            println!("✅ 已生成示例配置: {}", path.display());
            println!("   编辑后直接运行: astral-server run");
            Ok(())
        }
        // 子命令 export-web：把编译时内嵌的 index.html 导出到磁盘。
        Command::ExportWeb { output } => {
            // 自动创建输出路径中缺失的父目录。
            if let Some(parent) = output.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            std::fs::write(&output, web::INDEX_HTML)?;
            println!("✅ 已导出 Web 管理端: {}", output.display());
            Ok(())
        }
        // 子命令 run：进入服务主流程。
        Command::Run { config } => {
            let path = resolve_config_path(config);
            run_server(&path).await
        }
    }
}

/// 服务主流程：加载配置 → 组装共享状态 → 自动联网 → 启动 HTTP 服务 → 等待退出。
async fn run_server(config_path: &PathBuf) -> anyhow::Result<()> {
    // 读取并拆分配置：server 为本服务配置，instance_toml 为剥离后的 EasyTier 配置。
    let mut loaded = config::load(config_path)?;

    // 初始化日志：优先读 RUST_LOG 环境变量，否则用配置中的 log_level。
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

    // ---------- 组装全局共享状态（所有 Handler 通过 Axum State 共享） ----------
    // server_config 用 RwLock 包裹：面板修改设置后即时生效并可写回 config.toml。
    let server_config = Arc::new(std::sync::RwLock::new(loaded.server.clone()));
    // EasyTier 节点运行时（内部自管实例、日志环形缓冲与状态快照）。
    let node = node::NodeRuntime::new(server_config.clone());
    // 房间变动事件中心（WebSocket 广播通道，邮件发送也挂在它上面）。
    let hub = Arc::new(notify::EventHub::new());

    let app_state = http::AppState {
        node: node.clone(),
        config_path: config_path.clone(),
        server_config,
        // 当前房间会话仅保存在内存，重启服务后为空（需靠守护进程策略重建）。
        current_room: Arc::new(std::sync::Mutex::new(None)),
        // 已登录会话 token 集合（内存态，服务重启即全部失效）。
        sessions: Arc::new(std::sync::RwLock::new(std::collections::HashSet::new())),
        hub: hub.clone(),
        ws_registry: Arc::new(ws::WsRegistry::new()),
    };

    // 启动联网：优先执行守护进程自动创建预设房间；
    // 其次自动加入预设房间；均未启用时沿用旧逻辑——配置含 EasyTier 实例节则自动联网。
    // 注意 auto_create 优先级高于 auto_join，二者互斥（保存配置时也会强制二选一）。
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
        // 守护进程自动创建：复用 REST 接口同一套核心实现，保证行为一致。
        match api::create_room_impl(&app_state, game_id, game_name, None).await {
            Ok((room, _share_url, id)) => {
                tracing::info!(
                    "守护进程：已自动创建房间「{}」instance_id={id}",
                    room.game_name
                );
                // 异步发布事件：广播给面板/第三方并按需发送"服务器已启动/房间已创建"邮件。
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
        // 守护进程自动加入：输入支持短码 / 完整链接 / AG1. 离线串。
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
        // 兼容旧配置：配置文件里直接写了 [network_identity] 等 EasyTier 段时，照此启动。
        match node.start(loaded.instance_toml.clone()).await {
            Ok(id) => tracing::info!("实例已启动 instance_id={}", id),
            Err(e) => tracing::warn!("实例自动启动失败（可稍后在 Web 端重试）: {e}"),
        }
    }

    // 构建路由（鉴权中间件、登录路由、前端页面均在 http 模块装配）。
    let router = http::build_router(app_state.clone());

    // 绑定 Web 管理端监听地址。
    let addr = format!("{}:{}", loaded.server.web_bind, loaded.server.web_port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("无法监听 {}", addr))?;
    tracing::info!("Web 管理端已监听: http://{}", addr);

    // 服务启动事件：稍等节点快照稳定（2 秒）后，广播房间状态并按需发送通知邮件。
    let st = app_state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        notify::EventHub::publish(&st, "server.started").await;
    });

    // 启动 HTTP 服务；into_make_service_with_connect_info 让 WS 第三方端点可拿到客户端 IP。
    // with_graceful_shutdown 使 Ctrl-C / SIGTERM 到达后能排空在途请求再退出。
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("http server error")?;

    // HTTP 已退出：停止 EasyTier 实例（删除虚拟网卡、断开连接）。
    node.stop().await;
    tracing::info!("已停止");
    Ok(())
}

/// 判断 EasyTier 配置文本中是否真的包含网络身份段。
/// 仅用于"旧配置自动联网"兼容路径；[astral_server] 被剥离后只剩空表时不应启动实例。
fn has_instance_config(toml_str: &str) -> bool {
    toml_str.contains("[network_identity]")
}

/// 优雅关闭信号：Unix 上同时监听 Ctrl-C 与 SIGTERM（systemd stop 发的是 SIGTERM），
/// 非 Unix 平台只监听 Ctrl-C（terminate 分支用 pending 永久挂起占位）。
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

    // 任一信号到达即继续向下执行（另一个分支随之丢弃）。
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("收到退出信号，正在关闭…");
}
