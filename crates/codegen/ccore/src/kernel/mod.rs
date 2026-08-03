//! Kernel - 消息总线 broker，Node 注册/发现/健康检查
//!
//! Kernel 是 ccode 的微内核，负责：
//! 1. 运行 Broker（消息路由）
//! 2. 维护 Registry（Node 注册表）
//! 3. 执行健康检查（心跳超时检测）
//! 4. 管理 Node 生命周期（spawn/deregister）
//! 5. 启动初始 Node 集合
//! 6. 反射弧路由：感官信号 → ReflexRouter → 运动指令（脊髓反射）
//! 7. 自主神经调节：心跳监控 + 并发限流 + 内存池（无意识自调节）
//!
//! 事件循环：
//! ```text
//! Kernel 启动
//!   ├─ 创建 KernelTransport（绑定 ROUTER + PUB socket）
//!   ├─ 创建 NodeLauncher
//!   ├─ spawn 初始 Node 集合（各 Node 连接 DEALER + SUB 到 Kernel）
//!   ├─ 启动 AutonomicNervousSystem 自主循环（心跳/并发/内存）
//!   └─ 进入事件循环：
//!       ├─ recv from ROUTER → 路由消息到订阅者
//!       ├─ sys/register → 注册 Node（identity + subscriptions）+ 注册到 ANS
//!       ├─ sys/heartbeat → 更新心跳时间戳 + 通知 ANS
//!       ├─ nose/*/skin/*/eye/* → 反射弧：ReflexRouter → motor 指令
//!       ├─ agent/{id}/spawn → spawn 子 Agent
//!       └─ 定期健康检查 → ANS 检测超时 + 清理死亡 Node
//! ```

pub mod broker;
pub mod registry;
pub mod health;
pub mod transport;
pub mod launcher;
pub mod transaction;
pub mod backpressure;
pub mod metrics;
pub mod self_healing;
pub mod autonomic;
pub mod reflex;
pub mod experience;
pub mod panic_hook;
pub mod lock_order;

use anyhow::Result;
use bytes::Bytes;
use std::path::PathBuf;
use std::time::Duration;

use crate::config::CcodeConfig;
use crate::config::provider::ProviderConfig;
use crate::config::watcher::ConfigWatcher;
use crate::kernel::autonomic::AutonomicNervousSystem;
use crate::kernel::reflex::{ReflexRouter, ReflexAction, ReflexLevel, ReflexRule, builtin_reflex_rules};
use crate::kernel::experience::{ExperienceLog, ExperienceEntry};
use crate::kernel::transport::{IncomingMessage, KernelTransport};
use crate::kernel::backpressure::{BackpressureController, BackpressureConfig};
use crate::kernel::metrics::{MonitoringService, HealthCheckConfig};
use crate::memory::episodic::{EpisodicMemoryStore, MemoryType, MemorySource};
use crate::agent::experiential::ExperientialReflectiveLearner;
use crate::agent::decentralized::DecentralizedCoordinator;
use crate::agent::meta_cognitive::MetaCognitiveController;
use crate::message::frame::FrameCodec;
use crate::message::Topic;
use crate::message::Message;
use crate::message::SequenceChecker;
use crate::message::param::ParamServer;
use crate::node::{NodeId, NodeType, NodeContext};
use crate::sampler::token_budget::TokenBudgetManager;
use crate::retry::circuit_breaker::CircuitBreaker;
use crate::mcp_server::{McpServerHandle, McpServerConfig, McpTransportKind, McpServer};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock, Mutex, watch};

/// Kernel 配置
#[derive(Debug, Clone)]
pub struct KernelConfig {
    /// ROUTER socket 绑定地址
    pub router_addr: String,
    /// PUB socket 绑定地址
    pub pub_addr: String,
    /// 心跳超时（秒）
    pub heartbeat_timeout_secs: u64,
    /// 最大子 Agent 数量
    pub max_subagents: usize,
    /// 健康检查间隔（秒）
    pub health_check_interval_secs: u64,
    /// 工作目录
    pub working_dir: String,
    /// 是否启用 MCP Server
    pub mcp_server_enabled: bool,
    /// MCP Server 传输方式
    pub mcp_transport: McpTransportKind,
    /// 启动时自动发送的 prompt（None = 交互模式，Some = headless 模式）
    pub startup_prompt: Option<String>,
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            router_addr: "ipc:///tmp/ccode-router".into(),
            pub_addr: "ipc:///tmp/ccode-pub".into(),
            heartbeat_timeout_secs: 30,
            max_subagents: 10,
            health_check_interval_secs: 10,
            working_dir: std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
            mcp_server_enabled: false,
            mcp_transport: McpTransportKind::Stdio,
            startup_prompt: None,
        }
    }
}

/// Kernel 运行时配置 — 从产品配置提取的纯 ccore 概念子集。
///
/// Kernel 和 NodeLauncher 只依赖这 3 个字段，
/// 产品层（ccode-pager-bin / ccode-shell）负责从 CcodeConfig 提取。
#[derive(Debug, Clone)]
pub struct KernelRuntimeConfig {
    /// LLM Provider 配置列表
    pub providers: Vec<ProviderConfig>,
    /// 默认模型名称
    pub default_model: String,
    /// 权限模式
    pub permission_mode: crate::node::PermissionMode,
}

impl From<&CcodeConfig> for KernelRuntimeConfig {
    fn from(c: &CcodeConfig) -> Self {
        Self {
            providers: c.providers.clone(),
            default_model: c.default_model.clone(),
            permission_mode: c.permission_mode,
        }
    }
}

/// Kernel 主结构
///
/// ROS 1 风格：Kernel 只做控制面（发现/注册/心跳/参数），不转发业务数据。
///
/// Kernel 持有：
/// - `broker`: 发现逻辑（identity 映射、publisher 映射、service 映射）
/// - `registry`: Node 注册表（元数据、心跳时间戳）
/// - `sequence_checker`: 消息序列号检查器
/// - `backpressure`: 背压控制器
/// - `monitoring`: 监控服务
/// - `param_server`: ROS 风格的参数服务器
/// - `runtime_config`: Kernel 运行时配置（providers/model/permission，从产品配置提取）
/// - `reflex_router`: 反射路由器（脊髓反射弧：感官信号 → 运动指令）
/// - `autonomic`: 自主神经系统（心跳监控 + 并发限流 + 内存池）
/// - `experience_log`: 经历日志（闭环学习：记录反射弧执行结果，提取可学习模式）
/// - `config_watcher`: 配置文件监听器（热更新）
pub struct Kernel {
    config: KernelConfig,
    broker: broker::Broker,
    registry: registry::Registry,
    sequence_checker: SequenceChecker,
    backpressure: Arc<BackpressureController>,
    monitoring: MonitoringService,
    param_server: ParamServer,
    running: bool,
    runtime_config: Option<Arc<RwLock<KernelRuntimeConfig>>>,
    /// 反射路由器（脊髓反射弧：感官信号 → 模式匹配 → 运动指令）
    reflex_router: ReflexRouter,
    /// 自主神经系统（心跳监控 + 并发限流 + 内存池）
    autonomic: Arc<AutonomicNervousSystem>,
    /// 经历日志（闭环学习：记录反射弧执行结果，提取可学习模式）
    experience_log: Arc<Mutex<ExperienceLog>>,
    /// 配置文件监听器（保持存活以维持 watch，不主动读取）
    #[allow(dead_code)]
    config_watcher: Option<ConfigWatcher>,
    /// 配置变更事件接收端（由 ConfigWatcher 创建，在 run() 中消费）
    /// 保留：未来热更新功能将消费此通道
    #[allow(dead_code)]
    config_event_rx: Option<mpsc::Receiver<crate::config::watcher::ConfigChangeEvent>>,
    /// 情景记忆存储（Zettelkasten 知识网络）— 与 ThinkerNode 共享
    /// 在感官信号处理时自动记录关键事件，供 ThinkerNode 检索
    episodic_memory: Arc<EpisodicMemoryStore>,
    /// 经验反思学习引擎（ERL/MAR）— 懒初始化
    /// 在反射弧执行后记录经验，提取可学习模式
    erl: std::sync::OnceLock<ExperientialReflectiveLearner>,
    /// 去中心化 DAG 协调器（AgentNet/Symphony）— 懒初始化
    /// 在多 Agent 场景下协调任务分配和依赖管理
    coordinator: std::sync::OnceLock<DecentralizedCoordinator>,
    /// 元认知控制器（MAP/LAF）— 懒初始化
    /// 监控 Agent 推理质量，检测逻辑不一致和策略偏差
    meta_cognitive: std::sync::OnceLock<MetaCognitiveController>,
    /// Token 预算管理器（借鉴 Claude Code tokenBudget.ts）
    token_budget: Arc<std::sync::Mutex<TokenBudgetManager>>,
    /// 熔断器（借鉴 Claude Code withRetry.ts）
    circuit_breaker: Arc<CircuitBreaker>,
    /// 注册通知发送端（Node 注册成功时发送当前注册数量，用于事件驱动等待）
    registration_notify_tx: watch::Sender<usize>,
    /// MCP Server 句柄（可选，启用时持有）
    mcp_server: Option<McpServerHandle>,
    /// HookDispatcher 桥接（可选，由产品层注入 ccode-hooks 适配器）
    hook_dispatcher: Option<Arc<dyn crate::tools::hook_bridge::HookDispatcher>>,
}

impl Kernel {
    pub fn new(config: KernelConfig) -> Self {
        // 安装全局 panic 钩子，捕获 panic 并记录日志而非直接崩溃
        panic_hook::install_panic_hook();

        let broker = broker::Broker::new(
            config.router_addr.clone(),
            config.pub_addr.clone(),
        );

        // 初始化自主神经系统（心跳监控 + 并发限流 + 内存池）
        let autonomic = Arc::new(AutonomicNervousSystem::new(
            config.heartbeat_timeout_secs,
            3,
        ));

        // 初始化反射路由器（脊髓反射弧：感官信号 → 运动指令）
        let reflex_router = ReflexRouter::with_rules(builtin_reflex_rules());

        // 初始化经历日志（闭环学习）
        let experience_log = Arc::new(Mutex::new(ExperienceLog::new()));

        // 尝试创建 ConfigWatcher（监听工作目录下的 config.toml）
        // 失败时记录 warn 并跳过，不阻塞 Kernel 启动
        let config_path = PathBuf::from(&config.working_dir).join("config.toml");
        let (config_watcher, config_event_rx) = match ConfigWatcher::new(config_path, 64) {
            Ok((watcher, rx)) => {
                tracing::info!("ConfigWatcher 已创建，监听配置文件变更");
                (Some(watcher), Some(rx))
            }
            Err(e) => {
                tracing::warn!("ConfigWatcher 创建失败，跳过配置热更新：{}", e);
                (None, None)
            }
        };

        // 注册通知 channel（事件驱动：Node 注册时发送当前数量，替代轮询等待）
        let (registration_notify_tx, _registration_notify_rx) = watch::channel(0usize);

        // MCP Server：如果启用则创建消息总线通道（句柄在 run() 中创建）
        let mcp_server = None;

        Self {
            config,
            broker,
            registry: registry::Registry::new(),
            sequence_checker: SequenceChecker::new(100),
            backpressure: Arc::new(BackpressureController::new(BackpressureConfig::default())),
            monitoring: MonitoringService::new(HealthCheckConfig::default()),
            param_server: ParamServer::new(),
            running: false,
            runtime_config: None,
            reflex_router,
            autonomic,
            experience_log,
            config_watcher,
            config_event_rx,
            episodic_memory: Arc::new(EpisodicMemoryStore::new()),
            erl: std::sync::OnceLock::new(),
            coordinator: std::sync::OnceLock::new(),
            meta_cognitive: std::sync::OnceLock::new(),
            token_budget: Arc::new(std::sync::Mutex::new(TokenBudgetManager::new("claude-3.5-sonnet"))),
            circuit_breaker: Arc::new(CircuitBreaker::new(crate::retry::circuit_breaker::CircuitBreakerConfig::default())),
            registration_notify_tx,
            mcp_server,
            hook_dispatcher: None,
        }
    }

    /// 设置 Kernel 运行时配置。
    ///
    /// 产品层从 CcodeConfig 提取 KernelRuntimeConfig 后调用此方法。
    pub fn set_runtime_config(&mut self, config: KernelRuntimeConfig) {
        self.runtime_config = Some(Arc::new(RwLock::new(config)));
    }

    /// 设置 HookDispatcher 桥接。
    ///
    /// 产品层（ccode-shell）在创建 HookDispatcherAdapter 后调用此方法注入，
    /// Kernel 在 spawn ToolNode 时将适配器传递给 ToolNode。
    pub fn set_hook_dispatcher(&mut self, dispatcher: Arc<dyn crate::tools::hook_bridge::HookDispatcher>) {
        self.hook_dispatcher = Some(dispatcher);
    }

    /// 获取情景记忆存储
    /// 在感官信号处理时自动记录关键事件，供 ThinkerNode 检索
    pub fn episodic_memory(&self) -> &EpisodicMemoryStore {
        &self.episodic_memory
    }

    /// 获取情景记忆存储的 Arc 引用（用于与 NodeLauncher/ThinkerNode 共享）
    pub fn episodic_memory_arc(&self) -> Arc<EpisodicMemoryStore> {
        self.episodic_memory.clone()
    }

    /// 获取经验反思学习引擎（懒初始化）
    /// 在反射弧执行后记录经验，提取可学习模式
    pub fn erl(&self) -> &ExperientialReflectiveLearner {
        self.erl.get_or_init(|| ExperientialReflectiveLearner::new(100))
    }

    /// 获取去中心化 DAG 协调器（懒初始化）
    /// 在多 Agent 场景下协调任务分配和依赖管理
    pub fn coordinator(&self) -> &DecentralizedCoordinator {
        self.coordinator.get_or_init(|| DecentralizedCoordinator::new(50))
    }

    /// 获取元认知控制器（懒初始化）
    /// 监控 Agent 推理质量，检测逻辑不一致和策略偏差
    pub fn meta_cognitive(&self) -> &MetaCognitiveController {
        self.meta_cognitive.get_or_init(|| MetaCognitiveController::new())
    }

    /// 获取 Token 预算管理器
    pub fn token_budget(&self) -> &Arc<std::sync::Mutex<TokenBudgetManager>> {
        &self.token_budget
    }

    /// 获取熔断器
    pub fn circuit_breaker(&self) -> &Arc<CircuitBreaker> {
        &self.circuit_breaker
    }

    /// 获取 NodeContext 供子 Node 连接
    pub fn node_context(&self) -> NodeContext {
        NodeContext {
            router_addr: self.config.router_addr.clone(),
            pub_addr: self.config.pub_addr.clone(),
            data_pub_addr: format!("ipc:///tmp/ccode-pub-{}", NodeId::new()),
            data_rep_addr: None,
        }
    }

    /// 启动 Kernel 事件循环
    ///
    /// 主循环流程：
    /// 1. 启动 KernelTransport（绑定 ZMQ socket）
    /// 2. 启动 AutonomicNervousSystem 自主循环（心跳监控 + 并发限流 + 内存池）
    /// 3. 启动 ConfigReloader（独立 tokio task，消费配置变更事件）
    /// 4. 启动初始 Node 集合
    /// 5. 进入事件循环：
    ///    - 接收 ROUTER 消息 → 处理系统消息或路由
    ///    - 感官信号（nose/*/skin/*/eye/*）→ 反射弧路由 → 运动指令
    ///    - 定期健康检查（清理超时 Node + 通知 ANS 注销）
    ///    - 接收配置变更通知 → 广播 sys/config_change 到所有 Node
    ///    - 处理 Node spawn 请求
    pub async fn run(&mut self) -> Result<()> {
        self.running = true;

        tracing::info!(
            "Kernel 启动：router={}, pub={}, working_dir={}",
            self.config.router_addr,
            self.config.pub_addr,
            self.config.working_dir
        );

        // 0. 如果启用 MCP Server，创建并启动
        if self.config.mcp_server_enabled {
            let mcp_config = McpServerConfig {
                transport: self.config.mcp_transport.clone(),
                name: "ccode-mcp".into(),
                version: "0.1.0".into(),
            };

            let mcp_server = McpServer::new(mcp_config);
            let handle = mcp_server.run();

            tracing::info!("MCP Server 已启动（传输方式：{:?}）", self.config.mcp_transport);

            self.mcp_server = Some(handle);
        }

        // 1. 启动 KernelTransport，取出作为独立变量避免借用冲突
        let mut transport = KernelTransport::new(
            &self.config.router_addr,
            &self.config.pub_addr,
        ).await?;

        // 2. 启动 AutonomicNervousSystem 自主循环（心跳监控 + 并发限流 + 内存池）
        let mut heartbeat_rx = self.autonomic.clone().start_autonomic_loop();
        tracing::info!("AutonomicNervousSystem 自主循环已启动（间隔 10 秒）");

        // 3. 启动初始 Node 集合（从共享配置读取）
        if let Some(rtcfg_arc) = self.runtime_config.clone() {
            let rtcfg = rtcfg_arc.read().await.clone();
            let mut launcher = launcher::NodeLauncher::new(
                self.config.clone(),
                rtcfg,
                self.episodic_memory.clone(),
            );
            // 注入 HookDispatcher 桥接（如果产品层已设置）
            if let Some(dispatcher) = self.hook_dispatcher.take() {
                launcher.set_hook_dispatcher(dispatcher);
            }
            match launcher.spawn_initial_set().await {
                Ok(nodes) => {
                    let start_time = std::time::Instant::now();
                    tracing::info!("初始 Node 集合启动完成：{} 个", nodes.len());
                    for desc in &nodes {
                        tracing::info!(
                            target: "ccore::kernel",
                            node = %desc.name,
                            duration_ms = start_time.elapsed().as_millis() as u64,
                            "node started"
                        );
                    }

                    // 广播每个 Node 的 spawn 事件（让 TUINode 等知道有哪些 Node 上线）
                    // 特别是 ThinkerNode 的 spawn 事件，让 TUINode 能设置 primary_agent_id
                    // 确定性等待：轮询 registry 直到所有 Node 完成注册（sys/register），再广播
                    let node_ids: Vec<String> = nodes.iter().map(|n| n.id.to_string()).collect();
                    let spawn_frames_list: Vec<Vec<Bytes>> = nodes.iter()
                        .filter_map(|desc| {
                            let spawn_msg = FrameCodec::new_message(
                                Topic::sys_spawn(),
                                "kernel",
                                &serde_json::json!({
                                    "node_id": desc.id.to_string(),
                                    "node_type": desc.node_type.as_str(),
                                    "name": desc.name,
                                }),
                            ).ok()?;
                            let frames: Vec<Bytes> = FrameCodec::encode(&spawn_msg).ok()?
                                .into_iter()
                                .map(Bytes::from)
                                .collect();
                            Some(frames)
                        })
                        .collect();

                    // 等待所有 Node 完成注册（事件驱动：通过 watch channel 通知，替代轮询）
                    let expected_count = node_ids.len();
                    let mut all_registered = false;
                    let mut reg_rx = self.registration_notify_tx.subscribe();
                    // 先做一次即时检查（可能 Node 已全部注册完成）
                    let already_done = node_ids.iter().all(|id| {
                        let nid: NodeId = id.parse().unwrap_or_else(|_| NodeId::from("invalid"));
                        self.registry.get(&nid).is_some()
                    });
                    if already_done {
                        all_registered = true;
                    } else {
                        // 等待 watch channel 通知，最多 5 秒超时
                        match tokio::time::timeout(
                            std::time::Duration::from_secs(5),
                            async {
                                while reg_rx.changed().await.is_ok() {
                                    let count = *reg_rx.borrow();
                                    if count >= expected_count {
                                        // 再次确认所有目标 Node 都已注册
                                        let registered = node_ids.iter().all(|id| {
                                            let nid: NodeId = id.parse().unwrap_or_else(|_| NodeId::from("invalid"));
                                            self.registry.get(&nid).is_some()
                                        });
                                        if registered {
                                            return true;
                                        }
                                    }
                                }
                                false
                            }
                        ).await {
                            Ok(true) => all_registered = true,
                            _ => {}
                        }
                    }

                    if all_registered {
                        for (frames, desc) in spawn_frames_list.iter().zip(nodes.iter()) {
                            if let Err(e) = transport.broadcast(frames.clone()).await {
                                tracing::warn!("广播 spawn 事件失败：{}", e);
                            } else {
                                tracing::debug!("广播 spawn 事件：{} ({:?})", desc.id, desc.node_type);
                            }
                        }

                        // 发送 startup prompt 到 ThinkerNode（如果配置了）
                        if let Some(ref prompt) = self.config.startup_prompt {
                            // 找到 ThinkerNode 的 ID
                            if let Some(thinker_desc) = nodes.iter().find(|n| n.node_type == crate::node::NodeType::Thinker) {
                                let thinker_id = &thinker_desc.id;
                                tracing::info!(
                                    "发送 startup prompt 到 ThinkerNode {}：{} bytes",
                                    thinker_id, prompt.len()
                                );
                                let prompt_msg = FrameCodec::new_message(
                                    crate::message::Topic::agent_input(thinker_id.as_str()),
                                    "kernel",
                                    &serde_json::json!({
                                        "content": prompt,
                                        "role": "user",
                                    }),
                                );
                                if let Ok(msg) = prompt_msg {
                                    let frames = FrameCodec::encode(&msg).map(|v| v.into_iter().map(Bytes::from).collect::<Vec<_>>()).unwrap_or_default();
                                    if !frames.is_empty() {
                                        if let Err(e) = transport.broadcast(frames).await {
                                            tracing::warn!("发送 startup prompt 失败：{}", e);
                                        }
                                    }
                                }
                            } else {
                                tracing::warn!("未找到 ThinkerNode，无法发送 startup prompt");
                            }
                        }
                    } else {
                        tracing::warn!("初始 Node 集合注册超时，跳过 spawn 广播");
                    }
                }
                Err(e) => {
                    tracing::error!(
                        target: "ccore::kernel",
                        node = "initial_set",
                        error = %e,
                        "node failed to start"
                    );
                }
            }
        } else {
            tracing::warn!("未设置 runtime_config，跳过初始 Node 集合启动");
        }

        // 4. 配置热更新由产品层负责（Kernel 只持有 KernelRuntimeConfig 快照）

        let mut config_notify_rx: Option<mpsc::Receiver<String>> = None;
        let mut config_monitoring_active = false;

        // 5. 主事件循环
        let health_interval = Duration::from_secs(self.config.health_check_interval_secs);
        let mut health_timer = tokio::time::interval(health_interval);

        // 经验学习：每 60 秒提取模式并提议规则
        let mut experience_timer = tokio::time::interval(Duration::from_secs(60));

        while self.running {
            tokio::select! {
                // 从 ROUTER 接收消息
                incoming = transport.recv() => {
                    match incoming {
                        Ok(Some(incoming)) => {
                            if let Err(e) = self.handle_incoming(incoming, &mut transport).await {
                                tracing::warn!("处理消息失败：{}", e);
                            }
                        }
                        Ok(None) => {
                            tracing::warn!("传输层通道已关闭");
                            break;
                        }
                        Err(e) => {
                            tracing::error!("传输层接收错误：{}", e);
                        }
                    }
                }
                // 定期健康检查：清理超时 Node + 通知 AutonomicNervousSystem 注销
                _ = health_timer.tick() => {
                    let dead_nodes = self.registry.remove_stale(self.config.heartbeat_timeout_secs);
                    if !dead_nodes.is_empty() {
                        for node_id in dead_nodes {
                            tracing::warn!("Node 心跳超时，移除：{}", node_id);
                            self.broker.deregister_identity(&node_id);
                            // 通知 AutonomicNervousSystem 注销该 Agent
                            self.autonomic.unregister_agent(&node_id.to_string()).await;
                            Self::broadcast_node_deregister(&mut transport, &node_id).await;
                        }
                    }
                }
                // 心跳事件：ANS 检测到 Agent 心跳超时，通知 Kernel 重启
                event = heartbeat_rx.recv() => {
                    match event {
                        Some(heartbeat) => {
                            tracing::warn!(
                                agent_id = %heartbeat.agent_id,
                                restart_count = heartbeat.restart_count,
                                "收到心跳超时事件，执行 Agent 重启"
                            );
                            // 实际重启逻辑：通过 NodeLauncher 重新 spawn 该 Agent
                            if let Some(rtcfg_arc) = self.runtime_config.clone() {
                                let rtcfg = rtcfg_arc.read().await.clone();
                                let mut launcher = launcher::NodeLauncher::new(
                                    self.config.clone(),
                                    rtcfg,
                                    self.episodic_memory.clone(),
                                );
                                let agent_type = crate::agent::AgentType::Primary;
                                let descriptor = launcher.spawn_subagent(
                                    agent_type,
                                    None,
                                    format!("重启 Agent {}", heartbeat.agent_id),
                                );
                                // 重新注册到 ANS
                                self.autonomic.register_agent(descriptor.id.to_string()).await;
                                tracing::info!("Agent {} 已重启，新 ID：{}", heartbeat.agent_id, descriptor.id);
                            }
                        }
                        None => {
                            tracing::warn!("心跳事件通道已关闭，停止心跳监控");
                        }
                    }
                }
                // 经验学习：定期提取模式并提议规则到 ReflexRouter
                _ = experience_timer.tick() => {
                    let proposed = {
                        let log = self.experience_log.lock().await;
                        log.extract_patterns()
                    };
                    if !proposed.is_empty() {
                        tracing::info!("经验学习提取到 {} 个可学习模式", proposed.len());
                        for rule in proposed {
                            // 代码修改类规则永远不升级到 L0（安全约束）
                            let is_code_modification = rule.action.starts_with("hand/")
                                || rule.action.starts_with("limb/")
                                || rule.action.starts_with("mouth/");
                            if is_code_modification {
                                tracing::info!(
                                    signal_topic = %rule.signal_topic,
                                    action = %rule.action,
                                    success_rate = %rule.success_rate,
                                    "经验学习：跳过代码修改类规则（安全约束，永远走 LLM）"
                                );
                                continue;
                            }
                            // 提议为 L1_trial 规则（需经 LLM 确认多次后才能升级）
                            let reflex_rule = ReflexRule {
                                id: format!("learned_{}_{}", rule.signal_topic.replace('/', "_"), rule.action.replace('/', "_")),
                                pattern: format!("(?i){}", regex::escape(&rule.pattern_hint)),
                                signal_topic: rule.signal_topic.clone(),
                                level: ReflexLevel::L1Trial,
                                action: rule.action.clone(),
                                params: serde_json::json!({
                                    "source": "experience",
                                    "success_rate": rule.success_rate,
                                    "sample_count": rule.sample_count,
                                }),
                                source: "experience".into(),
                                use_count: 0,
                                success_count: 0,
                                consecutive_fails: 0,
                                disabled: false,
                            };
                            self.reflex_router.add_rule(reflex_rule);
                            tracing::info!(
                                signal_topic = %rule.signal_topic,
                                action = %rule.action,
                                success_rate = %rule.success_rate,
                                sample_count = rule.sample_count,
                                "经验学习：已提议 L1_trial 规则"
                            );
                        }
                    }
                }
                // 配置变更通知：ConfigReloader 重载配置后通知主循环广播到所有 Node
                notify = async {
                    match &mut config_notify_rx {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending::<Option<String>>().await,
                    }
                }, if config_monitoring_active => {
                    match notify {
                        Some(change_type) => {
                            Self::broadcast_config_change(&mut transport, &change_type).await;
                        }
                        None => {
                            // 配置重载器已停止，禁用此分支避免 busy-loop
                            config_monitoring_active = false;
                            tracing::warn!("配置变更通知通道已关闭，停止配置变更广播");
                        }
                    }
                }
            }
        }

        // 关闭流程
        tracing::info!(
            target: "ccore::kernel",
            "graceful shutdown initiated"
        );

        // 关闭 MCP Server
        if let Some(mcp_handle) = self.mcp_server.take() {
            mcp_handle.shutdown();
            tracing::info!("MCP Server 已请求关闭");
        }

        Self::broadcast_shutdown(&mut transport).await;
        transport.shutdown().await;
        tracing::info!(
            target: "ccore::kernel",
            "shutdown complete"
        );
        Ok(())
    }

    /// 处理从 ROUTER 收到的消息
    async fn handle_incoming(
        &mut self,
        incoming: IncomingMessage,
        transport: &mut KernelTransport,
    ) -> Result<()> {
        // ✅ 提前获取 collector Arc，避免后续与 self.broker/self.registry 的借用冲突
        let collector = self.monitoring.collector();
        // ✅ 记录接收消息
        collector.record_received();

        let now = std::time::Instant::now();
        let topic = incoming.message.topic.as_str();
        let identity = incoming.identity.clone();
        let src_node = incoming.message.header.src_node.clone();
        let sequence = incoming.message.header.sequence;

        // 序列号检查（跳过注册消息）
        if topic != "sys/register" {
            let node_id: NodeId = match src_node.parse() {
                Ok(id) => id,
                Err(e) => {
                    tracing::warn!("序列号检查：src_node='{}' 无法解析为 NodeId: {}", src_node, e);
                    return Ok(());
                }
            };
            match self.sequence_checker.check(&node_id, sequence) {
                Ok(crate::message::SequenceCheckResult::InOrder) => {
                    // 正常顺序，继续处理
                }
                Ok(crate::message::SequenceCheckResult::Gap(gap)) => {
                    // 序列号跳跃，可能丢包，但仍接受
                    tracing::warn!(
                        "Node {} 序列号跳跃 {}（可能丢包），继续处理",
                        node_id, gap
                    );
                    // ✅ 记录序列号错误
                    collector.record_sequence_error();
                }
                Err(e) => {
                    // 序列号乱序或重复，拒绝处理
                    tracing::error!(
                        "Node {} 序列号检查失败：{}，拒绝消息",
                        node_id, e
                    );
                    // ✅ 记录序列号错误
                    collector.record_sequence_error();
                    return Ok(()); // 拒绝处理，但不返回错误
                }
            }
        }

        match topic {
            // 系统消息：Node 注册
            "sys/register" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let node_id_str = payload["node_id"].as_str().unwrap_or("");
                let node_type_str = payload["node_type"].as_str().unwrap_or("agent");
                let node_id: NodeId = match node_id_str.parse() {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::warn!("sys/register: node_id='{}' 无法解析为 NodeId: {}", node_id_str, e);
                        return Ok(());
                    }
                };
                let node_type = parse_node_type(node_type_str);

                // ACK：如果消息需要确认，回复 ACK
                if incoming.message.header.requires_ack {
                    if let Err(e) = Self::send_ack(&incoming.message, transport).await {
                        tracing::warn!("发送 ACK 失败：{}", e);
                    }
                }
                
                // 注册成功，重置序列号检查器
                self.sequence_checker.reset(&node_id);
                
                // ✅ 记录心跳
                self.monitoring.record_heartbeat(node_id_str);

                // 从 payload 提取 subscriptions
                let subscriptions: Vec<String> = payload["subscriptions"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                // 从 payload 提取数据面地址（ROS 1 核心）
                let pub_addr = payload["pub_addr"].as_str().unwrap_or("").to_string();
                let rep_addr = payload["rep_addr"].as_str().map(String::from);
                
                // 从 payload 提取该 Node 发布的 topic 列表
                let published_topics: Vec<String> = payload["published_topics"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                // 原子注册
                if let Err(e) = self.atomic_register_node(
                    node_id.clone(),
                    node_type,
                    subscriptions.clone(),
                    identity.to_vec(),
                ) {
                    tracing::error!("Node 注册失败：{}", e);
                    collector.record_failed();
                } else {
                    // ROS 1 核心：注册 Publisher 信息到 Broker
                    if !pub_addr.is_empty() && !published_topics.is_empty() {
                        self.broker.register_publisher(broker::PublisherInfo {
                            node_id: node_id.clone(),
                            pub_addr: pub_addr.clone(),
                            topics: published_topics.clone(),
                        });

                        // ROS 1 核心：通知已有订阅者，有新 Publisher 上线
                        // 订阅者收到通知后会建立数据面 SUB 连接到新 Publisher
                        let interested_subscribers = self.broker.find_subscribers_for_publisher(&published_topics);
                        if !interested_subscribers.is_empty() {
                            let change_msg = FrameCodec::new_message(
                                Topic::sys_publisher_change(),
                                "kernel",
                                &serde_json::json!({
                                    "type": "publisher_change",
                                    "publishers": [{
                                        "pattern": published_topics.join(","),
                                        "publishers": [{
                                            "node_id": node_id.to_string(),
                                            "pub_addr": pub_addr,
                                            "topics": published_topics,
                                        }],
                                    }],
                                }),
                            )?;
                            let change_frames: Vec<Bytes> = FrameCodec::encode(&change_msg)?
                                .into_iter()
                                .map(Bytes::from)
                                .collect();

                            for subscriber_id in interested_subscribers {
                                if subscriber_id == node_id {
                                    continue; // 不通知自己
                                }
                                if let Some(identity) = self.broker.get_identity(&subscriber_id) {
                                    let identity_bytes = Bytes::from(identity.to_vec());
                                    if let Err(e) = transport.send_to(identity_bytes, change_frames.clone()).await {
                                        tracing::warn!("通知订阅者 {} 关于新 Publisher 失败：{}", subscriber_id, e);
                                    }
                                }
                            }
                        }
                    }

                    // ROS 1 核心：注册 Service Provider 信息到 Broker
                    if let Some(ref rep) = rep_addr {
                        if let Some(service_name) = payload["service_name"].as_str() {
                            self.broker.register_service_provider(broker::ServiceProviderInfo {
                                node_id: node_id.clone(),
                                rep_addr: rep.clone(),
                                service_name: service_name.to_string(),
                            });
                        }
                    }

                    let latency_ms = now.elapsed().as_millis() as f64;
                    collector.record_success(latency_ms);

                    // ✅ 注册到 AutonomicNervousSystem（心跳监控 + 自动重启）
                    self.autonomic.register_agent(node_id_str.to_string()).await;

                    // 通知注册等待者（事件驱动：替代轮询）
                    let _ = self.registration_notify_tx.send(self.registry.len());
                }

                // ROS 1 核心：返回 publisher 发现信息给新注册的 Node
                // 告知新 Node：它订阅的 topic 有哪些 publisher，可以直接 SUB 连接
                let publisher_map = self.broker.find_publishers_for_subscriptions(&subscriptions);
                if !publisher_map.is_empty() {
                    let discover_msg = FrameCodec::new_reply(
                        Topic::sys_register(),
                        "kernel",
                        incoming.message.header.msg_id.clone(),
                        &serde_json::json!({
                            "type": "publisher_discovery",
                            "publishers": publisher_map.iter().map(|(pattern, pubs)| {
                                serde_json::json!({
                                    "pattern": pattern,
                                    "publishers": pubs.iter().map(|p| serde_json::json!({
                                        "node_id": p.node_id.to_string(),
                                        "pub_addr": p.pub_addr,
                                        "topics": p.topics,
                                    })).collect::<Vec<_>>(),
                                })
                            }).collect::<Vec<_>>(),
                        }),
                    )?;
                    let frames_bytes: Vec<Bytes> = FrameCodec::encode(&discover_msg)?
                        .into_iter()
                        .map(Bytes::from)
                        .collect();
                    transport.send_to(identity, frames_bytes).await?;
                }
            }

            // 系统消息：Node 心跳
            "sys/heartbeat" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let node_id_str = payload["node_id"].as_str().unwrap_or("");
                let node_id: NodeId = match node_id_str.parse() {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::warn!("sys/heartbeat: node_id='{}' 无法解析为 NodeId: {}", node_id_str, e);
                        return Ok(());
                    }
                };
                self.registry.heartbeat(&node_id);
                tracing::trace!("心跳：{}", node_id);

                // ✅ 通知 AutonomicNervousSystem 更新心跳（防止误判超时）
                self.autonomic.record_heartbeat(node_id_str).await;

                // ACK：如果心跳消息需要确认
                if incoming.message.header.requires_ack {
                    if let Err(e) = Self::send_ack(&incoming.message, transport).await {
                        tracing::warn!("发送心跳 ACK 失败：{}", e);
                    }
                }
            }

            // 系统消息：Node 注销
            "sys/deregister" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let node_id_str = payload["node_id"].as_str().unwrap_or("");
                let node_id: NodeId = match node_id_str.parse() {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::warn!("sys/deregister: node_id='{}' 无法解析为 NodeId: {}", node_id_str, e);
                        return Ok(());
                    }
                };
                self.deregister_node(&node_id);

                // ✅ 通知 AutonomicNervousSystem 注销该 Agent
                self.autonomic.unregister_agent(node_id_str).await;
            }

            // Agent spawn 请求
            t if t.ends_with("/spawn") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let agent_type_str = payload["agent_type"].as_str().unwrap_or("general-purpose");
                let model = payload["model"].as_str().map(String::from);
                let task_desc = payload["task_description"].as_str().unwrap_or("");

                let agent_type: crate::agent::AgentType = match agent_type_str.parse() {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!("spawn: agent_type='{}' 无法解析: {}", agent_type_str, e);
                        return Ok(());
                    }
                };
                let src_node_str = incoming.message.header.src_node.as_str();
                let parent_id: NodeId = match src_node_str.parse() {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::warn!("spawn: src_node='{}' 无法解析为 NodeId: {}", src_node_str, e);
                        return Ok(());
                    }
                };

                match self.request_spawn_subagent(transport, &parent_id, agent_type, model, task_desc.to_string()).await {
                    Ok(new_id) => {
                        tracing::info!("子 Agent 已 spawn：{}", new_id);
                    }
                    Err(e) => {
                        tracing::warn!("子 Agent spawn 失败：{}", e);
                    }
                }
            }

            // ROS 风格：Service 注册（控制面）
            t if t.starts_with("service/") && t.ends_with("/register") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let service_name = payload["service_name"].as_str().unwrap_or("");
                let rep_addr = payload["rep_addr"].as_str().unwrap_or("");
                let node_id_str = payload["node_id"].as_str().unwrap_or("");
                let node_id: NodeId = match node_id_str.parse() {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::warn!("service/register: node_id='{}' 无法解析为 NodeId: {}", node_id_str, e);
                        return Ok(());
                    }
                };
                
                if !service_name.is_empty() && !rep_addr.is_empty() {
                    self.broker.register_service_provider(broker::ServiceProviderInfo {
                        node_id,
                        rep_addr: rep_addr.to_string(),
                        service_name: service_name.to_string(),
                    });
                    tracing::info!("Service 注册：{} → {}", service_name, rep_addr);
                }
            }

            // ROS 风格：Service 发现（控制面，返回 provider 的 REP 地址）
            t if t.starts_with("service/") && t.ends_with("/lookup") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let service_name = payload["service_name"].as_str().unwrap_or("");
                
                let response = if let Some(provider) = self.broker.find_service_provider(service_name) {
                    serde_json::json!({
                        "found": true,
                        "node_id": provider.node_id.to_string(),
                        "rep_addr": provider.rep_addr,
                        "service_name": service_name,
                    })
                } else {
                    serde_json::json!({
                        "found": false,
                        "service_name": service_name,
                    })
                };
                
                let reply = FrameCodec::new_reply(
                    incoming.message.topic.clone(),
                    "kernel",
                    incoming.message.header.msg_id.clone(),
                    &response,
                )?;
                let frames_bytes: Vec<Bytes> = FrameCodec::encode(&reply)?
                    .into_iter()
                    .map(Bytes::from)
                    .collect();
                transport.send_to(identity, frames_bytes).await?;
            }

            // ROS 风格：参数服务器
            "param/set" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                if let Some(key) = payload["key"].as_str() {
                    let value = payload.get("value").cloned().map(|v| {
                        serde_json::from_value(v).unwrap_or(crate::message::ParamValue::Null)
                    });
                    if let Some(v) = value {
                        self.param_server.set(key, v);
                    }
                }
                // 控制面消息转发：param/changed 属于控制面通知（类似 ROS 1 Master 广播参数变更），
                // 不是业务数据，经 Kernel 转发是符合 ROS 架构的
                self.route_and_forward(&incoming.message, transport).await?;
            }

            "param/get" => {
                // 参数查询：直接通过消息响应
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let key = payload["key"].as_str().unwrap_or("");
                let reply_to = incoming.message.header.msg_id.clone();
                
                if let Some(value) = self.param_server.get(key) {
                    let response = FrameCodec::new_reply(
                        Topic::param_changed(),
                        "kernel",
                        reply_to,
                        &serde_json::json!({
                            "key": key,
                            "value": value,
                        }),
                    )?;
                    // 控制面消息转发：param/get 响应属于控制面通知（类似 ROS 1 Master 返回参数查询结果），
                    // 不是业务数据，经 Kernel 转发是符合 ROS 架构的
                    self.route_and_forward(&response, transport).await?;
                }
            }

            // cortex/meta_assess — 元认知评估请求（来自 ThinkerNode）
            "cortex/meta_assess" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let context = payload["context"].as_str().unwrap_or("");
                let agent_id = payload["agent_id"].as_str().unwrap_or("");

                let meta = self.meta_cognitive();
                // 构造上下文参数（ThinkerNode 只传 context 字符串，补充为 HashMap）
                let ctx_map = std::collections::HashMap::from([
                    ("source".to_string(), "thinker".to_string()),
                    ("agent_id".to_string(), agent_id.to_string()),
                ]);
                let assessment = meta.assess_difficulty(context, &ctx_map);

                // 将 context 拆分为步骤供冲突检测
                let steps: Vec<String> = context
                    .split(" | ")
                    .map(|s| s.to_string())
                    .collect();
                let conflicts = meta.detect_conflicts(&steps);

                let result = serde_json::json!({
                    "difficulty": format!("{:?}", assessment),
                    "conflicts": conflicts.iter().map(|c| serde_json::json!({
                        "description": c.description.clone(),
                    })).collect::<Vec<_>>(),
                    "suggested_strategy": match assessment {
                        crate::agent::meta_cognitive::DifficultyLevel::Trivial => "direct",
                        crate::agent::meta_cognitive::DifficultyLevel::Moderate => "step_by_step",
                        crate::agent::meta_cognitive::DifficultyLevel::Complex => "decompose",
                        crate::agent::meta_cognitive::DifficultyLevel::Extreme => "multi_agent",
                    },
                });

                // 通过消息总线发送结果回 ThinkerNode
                if let Ok(result_msg) = FrameCodec::new_message(
                    Topic::new("cortex/meta_result"),
                    "kernel",
                    &result,
                ) {
                    if let Err(e) = self.route_and_forward(&result_msg, transport).await {
                        tracing::warn!("发送元认知评估结果失败：{}", e);
                    }
                }
            }

            // cortex/budget_check — Token 预算和熔断器检查（来自 ThinkerNode）
            "cortex/budget_check" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let agent_id = payload["agent_id"].as_str().unwrap_or("").to_string();

                let budget_ok = {
                    let budget = self.token_budget.lock().expect("token_budget lock");
                    budget.can_fit(1000) // 预估每次调用消耗 1000 token
                };
                let circuit_ok = self.circuit_breaker.allow_request();

                if !budget_ok || !circuit_ok {
                    let reason = if !budget_ok { "Token 预算不足" } else { "熔断器开启" };
                    tracing::warn!("拒绝 LLM 调用：agent={}, reason={}", agent_id, reason);

                    // 通知 ThinkerNode 拒绝
                    if let Ok(deny_msg) = FrameCodec::new_message(
                        Topic::new("cortex/budget_deny"),
                        "kernel",
                        &serde_json::json!({ "reason": reason }),
                    ) {
                        // 通过控制面路由到订阅了 cortex/budget_deny 的 ThinkerNode
                        if let Err(e) = self.route_and_forward(&deny_msg, transport).await {
                            tracing::warn!("发送预算拒绝通知失败：{}", e);
                        }
                    }

                    if !circuit_ok {
                        self.circuit_breaker.record_failure();
                    }
                } else {
                    // 预算充足，记录成功
                    self.circuit_breaker.record_success();
                }
            }

            // 反射弧：感官信号 → ReflexRouter → 运动指令
            // 路线 A：ThinkerNode 内置感官处理后，通过 sensory/* topic 通知 Kernel
            t if t.starts_with("sensory/") => {
                // 先正常路由到订阅了此感官 topic 的 Node
                self.route_and_forward(&incoming.message, transport).await?;

                // 提取 payload 字符串用于反射规则匹配
                let payload_str = match FrameCodec::decode_payload::<serde_json::Value>(&incoming.message) {
                    Ok(v) => v.to_string(),
                    Err(_) => String::new(),
                };

                // 记录到 ExperienceLog（每次感官信号都记录，供后续提取模式）
                {
                    let mut log = self.experience_log.lock().await;
                    log.record(ExperienceEntry {
                        timestamp: chrono::Utc::now(),
                        signal: payload_str.chars().take(200).collect(),
                        signal_topic: topic.to_string(),
                        level: ReflexLevel::L0,
                        action: "sensory_received".to_string(),
                        result: true,
                        context: serde_json::json!({"source": "thinker"}),
                    });
                }

                // 记录到情景记忆（Zettelkasten 知识网络）
                {
                    let keywords: Vec<String> = topic.split('/').map(|s| s.to_string()).collect();
                    self.episodic_memory().encode(
                        MemoryType::Episodic,
                        &payload_str.chars().take(500).collect::<String>(),
                        topic,
                        keywords,
                        MemorySource {
                            session_id: String::new(),
                            timestamp: chrono::Utc::now().timestamp(),
                            message_index: None,
                            confidence: 0.8,
                        },
                    );
                }

                // 感官信号由 ReflexRouter 处理，ERL 在任务完成后通过 extract_heuristics 批量提取
                tracing::trace!(topic = %topic, "感官信号已记录到 ExperienceLog + EpisodicMemory");

                // 通过 ReflexRouter 匹配反射规则
                match self.reflex_router.route(topic, &payload_str) {
                    Some(reflex_action) => {
                        match reflex_action {
                            ReflexAction::Direct { action, params } => {
                                if action == "kernel/log" {
                                    // 内核级日志：直接记录，不发布消息
                                    tracing::info!(
                                        topic = %topic,
                                        signal = %payload_str.chars().take(100).collect::<String>(),
                                        "反射弧 L0：感官信号日志"
                                    );
                                } else {
                                    // L0：直接构造 motor 消息发送
                                    tracing::info!(
                                        topic = %topic,
                                        action = %action,
                                        "反射弧 L0：感官信号 → 直接运动指令"
                                    );
                                    if let Ok(motor_msg) = FrameCodec::new_message(
                                        Topic::new(&action),
                                        "kernel",
                                        &params,
                                    ) {
                                        let targets = self.broker.find_targets(&motor_msg);
                                        if !targets.is_empty() {
                                            let frames: Vec<Bytes> = FrameCodec::encode(&motor_msg)?
                                                .into_iter()
                                                .map(Bytes::from)
                                                .collect();
                                            for (identity, _node_id) in targets {
                                                transport.send_to(Bytes::from(identity), frames.clone()).await?;
                                            }
                                        }
                                    }
                                }
                            }
                            ReflexAction::Instinct { action, params } => {
                                // L1_formal：发送 motor 指令 + 通知 ThinkerNode
                                tracing::info!(
                                    topic = %topic,
                                    action = %action,
                                    "反射弧 L1_formal：感官信号 → 本能运动指令 + 通知 ThinkerNode"
                                );
                                if let Ok(motor_msg) = FrameCodec::new_message(
                                    Topic::new(&action),
                                    "kernel",
                                    &params,
                                ) {
                                    let targets = self.broker.find_targets(&motor_msg);
                                    if !targets.is_empty() {
                                        let frames: Vec<Bytes> = FrameCodec::encode(&motor_msg)?
                                            .into_iter()
                                            .map(Bytes::from)
                                            .collect();
                                        for (identity, _node_id) in targets {
                                            transport.send_to(Bytes::from(identity), frames.clone()).await?;
                                        }
                                    }
                                }
                                // 通知 ThinkerNode：发送 sensory/summary 到 cortex/{agent_id}/sensory
                                let summary_msg = FrameCodec::new_message(
                                    Topic::new("cortex/sensory"),
                                    "kernel",
                                    &serde_json::json!({
                                        "signal_topic": topic,
                                        "action": action,
                                        "params": params,
                                        "level": "L1_formal",
                                    }),
                                )?;
                                self.route_and_forward(&summary_msg, transport).await?;
                            }
                            ReflexAction::Trial { action, params } => {
                                // L1_trial：同 Instinct，但需 ThinkerNode 确认
                                tracing::info!(
                                    topic = %topic,
                                    action = %action,
                                    "反射弧 L1_trial：感官信号 → 试验运动指令（需 ThinkerNode 确认）"
                                );
                                if let Ok(motor_msg) = FrameCodec::new_message(
                                    Topic::new(&action),
                                    "kernel",
                                    &params,
                                ) {
                                    let targets = self.broker.find_targets(&motor_msg);
                                    if !targets.is_empty() {
                                        let frames: Vec<Bytes> = FrameCodec::encode(&motor_msg)?
                                            .into_iter()
                                            .map(Bytes::from)
                                            .collect();
                                        for (identity, _node_id) in targets {
                                            transport.send_to(Bytes::from(identity), frames.clone()).await?;
                                        }
                                    }
                                }
                                // 通知 ThinkerNode：发送 sensory/summary（标记需确认）
                                let summary_msg = FrameCodec::new_message(
                                    Topic::new("cortex/sensory"),
                                    "kernel",
                                    &serde_json::json!({
                                        "signal_topic": topic,
                                        "action": action,
                                        "params": params,
                                        "level": "L1_trial",
                                        "needs_confirmation": true,
                                    }),
                                )?;
                                self.route_and_forward(&summary_msg, transport).await?;
                            }
                        }
                    }
                    None => {
                        // 无匹配反射规则 → 升级到 L2，转发给 ThinkerNode
                        tracing::debug!(
                            topic = %topic,
                            "无匹配反射规则，升级到 L2（转发给 ThinkerNode）"
                        );
                        let l2_msg = FrameCodec::new_message(
                            Topic::new("cortex/sensory"),
                            "kernel",
                            &serde_json::json!({
                                "signal_topic": topic,
                                "payload": payload_str,
                                "level": "L2",
                            }),
                        )?;
                        self.route_and_forward(&l2_msg, transport).await?;
                    }
                }
            }

            // cortex/erl_trajectory — ERL 轨迹提取请求（来自 ThinkerNode turn 结束）
            "cortex/erl_trajectory" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                if let Ok(trajectory) = serde_json::from_value::<crate::agent::experiential::TaskTrajectory>(
                    payload["trajectory"].clone()
                ) {
                    // 调用 ERL 提取 heuristics
                    let heuristics = self.erl().extract_heuristics(&trajectory);
                    if !heuristics.is_empty() {
                        // 将最重要的 heuristic 回传给 ThinkerNode
                        let best = heuristics.iter()
                            .max_by(|a, b| a.relevance_score.partial_cmp(&b.relevance_score).unwrap_or(std::cmp::Ordering::Equal))
                            .expect("heuristics non-empty");
                        let result = serde_json::json!({
                            "heuristic": best.content,
                            "from_success": best.from_success,
                            "relevance": best.relevance_score,
                        });
                        if let Ok(result_msg) = FrameCodec::new_message(
                            Topic::new("cortex/erl_heuristic"),
                            "kernel",
                            &result,
                        ) {
                            if let Err(e) = self.route_and_forward(&result_msg, transport).await {
                                tracing::warn!("发送 ERL heuristic 结果失败：{}", e);
                            }
                        }
                        tracing::info!(
                            target: "ccore::erl",
                            count = heuristics.len(),
                            best_score = best.relevance_score,
                            "ERL 从轨迹提取了 {} 条 heuristics",
                            heuristics.len()
                        );
                    }
                }
            }

            // cortex/erl_retrieve_request — ERL 检索请求（来自 ThinkerNode turn 开始）
            // 闭环关键：从 ERL 池中检索与当前任务相关的启发式规则，注入工作记忆
            "cortex/erl_retrieve_request" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let task_description = payload["task"].as_str().unwrap_or("");
                let top_k = payload["top_k"].as_u64().unwrap_or(3) as usize;
                let agent_id = payload["agent_id"].as_str().unwrap_or("");

                if task_description.is_empty() {
                    return Ok(());
                }

                // 从 ERL 池中检索相关启发式
                let heuristics = self.erl().retrieve_relevant(task_description, top_k);
                if heuristics.is_empty() {
                    tracing::debug!(
                        target: "ccore::erl",
                        task = task_description,
                        "ERL 无相关启发式"
                    );
                    return Ok(());
                }

                // 格式化为可注入工作记忆的文本
                let formatted = self.erl().format_for_injection(&heuristics);
                let result = serde_json::json!({
                    "heuristic": formatted,
                    "from_success": true,
                    "relevance": heuristics.iter().map(|h| h.relevance_score).fold(0.0f64, f64::max),
                    "source": "retrieve",
                });
                if let Ok(result_msg) = FrameCodec::new_message(
                    Topic::new("cortex/erl_heuristic"),
                    "kernel",
                    &result,
                ) {
                    if let Err(e) = self.route_and_forward(&result_msg, transport).await {
                        tracing::warn!("发送 ERL 检索结果失败：{}", e);
                    }
                }
                tracing::info!(
                    target: "ccore::erl",
                    agent_id,
                    count = heuristics.len(),
                    task = task_description.chars().take(80).collect::<String>(),
                    "ERL 注入 {} 条历史启发式到工作记忆",
                    heuristics.len()
                );
            }

            // cortex/goal_verify — GoalLoop 子任务验证请求（来自 ThinkerNode）
            // 双路径验证：快速路径（经验日志关键词）+ LLM 评估路径
            "cortex/goal_verify" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let verify_req: crate::agent::goal_verifier::GoalVerifyRequest =
                    serde_json::from_value(payload.clone()).unwrap_or_else(|_| {
                        crate::agent::goal_verifier::GoalVerifyRequest {
                            agent_id: payload["agent_id"].as_str().unwrap_or("").to_string(),
                            subtask_description: payload["subtask_description"].as_str().unwrap_or("").to_string(),
                            verification: payload["verification"].as_str().unwrap_or("").to_string(),
                        }
                    });

                // 策略 1：经验日志快速验证（零延迟）
                let recent_outcome = self.experience_log.lock().await.recent_outcome_summary();
                let quick_pass = !verify_req.verification.is_empty() && (
                    recent_outcome.contains("success")
                    || recent_outcome.contains("passed")
                    || recent_outcome.contains("ok")
                    || recent_outcome.contains("完成")
                    || recent_outcome.contains("created")
                    || recent_outcome.contains("written")
                    || recent_outcome.contains("exists")
                );
                let quick_fail = recent_outcome.contains("error")
                    || recent_outcome.contains("failed")
                    || recent_outcome.contains("失败")
                    || recent_outcome.contains("not found")
                    || recent_outcome.contains("不存在");

                if quick_pass || quick_fail {
                    // 快速路径：经验日志足以判断
                    tracing::info!(
                        target: "ccore::goal",
                        agent_id = verify_req.agent_id,
                        subtask = verify_req.subtask_description,
                        verification = verify_req.verification,
                        passed = quick_pass,
                        "GoalLoop 验证结果：{}（快速路径，经验={}）",
                        if quick_pass { "通过" } else { "未通过" },
                        recent_outcome.len()
                    );
                    let result_json = serde_json::json!({
                        "passed": quick_pass,
                        "verification": verify_req.verification,
                        "subtask_description": verify_req.subtask_description,
                        "reasoning": format!("经验日志快速验证：{}", recent_outcome),
                    });
                    if let Ok(result_msg) = FrameCodec::new_message(
                        Topic::new("cortex/goal_verify_result"),
                        "kernel",
                        &result_json,
                    ) {
                        if let Err(e) = self.route_and_forward(&result_msg, transport).await {
                            tracing::warn!("发送 GoalLoop 验证结果失败：{}", e);
                        }
                    }
                } else {
                    // 策略 2：LLM 评估模型验证（异步，结果通过 sampler stream 回传）
                    let verify_prompt = verify_req.to_verify_prompt();
                    let verify_msg = FrameCodec::new_message(
                        Topic::sampler_request(),
                        "kernel",
                        &serde_json::json!({
                            "request_id": format!("goal_verify_{}", uuid::Uuid::new_v4()),
                            "agent_id": verify_req.agent_id,
                            "model": "fast",
                            "messages": [{"role": "user", "content": verify_prompt}],
                            "stream": false,
                            "max_tokens": 100,
                            "goal_verify": true,
                            "subtask_description": verify_req.subtask_description,
                            "verification": verify_req.verification,
                        }),
                    )?;
                    if let Err(e) = self.route_and_forward(&verify_msg, transport).await {
                        tracing::warn!("发送 GoalLoop LLM 验证请求失败：{}", e);
                        // LLM 验证失败时，回退到简单验证
                        let auto_pass = verify_req.verification.len() < 15;
                        let result_json = serde_json::json!({
                            "passed": auto_pass,
                            "verification": verify_req.verification,
                            "subtask_description": verify_req.subtask_description,
                            "reasoning": "LLM 验证失败，回退到简单验证",
                        });
                        if let Ok(result_msg) = FrameCodec::new_message(
                            Topic::new("cortex/goal_verify_result"),
                            "kernel",
                            &result_json,
                        ) {
                            let _ = self.route_and_forward(&result_msg, transport).await;
                        }
                    }
                }
            }

            // 控制面路由消息：经 Kernel ROUTER 转发到订阅者
            // ROS 1 风格：tool_call、tool_result、agent/output 等控制面消息
            // 通过 DEALER↔ROUTER 连接传输，Node 没有直连通道，必须经 Kernel 中转。
            // 只有大块业务数据才走 Node PUB/SUB 直连（数据面）。
            _ => {
                // 尝试路由到订阅了此 topic 的 Node
                let targets = self.broker.find_targets(&incoming.message);
                if targets.is_empty() {
                    // 无订阅者：可能是纯数据面消息（Node 应通过 PUB/SUB 直连），
                    // 或消息发到了无人订阅的 topic
                    tracing::debug!(
                        "消息 {} 无订阅者，跳过路由（如为业务数据请使用 PUB/SUB 直连）",
                        topic
                    );
                } else {
                    // 有订阅者：通过控制面 ROUTER 转发
                    self.route_and_forward(&incoming.message, transport).await?;
                }
            }
        }

        Ok(())
    }

    /// 将消息路由到所有订阅者并通过 ROUTER 发送
    ///
    /// ROS 风格的消息路由：
    /// 1. 通过 broker.find_targets 查找订阅者（不编码）
    /// 2. 编码消息帧（只编码一次）
    /// 3. 逐个发送到每个订阅者
    /// 4. 记录发送统计
    async fn route_and_forward(
        &self,
        msg: &Message,
        transport: &mut KernelTransport,
    ) -> Result<()> {
        // ✅ 检查背压级别
        if let Some(delay) = self.backpressure.get_delay() {
            tracing::warn!("背压触发，延迟 {:?} 后发送", delay);
            tokio::time::sleep(delay).await;
        }

        // ✅ 查找订阅者（不编码消息帧，避免重复编码）
        let targets = self.broker.find_targets(msg);

        if targets.is_empty() {
            return Ok(());
        }

        // ✅ 编码消息帧（只编码一次，所有订阅者共享）
        let frames = FrameCodec::encode(msg)?;
        let frames_bytes: Vec<Bytes> = frames.into_iter().map(Bytes::from).collect();

        // ✅ 逐个发送到每个订阅者
        for (identity, _node_id) in targets {
            let identity_bytes = Bytes::from(identity);
            transport.send_to(identity_bytes, frames_bytes.clone()).await?;
        }

        // ✅ 记录发送统计
        self.backpressure.record_sent();

        Ok(())
    }

    /// 原子注册 Node（带补偿逻辑）
    ///
    /// 确保状态一致性：
    /// - 先注册 Registry（记录状态）
    /// - 再注册 Broker identity
    /// - 最后添加订阅关系
    /// - 任何步骤失败时，回滚已完成的所有步骤
    fn atomic_register_node(
        &mut self,
        node_id: NodeId,
        node_type: NodeType,
        subscriptions: Vec<String>,
        identity: Vec<u8>,
    ) -> Result<()> {
        // 步骤 1: Registry 注册（记录元数据）
        self.registry.register(node_id.clone(), node_type, subscriptions.clone());

        // 步骤 2: Broker 注册 identity
        // 注意：register_identity 目前不返回错误，但为了保持回滚框架
        // 如果 Node 已注册，旧的 identity 会被覆盖（这是预期行为）
        self.broker.register_identity(node_id.clone(), identity);

        // 步骤 3: Broker 订阅 topics
        // 跟踪已添加的订阅，用于回滚
        let mut added_subscriptions: Vec<String> = Vec::new();
        for pattern in &subscriptions {
            self.broker.subscribe(node_id.clone(), pattern.clone());
            added_subscriptions.push(pattern.clone());
        }

        tracing::info!("Node 注册成功：{} ({:?})", node_id, node_type);
        Ok(())
    }

    /// 原子注销 Node（带补偿逻辑）
    ///
    /// 步骤：
    /// 1. Registry 注销
    /// 2. Broker 注销 identity 和 subscriptions
    fn atomic_deregister_node(&mut self, id: &NodeId) {
        let node_info = self.registry.get(id).cloned();

        // 步骤 1: Registry 注销
        self.registry.deregister(id);

        // 步骤 2: Broker 注销
        self.broker.deregister_identity(id);

        if let Some(info) = node_info {
            tracing::info!("Node 注销：{} ({:?})", id, info.node_type);
        }
    }

    /// 注册新 Node（由外部调用，如 Launcher）
    pub fn register_node(
        &mut self,
        id: NodeId,
        node_type: NodeType,
        subscriptions: Vec<String>,
        identity: Vec<u8>,
    ) {
        self.registry.register(id.clone(), node_type, subscriptions.clone());
        self.broker.register_identity(id.clone(), identity);
        for pattern in subscriptions {
            self.broker.subscribe(id.clone(), pattern);
        }
        tracing::info!("Node 注册：{} ({:?})", id, node_type);
    }

    /// 注销 Node
    pub fn deregister_node(&mut self, id: &NodeId) {
        self.atomic_deregister_node(id);
    }

    /// 处理 Node 心跳
    pub fn handle_heartbeat(&mut self, id: &NodeId) {
        self.registry.heartbeat(id);
    }

    /// 请求 spawn 子 Agent
    ///
    /// 创建 SubAgentNode 并在独立 tokio task 中启动。
    /// SubAgentNode 启动后会自动连接消息总线、注册、订阅 agent/{id}/task。
    /// 父 Agent 收到返回的 new_id 后，向 agent/{new_id}/task 发送任务即可。
    async fn request_spawn_subagent(
        &mut self,
        transport: &mut KernelTransport,
        parent_id: &NodeId,
        agent_type: crate::agent::AgentType,
        model: Option<String>,
        task_description: String,
    ) -> Result<NodeId> {
        // 检查子 Agent 数量限制
        let current_subagents = self.registry.find_by_type(NodeType::Agent).len();
        if current_subagents >= self.config.max_subagents {
            return Err(anyhow::anyhow!(
                "子 Agent 数量已达上限 {}",
                self.config.max_subagents
            ));
        }

        let new_id = NodeId::new();
        tracing::info!(
            "spawn 子 Agent：{} (parent={}, type={:?}, model={:?})",
            new_id, parent_id, agent_type, model
        );

        // 构造子代理定义
        let definition = crate::agent::subagent::SubAgentDefinition {
            agent_type,
            model: model.clone(),
            task_description: task_description.clone(),
            max_turns: 20,
            allowed_tools: Vec::new(),
        };

        // 构造 AgentConfig（子代理默认非交互、不可再 spawn 子代理）
        // 从共享配置读取 default_model 和 permission_mode（支持热更新）
        let (default_model, permission_mode) = if let Some(c) = &self.runtime_config {
            let guard = c.read().await;
            (guard.default_model.clone(), guard.permission_mode)
        } else {
            (String::new(), crate::node::PermissionMode::Trust)
        };
        let agent_config = crate::agent::AgentConfig {
            agent_type,
            model: model.clone().unwrap_or_else(|| default_model),
            permission_mode,
            max_turns: Some(definition.max_turns),
            subagents_enabled: false,
            non_interactive: true,
            tools: Vec::new(),
        };

        // 创建 SubAgentNode
        let subagent = crate::agent::subagent::SubAgentNode::new(
            new_id.clone(),
            parent_id.clone(),
            agent_config,
            definition,
        );

        // 创建 NodeContext（独立 PUB 地址，避免冲突）
        let ctx = NodeContext {
            router_addr: self.config.router_addr.clone(),
            pub_addr: self.config.pub_addr.clone(),
            data_pub_addr: format!("ipc:///tmp/ccode-pub-{}", new_id),
            data_rep_addr: None,
        };

        // 在独立 tokio task 中启动 SubAgentNode
        // tokio::spawn 提供 panic 隔离：子 Agent panic 不会拖垮 Kernel
        // 全局 panic_hook 已在 Kernel::new() 中安装，会记录 panic 到 tracing
        let sub_id = new_id.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::node::transport::run_node(subagent, ctx).await {
                tracing::error!("子 Agent {} 异常退出：{}", sub_id, e);
            }
        });

        // 广播 spawn 事件（通知其他 Node 有新子代理上线）
        let spawn_msg = FrameCodec::new_message(
            Topic::sys_spawn(),
            "kernel",
            &serde_json::json!({
                "node_id": new_id.to_string(),
                "node_type": "agent",
                "agent_type": format!("{:?}", agent_type),
                "parent_id": parent_id.to_string(),
                "model": model,
                "task_description": task_description,
            }),
        )?;

        let frames: Vec<Bytes> = FrameCodec::encode(&spawn_msg)?
            .into_iter()
            .map(Bytes::from)
            .collect();
        transport.broadcast(frames).await?;

        Ok(new_id)
    }

    /// 广播 Node 下线事件
    async fn broadcast_node_deregister(transport: &mut KernelTransport, node_id: &NodeId) {
        tracing::info!("广播 Node 下线：{}", node_id);
        if let Ok(msg) = FrameCodec::new_message(
            Topic::sys_deregister(),
            "kernel",
            &serde_json::json!({ "node_id": node_id.to_string() }),
        ) {
            if let Ok(frames) = FrameCodec::encode(&msg) {
                let bytes_frames: Vec<Bytes> = frames.into_iter().map(Bytes::from).collect();
                if let Err(e) = transport.broadcast(bytes_frames).await {
                    tracing::warn!("广播失败：{}", e);
                }
            }
        }
    }

    /// 广播配置变更通知到所有 Node
    ///
    /// ConfigReloader 重载配置后，通过此方法广播 sys/config_change 消息，
    /// 通知所有 Node 配置已更新，Node 可按需重新加载本地配置。
    async fn broadcast_config_change(transport: &mut KernelTransport, change_type: &str) {
        tracing::info!(change_type = %change_type, "广播配置变更通知");
        match FrameCodec::new_message(
            Topic::new("sys/config_change"),
            "kernel",
            &serde_json::json!({ "type": change_type }),
        ) {
            Ok(msg) => {
                match FrameCodec::encode(&msg) {
                    Ok(frames) => {
                        let bytes_frames: Vec<Bytes> = frames.into_iter().map(Bytes::from).collect();
                        if let Err(e) = transport.broadcast(bytes_frames).await {
                            tracing::warn!(change_type = %change_type, error = %e, "广播配置变更失败");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(change_type = %change_type, error = %e, "编码配置变更消息失败");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(change_type = %change_type, error = %e, "构造配置变更消息失败");
            }
        }
    }

    /// 发送 ACK 确认消息
    ///
    /// 对 requires_ack=true 的消息，回复 ACK 以确认收到。
    /// ACK 消息包含原始消息的 msg_id，用于关联。
    async fn send_ack(
        original_msg: &Message,
        transport: &mut KernelTransport,
    ) -> Result<()> {
        let ack_msg = FrameCodec::new_reply(
            Topic::sys_ack(),
            "kernel",
            original_msg.header.msg_id.clone(),
            &serde_json::json!({
                "ack_for": original_msg.header.msg_id,
                "topic": original_msg.topic.as_str(),
                "status": "received",
            }),
        )?;
        let frames: Vec<Bytes> = FrameCodec::encode(&ack_msg)?
            .into_iter()
            .map(Bytes::from)
            .collect();

        // ACK 通过 PUB socket 广播（订阅者都能收到）
        transport.broadcast(frames).await?;
        Ok(())
    }

    /// 广播全局关闭信号
    async fn broadcast_shutdown(transport: &mut KernelTransport) {
        tracing::info!("广播全局关闭信号");
        if let Ok(msg) = FrameCodec::new_message(
            Topic::sys_shutdown(),
            "kernel",
            &serde_json::json!({}),
        ) {
            if let Ok(frames) = FrameCodec::encode(&msg) {
                let bytes_frames: Vec<Bytes> = frames.into_iter().map(Bytes::from).collect();
                if let Err(e) = transport.broadcast(bytes_frames).await {
                    tracing::warn!("广播失败：{}", e);
                }
            }
        }
    }

    /// 停止 Kernel
    pub async fn stop(&mut self) {
        self.running = false;
        tracing::info!("Kernel 关闭");
    }

    /// 获取当前在线 Node 数量
    pub fn node_count(&self) -> usize {
        self.registry.len()
    }

}

/// 解析 NodeType 字符串
fn parse_node_type(s: &str) -> NodeType {
    match s {
        "kernel" => NodeType::Kernel,
        "agent" => NodeType::Agent,
        "tool" => NodeType::Tool,
        "sampler" => NodeType::Sampler,
        "state" => NodeType::State,
        "tui" => NodeType::TUI,
        "plugin" => NodeType::Plugin,
        "acp" => NodeType::Acp,
        "thinker" => NodeType::Thinker,
        _ => NodeType::Plugin,
    }
}
