//! ccode CLI 入口 - 参数解析 + 启动 kernel
//!
//! 启动模式：
//! 1. FFI 模式：通过 libccore 动态库启动（生产发布）
//! 2. Direct 模式：直接通过 Rust API 启动（开发调试）
//!
//! Direct 模式始终走分布式路径：kernel.run() → ZMQ 消息总线 → 5 Nodes

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
    #[arg(long = "router-addr", default_value = "ipc:///tmp/ccode-router")]
    router_addr: String,

    /// PUB socket 地址
    #[arg(long = "pub-addr", default_value = "ipc:///tmp/ccode-pub")]
    pub_addr: String,

    /// 工作目录
    #[arg(long = "work-dir")]
    work_dir: Option<String>,
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
    let config = load_config(&args)?;

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
        // Direct 模式：始终走分布式路径
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async move {
            // 分布式模式：ZMQ → 心跳 → Node spawn → 事件循环
            let kernel_config = ccore::kernel::KernelConfig {
                router_addr: args.router_addr.clone(),
                pub_addr: args.pub_addr.clone(),
                working_dir: args.work_dir.clone().unwrap_or_else(|| ".".into()),
                startup_prompt: args.prompt.clone(),
                ..Default::default()
            };
            let mut kernel = ccore::kernel::Kernel::new(kernel_config);
            kernel.set_runtime_config(ccore::kernel::KernelRuntimeConfig::from(&config));

            // 注入 HookDispatcher 桥接（让 ToolNode 的 pre/post_tool_use 生效）
            let hook_adapter = ccode_shell::agent::hook_adapter::HookDispatcherAdapter::new(
                ccode_hooks::discovery::HookRegistry::default(),
                None,
                false,
                uuid::Uuid::new_v4().to_string(),
                args.work_dir.clone().unwrap_or_else(|| ".".into()),
            );
            kernel.set_hook_dispatcher(std::sync::Arc::new(hook_adapter));

            kernel.run().await
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
