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

    /// Router socket 地址（默认 PID 隔离，多实例互不冲突）
    #[arg(long = "router-addr")]
    router_addr: Option<String>,

    /// PUB socket 地址（默认 PID 隔离，多实例互不冲突）
    #[arg(long = "pub-addr")]
    pub_addr: Option<String>,

    /// 工作目录
    #[arg(long = "work-dir")]
    work_dir: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // 初始化日志
    let log_level = std::env::var("CCODE_LOG").unwrap_or_else(|_| "info".into());

    // 默认写入日志文件 ~/.ccode/logs/ccode.log，避免 tracing 日志污染 TUI 终端
    // 设置 CCODE_LOG_STDERR=1 可恢复 stderr 输出（调试用）
    if std::env::var("CCODE_LOG_STDERR").is_ok_and(|v| v == "1") {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&log_level))
            )
            .init();
    } else {
        // 使用 HOME 环境变量或标准 home 目录
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".into());
        let log_dir = std::path::Path::new(&home).join(".ccode/logs");
        let _ = std::fs::create_dir_all(&log_dir);
        let log_path = log_dir.join("ccode.log");
        // 追加模式，避免每次启动覆盖
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            Ok(log_file) => {
                tracing_subscriber::fmt()
                    .with_writer(std::sync::Mutex::new(log_file))
                    .with_env_filter(
                        tracing_subscriber::EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&log_level))
                    )
                    .init();
            }
            Err(e) => {
                // 降级到 stderr：日志文件创建失败时不阻塞启动
                tracing_subscriber::fmt()
                    .with_env_filter(
                        tracing_subscriber::EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&log_level))
                    )
                    .init();
                tracing::warn!("无法创建日志文件：{:?}（{}），降级到 stderr", log_path, e);
            }
        }
    }

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
            // PID 隔离：多实例默认使用独立 socket，互不冲突
            let pid = std::process::id();
            let router_addr = args.router_addr
                .unwrap_or_else(|| format!("ipc:///tmp/ccode-router-{pid}"));
            let pub_addr = args.pub_addr
                .unwrap_or_else(|| format!("ipc:///tmp/ccode-pub-{pid}"));

            // Headless 模式：当使用 -p 参数时，设置环境变量让 TUI 跳过终端初始化
            if args.prompt.is_some() {
                std::env::set_var("CCODE_HEADLESS", "1");
            }

            // 分布式模式：ZMQ → 心跳 → Node spawn → 事件循环
            let kernel_config = ccore::kernel::KernelConfig {
                router_addr,
                pub_addr,
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

            // 扫描 Skills 目录并传递给 TUI 显示
            let skills_count = count_skills(&args.work_dir.clone().unwrap_or_else(|| ".".into()));
            kernel.set_skills_count(skills_count);

            kernel.run().await
        })?;
        // Kernel 已 shutdown，直接退出进程（避免残留 task 阻止 runtime drop）
        std::process::exit(0);
    }

    Ok(())
}

/// 从 Claude Code 配置同步 LLM 设置
///
/// 对标 Claude Code 行为：读取 ~/.claude/settings.json 中的 env 字段，
/// 导出 ANTHROPIC_BASE_URL / ANTHROPIC_MODEL / ANTHROPIC_AUTH_TOKEN 环境变量，
/// 后续 load_config 中的 env var 覆盖逻辑自动生效。
fn sync_from_claude_settings() {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return,
    };
    let settings_path = std::path::Path::new(&home).join(".claude/settings.json");
    if !settings_path.exists() {
        return;
    }
    let content = match std::fs::read_to_string(&settings_path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let settings: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return,
    };
    let env = match settings.get("env") {
        Some(v) => v,
        None => return,
    };

    // Claude Code settings.json 使用全大写的 env var 名称作为 key
    let vars = [
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_MODEL",
        "ANTHROPIC_AUTH_TOKEN",
    ];

    let mut synced_model = String::new();
    for &env_var in &vars {
        if let Some(val) = env.get(env_var).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            // 只在环境变量未设置时写入，让已有环境变量优先级更高
            if std::env::var(env_var).ok().filter(|s| !s.is_empty()).is_none() {
                std::env::set_var(env_var, val);
            }
            if env_var == "ANTHROPIC_MODEL" {
                synced_model = val.to_string();
            }
        }
    }

    if !synced_model.is_empty() {
        tracing::info!("[ccode] Synced from Claude Code: model={}", synced_model);
    }
}

/// 从 CLI 参数和配置文件构建 CcodeConfig
fn load_config(args: &Args) -> anyhow::Result<ccore::config::CcodeConfig> {
    // 对标 Claude Code：启动时自动读取 ~/.claude/settings.json 同步 LLM 配置
    sync_from_claude_settings();
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

    // 环境变量覆盖：与 Claude Code 共享 LLM 配置
    // 当 ANTHROPIC_BASE_URL 或 ANTHROPIC_MODEL 设置时，自动配置 provider
    let env_base_url = std::env::var("ANTHROPIC_BASE_URL").ok().filter(|s| !s.is_empty());
    let env_model = std::env::var("ANTHROPIC_MODEL").ok().filter(|s| !s.is_empty());
    let env_auth_token = std::env::var("ANTHROPIC_AUTH_TOKEN").ok().filter(|s| !s.is_empty());

    if env_base_url.is_some() || env_model.is_some() {
        let base_url = env_base_url.clone().unwrap_or_else(|| "https://api.anthropic.com".into());
        let model = env_model.clone().unwrap_or_else(|| "claude-sonnet-4-20250514".into());
        let api_key = env_auth_token.unwrap_or_default();

        // 检查是否已有匹配的 provider（同名或同 base_url），有则更新，无则添加
        let existing = config.providers.iter_mut().find(|p| {
            p.base_url.trim_end_matches('/') == base_url.trim_end_matches('/')
                || p.name == "claude"
        });

        if let Some(p) = existing {
            p.base_url = base_url;
            p.api_key = api_key;
            if !p.models.contains(&model) {
                p.models.push(model.clone());
            }
        } else {
            config.providers.push(ccore::config::provider::ProviderConfig {
                name: "claude".into(),
                api_key,
                base_url,
                provider_type: "claude".into(),
                models: vec![model.clone()],
                fallback: vec![],
                rate_limit: Some(50),
                api_version: Some("2023-06-01".into()),
                context_window: Some(200_000),
                supports_thinking: true,
                supports_vision: true,
                extra_headers: {
                    let mut h = std::collections::HashMap::new();
                    h.insert("anthropic-version".into(), "2023-06-01".into());
                    h
                },
            });
        }

        // 默认模型跟随 ANTHROPIC_MODEL
        config.default_model = model;
    }

    // CLI 参数覆盖配置文件（优先级最高）
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

/// 扫描 Skills 目录，统计已加载的 Skill 数量
fn count_skills(work_dir: &str) -> usize {
    let mut count = 0;
    let home = std::env::var("HOME").unwrap_or_default();

    // 扫描的目录：~/.ccode/skills/、<work_dir>/.ccode/skills/、<work_dir>/.agents/skills/
    let dirs = [
        format!("{}/.ccode/skills", home),
        format!("{}/.ccode/skills", work_dir),
        format!("{}/.agents/skills", work_dir),
    ];

    for dir in &dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // 每个子目录对应一个 Skill
                    let skill_md = path.join("SKILL.md");
                    if skill_md.exists() {
                        count += 1;
                    }
                }
            }
        }
    }
    count
}
