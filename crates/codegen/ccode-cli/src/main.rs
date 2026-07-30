//! ccode CLI 入口 - 参数解析 + 启动 kernel
//!
//! 两种启动模式：
//! 1. FFI 模式：通过 libccore 动态库启动（生产发布）
//! 2. Direct 模式：直接通过 Rust API 启动（开发调试）

use clap::Parser;
use std::ffi::CString;

/// ccode - 终端 AI 编程代理
#[derive(Parser, Debug)]
#[command(name = "ccode", version, about = "终端 AI 编程代理")]
struct Args {
    /// 直接传入 prompt（headless 模式）
    #[arg(short = 'p', long = "prompt")]
    prompt: Option<String>,

    /// 使用的模型
    #[arg(short = 'm', long = "model")]
    model: Option<String>,

    /// Agent 类型：primary / explore / plan / general
    #[arg(long = "agent")]
    agent: Option<String>,

    /// 权限模式：yolo / trust / ask
    #[arg(long = "permission", default_value = "trust")]
    permission: String,

    /// 最大轮次
    #[arg(long = "max-turns")]
    max_turns: Option<u32>,

    /// 推理强度 (0.0 - 1.0)
    #[arg(long = "reasoning-effort")]
    reasoning_effort: Option<f64>,

    /// 配置文件路径
    #[arg(long = "config")]
    config: Option<String>,

    /// 输出格式：plain / json / streaming-json
    #[arg(long = "output-format", default_value = "plain")]
    output_format: String,

    /// 使用 FFI 模式启动（通过动态库）
    #[arg(long = "ffi")]
    ffi_mode: bool,

    /// Router socket 地址
    #[arg(long = "router-addr", default_value = "tcp://127.0.0.1:5555")]
    router_addr: String,

    /// PUB socket 地址
    #[arg(long = "pub-addr", default_value = "tcp://127.0.0.1:5556")]
    pub_addr: String,

    /// 工作目录
    #[arg(long = "work-dir")]
    work_dir: Option<String>,

    /// 使用分布式模式（启动 ZMQ 消息总线 + 5 Node）
    #[arg(long = "distributed")]
    distributed: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // 初始化日志
    let log_level = if std::env::var("CCODE_LOG").is_ok() {
        std::env::var("CCODE_LOG").unwrap()
    } else {
        "info".into()
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&log_level))
        )
        .init();

    // 构建配置
    let mut config = load_config(&args)?;

    // Headless 模式参数
    if let Some(prompt) = &args.prompt {
        tracing::info!("Headless 模式：prompt={:?} bytes", prompt.len());
    }

    if args.ffi_mode {
        // FFI 模式：通过动态库启动
        let config_json = serde_json::to_string(&config)?;
        let c_config = CString::new(config_json)?;
        let result = unsafe { ccore::ffi::ccore_start(c_config.as_ptr()) };
        if result != 0 {
            eprintln!("ccode 启动失败，错误码：{}", result);
            std::process::exit(result);
        }
    } else {
        // Direct 模式
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async move {
            if args.distributed {
                // 分布式模式：ZMQ → 心跳 → Node spawn → 事件循环（8步）
                let kernel_config = ccore::kernel::KernelConfig {
                    router_addr: args.router_addr.clone(),
                    pub_addr: args.pub_addr.clone(),
                    working_dir: args.work_dir.clone().unwrap_or_else(|| ".".into()),
                    ..Default::default()
                };
                let mut kernel = ccore::kernel::Kernel::new(kernel_config);
                kernel.set_ccode_config(config);
                kernel.run().await
            } else {
                // 快速启动：panic hook → AgentConfig → shell（3步到达用户交互）
                // 比 kernel.run() 少 5 步：不需要 ZMQ/Node/心跳/注册/事件循环
                quick_start(config).await
            }
        })?;
    }

    Ok(())
}

/// 从 CLI 参数和配置文件构建 CcodeConfig
fn load_config(args: &Args) -> anyhow::Result<ccore::config::CcodeConfig> {
    let mut config = if let Some(config_path) = &args.config {
        let content = std::fs::read_to_string(config_path)?;
        serde_json::from_str(&content).unwrap_or_else(|_| {
            // 尝试 TOML 格式
            toml::from_str(&content).unwrap_or_default()
        })
    } else {
        // 尝试从默认路径加载
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let default_path = std::path::Path::new(&home).join(".ccode/config.json");
        if default_path.exists() {
            let content = std::fs::read_to_string(&default_path)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            ccore::config::CcodeConfig::default()
        }
    };

    // CLI 参数覆盖配置文件
    if let Some(model) = &args.model {
        config.default_model = model.clone();
    }
    if let Some(agent) = &args.agent {
        config.default_agent_type = agent.clone();
    }

    // 解析权限模式
    config.permission_mode = match args.permission.to_lowercase().as_str() {
        "yolo" => ccore::node::PermissionMode::Yolo,
        "ask" => ccore::node::PermissionMode::Ask,
        _ => ccore::node::PermissionMode::Trust,
    };

    Ok(config)
}

/// 快速启动：跳过 ZMQ/Node，3步到达用户交互
///
/// 对比 kernel.run() 的完整启动（8步）：
/// - kernel.run(): Kernel::new → ZMQ绑定 → 心跳循环 → Node spawn → 注册等待 → 事件循环
/// - quick_start(): panic hook → shell Config → run_stdio_agent
async fn quick_start(config: ccore::config::CcodeConfig) -> anyhow::Result<()> {
    // 1. 安装 panic hook（与 kernel.run() 共享）
    ccore::kernel::panic_hook::install_panic_hook();

    tracing::info!("ccode 快速启动模式（跳过 ZMQ 消息总线）");

    // 2. 构造 shell Config（从 ccode 配置映射）
    let mut shell_config = ccode_shell::agent::config::Config::default();
    // 应用模型覆盖
    if !config.default_model.is_empty() {
        shell_config.default_model_override = Some(config.default_model.clone());
    }
    // 应用权限模式
    match config.permission_mode {
        ccore::node::PermissionMode::Yolo => {
            shell_config.default_auto_mode = true;
        }
        ccore::node::PermissionMode::Ask => {
            shell_config.default_auto_mode = false;
        }
        ccore::node::PermissionMode::Trust => {
            // 默认行为
        }
    }

    // 3. 直接启动 shell
    // shell 内部：SessionActor::new → CcoreSessionState → run_loop → 用户交互
    // 这条路径不需要 ZMQ，直接调用 ccode-sampler/ccode-tools
    ccode_shell::agent::app::run_stdio_agent(&shell_config, None, None).await
}
