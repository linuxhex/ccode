//! ccode CLI 入口 — 终端 AI 编程代理
//!
//! 统一架构：单进程消息总线 + 多 Node（Thinker / Sampler / Tool / TUI / ...）
//! 启动路径：kernel.run() → ZMQ 消息总线 → Node 协作

use clap::Parser;

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

    /// Router socket 地址（默认 PID 隔离，多实例互不冲突）
    #[arg(long = "router-addr")]
    router_addr: Option<String>,

    /// PUB socket 地址（默认 PID 隔离，多实例互不冲突）
    #[arg(long = "pub-addr")]
    pub_addr: Option<String>,

    /// 工作目录
    #[arg(long = "work-dir")]
    work_dir: Option<String>,

    /// 恢复会话：`--resume <ID>` 恢复指定会话；不带参数则列出可选会话
    #[arg(long = "resume", num_args = 0..=1, default_missing_value = "")]
    resume: Option<String>,

    /// 继续当前目录最近的一次会话
    #[arg(short = 'c', long = "continue")]
    continue_session: bool,

    /// 列出已保存的会话后退出
    #[arg(long = "list-sessions")]
    list_sessions: bool,
}

/// 把三个会话相关 flag 归约成 `ResumeTarget`
///
/// 返回 `Ok(None)` = 开新会话；`Err` 里带的是给用户看的可操作错误信息
/// （互斥组合直接报错而不是随便挑一个生效——静默挑一个会让用户以为另一个也起作用了）。
fn resolve_resume_target(
    resume: Option<&str>,
    continue_session: bool,
) -> anyhow::Result<Option<ccore::node::session_index::ResumeTarget>> {
    use ccore::node::session_index::ResumeTarget;

    match (resume, continue_session) {
        (Some(_), true) => anyhow::bail!(
            "`--resume` 与 `--continue` 不能同时使用。\n  \
             继续当前目录最近一次会话用 `ccode --continue`\n  \
             恢复指定会话用 `ccode --resume <SESSION_ID>`"
        ),
        // 裸 `--resume`（不带 ID）由调用方走列表分支，不该到这里
        (Some(""), false) => Ok(None),
        (Some(id), false) => {
            ccore::node::session_index::validate_session_id(id)?;
            Ok(Some(ResumeTarget::Id(id.to_string())))
        }
        (None, true) => Ok(Some(ResumeTarget::LastInCwd)),
        (None, false) => Ok(None),
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // 会话列表：`--list-sessions` 或裸 `--resume`（不带 ID）都只打印列表后退出，
    // 不进日志初始化也不起 Kernel
    let bare_resume = args.resume.as_deref() == Some("");
    if args.list_sessions || bare_resume {
        let root = ccore::node::session_index::sessions_root();
        let sessions = ccore::node::session_index::list_sessions(&root);
        print!("{}", ccore::node::session_index::render_session_list(&sessions));
        return Ok(());
    }

    // flag 互斥校验放在日志初始化之前，错误直接打到 stderr 而不是被写进日志文件
    let resume_target = resolve_resume_target(args.resume.as_deref(), args.continue_session)?;

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

        // 消息总线 + Node 架构：ZMQ 总线 → 各 Node 协作
        let kernel_config = ccore::kernel::KernelConfig {
            router_addr,
            pub_addr,
            working_dir: args.work_dir.clone().unwrap_or_else(|| ".".into()),
            startup_prompt: args.prompt.clone(),
            resume: resume_target,
            ..Default::default()
        };
        let mut kernel = ccore::kernel::Kernel::new(kernel_config);
        kernel.set_runtime_config(ccore::kernel::KernelRuntimeConfig::from(&config));

        // 注入 HookDispatcher 桥接（让 ToolNode 的 pre/post_tool_use 生效）
        let hook_adapter = ccore::tools::hook_adapter::HookDispatcherAdapter::new(
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
    // 运行期发生过致命错误（如认证失败）时以非零码退出，
    // 让 headless 调用方（脚本/CI）能感知失败
    if ccore::has_fatal_error() {
        std::process::exit(1);
    }
    std::process::exit(0);
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

#[cfg(test)]
mod tests {
    use super::*;
    use ccore::node::session_index::ResumeTarget;

    #[test]
    fn no_flags_means_new_session() {
        assert!(resolve_resume_target(None, false).unwrap().is_none());
    }

    #[test]
    fn continue_resolves_to_last_in_cwd() {
        assert_eq!(
            resolve_resume_target(None, true).unwrap(),
            Some(ResumeTarget::LastInCwd)
        );
    }

    #[test]
    fn resume_with_id_resolves_to_that_id() {
        assert_eq!(
            resolve_resume_target(Some("9f2c4a11"), false).unwrap(),
            Some(ResumeTarget::Id("9f2c4a11".into()))
        );
    }

    #[test]
    fn resume_and_continue_are_mutually_exclusive() {
        let err = resolve_resume_target(Some("abc"), true).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("不能同时使用"));
        // 报错要顺手给出两条正确用法，而不是只说「错了」
        assert!(msg.contains("--continue"));
        assert!(msg.contains("--resume <SESSION_ID>"));
    }

    #[test]
    fn resume_rejects_path_traversal_id() {
        assert!(resolve_resume_target(Some("../../etc/passwd"), false).is_err());
    }

    #[test]
    fn bare_resume_defers_to_list_branch() {
        // 裸 `--resume` 由 main 走列表分支，这里只需保证它不被当成会话 ID
        assert!(resolve_resume_target(Some(""), false).unwrap().is_none());
    }

    #[test]
    fn args_parse_resume_variants() {
        use clap::Parser;

        let bare = Args::try_parse_from(["ccode", "--resume"]).unwrap();
        assert_eq!(bare.resume.as_deref(), Some(""));

        let with_id = Args::try_parse_from(["ccode", "--resume", "abc123"]).unwrap();
        assert_eq!(with_id.resume.as_deref(), Some("abc123"));

        let cont = Args::try_parse_from(["ccode", "-c"]).unwrap();
        assert!(cont.continue_session);

        let none = Args::try_parse_from(["ccode"]).unwrap();
        assert!(none.resume.is_none());
        assert!(!none.continue_session);
        assert!(!none.list_sessions);
    }
}
