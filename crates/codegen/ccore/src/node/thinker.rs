//! Thinker Node — 大脑皮层（仿生架构，路线 A：感官内置）
//!
//! Fusion: 唯一决策 Node（不是上帝进程）。
//! - 拥有：感知(Ear/Eye/Nose/Skin 内置)、agentic loop、doom-loop、max_turns、goal
//! - 不拥有：工具执行、JSONL 持久化实现、LLM HTTP
//! - 协作：sampler/request、agent/{id}/tool_call、state/persist|query|compact、agent/{id}/output
//!
//! ThinkerNode 是 AgentNode 的仿生架构演进版，核心改进：
//! 1. 感官层内置：Eye/Ear/Nose/Skin 作为内部方法，不拆独立进程
//! 2. 运动层走 ToolNode：工具调用通过 agent/{id}/tool_call 发给 ToolNode
//! 3. 输出层走 TUINode：文本输出通过 agent/{id}/output 发给 TUINode
//!
//! 仿生隐喻保留在概念层面：
//! - observe()：视觉（Eye）— 观察 tool 结果中的文件内容
//! - listen()：听觉（Ear）— 监听用户输入
//! - sniff()：嗅觉（Nose）— 解析编译错误/代码异味
//! - feel()：触觉（Skin）— 感知工具执行反馈
//!
//! 通信路径（与 AgentNode 完全兼容）：
//! - 输入：agent/{id}/input ← TUINode
//! - 采样：sampler/request → SamplerNode → sampler/*/stream
//! - 工具调用：agent/{id}/tool_call → ToolNode → agent/{id}/tool_result
//! - 输出：agent/{id}/output → TUINode
//! - 取消：agent/{id}/cancel ← AcpNode/TUINode
//!
//! 核心循环：
//! 1. 收到 input → listen() → 构建 L0 工作记忆 → 发送 sampler/request
//! 2. 收到 LLM 流式响应 → 解析 tool_call 或 text
//! 3. 如果有 tool_call → 发送到 agent/{id}/tool_call → 收到 tool_result → feel() + sniff() → 回到步骤 2
//! 4. 如果是纯文本 → 发送到 agent/{id}/output → 等待下一轮 input
//! 5. 每轮结束执行滑动窗口更新
//! 6. Doom Loop 检测：重复工具调用超过阈值则终止

use async_trait::async_trait;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::metrics::AgentMetrics;
use crate::node::{Node, NodeId, NodeType, NodeContext};
use crate::node::agent::PendingToolCall;
use crate::node::transport::NodeTransportHandle;
use crate::agent::{AgentConfig, AgentState};
use crate::agent::doom_loop::{DoomLoopDetector, DoomLoopResult, EscapeAction};
use crate::agent::loop_state::{LoopStateMachine, LoopEvent, LoopAction, ToolExecutionOutcome};
use crate::agent::goal_loop::{GoalLoop, GoalAction};
use crate::agent::orchestrator::Orchestrator;
use crate::agent::subagent::SubAgentCrashed;

use crate::memory::working::{WorkingMemory, WorkingEntry, MessageRole};
use crate::memory::short_term::ShortTermMemory;
use crate::memory::window::SlidingWindow;
use crate::memory::embedding::EmbeddingIndex;
use crate::memory::intent_retriever::IntentRetriever;

/// 记忆桥接 trait — 连接外部记忆系统（如 ccode-memory）到 ThinkerNode
///
/// 实现此 trait 的模块负责：
/// - 根据用户输入搜索相关长期记忆（冷区→热区注入）
/// - 在会话结束时提取关键知识（热区→冷区持久化）
pub trait MemoryBridge: Send + Sync {
    /// 根据查询文本搜索相关记忆，返回注入工作记忆的文本片段
    fn search_relevant(&self, query: &str, top_k: usize) -> Vec<String> { let _ = (query, top_k); Vec::new() }
    /// 会话结束时提取关键知识并持久化
    fn extract_and_store(&self, _messages: &[(String, String)]) {}
}

/// 空记忆桥接（默认实现，不做任何事）
struct NoopMemoryBridge;
impl MemoryBridge for NoopMemoryBridge {}

/// 情景记忆桥接 — 连接 EpisodicMemoryStore 到 ThinkerNode
pub struct EpisodicMemoryBridge {
    store: std::sync::Arc<crate::memory::episodic::EpisodicMemoryStore>,
}

impl EpisodicMemoryBridge {
    pub fn new(store: std::sync::Arc<crate::memory::episodic::EpisodicMemoryStore>) -> Self {
        Self { store }
    }
}

impl MemoryBridge for EpisodicMemoryBridge {
    fn search_relevant(&self, query: &str, top_k: usize) -> Vec<String> {
        let context = self.store.reconstruct_context(query, top_k);
        if context.is_empty() {
            Vec::new()
        } else {
            vec![context]
        }
    }

    fn extract_and_store(&self, messages: &[(String, String)]) {
        use crate::memory::episodic::{MemoryType, MemorySource};
        for (i, (role, content)) in messages.iter().enumerate() {
            if content.len() < 50 {
                continue;
            }
            let keywords: Vec<String> = content.split_whitespace().take(8).map(|s| s.to_string()).collect();
            self.store.encode(
                if role == "assistant" { MemoryType::Episodic } else { MemoryType::Semantic },
                &content.chars().take(500).collect::<String>(),
                role,
                keywords,
                MemorySource {
                    session_id: String::new(),
                    timestamp: chrono::Utc::now().timestamp(),
                    message_index: Some(i),
                    confidence: 0.7,
                },
            );
        }
    }
}
use crate::sampler::provider::{
    SampleRequest, ChatMessage, StreamChunk, StreamChannel, ToolDefinition as SamplerToolDefinition,
};

/// 压缩管道配置（驱动 5 层压缩：Budget → Snip → MicroCompact → Collapse → Auto）
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// 微压缩阈值：超过此消息数时触发微压缩
    pub microcompact_threshold: usize,
    /// 上下文折叠阈值：token 占用率超过此百分比时触发
    pub collapse_threshold_percent: u32,
    /// 自动压缩阈值：token 占用率超过此百分比时触发全量压缩
    pub auto_compact_threshold_percent: u32,
    /// 保留最近 K 轮不压缩（热区）
    pub keep_recent: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            microcompact_threshold: 20,
            collapse_threshold_percent: 85,
            auto_compact_threshold_percent: 95,
            keep_recent: 5,
        }
    }
}

/// 感官信号缓冲最大容量
const SENSORY_BUFFER_CAPACITY: usize = 20;

/// 感官信号（内部使用，不经消息总线）
#[derive(Debug, Clone)]
pub struct SensorySignal {
    /// 来源器官（如 "nose", "skin", "eye"）
    pub source_organ: String,
    /// 信号类型（如 "compile_error", "touch"）
    pub signal_type: String,
    /// 供 LLM 理解的摘要
    pub summary: String,
    /// 严重程度（"info", "warning", "error"）
    pub severity: String,
}

/// Thinker Node 实现（仿生架构，路线 A：感官内置）
///
/// 主循环由 LoopStateMachine 驱动（借鉴 Claude Code queryLoop 设计），
/// 状态变迁通过 transition() 显式执行，可观测、可中断、可恢复。
pub struct ThinkerNode {
    /// Node 唯一 ID
    id: NodeId,
    /// Agent 配置（复用 AgentConfig）
    config: AgentConfig,
    /// 当前状态（由 LoopStateMachine 驱动）
    state: AgentState,
    /// 主循环状态机（借鉴 Claude Code，驱动 Idle→Thinking→ToolCalling→Done 变迁）
    loop_state_machine: LoopStateMachine,
    /// L0 工作记忆
    working_memory: WorkingMemory,
    /// L1 短期记忆
    short_term_memory: ShortTermMemory,
    /// 子 Agent 编排器
    orchestrator: Orchestrator,
    /// Doom Loop 检测器
    doom_loop_detector: DoomLoopDetector,
    /// 下一轮需禁用的工具名（Doom Loop 逃脱：仅禁用一轮）
    disabled_tool_next_round: Option<String>,
    /// 滑动窗口更新器（驱动 5 层压缩管道的 token 计数来源）
    sliding_window: SlidingWindow,
    /// 压缩管道配置
    compaction_config: CompactionConfig,
    /// 上次微压缩的消息数（避免重复压缩）
    last_microcompact_count: usize,
    /// 上下文折叠是否激活
    context_collapse_active: bool,
    /// 记忆桥接（连接 ccode-memory 等外部记忆系统）
    memory_bridge: Box<dyn MemoryBridge>,
    /// 等待中的工具调用（tool_call_id → PendingToolCall）
    pending_tool_calls: HashMap<String, PendingToolCall>,
    /// 正在执行中的工具调用（tool_call_id → tool_name，等待结果返回）
    pending_tool_results: HashMap<String, String>,
    /// 当前 LLM 采样请求 ID
    current_sample_request_id: Option<String>,
    /// 当前推理周期的开始时间
    inference_start: Option<std::time::Instant>,
    /// 已使用的 token 数
    #[allow(dead_code)]
    tokens_used: u32,
    /// 已执行轮次
    turns_executed: u32,
    /// 感官信号缓冲（最近 20 条，内置处理）
    sensory_buffer: Vec<SensorySignal>,
    /// 取消请求标志（收到 agent/{id}/cancel 后设置）
    cancel_requested: bool,
    /// Agentic 会话是否活跃（input → sampling → tool → re-sample 循环中）
    agentic_session_active: bool,
    /// 意图检索器（Context Engine 核心：意图扩展 + 代码块检索）
    intent_retriever: IntentRetriever,
    /// Goal Loop（目标驱动循环，/goal 命令触发）
    goal_loop: Option<GoalLoop>,
    /// Doom Loop 逃脱尝试次数（超过 3 次才真正终止）
    doom_loop_escape_attempts: u32,
}

impl ThinkerNode {
    pub fn new(id: NodeId, config: AgentConfig) -> Self {
        let max_tokens = 128_000;
        Self {
            id,
            state: AgentState::Idle,
            loop_state_machine: LoopStateMachine::new(),
            working_memory: WorkingMemory::new(max_tokens),
            short_term_memory: ShortTermMemory::new(),
            orchestrator: Orchestrator::new(10),
            doom_loop_detector: DoomLoopDetector::new(10, 3),
            disabled_tool_next_round: None,
            sliding_window: SlidingWindow::new(max_tokens),
            compaction_config: CompactionConfig::default(),
            last_microcompact_count: 0,
            context_collapse_active: false,
            memory_bridge: Box::new(NoopMemoryBridge),
            config,
            pending_tool_calls: HashMap::new(),
            pending_tool_results: HashMap::new(),
            current_sample_request_id: None,
            inference_start: None,
            tokens_used: 0,
            turns_executed: 0,
            sensory_buffer: Vec::with_capacity(SENSORY_BUFFER_CAPACITY),
            cancel_requested: false,
            agentic_session_active: false,
            intent_retriever: IntentRetriever::new(EmbeddingIndex::new()),
            goal_loop: None,
            doom_loop_escape_attempts: 0,
        }
    }

    /// 设置记忆桥接（接入 ccode-memory 等外部记忆系统）
    pub fn set_memory_bridge(&mut self, bridge: Box<dyn MemoryBridge>) {
        self.memory_bridge = bridge;
    }

    /// 启动目标驱动循环
    ///
    /// 从用户描述创建 GoalLoop，自动拆解子任务、执行、验证。
    pub fn start_goal(&mut self, description: String) {
        let goal_loop = GoalLoop::from_description(description);
        tracing::info!(
            target: "ccore::goal",
            description = %goal_loop.description(),
            "GoalLoop 已启动"
        );
        self.goal_loop = Some(goal_loop);
    }

    /// 处理 GoalLoop 动作（由 on_turn_complete / on_verification_result 产生）
    ///
    /// 根据动作类型：注入工作记忆、发送验证请求、或清除 goal_loop。
    async fn process_goal_action(&mut self, action: GoalAction, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        match action {
            GoalAction::ExecuteSubTask { description } => {
                // 将子任务描述注入工作记忆，驱动下一轮对话
                self.working_memory.push_user(
                    description.clone(),
                    Self::estimate_tokens(&description),
                );
                tracing::info!(target: "ccore::goal", description = %description, "GoalLoop：执行子任务");
            }
            GoalAction::VerifySubTask { verification } => {
                // 通过 bus 发送验证请求
                let verify_msg = FrameCodec::new_message(
                    Topic::new("cortex/goal_verify"),
                    self.id.as_str(),
                    &serde_json::json!({
                        "agent_id": self.id.to_string(),
                        "verification": verification,
                    }),
                )?;
                if let Err(e) = transport.send_message(&verify_msg).await {
                    tracing::debug!("发送 GoalLoop 验证请求失败：{}", e);
                }
                tracing::info!(target: "ccore::goal", "GoalLoop：请求验证子任务");
            }
            GoalAction::GoalComplete { reason } => {
                tracing::info!(target: "ccore::goal", reason = ?reason, "GoalLoop：目标完成");
                self.goal_loop = None;
            }
            GoalAction::SubTaskFailed { subtask_idx, will_retry } => {
                tracing::warn!(
                    target: "ccore::goal",
                    subtask_idx,
                    will_retry,
                    "GoalLoop：子任务失败"
                );
                if !will_retry {
                    // 不再重试时，继续循环让 GoalLoop 前进到下一个子任务
                }
            }
            GoalAction::PlanSubTasks => {
                // 规划阶段：注入提示让 LLM 生成子任务列表
                if let Some(ref gl) = self.goal_loop {
                    let desc = gl.description().to_string();
                    self.working_memory.push_user(
                        format!("请将以下目标拆解为可执行的子任务列表：\n{}", desc),
                        Self::estimate_tokens(&desc),
                    );
                }
            }
        }
        Ok(())
    }

    /// 处理目标验证结果回调
    pub async fn on_goal_verification_result(&mut self, passed: bool, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        if let Some(ref mut gl) = self.goal_loop {
            let action = gl.on_verification_result(passed);
            self.process_goal_action(action, transport).await?;
        }
        Ok(())
    }

    // ── 内置感官模块（路线 A：不拆独立进程） ──

    /// 听觉（Ear）：处理用户输入
    ///
    /// 将用户输入存入工作记忆，同时从长期记忆中搜索相关内容注入热区。
    fn listen(&mut self, content: &str, role: &str) {
        let _entry_id = self.short_term_memory.store(
            role.to_string(),
            content.to_string(),
            Self::estimate_tokens(content),
            false,
        );

        self.working_memory.push_hot(
            MessageRole::try_from(role).unwrap_or(MessageRole::User),
            content.to_string(),
            Self::estimate_tokens(content),
        );

        // 从长期记忆搜索相关内容，注入工作记忆（冷区→热区）
        let relevant = self.memory_bridge.search_relevant(content, 5);
        for memory in relevant {
            let token_count = Self::estimate_tokens(&memory);
            self.working_memory.push_system(
                format!("[长期记忆] {}", memory),
                token_count,
            );
            tracing::debug!("注入长期记忆：{} 字符", memory.len());
        }

        // Context Engine：意图扩展 + 代码块检索，注入代码级上下文
        let intents = IntentRetriever::expand_intents(content);
        if !intents.is_empty() {
            // 简单用零向量占位（真实场景需调 embedding 模型生成 query embedding）
            let query_embedding = vec![0.0f32; 1536];
            let results = self.intent_retriever.search_by_intents(&intents, &query_embedding, 5);
            for result in results {
                let token_count = Self::estimate_tokens(&result.preview);
                self.working_memory.push_system(
                    format!("[代码上下文] {} ({}:{}, 相关度:{:.2})", result.name, result.file_path.display(), result.source_intent, result.relevance_score),
                    token_count,
                );
                tracing::debug!("注入代码上下文：{} (score={:.2})", result.name, result.relevance_score);
            }
        }

        self.state = AgentState::Thinking;
        tracing::debug!("Thinker {} 听到输入，轮次 {}", self.id, self.turns_executed);
    }

    /// 触觉（Skin）：感知工具执行结果
    ///
    /// 解析工具输出，提取关键信息注入工作记忆。
    /// 对于编译/检查类工具输出，自动调用 sniff() 解析错误。
    /// 同时通过消息总线向 Kernel 发送感官信号，让反射弧和经验学习有输入。
    async fn feel(&mut self, tool_name: &str, output: &str, success: bool, transport: &NodeTransportHandle) {
        let summary = if success {
            format!("[工具 {} 执行成功]", tool_name)
        } else {
            format!("[工具 {} 执行失败]", tool_name)
        };

        // 记录触觉信号
        self.push_sensory(SensorySignal {
            source_organ: "skin".into(),
            signal_type: "tool_result".into(),
            summary: summary.clone(),
            severity: if success { "info" } else { "error" }.into(),
        });

        // 向 Kernel 发送感官信号（让反射弧和经验学习有输入）
        if let Err(e) = self.publish_sensory_to_kernel(
            &format!("sensory/skin/{}", tool_name),
            &serde_json::json!({
                "tool_name": tool_name,
                "success": success,
                "output_preview": output.chars().take(200).collect::<String>(),
                "agent_id": self.id.to_string(),
            }),
            transport,
        ).await {
            tracing::debug!("发送感官信号到 Kernel 失败：{}", e);
        }

        // 如果是编译/检查类工具，自动嗅探
        if !success && Self::is_compile_related_tool(tool_name) {
            self.sniff(output, transport).await;
        }
    }

    /// 嗅觉（Nose）：解析编译错误和代码异味
    ///
    /// 从工具输出中提取错误信息，格式化后注入工作记忆。
    /// 同时向 Kernel 发送感官信号，让反射弧和经验学习有输入。
    /// 编译错误格式：error[E0xxx]: message
    async fn sniff(&mut self, output: &str, transport: &NodeTransportHandle) {
        let error_lines: Vec<&str> = output
            .lines()
            .filter(|line| line.contains("error[") || line.contains("error:"))
            .take(5) // 最多取 5 条错误
            .collect();

        if error_lines.is_empty() {
            return;
        }

        let summary = format!("编译/检查发现 {} 个错误：\n{}", error_lines.len(), error_lines.join("\n"));

        self.push_sensory(SensorySignal {
            source_organ: "nose".into(),
            signal_type: "compile_error".into(),
            summary: summary.clone(),
            severity: "error".into(),
        });

        // 注入工作记忆供 LLM 处理（感官信号作为 system 消息）
        let token_count = Self::estimate_tokens(&summary);
        self.working_memory.push_system(summary.clone(), token_count);

        // 向 Kernel 发送感官信号（让反射弧和经验学习有输入）
        if let Err(e) = self.publish_sensory_to_kernel(
            "sensory/nose/compile_error",
            &serde_json::json!({
                "error_count": error_lines.len(),
                "errors": error_lines,
                "agent_id": self.id.to_string(),
            }),
            transport,
        ).await {
            tracing::debug!("发送嗅探感官信号到 Kernel 失败：{}", e);
        }
    }

    /// 视觉（Eye）：观察工具结果中的文件内容
    ///
    /// 从 Read/Glob/Grep 等工具输出中提取文件内容摘要，
    /// 并向 Kernel 发送感官信号
    async fn observe(&mut self, tool_name: &str, output: &str, transport: &NodeTransportHandle) {
        let summary = match tool_name {
            "Read" => {
                let line_count = output.lines().count();
                format!("[观察到文件内容，共 {} 行]", line_count)
            }
            "Glob" | "Grep" => {
                let match_count = output.lines().count();
                format!("[观察到 {} 条匹配结果]", match_count)
            }
            _ => return, // 非观察类工具，不处理
        };

        self.push_sensory(SensorySignal {
            source_organ: "eye".into(),
            signal_type: "file_observation".into(),
            summary,
            severity: "info".into(),
        });

        // 向 Kernel 发送感官信号
        if let Err(e) = self.publish_sensory_to_kernel(
            &format!("sensory/eye/{}", tool_name.to_lowercase()),
            &serde_json::json!({
                "tool_name": tool_name,
                "agent_id": self.id.to_string(),
            }),
            transport,
        ).await {
            tracing::debug!("发送视觉感官信号到 Kernel 失败：{}", e);
        }
    }

    /// 向 Kernel 发送感官信号
    ///
    /// ThinkerNode 内置感官处理后，通过控制面消息通知 Kernel，
    /// 让 Kernel 的 ReflexRouter 和 ExperienceLog 有输入源。
    /// topic 格式：sensory/{organ}/{detail}，如 sensory/nose/compile_error
    async fn publish_sensory_to_kernel(
        &self,
        topic: &str,
        payload: &serde_json::Value,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        let msg = FrameCodec::new_message(
            Topic::new(topic),
            self.id.as_str(),
            payload,
        )?;
        // 感官信号走控制面（经 Kernel ROUTER），确保 Kernel 一定收到
        transport.send_message(&msg).await?;
        Ok(())
    }

    /// 请求元认知评估（通过消息总线，异步回调）
    ///
    /// ThinkerNode 不直接持有 MetaCognitiveController，
    /// 而是通过 bus 发送 cortex/meta_assess 请求，
    /// Kernel 收到后评估并将结果通过 cortex/meta_result 返回。
    async fn request_meta_assessment(&self, context: &str, transport: &NodeTransportHandle) {
        let msg = FrameCodec::new_message(
            Topic::new("cortex/meta_assess"),
            self.id.as_str(),
            &serde_json::json!({
                "agent_id": self.id.to_string(),
                "context": context,
            }),
        );
        if let Ok(msg) = msg {
            if let Err(e) = transport.send_message(&msg).await {
                tracing::debug!("发送元认知评估请求失败：{}", e);
            }
        }
    }

    /// 判断工具是否与编译/检查相关
    fn is_compile_related_tool(tool_name: &str) -> bool {
        matches!(tool_name, "Bash" | "RunCommand" | "CargoCheck" | "CargoBuild" | "CargoTest")
    }

    /// 向感官缓冲中追加信号
    fn push_sensory(&mut self, signal: SensorySignal) {
        if self.sensory_buffer.len() >= SENSORY_BUFFER_CAPACITY {
            self.sensory_buffer.remove(0);
        }
        self.sensory_buffer.push(signal);
    }

    // ── 核心推理循环 ──

    /// 构建采样请求，发送到 Sampler Node
    fn build_sample_request(&mut self) -> SampleRequest {
        let messages: Vec<ChatMessage> = self.working_memory
            .to_chat_messages()
            .into_iter()
            .map(|(role, content)| ChatMessage { role, content })
            .collect();

        // Doom Loop 逃脱：若上一轮检测到循环，本轮过滤掉被禁用的工具
        let mut tools = self.config.tools.clone();
        if let Some(tool_name) = self.disabled_tool_next_round.take() {
            tools.retain(|t| t.name != tool_name);
            tracing::warn!("Doom Loop 逃脱：本轮采样已过滤工具 {}", tool_name);
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        self.current_sample_request_id = Some(request_id.clone());

        SampleRequest {
            request_id,
            agent_id: self.id.to_string(),
            model: self.config.model.clone(),
            messages,
            tools,
            stream: true,
            reasoning_effort: self.doom_loop_detector.current_reasoning_effort(),
            max_tokens: None,
            temperature: None,
            system_prompt: None,
            tool_choice: None,
        }
    }

    /// 处理 LLM 流式返回的 chunk
    async fn handle_stream_chunk(
        &mut self,
        chunk: &StreamChunk,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        match chunk.channel {
            StreamChannel::Text => {
                self.state = AgentState::Outputting;
                // 文本内容流式转发到 agent/{id}/output（TUINode 渲染）
                let output_msg = FrameCodec::new_message(
                    Topic::agent_output(self.id.as_str()),
                    self.id.as_str(),
                    &serde_json::json!({
                        "channel": "text",
                        "content": chunk.content,
                    }),
                )?;
                if let Err(e) = transport.publish_data(&output_msg).await {
                    tracing::debug!("数据面 PUB 发送 output 失败，回退到控制面：{}", e);
                    transport.send_message(&output_msg).await?;
                }
            }
            StreamChannel::Reasoning => {
                // 推理链内容，可选展示给用户
            }
            StreamChannel::ToolCall => {
                self.state = AgentState::ToolCalling;
                let tool_call = if let Some(tc) = &chunk.tool_call {
                    PendingToolCall {
                        tool_call_id: tc.tool_call_id.clone(),
                        tool_name: tc.tool_name.clone(),
                        arguments: serde_json::from_str(&tc.arguments)
                            .unwrap_or(serde_json::Value::Null),
                    }
                } else {
                    serde_json::from_str(&chunk.content)
                        .unwrap_or_else(|_| PendingToolCall {
                            tool_call_id: uuid::Uuid::new_v4().to_string(),
                            tool_name: "unknown".into(),
                            arguments: serde_json::Value::Null,
                        })
                };

                let args_hash = Self::hash_tool_args(&tool_call.arguments);
                self.doom_loop_detector.record(tool_call.tool_name.clone(), args_hash);

                self.pending_tool_calls.insert(tool_call.tool_call_id.clone(), tool_call);
            }
        }
        Ok(())
    }

    /// 处理工具调用结果，返回 true 表示所有工具调用已完成，应重新采样
    async fn handle_tool_result(&mut self, tool_call_id: &str, tool_name: &str, output: &str, success: bool, transport: &NodeTransportHandle) -> bool {
        self.pending_tool_results.remove(tool_call_id);

        // 内置感官处理（路线 A 核心：不经过独立器官 Node）
        // 同时向 Kernel 发送感官信号，让反射弧和经验学习有输入
        self.feel(tool_name, output, success, transport).await;   // 触觉：感知执行结果
        self.observe(tool_name, output, transport).await;          // 视觉：观察文件内容
        // sniff() 在 feel() 中自动触发（当工具失败且为编译相关工具时）

        let token_count = Self::estimate_tokens(output);
        self.short_term_memory.store(
            "tool".to_string(),
            output.to_string(),
            token_count,
            true,
        );
        // 将工具返回注入工作记忆（作为 user 消息，LLM 需要看到执行结果）
        let token_count = Self::estimate_tokens(&output);
        self.working_memory.push_user(output.to_string(), token_count);

        if self.pending_tool_results.is_empty() {
            // 通过状态机记录工具执行完成（借鉴 Claude Code）
            let outcome = if success {
                ToolExecutionOutcome::Success
            } else {
                ToolExecutionOutcome::Failure(output.chars().take(100).collect())
            };
            self.loop_state_machine.transition(LoopEvent::ToolExecutionCompleted {
                tool_use_id: tool_call_id.to_string(),
                tool_name: tool_name.to_string(),
                result: outcome,
            });
            self.state = self.loop_state_machine.state();
            true
        } else {
            false
        }
    }

    /// 每轮结束时执行滑动窗口更新，驱动冷热分层
    ///
    /// 将工作记忆中的消息按冷热评分分类：Hot 保留完整、Warm 摘要、Cold 占位。
    /// 更新后替换工作记忆条目，实现有限 token 预算内保留最关键信息。
    fn update_context_window(&mut self) {
        self.turns_executed += 1;

        // 从工作记忆构建 MessageMeta（滑动窗口需要每条消息的元数据）
        let entries = self.working_memory.entries();
        let messages: Vec<crate::memory::window::MessageMeta> = entries
            .iter()
            .enumerate()
            .map(|(idx, e)| {
                let (role, content, token_count) = match e {
                    crate::memory::working::WorkingEntry::Hot { role, content, token_count } => {
                        (format!("{:?}", role), content.clone(), *token_count)
                    }
                    crate::memory::working::WorkingEntry::Warm { summary, token_count, .. } => {
                        ("warm".into(), summary.clone(), *token_count)
                    }
                    crate::memory::working::WorkingEntry::Cold { placeholder, token_count, .. } => {
                        ("cold".into(), placeholder.clone(), *token_count)
                    }
                };
                crate::memory::window::MessageMeta {
                    elapsed_turns: self.turns_executed.saturating_sub(idx as u32),
                    relevance: if role == "User" || role == "Assistant" { 0.8 } else { 0.5 },
                    recall_count: 0,
                    is_tool_result: role == "User" && content.starts_with('['),
                    tool_importance: 0.5,
                    role,
                    content,
                    token_count,
                    source_range: (idx, idx + 1),
                }
            })
            .collect();

        // 滑动窗口更新：按冷热评分重新分类
        let updated = self.sliding_window.update(&messages);
        if !updated.is_empty() {
            self.working_memory.replace_entries(updated);
        }

        let current_tokens = self.working_memory.used_tokens();
        let max_tokens = self.working_memory.max_tokens();
        tracing::debug!(
            "滑动窗口更新完成：turn={}, tokens={}/{}",
            self.turns_executed, current_tokens, max_tokens
        );
    }

    /// 检查是否达到最大轮次
    fn check_max_turns(&self) -> bool {
        if let Some(max) = self.config.max_turns {
            self.turns_executed >= max
        } else {
            false
        }
    }

    /// 检查 Doom Loop
    fn check_doom_loop(&mut self) -> DoomLoopResult {
        self.doom_loop_detector.detect()
    }

    /// 估算文本的 token 数（粗略：1 token ≈ 4 字符）
    fn estimate_tokens(text: &str) -> u32 {
        (text.len() as f32 / 4.0).ceil() as u32
    }

    /// 计算工具参数的哈希值
    fn hash_tool_args(args: &serde_json::Value) -> u64 {
        let mut hasher = DefaultHasher::new();
        let canonical = serde_json::to_string(args).unwrap_or_else(|_| args.to_string());
        canonical.hash(&mut hasher);
        hasher.finish()
    }

    /// 运行 5 层压缩管道（在每次回到 sampler 前调用）
    ///
    /// 管道顺序：
    /// 1. MicroCompact：清除旧工具结果（按消息数阈值）
    /// 2. Context Collapse：LLM 摘要压缩早期轮次（按 token 占用率）
    /// 3. Auto Compact：全量压缩（最后手段）
    fn run_compaction_pipeline(&mut self) {
        let current_tokens = self.working_memory.used_tokens();
        let max_tokens = self.working_memory.max_tokens();
        let _usage_percent = if max_tokens > 0 {
            (current_tokens * 100) / max_tokens
        } else {
            0
        };

        // 层 1：MicroCompact — 清除旧工具结果
        let msg_count = self.working_memory.entries().len();
        if msg_count > self.compaction_config.microcompact_threshold
            && msg_count > self.last_microcompact_count
        {
            let result = self.working_memory.compact();
            self.last_microcompact_count = self.working_memory.entries().len();
            tracing::info!(
                "压缩管道 L1 MicroCompact：compacted={}, tokens {}→{}",
                result.entries_compacted, result.tokens_before, result.tokens_after
            );
        }

        // 重新计算占用率（MicroCompact 可能已释放空间）
        let current_tokens = self.working_memory.used_tokens();
        let usage_percent = if max_tokens > 0 {
            (current_tokens * 100) / max_tokens
        } else {
            0
        };

        // 层 2：Context Collapse — LLM 摘要（需要外部 sampler，此处标记）
        if usage_percent > self.compaction_config.collapse_threshold_percent
            && !self.context_collapse_active
        {
            tracing::info!(
                "压缩管道 L2 Context Collapse 触发：usage={}%>{}%",
                usage_percent, self.compaction_config.collapse_threshold_percent
            );
            self.context_collapse_active = true;
            // 通过 working_memory 的 compact 做本地摘要（不依赖外部 LLM）
            let result = self.working_memory.compact();
            self.context_collapse_active = false;
            tracing::info!(
                "压缩管道 L2 Context Collapse 完成：tokens {}→{}",
                result.tokens_before, result.tokens_after
            );
        }

        // 层 3：Auto Compact — 最后手段
        let current_tokens = self.working_memory.used_tokens();
        let usage_percent = if max_tokens > 0 {
            (current_tokens * 100) / max_tokens
        } else {
            0
        };
        if usage_percent > self.compaction_config.auto_compact_threshold_percent {
            tracing::warn!(
                "压缩管道 L3 Auto Compact 触发：usage={}%>{}%，强制压缩",
                usage_percent, self.compaction_config.auto_compact_threshold_percent
            );
            let result = self.working_memory.compact();
            tracing::info!(
                "压缩管道 L3 Auto Compact 完成：tokens {}→{}",
                result.tokens_before, result.tokens_after
            );
        }
    }

    /// 处理反射弧 motor 指令（来自 Kernel ReflexRouter）
    ///
    /// 反射弧闭环：感官信号 → Kernel ReflexRouter → motor 指令 → Thinker 调整行为
    fn handle_motor_adjust(&mut self, payload: &serde_json::Value) {
        let action = payload["action"].as_str().unwrap_or("none");
        match action {
            "slow_down" => {
                // 反射弧建议减速：提高 doom loop 检测灵敏度（降低重复阈值，保留历史计数）
                self.doom_loop_detector.set_repeat_threshold(2);
                tracing::info!("反射弧 motor：减速，doom loop 阈值降至 2");
            }
            "switch_strategy" => {
                let hint = payload["hint"].as_str().unwrap_or("尝试不同的方法");
                self.working_memory.push_system(
                    format!("[反射弧建议] {}", hint),
                    Self::estimate_tokens(hint),
                );
                tracing::info!("反射弧 motor：注入策略切换提示");
            }
            "disable_tool" => {
                if let Some(tool) = payload["tool"].as_str() {
                    self.disabled_tool_next_round = Some(tool.to_string());
                    tracing::info!("反射弧 motor：禁用工具 {}", tool);
                }
            }
            _ => {
                tracing::debug!("反射弧 motor：未知 action={}", action);
            }
        }
    }

    /// 持久化工作记忆到 StateNode（turn 结束时调用）
    ///
    /// 限制：
    /// - 排除 Cold 条目（仅占位符，无信息量）
    /// - 最多保留 50 条 Hot/Warm 条目
    /// - 总 token 上限 4000（超出则从最旧条目截断）
    async fn persist_to_state(&self, transport: &NodeTransportHandle) {
        const MAX_PERSIST_ENTRIES: usize = 50;
        const MAX_PERSIST_TOKENS: u32 = 4000;

        let mut entries: Vec<serde_json::Value> = Vec::new();
        let mut total_tokens: u32 = 0;

        for e in self.working_memory.entries() {
            let (role, content) = match e {
                crate::memory::working::WorkingEntry::Hot { role, content, .. } => {
                    (format!("{:?}", role), content.chars().take(200).collect::<String>())
                }
                crate::memory::working::WorkingEntry::Warm { summary, .. } => {
                    ("Warm".to_string(), summary.chars().take(200).collect::<String>())
                }
                crate::memory::working::WorkingEntry::Cold { .. } => continue,
            };

            let tokens = e.token_count();
            if total_tokens + tokens > MAX_PERSIST_TOKENS {
                break;
            }
            if entries.len() >= MAX_PERSIST_ENTRIES {
                break;
            }

            total_tokens += tokens;
            entries.push(serde_json::json!({
                "role": role,
                "content": content,
                "tokens": tokens,
            }));
        }

        if entries.is_empty() {
            return;
        }

        let persist_payload = serde_json::json!({
            "agent_id": self.id.as_str(),
            "entries": entries,
            "total_tokens": total_tokens,
        });
        if let Ok(msg) = FrameCodec::new_message(
            Topic::new("state/persist"),
            self.id.as_str(),
            &persist_payload,
        ) {
            let _ = transport.send_message(&msg).await;
            tracing::debug!(
                "Thinker {} turn 结束持久化：{} 条记录，{} tokens",
                self.id, entries.len(), total_tokens
            );
        }
    }

    /// 请求 Token 预算和熔断器检查（通过消息总线）
    ///
    /// 返回 true 表示允许执行，false 表示应跳过（预算不足或熔断器开启）。
    /// 采用 fire-and-forget 模式：默认放行，如果预算不足 Kernel 会通过
    /// cortex/budget_deny 通知，ThinkerNode 在 handle_message 中处理拒绝。
    async fn check_budget_and_circuit(&self, transport: &NodeTransportHandle) -> bool {
        let msg = FrameCodec::new_message(
            Topic::new("cortex/budget_check"),
            self.id.as_str(),
            &serde_json::json!({
                "agent_id": self.id.to_string(),
            }),
        );
        if let Ok(msg) = msg {
            if let Err(e) = transport.send_message(&msg).await {
                tracing::debug!("发送预算检查请求失败（放行）：{}", e);
            }
        }
        // fire-and-forget 模式：默认放行，如果预算不足 Kernel 会通过 cortex/budget_deny 通知
        true
    }

    /// 发送采样请求（复用逻辑）
    ///
    /// 发送前先通过消息总线请求 Token 预算和熔断器检查。
    async fn send_sample_request(&mut self, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        // 请求预算和熔断器检查（fire-and-forget，拒绝通过 cortex/budget_deny 异步通知）
        self.check_budget_and_circuit(transport).await;

        self.current_sample_request_id = None;
        AgentMetrics::global().record_loop_count(1);
        self.inference_start = Some(std::time::Instant::now());
        let request = self.build_sample_request();
        let sample_msg = FrameCodec::new_message(
            Topic::sampler_request(),
            self.id.as_str(),
            &request,
        )?;
        if let Err(e) = transport.publish_data(&sample_msg).await {
            tracing::debug!("数据面 PUB 发送采样请求失败，回退到控制面：{}", e);
            transport.send_message(&sample_msg).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl Node for ThinkerNode {
    fn node_type(&self) -> NodeType {
        NodeType::Thinker
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!(
            "Thinker Node 启动：{} (type={:?}, model={})",
            self.id, self.config.agent_type, self.config.model
        );
        self.state = AgentState::Idle;
        AgentMetrics::global().record_agent_started();
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        let topic = msg.topic.as_str();

        match topic {
            // agent/{agent_id}/input — 收到用户输入或父 Agent 指令（兼容 AgentNode 路径）
            t if t.starts_with("agent/") && t.ends_with("/input") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let content = payload["content"].as_str().unwrap_or("");
                let role = payload["role"].as_str().unwrap_or("user");
                self.listen(content, role); // 内置听觉

                self.agentic_session_active = true;
                AgentMetrics::global().record_loop_count(1);
                self.inference_start = Some(std::time::Instant::now());

                // 请求预算和熔断器检查（fire-and-forget，拒绝通过 cortex/budget_deny 异步通知）
                self.check_budget_and_circuit(transport).await;

                let request = self.build_sample_request();
                let sample_msg = FrameCodec::new_message(
                    Topic::sampler_request(),
                    self.id.as_str(),
                    &request,
                )?;
                if let Err(e) = transport.publish_data(&sample_msg).await {
                    tracing::debug!("数据面 PUB 发送失败，回退到控制面：{}", e);
                    transport.send_message(&sample_msg).await?;
                }
            }

            // sampler/*/stream — 收到 LLM 流式返回
            t if t.starts_with("sampler/") && t.ends_with("/stream") => {
                let raw_value: serde_json::Value = FrameCodec::decode_payload(&msg)?;

                // 检查是否为 done 消息
                if raw_value.get("type").and_then(|v| v.as_str()) == Some("done") {
                    if let Some(req_id) = raw_value.get("request_id").and_then(|v| v.as_str()) {
                        if Some(req_id) != self.current_sample_request_id.as_deref() {
                            return Ok(());
                        }
                    }

                    if let Some(start) = self.inference_start.take() {
                        AgentMetrics::global()
                            .record_inference_latency(start.elapsed().as_millis() as f64);
                    }

                    if !self.pending_tool_calls.is_empty() {
                        self.state = AgentState::ToolCalling;
                        let tool_calls: Vec<(String, PendingToolCall)> =
                            self.pending_tool_calls.drain().collect();
                        for (tool_call_id, pending) in tool_calls {
                            self.pending_tool_results
                                .insert(tool_call_id.clone(), pending.tool_name.clone());

                            // 发送到 agent/{id}/tool_call（ToolNode 标准路径）
                            let tool_call_msg = FrameCodec::new_message(
                                Topic::agent_tool_call(self.id.as_str()),
                                self.id.as_str(),
                                &serde_json::json!({
                                    "tool_call_id": tool_call_id,
                                    "tool_name": pending.tool_name,
                                    "arguments": pending.arguments,
                                    "agent_id": self.id.to_string(),
                                }),
                            )?;
                            if let Err(e) = transport.publish_data(&tool_call_msg).await {
                                tracing::debug!("数据面 PUB 发送 tool_call 失败，回退到控制面：{}", e);
                                transport.send_message(&tool_call_msg).await?;
                            }

                            tracing::debug!(
                                "Thinker {} 发送工具调用：{} ({})",
                                self.id, pending.tool_name, tool_call_id
                            );
                        }
                    } else {
                        self.current_sample_request_id = None;
                        self.agentic_session_active = false;
                        self.state = AgentState::Idle;
                        self.update_context_window();

                        // GoalLoop：turn 完成后通知目标循环
                        if self.goal_loop.is_some() {
                            let success = !self.cancel_requested;
                            if let Some(ref mut gl) = self.goal_loop {
                                let action = gl.on_turn_complete(success);
                                self.process_goal_action(action, transport).await?;
                            }
                        }

                        // ERL 轨迹提取：turn 结束时（Done 状态）从状态机提取 TaskTrajectory
                        if self.loop_state_machine.state() == AgentState::Done {
                            let trajectory = self.loop_state_machine.extract_trajectory();
                            if !trajectory.steps.is_empty() {
                                let traj_payload = serde_json::json!({
                                    "agent_id": self.id.to_string(),
                                    "trajectory": trajectory,
                                });
                                if let Ok(traj_msg) = FrameCodec::new_message(
                                    Topic::new("cortex/erl_trajectory"),
                                    self.id.as_str(),
                                    &traj_payload,
                                ) {
                                    if let Err(e) = transport.send_message(&traj_msg).await {
                                        tracing::debug!("发送 ERL 轨迹失败：{}", e);
                                    }
                                }
                            }
                        }

                        // 请求元认知评估当前推理质量（异步，结果通过 cortex/meta_result 回传）
                        let context_summary: String = self.working_memory.entries().iter().rev().take(3)
                            .filter_map(|e| match e {
                                WorkingEntry::Hot { content, .. } => Some(content.chars().take(100).collect::<String>()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join(" | ");
                        self.request_meta_assessment(&context_summary, transport).await;

                        // turn 结束：持久化工作记忆到 StateNode
                        self.persist_to_state(transport).await;

                        tracing::debug!("Thinker {} agentic turn 完成", self.id);
                    }
                    return Ok(());
                }

                // 检查是否为错误消息
                if raw_value.get("error").is_some() {
                    tracing::error!(
                        "Thinker {} 收到采样错误：{:?}",
                        self.id,
                        raw_value.get("error")
                    );
                    self.inference_start = None;
                    AgentMetrics::global().record_error("sampler_error");
                    self.current_sample_request_id = None;
                    self.state = AgentState::Error;
                    return Ok(());
                }

                // 正常 StreamChunk
                let chunk: StreamChunk = FrameCodec::decode_payload(&msg)?;
                if Some(&chunk.request_id) != self.current_sample_request_id.as_ref() {
                    return Ok(());
                }
                self.handle_stream_chunk(&chunk, transport).await?;
            }

            // agent/{agent_id}/tool_result — 工具执行结果（ToolNode 标准路径）
            t if t.starts_with("agent/") && t.ends_with("/tool_result") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let tool_call_id = payload["tool_call_id"].as_str().unwrap_or("");
                let output = payload["output"].as_str().unwrap_or("");
                let success = payload["success"].as_bool().unwrap_or(true);
                let duration_ms = payload["duration_ms"].as_u64().unwrap_or(0);

                if let Some(tool_name) = self.pending_tool_results.get(tool_call_id).cloned() {
                    AgentMetrics::global()
                        .record_tool_execution_time(&tool_name, duration_ms as f64);
                    if !success {
                        AgentMetrics::global().record_error("tool_execution_failed");
                    }

                    let all_done = self.handle_tool_result(tool_call_id, &tool_name, output, success, transport).await;

                    if all_done {
                        // 工具执行完毕，回到 sampler 前先跑压缩管道
                        self.run_compaction_pipeline();

                        self.send_sample_request(transport).await?;
                    }
                } else {
                    tracing::warn!("Thinker {} 收到未知 tool_call_id 的结果：{}", self.id, tool_call_id);
                }
            }

            // agent/{agent_id}/cancel — 取消请求（来自 AcpNode/TUINode）
            t if t.starts_with("agent/") && t.ends_with("/cancel") => {
                tracing::info!("Thinker {} 收到取消请求", self.id);
                self.cancel_requested = true;

                // 如果有正在进行的采样请求，转发取消到 sampler
                if let Some(ref req_id) = self.current_sample_request_id {
                    let cancel_msg = FrameCodec::new_message(
                        Topic::sampler_cancel(req_id),
                        self.id.as_str(),
                        &serde_json::json!({
                            "request_id": req_id,
                            "agent_id": self.id.to_string(),
                        }),
                    )?;
                    if let Err(e) = transport.publish_data(&cancel_msg).await {
                        tracing::debug!("数据面 PUB 发送采样取消失败，回退到控制面：{}", e);
                        transport.send_message(&cancel_msg).await?;
                    }
                    tracing::info!("Thinker {} 已转发取消到 sampler: {}", self.id, req_id);
                }

                // 清理待处理的工具调用
                self.pending_tool_calls.clear();
                self.pending_tool_results.clear();
                self.current_sample_request_id = None;
                self.agentic_session_active = false;
                self.state = AgentState::Idle;
            }

            // motor/{agent_id}/adjust — 反射弧 motor 指令（来自 Kernel ReflexRouter）
            t if t.starts_with("motor/") && t.ends_with("/adjust") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                self.handle_motor_adjust(&payload);
            }

            // cortex/budget_deny — Token 预算不足或熔断器开启（来自 Kernel）
            "cortex/budget_deny" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let reason = payload["reason"].as_str().unwrap_or("unknown");
                tracing::warn!("LLM 调用被拒绝：{}", reason);
                // 通知用户
                let notice_msg = FrameCodec::new_message(
                    Topic::agent_output(self.id.as_str()),
                    self.id.as_str(),
                    &serde_json::json!({
                        "channel": "text",
                        "content": format!("[系统保护] LLM 调用被拒绝：{}", reason),
                    }),
                );
                if let Ok(notice_msg) = notice_msg {
                    if let Err(e) = transport.publish_data(&notice_msg).await {
                        tracing::debug!("发送预算拒绝通知失败：{}", e);
                    }
                }
                self.state = AgentState::Idle;
                self.current_sample_request_id = None;
            }

            // cortex/sensory — L1 本能反射通知（来自 Kernel ReflexRouter）
            "cortex/sensory" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let signal_topic = payload["signal_topic"].as_str().unwrap_or("");
                let action = payload["action"].as_str().unwrap_or("");
                let summary = format!("[反射弧L1] 信号={} 动作={}", signal_topic, action);
                self.working_memory.push_system(&summary, summary.len() as u32 / 3);
                tracing::info!(signal_topic, action, "Thinker 收到 L1 本能反射通知");
            }

            // cortex/meta_result — 元认知评估结果（来自 Kernel MetaCognitiveController）
            "cortex/meta_result" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                if let Some(conflicts) = payload.get("conflicts").and_then(|v| v.as_array()) {
                    if !conflicts.is_empty() {
                        tracing::warn!("元认知检测到 {} 个逻辑冲突，注入工作记忆", conflicts.len());
                        for conflict in conflicts {
                            if let Some(desc) = conflict["description"].as_str() {
                                self.working_memory.push_system(
                                    format!("[元认知警告] {}", desc),
                                    Self::estimate_tokens(desc),
                                );
                            }
                        }
                    }
                }
                // 如果元认知建议切换策略
                if let Some(strategy) = payload.get("suggested_strategy").and_then(|v| v.as_str()) {
                    tracing::info!("元认知建议策略：{}", strategy);
                    self.working_memory.push_system(
                        format!("[元认知建议] 考虑切换到 {} 策略", strategy),
                        Self::estimate_tokens(strategy),
                    );
                }
            }

            // cortex/erl_heuristic — 经验反思学习结果（来自 Kernel ERL）
            "cortex/erl_heuristic" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                if let Some(heuristic) = payload["heuristic"].as_str() {
                    tracing::info!("ERL 提取经验教训：{}", heuristic);
                    self.working_memory.push_system(
                        format!("[经验教训] {}", heuristic),
                        Self::estimate_tokens(heuristic),
                    );
                }
            }

            // tool/register — 收到工具注册消息
            "tool/register" => {
                let tools: Vec<SamplerToolDefinition> = FrameCodec::decode_payload(&msg)?;
                self.config.tools = tools;
                tracing::info!("Thinker {} 收到工具注册，共 {} 个工具", self.id, self.config.tools.len());
            }

            // sys/shutdown — 收到系统 shutdown 信号
            "sys/shutdown" => {
                self.state = AgentState::Done;
            }

            // 收到子 Agent 事件
            t if t.ends_with("/event") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                if let Some(error) = payload.get("error") {
                    let crashed = SubAgentCrashed {
                        node_id: payload["node_id"].as_str().unwrap_or("").into(),
                        error: error.as_str().unwrap_or("unknown").into(),
                    };
                    tracing::warn!("子 Agent 崩溃：{} - {}", crashed.node_id, crashed.error);
                    AgentMetrics::global().record_error("subagent_crashed");
                    let node_id: NodeId = crashed.node_id.clone().into();
                    self.orchestrator.remove_subagent(&node_id);
                } else if payload.get("type").and_then(|v| v.as_str()) == Some("completed") {
                    let subagent_id = payload["node_id"].as_str().unwrap_or("");
                    let output = payload["output"].as_str().unwrap_or("");
                    tracing::info!("子 Agent 完成：{}, 输出长度={}", subagent_id, output.len());
                    let token_count = Self::estimate_tokens(output);
                    self.working_memory.push_user(output.to_string(), token_count);
                    let node_id: NodeId = subagent_id.to_string().into();
                    self.orchestrator.remove_subagent(&node_id);
                }
            }

            _ => {
                tracing::debug!("Thinker 收到未处理 topic：{}", topic);
            }
        }

        // 检查 Doom Loop 并应用逃脱策略
        let doom_result = self.check_doom_loop();
        if doom_result.detected {
            tracing::warn!(
                "Doom Loop 检测触发：工具 {:?} 重复 {} 次，应用逃脱策略（{} 个动作），已尝试 {} 次",
                doom_result.repeated_tool, doom_result.repeat_count, doom_result.escape_actions.len(), self.doom_loop_escape_attempts
            );
            AgentMetrics::global().record_error("doom_loop");

            // 逃脱尝试超过 3 次，强制终止
            if self.doom_loop_escape_attempts >= 3 {
                tracing::warn!("Doom Loop 逃脱已尝试 {} 次，强制终止循环", self.doom_loop_escape_attempts);
                if let Some(loop_event) = LoopEvent::from_doom_loop(&doom_result) {
                    let action = self.loop_state_machine.transition(loop_event);
                    self.state = self.loop_state_machine.state();
                    tracing::info!(
                        "LoopStateMachine 状态变迁：action={:?}, state={:?}",
                        action, self.state
                    );
                    if let LoopAction::EndTurn { reason } = action {
                        tracing::warn!("LoopStateMachine 判定结束：reason={:?}", reason);
                        let notice_msg = FrameCodec::new_message(
                            Topic::agent_output(self.id.as_str()),
                            self.id.as_str(),
                            &serde_json::json!({
                                "channel": "text",
                                "content": format!("Agent 循环终止：{:?}", reason),
                            }),
                        )?;
                        if let Err(e) = transport.publish_data(&notice_msg).await {
                            tracing::debug!("数据面 PUB 发送循环终止通知失败：{}", e);
                            transport.send_message(&notice_msg).await?;
                        }
                        return Ok(());
                    }
                }
            }

            // 应用逃脱动作，然后继续循环给 Agent 逃脱机会
            self.doom_loop_escape_attempts += 1;

            for action in &doom_result.escape_actions {
                match action {
                    EscapeAction::InjectHint(hint) => {
                        // 注入提示到工作记忆作为系统消息
                        self.working_memory.push_system(hint.clone(), Self::estimate_tokens(hint));
                        tracing::warn!("Doom Loop 逃脱：已注入换策略提示到工作记忆");
                    }
                    EscapeAction::DisableTool(tool) => {
                        // 禁用工具：下一轮生效
                        self.disabled_tool_next_round = Some(tool.clone());
                        tracing::warn!("Doom Loop 逃脱：下一轮禁用工具 {}", tool);
                    }
                    EscapeAction::DegradeModel => {
                        // 降级模型：reasoning_effort 将由 current_reasoning_effort() 自动拾取
                        let level = self.doom_loop_detector.model_degrade_level();
                        tracing::warn!(
                            "Doom Loop 逃脱：降级模型（reasoning_effort level={}）",
                            level
                        );
                    }
                }
            }

            // 通知用户 Doom Loop 逃脱正在尝试
            let notice_msg = FrameCodec::new_message(
                Topic::agent_output(self.id.as_str()),
                self.id.as_str(),
                &serde_json::json!({
                    "channel": "text",
                    "content": format!(
                        "检测到工具循环（{} 重复 {} 次），已注入换策略提示、临时禁用该工具并降级模型，正在尝试跳出循环（第 {} 次）。",
                        doom_result.repeated_tool.as_deref().unwrap_or("unknown"),
                        doom_result.repeat_count,
                        self.doom_loop_escape_attempts
                    ),
                }),
            )?;
            if let Err(e) = transport.publish_data(&notice_msg).await {
                tracing::debug!("数据面 PUB 发送 doom loop 通知失败，回退到控制面：{}", e);
                transport.send_message(&notice_msg).await?;
            }

            // 不结束 turn，继续调用 LLM 给 Agent 逃脱循环的机会
            self.send_sample_request(transport).await?;
        } else {
            // 未检测到 Doom Loop，重置逃脱尝试计数
            self.doom_loop_escape_attempts = 0;
        }

        // 检查是否需要自动压缩（借鉴 Claude Code CompactionPolicy）
        if self.working_memory.should_compact() {
            tracing::info!(
                "工作记忆使用率达压缩阈值，执行自动压缩（tokens_used={}/max={})",
                self.working_memory.used_tokens(),
                self.working_memory.max_tokens()
            );
            let result = self.working_memory.compact();
            tracing::info!(
                "工作记忆压缩完成：before={}, after={}, compacted_entries={}",
                result.tokens_before, result.tokens_after, result.entries_compacted
            );
        }

        // 检查最大轮次（通过状态机）
        if self.check_max_turns() {
            tracing::info!("达到最大轮次 {}，Thinker 结束", self.turns_executed);
            let action = self.loop_state_machine.transition(LoopEvent::MaxTurnsReached);
            self.state = self.loop_state_machine.state();
            tracing::debug!("LoopStateMachine MaxTurnsReached：action={:?}", action);
        }

        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        let mut subs = vec![
            format!("agent/{}/input", self.id),       // 用户输入（替代 cortex/{id}/input）
            format!("agent/{}/tool_result", self.id),  // 工具结果（替代 skin/touch）
            format!("agent/{}/cancel", self.id),       // 取消请求（来自 AcpNode/TUINode）
            format!("motor/{}/adjust", self.id),       // 反射弧 motor 指令（来自 Kernel ReflexRouter）
            "cortex/sensory".into(),                   // L1 本能反射通知（来自 Kernel ReflexRouter）
            "cortex/budget_deny".into(),               // Token 预算拒绝通知
            "cortex/meta_result".into(),               // 元认知评估结果
            "cortex/erl_heuristic".into(),             // 经验反思学习结果
            "sampler/*/stream".into(),
            "tool/register".into(),
            "sys/shutdown".into(),
        ];

        // 如果有活跃子 Agent，也订阅它们的输出和事件
        for sub in self.orchestrator.active_subagents() {
            subs.push(format!("agent/{}/output", sub.node_id));
            subs.push(format!("agent/{}/event", sub.node_id));
        }

        subs
    }

    fn published_topics(&self) -> Vec<String> {
        vec![
            format!("agent/{}/output", self.id),       // 文本输出（替代 cortex/{id}/speak）
            format!("agent/{}/tool_call", self.id),     // 工具调用（替代 hand/limb/*）
        ]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        // 会话结束时提取关键知识并持久化（热区→冷区）
        let messages: Vec<(String, String)> = self.working_memory.entries().iter().map(|e| {
            match e {
                crate::memory::working::WorkingEntry::Hot { role, content, .. } => {
                    (format!("{:?}", role), content.clone())
                }
                crate::memory::working::WorkingEntry::Warm { summary, .. } => {
                    ("warm".into(), summary.clone())
                }
                crate::memory::working::WorkingEntry::Cold { placeholder, .. } => {
                    ("cold".into(), placeholder.clone())
                }
            }
        }).collect();
        if !messages.is_empty() {
            self.memory_bridge.extract_and_store(&messages);
            tracing::info!("会话结束：已提取 {} 条消息到长期记忆", messages.len());
        }

        self.state = AgentState::Done;
        tracing::info!("Thinker Node 关闭：{}", self.id);
        AgentMetrics::global().record_agent_stopped();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentType;

    fn test_config() -> AgentConfig {
        AgentConfig {
            agent_type: AgentType::Primary,
            model: "test-model".into(),
            permission_mode: crate::node::PermissionMode::Yolo,
            max_turns: Some(10),
            subagents_enabled: false,
            non_interactive: true,
            tools: Vec::new(),
        }
    }

    #[test]
    fn cancel_requested_initially_false() {
        let thinker = ThinkerNode::new(NodeId::new(), test_config());
        assert!(!thinker.cancel_requested, "cancel_requested should be false initially");
    }

    #[test]
    fn cancel_requested_set_to_true() {
        let mut thinker = ThinkerNode::new(NodeId::new(), test_config());
        thinker.cancel_requested = true;
        assert!(thinker.cancel_requested, "cancel_requested should be true after setting");
    }

    #[test]
    fn pending_tool_calls_cleared_on_cancel() {
        let mut thinker = ThinkerNode::new(NodeId::new(), test_config());

        thinker.pending_tool_calls.insert("tc-1".into(), PendingToolCall {
            tool_call_id: "tc-1".into(),
            tool_name: "Bash".into(),
            arguments: serde_json::json!({}),
        });

        assert_eq!(thinker.pending_tool_calls.len(), 1);

        thinker.pending_tool_calls.clear();
        thinker.cancel_requested = true;

        assert!(thinker.pending_tool_calls.is_empty(), "pending tool calls should be cleared");
        assert!(thinker.cancel_requested, "cancel_requested should be set");
    }
}
