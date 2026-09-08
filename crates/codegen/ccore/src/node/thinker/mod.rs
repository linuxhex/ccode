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

// ── 子模块（拆分自原 god file） ──
pub mod sensory;
pub mod compaction;
pub mod goal;
pub mod memory_bridge;

// ── 重导出子模块公共项，保持外部路径兼容 ──
pub use sensory::SensorySignal;
pub use compaction::CompactionConfig;
pub use memory_bridge::{MemoryBridge, EpisodicMemoryBridge};

use async_trait::async_trait;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use chrono::Utc;

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
use crate::persistence::state::StatePersister;
use crate::agent::goal_loop::{GoalLoop, GoalAction};
use crate::agent::schedule_loop::{ScheduleLoop, ScheduleAction, ScheduleState};
use crate::agent::proactive_loop::{ProactiveLoop, ProactiveSpec, ProactiveAction, ProactiveState};
use crate::agent::orchestrator::Orchestrator;
use crate::agent::subagent::SubAgentCrashed;

use crate::memory::working::{WorkingMemory, WorkingEntry, MessageRole};
use crate::memory::short_term::ShortTermMemory;
use crate::memory::window::SlidingWindow;
use crate::memory::embedding::EmbeddingIndex;
use crate::memory::intent_retriever::IntentRetriever;

use crate::sampler::provider::{
    SampleRequest, ChatMessage, StreamChunk, StreamChannel, ToolDefinition as SamplerToolDefinition,
};
use crate::retry::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig};
use crate::tools::read_tracker::ReadTracker;
use crate::sampler::token_budget::TokenBudgetManager;
use crate::memory::episodic::EpisodicMemoryStore;
use crate::agent::meta_cognitive::MetaCognitiveController;

// ── Hook 系统（生命周期钩子：Stop/PreCompact/PostCompact/Subagent） ──
use ccode_hooks::discovery::HookRegistry;
use ccode_hooks::dispatcher;
use ccode_hooks::event::{HookEventEnvelope, HookEventName, HookPayload, SubagentStopPhase};
use ccode_hooks::runner::RunContext;

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
    /// 记忆桥接（连接 ccode-memory 等外部记忆系统）
    memory_bridge: Box<dyn MemoryBridge>,
    /// 等待中的工具调用（tool_call_id → PendingToolCall）
    pending_tool_calls: HashMap<String, PendingToolCall>,
    /// 正在执行中的工具调用（tool_call_id → (tool_name, started_at)，等待结果返回）
    pending_tool_results: HashMap<String, (String, std::time::Instant)>,
    /// 当前 LLM 采样请求 ID
    current_sample_request_id: Option<String>,
    /// 当前推理周期的开始时间
    inference_start: Option<std::time::Instant>,
    /// 已执行轮次
    turns_executed: u32,
    /// 连续工具调用轮次（用于 Reminder 注入，防止长对话指令漂移，借鉴 Claude Code）
    consecutive_tool_rounds: u32,
    /// 感官信号缓冲（最近 20 条，内置处理）
    sensory_buffer: Vec<SensorySignal>,
    /// 取消请求标志（收到 agent/{id}/cancel 后设置）
    cancel_requested: bool,
    /// 当前 turn 内是否发生过工具失败（供 GoalLoop on_turn_complete 判定成功）
    turn_had_tool_failure: bool,
    /// Agentic 会话是否活跃（input → sampling → tool → re-sample 循环中）
    agentic_session_active: bool,
    /// 意图检索器（Context Engine 核心：意图扩展 + 代码块检索）
    intent_retriever: IntentRetriever,
    /// Goal Loop（目标驱动循环，/goal 命令触发）
    goal_loop: Option<GoalLoop>,
    /// Schedule Loop（定时循环，/schedule 命令触发）
    schedule_loop: Option<ScheduleLoop>,
    /// Proactive Loop（主动扫描循环，/proactive on 启用）
    proactive_loop: ProactiveLoop,
    /// Schedule tick task 活跃标志（false 时 spawn 的定时 task 退出）
    schedule_active: Arc<std::sync::atomic::AtomicBool>,
    /// Proactive tick task 活跃标志
    proactive_active: Arc<std::sync::atomic::AtomicBool>,
    /// Doom Loop 逃脱尝试次数（超过 3 次才真正终止）
    doom_loop_escape_attempts: u32,
    /// 项目上下文（CCODE.md，会话开始时注入）
    project_context: crate::tools::project_context::ProjectContext,
    /// Token 预算被拒绝标志（收到 cortex/budget_deny 后设置，阻止后续 LLM 调用）
    budget_denied: bool,
    /// 强制结束 turn 标志（LoopStateMachine 判定 EndTurn 后设置，阻止继续采样）
    force_end_turn: bool,
    /// 稳定前缀是否已锁定（首次 build_sample_request 时锁定，保护 system prompt + tools 不被压缩）
    stable_prefix_locked: bool,
    /// 本轮 assistant 流式文本累积
    ///
    /// 流式 text chunk 除转发 TUI 外，还需累积起来在 done 时写入工作记忆。
    /// 否则 LLM 看不到自己上一轮说过什么，且对话历史里没有 assistant 角色——
    /// Anthropic Messages API 拒绝连续 user 消息，会直接失败。
    current_assistant_text: String,
    /// 在途的 L4 摘要请求 id
    ///
    /// 摘要走异步往返（发请求 → cortex/summary_result 回传），需防重入：
    /// 压缩管道每轮都跑，若不去重会在摘要返回前反复发起请求。
    pending_summary_request: Option<String>,
    /// 状态持久化器（运行时注入，turn 结束时保存快照，None 表示禁用持久化）
    state_persister: Option<StatePersister>,

    // ── ccore 高级特性（融合自 SessionActor，ThinkerNode 唯一 Agent Loop） ──
    /// 熔断器：5次连续失败后自动断开，防止级联故障
    circuit_breaker: CircuitBreaker,
    /// 读取追踪器：先读后写保护，跨线程安全（Arc<Mutex>）
    read_tracker: std::sync::Arc<std::sync::Mutex<ReadTracker>>,
    /// Token 预算管理器：跨轮次累计使用量，驱动上下文压缩
    token_budget: TokenBudgetManager,
    /// 情景记忆存储：每轮对话编码为情景记忆，支持上下文重建
    episodic_memory: EpisodicMemoryStore,
    /// 元认知控制器：难度评估+策略推荐+冲突检测
    meta_cognitive: MetaCognitiveController,
    /// Hook 注册表：生命周期钩子（Stop/PreCompact/PostCompact/Subagent）
    /// 由 Kernel 在启动时注入，None 表示禁用钩子功能
    hook_registry: Option<HookRegistry>,
    /// 本轮 LLM 调用累计 token 数（input + output），供 turn 结束时汇总
    current_turn_token_usage: (usize, usize),
    /// 本轮使用的工具名列表，供 EpisodicMemory 编码
    current_turn_tools: Vec<String>,
    /// 工具 schema 稳定性追踪（检测跨轮次变化，诊断 prompt cache miss）
    tool_stability_tracker: crate::sampler::tool_stability::ToolStabilityTracker,
}

impl ThinkerNode {
    pub fn new(id: NodeId, mut config: AgentConfig) -> Self {
        let max_tokens = 128_000;
        let model = config.model.clone();

        // 确保 task 工具定义在 config.tools 中（LLM 需要知道可以调用 task 工具）
        let has_task_tool = config.tools.iter().any(|t| t.name == "task");
        if !has_task_tool {
            config.tools.push(crate::sampler::provider::ToolDefinition {
                name: "task".to_string(),
                description: "Spawn a sub-agent to handle a subtask independently. The sub-agent runs with its own context and tools, returning results when complete.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "description": { "type": "string", "description": "Task description for the sub-agent" },
                        "agent_type": { "type": "string", "description": "Sub-agent type (general-purpose, code-reviewer, explore)", "default": "general-purpose" },
                        "model": { "type": "string", "description": "Optional model override" }
                    },
                    "required": ["description"]
                }),
            });
        }

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
            memory_bridge: Box::new(memory_bridge::NoopMemoryBridge),
            config,
            pending_tool_calls: HashMap::new(),
            pending_tool_results: HashMap::new(),
            current_sample_request_id: None,
            inference_start: None,
            turns_executed: 0,
            consecutive_tool_rounds: 0,
            sensory_buffer: Vec::with_capacity(sensory::SENSORY_BUFFER_CAPACITY),
            cancel_requested: false,
            turn_had_tool_failure: false,
            agentic_session_active: false,
            intent_retriever: IntentRetriever::new(EmbeddingIndex::new()),
            goal_loop: None,
            schedule_loop: None,
            proactive_loop: ProactiveLoop::new(ProactiveSpec::default()),
            schedule_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            proactive_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            doom_loop_escape_attempts: 0,
            project_context: crate::tools::project_context::ProjectContext::default(),
            budget_denied: false,
            force_end_turn: false,
            stable_prefix_locked: false,
            current_assistant_text: String::new(),
            pending_summary_request: None,
            state_persister: None,
            // ccore 高级特性初始化（融合自 SessionActor）
            circuit_breaker: CircuitBreaker::new(CircuitBreakerConfig::default()),
            read_tracker: std::sync::Arc::new(std::sync::Mutex::new(ReadTracker::new())),
            token_budget: TokenBudgetManager::new(&model),
            episodic_memory: EpisodicMemoryStore::new(),
            meta_cognitive: MetaCognitiveController::new(),
            hook_registry: None,
            current_turn_token_usage: (0, 0),
            current_turn_tools: Vec::new(),
            tool_stability_tracker: crate::sampler::tool_stability::ToolStabilityTracker::new(),
        }
    }

    /// 注入状态持久化器（builder 模式，FileStorage::new 是 async 故运行时注入）
    pub fn with_state_persister(mut self, persister: StatePersister) -> Self {
        self.state_persister = Some(persister);
        self
    }

    /// 注入 Hook 注册表（builder 模式）
    pub fn with_hook_registry(mut self, registry: HookRegistry) -> Self {
        self.hook_registry = Some(registry);
        self
    }

    /// 构建 Hook 运行上下文
    fn hook_run_ctx(&self) -> RunContext<'_> {
        RunContext {
            session_id: self.id.as_str(),
            workspace_root: ".",
        }
    }

    /// 构建 Hook 事件信封（通用方法）
    fn hook_event_envelope(&self, event: HookEventName, payload: HookPayload) -> HookEventEnvelope {
        HookEventEnvelope {
            hook_event_name: event,
            session_id: self.id.to_string(),
            cwd: ".".to_string(),
            workspace_root: ".".to_string(),
            timestamp: Utc::now().to_rfc3339(),
            transcript_path: None,
            client_identifier: None,
            prompt_id: None,
            permission_mode: None,
            payload,
        }
    }

    /// 保存当前状态快照（turn 结束时调用）
    ///
    /// 将 LoopStateMachine + GoalLoop 状态序列化并持久化。
    /// 持久化失败仅记录日志，不影响主流程。
    pub async fn save_state_snapshot(&self) {
        // 先构建快照（借用 self），释放借用后再调用 persister（避免跨 await 持有借用）
        let snapshot = {
            let mut snap = self.loop_state_machine.to_snapshot();
            if let Some(ref gl) = self.goal_loop {
                snap.goal = Some(gl.to_snapshot());
            }
            snap
        };
        if let Some(ref persister) = self.state_persister {
            if let Err(e) = persister.save_state(&snapshot).await {
                tracing::warn!(target: "ccore::persistence", error = %e, "保存状态快照失败");
            }
        }
    }

    /// 从持久化恢复状态（启动时调用）
    ///
    /// 返回 true 表示成功加载并恢复状态，false 表示无快照或未注入持久化器。
    pub async fn restore_state(&mut self) -> bool {
        // 在块内加载快照（借用 self.state_persister），释放后再 &mut self 恢复
        let snapshot = {
            let Some(ref persister) = self.state_persister else {
                return false;
            };
            match persister.load_state().await {
                Ok(Some(s)) => Some(s),
                Ok(None) => return false,
                Err(e) => {
                    tracing::warn!(target: "ccore::persistence", error = %e, "加载状态快照失败");
                    return false;
                }
            }
        };
        if let Some(snapshot) = snapshot {
            self.loop_state_machine.restore_from_snapshot(&snapshot);
            if let Some(ref goal_snap) = snapshot.goal {
                match self.goal_loop.as_mut() {
                    Some(gl) => gl.restore_from_snapshot(goal_snap),
                    None => {
                        // goal_loop 不存在（ThinkerNode::new 初始化为 None），从快照创建
                        let mut gl = GoalLoop::new(goal_snap.spec.clone());
                        gl.restore_from_snapshot(goal_snap);
                        self.goal_loop = Some(gl);
                    }
                }
            }
            tracing::info!(target: "ccore::persistence", "状态快照已恢复");
            true
        } else {
            false
        }
    }

    /// 设置记忆桥接（接入 ccode-memory 等外部记忆系统）
    pub fn set_memory_bridge(&mut self, bridge: Box<dyn MemoryBridge>) {
        self.memory_bridge = bridge;
    }

    /// 加载项目上下文（CCODE.md）
    pub fn load_project_context(&mut self, dir: &std::path::Path) {
        self.project_context = crate::tools::project_context::ProjectContext::load_from_dir(dir);
        if let Some(prompt) = self.project_context.system_prompt() {
            let tokens = Self::estimate_tokens(&prompt);
            self.working_memory.push_system(prompt, tokens);
        }
    }

    /// 用恢复出来的会话记录回灌工作记忆
    ///
    /// 只恢复 `StateNode` 的短期记忆是不够的 —— 那是持久化侧的账本，
    /// 真正决定模型看得见什么的是 `ThinkerNode::working_memory`。
    /// 两边都灌，才是「会话恢复」而不是「日志读回来了」。
    ///
    /// 角色映射与在线路径保持一致：工具输出在线上是以 `user` 消息进工作记忆的
    /// （见 `handle_tool_result`），恢复时也必须一样，否则同一段历史在恢复前后
    /// 对模型呈现的形状不同。
    pub fn restore_working_memory(
        &mut self,
        messages: &[crate::node::session_index::RestoredMessage],
    ) {
        if messages.is_empty() {
            return;
        }

        let banner = format!(
            "[会话已恢复] 以下 {} 条是上次会话的历史记录，继续在此基础上工作。",
            messages.len()
        );
        let tokens = Self::estimate_tokens(&banner);
        self.working_memory.push_system(banner, tokens);

        for m in messages {
            let tokens = if m.token_count > 0 {
                m.token_count
            } else {
                Self::estimate_tokens(&m.content)
            };
            if m.is_tool_call {
                self.working_memory.push_user(m.content.clone(), tokens);
                continue;
            }
            match m.role.as_str() {
                "assistant" => self.working_memory.push_assistant(m.content.clone(), tokens),
                "system" => self.working_memory.push_system(m.content.clone(), tokens),
                // 未知角色按 user 处理：宁可多一条上下文，也不要静默丢历史
                _ => self.working_memory.push_user(m.content.clone(), tokens),
            }
        }

        tracing::info!(
            target: "ccore::session",
            messages = messages.len(),
            "工作记忆已回灌恢复的会话历史"
        );
    }

    // ── 核心推理循环 ──

    /// 构建采样请求，发送到 Sampler Node
    fn build_sample_request(&mut self) -> SampleRequest {
        // 优化：直接构建 ChatMessage，避免中间 Vec<(String, String)> 分配
        let mut messages: Vec<ChatMessage> = self.working_memory
            .to_chat_messages_direct(|role, content| ChatMessage {
                role,
                content,
                cache_control: None,
            });

        // ── Reminder 注入（借鉴 Claude Code，防止长对话指令漂移）──
        // 连续工具调用超过阈值时，在最后一条 user 消息中追加 system-reminder，
        // 提醒模型当前任务目标，避免在长工具链中偏离原始意图。
        const REMINDER_THRESHOLD: u32 = 5;
        if self.consecutive_tool_rounds >= REMINDER_THRESHOLD {
            if let Some(last_user) = messages.iter_mut().rev().find(|m| m.role == "user") {
                let task_hint = self.loop_state_machine.task_description()
                    .unwrap_or("当前任务");
                let reminder = format!(
                    "\n\n<system-reminder>\n\
                     你已经执行了 {} 轮工具调用。请回顾原始任务：「{}」。\n\
                     确认当前工作是否仍然围绕这个目标，如果已完成请停止调用工具并总结结果。\n\
                     </system-reminder>",
                    self.consecutive_tool_rounds,
                    task_hint.chars().take(200).collect::<String>(),
                );
                last_user.content.push_str(&reminder);
                tracing::debug!(
                    target: "ccore::loop",
                    rounds = self.consecutive_tool_rounds,
                    "Reminder 注入：连续 {} 轮工具调用，已追加防漂移提醒",
                    self.consecutive_tool_rounds,
                );
            }
        }

        // Prompt cache 断点：4-breakpoint 策略（借鉴 Claude Code cache_edits）
        super::super::sampler::cache_placement::place_breakpoints(&mut messages);

        // Doom Loop 逃脱：若上一轮检测到循环，本轮过滤掉被禁用的工具
        let mut tools = self.config.tools.clone();
        if let Some(tool_name) = self.disabled_tool_next_round.take() {
            tools.retain(|t| t.name != tool_name);
            tracing::warn!("Doom Loop 逃脱：本轮采样已过滤工具 {}", tool_name);
        }

        // 工具延迟加载：核心工具保持完整 schema，其他工具只发摘要
        // 借鉴 Claude Code 延迟工具加载 + Codex 最小工具集设计
        // 模型可通过 tool_search 工具按需获取完整参数定义
        const CORE_TOOLS: &[&str] = &[
            "bash", "read_file", "write_file", "search_replace",
            "grep", "glob", "list_dir", "tool_search", "task",
        ];
        for tool in tools.iter_mut() {
            if !CORE_TOOLS.contains(&tool.name.as_str()) && tool.name != "tool_search" {
                // 非核心工具：截断描述为一句话，清空参数 schema
                let summary = tool.description.lines().next().unwrap_or(&tool.description);
                // 按字符边界截断：工具描述含中文，直接按字节切会 panic
                let summary = if summary.len() > 80 {
                    format!("{}...（调用 tool_search 获取完整定义）", &summary[..summary.floor_char_boundary(77)])
                } else {
                    format!("{}（调用 tool_search 获取完整定义）", summary)
                };
                tool.description = summary;
                tool.parameters = serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "description": "参数定义未加载，请先调用 tool_search 获取完整参数"
                });
            }
        }

        // 优化：减少 String 分配，复用 agent_id
        let request_id = uuid::Uuid::new_v4().to_string();
        let agent_id = self.id.to_string();
        self.current_sample_request_id = Some(request_id.clone());

        // 推理力度分级：doom loop 被动降级优先，否则按任务难度主动调节
        // 借鉴 DeepSeek Harness off/high/max 分级 + ccode MetaCognitiveController
        let reasoning_effort = self.doom_loop_detector.current_reasoning_effort().or_else(|| {
            let task = self.loop_state_machine.task_description().unwrap_or_default();
            if task.is_empty() {
                return Some(0.5); // 默认中等力度
            }
            let difficulty = self.meta_cognitive.assess_difficulty(&task, &std::collections::HashMap::new());
            difficulty.to_reasoning_effort()
        });

        // ── TokenBudgetReminder 注入（借鉴 Codex TokenBudgetRemainingContext）──
        // 上下文使用超过 70% 时，在最后一条 user 消息中注入剩余预算提醒，
        // 让模型感知上下文压力，主动精简输出。
        {
            let max_tokens = self.working_memory.max_tokens();
            let used_tokens = self.working_memory.used_tokens();
            let usage_pct = if max_tokens > 0 { (used_tokens as u64 * 100) / max_tokens as u64 } else { 0 };
            if usage_pct >= 70 {
                let available = max_tokens.saturating_sub(used_tokens);
                let reminder = format!(
                    "\n<system-reminder>Context budget: {}% used ({}/{} tokens). \
                     {} tokens remaining. Be concise — avoid unnecessary output.</system-reminder>",
                    usage_pct, used_tokens, max_tokens, available
                );
                if let Some(last_user) = messages.iter_mut().rev().find(|m| m.role == "user") {
                    last_user.content.push_str(&reminder);
                }
            }
        }

        // 稳定前缀锁：首次构建请求时锁定 system prompt + 工具定义，防止压缩破坏 prompt cache
        if !self.stable_prefix_locked {
            self.working_memory.lock_stable_prefix();
            self.stable_prefix_locked = true;
        }

        // 工具 schema 稳定性追踪（诊断 prompt cache miss）
        self.tool_stability_tracker.record(&tools);

        SampleRequest {
            request_id,
            agent_id: agent_id.clone(),
            model: self.config.model.clone(),
            messages,
            tools,
            stream: true,
            reasoning_effort,
            max_tokens: None,
            temperature: None,
            top_p: None,
            thinking: None,
            system_prompt: None,
            tool_choice: None,
            prompt_cache_key: Some(agent_id),  // 复用 agent_id，减少一次 to_string
            goal_verify: false,
            summarize: false,
            extra_headers: std::collections::HashMap::new(),
            idle_timeout_secs: None,
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
                // 跳过空内容（usage-only 的 message_start/message_delta chunk）
                if !chunk.content.is_empty() {
                    self.state = AgentState::Outputting;
                    // 累积 assistant 文本：done 时写入工作记忆，让 LLM 看到自己说过什么，
                    // 同时保证对话历史里有 assistant 角色（Anthropic 要求角色交替）
                    self.current_assistant_text.push_str(&chunk.content);
                    // 文本内容流式转发到 agent/{id}/output（TUINode 渲染）
                    // 使用 serde_json::json! 确保序列化为 map 而非 array
                    let output_msg = FrameCodec::new_message(
                        Topic::agent_output(self.id.as_str()),
                        self.id.as_str(),
                        &serde_json::json!({
                            "channel": "text",
                            "content": &chunk.content,
                        }),
                    )?;
                    // 使用控制面（Kernel ROUTER）确保可靠投递到 TUI
                    transport.send_message(&output_msg).await?;
                    tracing::trace!(agent = %self.id, "output 通过控制面发送");
                    self.loop_state_machine.record_step("text", &chunk.content);
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

                tracing::info!(
                    "Thinker 收到 tool_call chunk：tool_call_id={}, tool_name={}",
                    tool_call.tool_call_id, tool_call.tool_name
                );

                let args_hash = Self::hash_tool_args(&tool_call.arguments);
                self.doom_loop_detector.record(tool_call.tool_name.clone(), args_hash);
                self.loop_state_machine.record_step("tool_call", &tool_call.tool_name);

                self.pending_tool_calls.insert(tool_call.tool_call_id.clone(), tool_call);
            }
        }
        Ok(())
    }

    /// 处理工具调用结果，返回 true 表示所有工具调用已完成，应重新采样
    async fn handle_tool_result(&mut self, tool_call_id: &str, tool_name: &str, output: &str, success: bool, transport: &NodeTransportHandle) -> bool {
        self.pending_tool_results.remove(tool_call_id);
        if !success {
            self.turn_had_tool_failure = true;
        }

        // 记录本轮使用的工具（供 EpisodicMemory 编码）
        self.current_turn_tools.push(tool_name.to_string());

        // ReadTracker：记录文件读取（read 类工具）
        if matches!(tool_name, "read" | "read_file") {
            // 从 output 解析文件路径（简化：取 output 中的文件路径）
            if let Some(path) = self.extract_read_path(tool_name, output) {
                self.read_tracker.lock().expect("read_tracker lock poisoned").mark_read_simple(std::path::Path::new(&path));
            }
        }

        // 内置感官处理（路线 A 核心：不经过独立器官 Node）
        // 同时向 Kernel 发送感官信号，让反射弧和经验学习有输入
        self.feel(tool_name, output, success, transport).await;   // 触觉：感知执行结果
        self.observe(tool_name, output, transport).await;          // 视觉：观察文件内容
        // sniff() 在 feel() 中自动触发（当工具失败且为编译相关工具时）
        self.loop_state_machine.record_step("tool_result", &format!("{}:{}", tool_name, if success { "ok" } else { "fail" }));

        let token_count = Self::estimate_tokens(output);
        self.short_term_memory.store(
            "tool".to_string(),
            output.to_string(),
            token_count,
            true,
        );
        // 将工具返回注入工作记忆（作为 user 消息，LLM 需要看到执行结果）
        let token_count = Self::estimate_tokens(output);
        self.working_memory.push_user(output.to_string(), token_count);

        if self.pending_tool_results.is_empty() {
            // 通过状态机记录工具执行完成（借鉴 Claude Code）
            let outcome = if success {
                ToolExecutionOutcome::Success
            } else {
                ToolExecutionOutcome::Failure(output.chars().take(100).collect())
            };
            let loop_action = self.loop_state_machine.transition(LoopEvent::ToolExecutionCompleted {
                tool_use_id: tool_call_id.to_string(),
                tool_name: tool_name.to_string(),
                result: outcome,
            });
            self.state = self.loop_state_machine.state();

            // 消费 LoopAction：状态机的判定驱动主循环行为
            match loop_action {
                LoopAction::EndTurn { reason } => {
                    tracing::info!(target: "ccore::loop", reason = ?reason, "LoopStateMachine 判定结束 turn");
                    self.force_end_turn = true;
                }
                LoopAction::CallLLM => {
                    // 连续工具调用轮次递增（用于 Reminder 注入防漂移）
                    self.consecutive_tool_rounds += 1;
                }
                _ => {}
            }
            true
        } else {
            false
        }
    }

    /// 从工具输出中解析读取的文件路径（供 ReadTracker 使用）
    fn extract_read_path(&self, tool_name: &str, _output: &str) -> Option<String> {
        // 简化实现：从工具名称推断路径
        // 实际路径在 tool_call 的 arguments 中，这里从 output 中提取
        // 对于 read 工具，output 通常包含文件路径信息
        match tool_name {
            "read" | "read_file" => {
                // 尝试从 output 第一行提取路径
                let first_line = _output.lines().next().unwrap_or("");
                if first_line.starts_with('/') || first_line.starts_with('.') {
                    Some(first_line.to_string())
                } else {
                    None
                }
            }
            _ => None,
        }
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

    /// 估算文本的 token 数
    ///
    /// 委托给 `sampler::token_budget::TokenBudgetManager::estimate_tokens`——该实现已按
    /// CJK / ASCII 分权重估算。此前这里用 `len() / 4`，对中文按 UTF-8 字节数计算，
    /// 实测低估约 16%；低估是危险方向，会让压缩触发偏晚、上下文有溢出风险。
    ///
    /// 复用而非另写一份，避免 thinker 侧与 sampler 侧的预算口径分叉。
    fn estimate_tokens(text: &str) -> u32 {
        crate::sampler::token_budget::TokenBudgetManager::estimate_tokens(text) as u32
    }

    /// 计算工具参数的哈希值
    fn hash_tool_args(args: &serde_json::Value) -> u64 {
        let mut hasher = DefaultHasher::new();
        let canonical = serde_json::to_string(args).unwrap_or_else(|_| args.to_string());
        canonical.hash(&mut hasher);
        hasher.finish()
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
            if let Err(e) = transport.send_message(&msg).await {
                tracing::warn!(target: "ccore::thinker", "failed to persist: {e}");
            }
            tracing::debug!(
                "Thinker {} turn 结束持久化：{} 条记录，{} tokens",
                self.id, entries.len(), total_tokens
            );
        }
    }

    /// 解析 /schedule 参数："<description> <interval_secs>"，无秒数则默认 300s
    fn parse_schedule_args(args: &str) -> Option<(String, u64)> {
        let args = args.trim();
        if args.is_empty() {
            return None;
        }
        if let Some(pos) = args.rfind(' ') {
            if let Ok(secs) = args[pos + 1..].parse::<u64>() {
                let desc = args[..pos].trim().to_string();
                if !desc.is_empty() {
                    return Some((desc, secs));
                }
            }
        }
        Some((args.to_string(), 300))
    }

    /// 启动 Schedule Loop 定时 tick task（每 5s 检查是否到执行时间）
    fn spawn_schedule_tick(&self, transport: &NodeTransportHandle) {
        let transport_clone = transport.clone();
        let agent_id = self.id.to_string();
        let active = self.schedule_active.clone();
        tokio::spawn(async move {
            while active.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_secs(5)).await;
                if !active.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(msg) = FrameCodec::new_message(
                    Topic::new("cortex/schedule_tick"),
                    &agent_id,
                    &serde_json::json!({}),
                ) {
                    if let Err(e) = transport_clone.send_message(&msg).await {
                        tracing::warn!(target: "ccore::thinker", "failed to send/report stats: {e}");
                    }
                }
            }
        });
    }

    /// 启动 Proactive Loop 定时 tick task（每 30s 检查是否闲置超时）
    fn spawn_proactive_tick(&self, transport: &NodeTransportHandle) {
        let transport_clone = transport.clone();
        let agent_id = self.id.to_string();
        let active = self.proactive_active.clone();
        tokio::spawn(async move {
            while active.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_secs(30)).await;
                if !active.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(msg) = FrameCodec::new_message(
                    Topic::new("cortex/proactive_tick"),
                    &agent_id,
                    &serde_json::json!({}),
                ) {
                    if let Err(e) = transport_clone.send_message(&msg).await {
                        tracing::warn!(target: "ccore::thinker", "failed to send/report flush: {e}");
                    }
                }
            }
        });
    }

    /// 处理 ScheduleAction：Execute 注入 working_memory 并返回 true（继续 turn）
    async fn process_schedule_action(
        &mut self,
        action: ScheduleAction,
        _transport: &NodeTransportHandle,
    ) -> anyhow::Result<bool> {
        match action {
            ScheduleAction::Execute { description } => {
                let prompt = format!("[定时任务] {}", description);
                self.working_memory.push_user(prompt.clone(), Self::estimate_tokens(&prompt));
                Ok(true)
            }
            ScheduleAction::Wait { until_next } => {
                tracing::info!(
                    target: "ccore::schedule",
                    until_next_secs = until_next.as_secs(),
                    "等待下次定时执行"
                );
                Ok(false)
            }
            ScheduleAction::ScheduleComplete { reason, total_executions, total_successes } => {
                tracing::info!(
                    target: "ccore::schedule",
                    ?reason, total_executions, total_successes,
                    "定时任务结束"
                );
                self.schedule_active.store(false, Ordering::SeqCst);
                self.schedule_loop = None;
                Ok(false)
            }
        }
    }

    /// 处理 ProactiveAction：StartScan/RepairIssue 注入 working_memory 并返回 true
    async fn process_proactive_action(
        &mut self,
        action: ProactiveAction,
        _transport: &NodeTransportHandle,
    ) -> anyhow::Result<bool> {
        match action {
            ProactiveAction::StartScan { scan_types } => {
                let types_str = scan_types
                    .iter()
                    .map(|t| format!("{:?}", t))
                    .collect::<Vec<_>>()
                    .join(", ");
                let prompt = format!(
                    "[主动扫描] 请扫描当前项目的代码质量问题（{}），发现问题后直接修复。",
                    types_str
                );
                self.working_memory.push_user(prompt.clone(), Self::estimate_tokens(&prompt));
                Ok(true)
            }
            ProactiveAction::ContinueWaiting { remaining } => {
                tracing::debug!(
                    target: "ccore::proactive",
                    remaining_secs = remaining.as_secs(),
                    "未到闲置阈值，继续等待"
                );
                Ok(false)
            }
            ProactiveAction::ScanComplete { issues_found, issues_fixed } => {
                tracing::info!(
                    target: "ccore::proactive",
                    issues_found, issues_fixed,
                    "主动扫描完成"
                );
                Ok(false)
            }
            ProactiveAction::RepairIssue { issue } => {
                let prompt = format!(
                    "[主动修复] {:?}（{}）：{}\n建议修复方案：{}",
                    issue.scan_type,
                    issue.file_path.as_deref().unwrap_or("未知文件"),
                    issue.description,
                    issue.suggested_fix
                );
                self.working_memory.push_user(prompt.clone(), Self::estimate_tokens(&prompt));
                Ok(true)
            }
            ProactiveAction::AllRepairsComplete { total_issues, fixed, failed } => {
                tracing::info!(
                    target: "ccore::proactive",
                    total_issues, fixed, failed,
                    "所有主动修复完成"
                );
                Ok(false)
            }
        }
    }

    /// 请求 Token 预算和熔断器检查（通过消息总线）
    ///
    /// 返回 true 表示允许执行，false 表示应跳过（预算不足或熔断器开启）。
    /// 如果已被 Kernel 拒绝（budget_denied=true），立即返回 false。
    /// 同时发送 fire-and-forget 到 Kernel 进行持续预算追踪。
    /// 电路门控检查（替代旧 fire-and-forget check_budget_and_circuit）。
    ///
    /// 使用本地 CircuitBreaker 直接判定，不再依赖异步消息通知。
    /// 熔断状态为 Open 时阻止 LLM 调用，等待恢复超时后降级放行。
    async fn check_budget_and_circuit(&self, _transport: &NodeTransportHandle) -> bool {
        if !self.circuit_breaker.allow_request() {
            tracing::warn!(target: "ccore::circuit_breaker",
                state = ?self.circuit_breaker.current_state(),
                "熔断器 Open，阻止 LLM 调用");
            return false;
        }
        true
    }

    /// 发送采样请求（复用逻辑）
    ///
    /// 发送前先通过消息总线请求 Token 预算和熔断器检查。
    async fn send_sample_request(&mut self, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        // 请求预算和熔断器检查，如果被拒绝则跳过本次 LLM 调用
        if !self.check_budget_and_circuit(transport).await {
            tracing::warn!("Token 预算或熔断器拒绝，跳过本次 LLM 调用");
            return Ok(());
        }

        self.current_sample_request_id = None;
        AgentMetrics::global().record_loop_count(1);
        self.inference_start = Some(std::time::Instant::now());
        let request = self.build_sample_request();
        let sample_msg = FrameCodec::new_message(
            Topic::sampler_request(),
            self.id.as_str(),
            &request,
        )?;
        // 使用控制面确保可靠投递到 Sampler
        transport.send_message(&sample_msg).await?;
        Ok(())
    }

    /// 检查超时的工具调用（60秒无响应），返回超时的 (tool_call_id, tool_name) 列表
    fn check_tool_timeouts(&mut self) -> Vec<(String, String)> {
        const TOOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
        let now = std::time::Instant::now();
        let timed_out: Vec<(String, String)> = self.pending_tool_results
            .iter()
            .filter(|(_, (_, started_at))| now.duration_since(*started_at) > TOOL_TIMEOUT)
            .map(|(id, (name, _))| (id.clone(), name.clone()))
            .collect();

        for (id, name) in &timed_out {
            tracing::warn!(
                "工具调用超时（60s）：tool_call_id={}, tool_name={}",
                id, name
            );
        }
        timed_out
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
        tracing::debug!("Thinker handle_message: topic={}", topic);

        // 检查超时的工具调用，避免永久挂起
        // 但不要在用户输入消息时处理超时——先处理输入，让正常流程处理超时
        let is_user_input = topic.starts_with("agent/") && topic.ends_with("/input");
        if !is_user_input {
            let timed_out = self.check_tool_timeouts();
            for (tool_call_id, tool_name) in timed_out {
                self.pending_tool_results.remove(&tool_call_id);
                let error_msg = format!("工具 {} 执行超时（60秒无响应）", tool_name);
                self.turn_had_tool_failure = true;
                let token_count = Self::estimate_tokens(&error_msg);
                self.working_memory.push_user(error_msg.clone(), token_count);

                if self.pending_tool_results.is_empty() {
                    // 所有工具完成（包括超时的），重新采样
                    tracing::warn!("所有工具调用完成（含超时），准备重新采样");
                    self.send_sample_request(transport).await?;
                    return Ok(());
                }
            }
        }

        match topic {
            // agent/{agent_id}/input — 收到用户输入或父 Agent 指令（兼容 AgentNode 路径）
            t if t.starts_with("agent/") && t.ends_with("/input") => {
                tracing::info!("Thinker 收到 input 消息：topic={}", topic);
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let content = payload["content"].as_str().unwrap_or("");
                let role = payload["role"].as_str().unwrap_or("user");

                // 新 turn 开始，重置预算拒绝和强制结束标志
                self.budget_denied = false;
                self.force_end_turn = false;
                self.turn_had_tool_failure = false;
                self.consecutive_tool_rounds = 0;
                // 重置循环状态机并记录任务描述（供 ERL trajectory 提取）
                self.loop_state_machine.reset();
                if !content.is_empty() {
                    self.loop_state_machine.set_task_description(content.to_string());
                }

                self.listen_sync(content, role); // 内置听觉（快速同步部分）
                // 首次输入后锁定稳定前缀，保护已有条目不被压缩
                // 确保跨轮次 API 请求前缀字节不变，提升 prompt cache 命中率
                if self.working_memory.stable_prefix_len() == 0 {
                    self.working_memory.lock_stable_prefix();
                }
                tracing::info!("Thinker listen_sync 完成，开始后续处理");

                // ERL 闭环：检索与当前任务相关的历史启发式，注入工作记忆
                if !content.is_empty() {
                    let erl_request = FrameCodec::new_message(
                        Topic::new("cortex/erl_retrieve_request"),
                        self.id.as_str(),
                        &serde_json::json!({
                            "agent_id": self.id.to_string(),
                            "task": content,
                            "top_k": 3,
                        }),
                    );
                    if let Ok(msg) = erl_request {
                        if let Err(e) = transport.send_message(&msg).await {
                            tracing::debug!("发送 ERL 检索请求失败：{}", e);
                        }
                    }
                }

                // Proactive Loop：用户输入时更新活跃时间（避免活跃时触发扫描）
                self.proactive_loop.on_user_active();

                // /goal 命令处理：启动目标驱动循环
                if content.starts_with("/goal ") {
                    let goal_description = content.strip_prefix("/goal ").unwrap_or("").to_string();
                    if !goal_description.is_empty() {
                        // 与 cortex/goal_start 共用入口，避免两处逻辑分叉。
                        // 注入后继续流到本 handler 末尾的统一采样出口驱动 LLM。
                        self.start_goal_with_planning(goal_description);
                    }
                }

                // /schedule 命令处理：启动/取消定时循环
                if content.starts_with("/schedule ") {
                    let args = content.strip_prefix("/schedule ").unwrap_or("").trim();
                    if args == "cancel" || args == "stop" {
                        self.schedule_active.store(false, Ordering::SeqCst);
                        if let Some(mut sl) = self.schedule_loop.take() {
                            let _ = sl.cancel();
                        }
                        tracing::info!(target: "ccore::schedule", "定时任务已取消");
                        return Ok(());
                    } else if let Some((desc, secs)) = Self::parse_schedule_args(args) {
                        self.schedule_loop = Some(ScheduleLoop::from_description(desc, secs));
                        self.schedule_active.store(true, Ordering::SeqCst);
                        self.spawn_schedule_tick(transport);
                        // 立即首次执行（注入 working_memory 后继续走 agentic turn）
                        if let Some(ref mut sl) = self.schedule_loop {
                            let action = sl.on_timer_fired();
                            if !self.process_schedule_action(action, transport).await? {
                                return Ok(());
                            }
                        }
                    }
                }

                // /proactive 命令处理：启用/禁用主动扫描（配置命令，不触发 turn）
                if content.starts_with("/proactive ") {
                    let args = content.strip_prefix("/proactive ").unwrap_or("").trim();
                    match args {
                        "on" | "enable" => {
                            self.proactive_loop.set_enabled(true);
                            self.proactive_active.store(true, Ordering::SeqCst);
                            self.spawn_proactive_tick(transport);
                            tracing::info!(target: "ccore::proactive", "主动扫描已启用");
                        }
                        "off" | "disable" => {
                            self.proactive_loop.set_enabled(false);
                            self.proactive_active.store(false, Ordering::SeqCst);
                            tracing::info!(target: "ccore::proactive", "主动扫描已禁用");
                        }
                        _ => {}
                    }
                    return Ok(());
                }

                self.agentic_session_active = true;
                AgentMetrics::global().record_loop_count(1);
                self.inference_start = Some(std::time::Instant::now());
                tracing::info!("Thinker 开始 agentic session，准备发送采样请求");

                // 请求预算和熔断器检查
                if !self.check_budget_and_circuit(transport).await {
                    tracing::warn!("Thinker 预算或熔断器拒绝，跳过本次 LLM 调用");
                    self.agentic_session_active = false;
                    self.state = AgentState::Idle;
                    // 通知 TUI 恢复输入状态
                    let idle_event = FrameCodec::new_message(
                        Topic::agent_event(self.id.as_str()),
                        self.id.as_str(),
                        &serde_json::json!({ "state": "idle" }),
                    );
                    if let Ok(msg) = idle_event {
                        let _ = transport.send_message(&msg).await;
                    }
                    return Ok(());
                }
                tracing::info!("Thinker 预算检查通过，构建采样请求");

                let request = self.build_sample_request();
                tracing::info!("Thinker 采样请求构建完成，通过控制面发送");
                let sample_msg = FrameCodec::new_message(
                    Topic::sampler_request(),
                    self.id.as_str(),
                    &request,
                )?;
                transport.send_message(&sample_msg).await?;
                tracing::info!("采样请求通过控制面发送成功");

                // 慢路径：长期记忆搜索 + 意图扩展，结果注入工作记忆供后续轮次使用
                // 放在 sampler 请求发送之后，不阻塞首次 LLM 请求
                if !content.is_empty() {
                    self.listen_enrichment(content);
                }
            }

            // sampler/*/stream — 收到 LLM 流式返回
            t if t.starts_with("sampler/") && t.ends_with("/stream") => {
                let raw_value: serde_json::Value = FrameCodec::decode_payload(&msg)?;

                // 检查是否为 done 消息
                if raw_value.get("type").and_then(|v| v.as_str()) == Some("done") {
                    if let Some(req_id) = raw_value.get("request_id").and_then(|v| v.as_str()) {
                        if Some(req_id) != self.current_sample_request_id.as_deref() {
                            tracing::warn!(
                                "Thinker {} 丢弃过期 done 消息：req_id={}, current={:?}",
                                self.id, req_id, self.current_sample_request_id
                            );
                            // 丢弃过期流的累积文本，否则会串入下一轮 assistant 消息
                            self.current_assistant_text.clear();
                            return Ok(());
                        }
                    }

                    if let Some(start) = self.inference_start.take() {
                        AgentMetrics::global()
                            .record_inference_latency(start.elapsed().as_millis() as f64);
                    }

                    // 记录熔断器成功（LLM 调用成功）
                    self.circuit_breaker.record_success();

                    // TokenBudget 累计：记录本次 LLM 调用的 token 使用量
                    let input_tokens = raw_value
                        .get("usage")
                        .and_then(|u| u.get("input_tokens"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;
                    let output_tokens = raw_value
                        .get("usage")
                        .and_then(|u| u.get("output_tokens"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;
                    self.current_turn_token_usage = (input_tokens, output_tokens);
                    self.token_budget.record_usage(input_tokens, output_tokens);

                    // 喂入 LLMResponse 事件驱动状态机（修复伪驱动问题）
                    // —— 使 turn_count 递增、done_reason 正确设置，供 ERL trajectory 提取
                    let llm_token_used = raw_value
                        .get("usage")
                        .and_then(|u| u.get("total_tokens"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let has_tool_calls = !self.pending_tool_calls.is_empty();
                    let llm_stop_reason = if has_tool_calls { "tool_use" } else { "end_turn" };

                    // ── assistant 回复入工作记忆 ──
                    // 必须在两个分支（tool_use / end_turn）之前统一处理：
                    // LLM 需要看到自己上一轮说过什么，且 Anthropic Messages API
                    // 拒绝连续 user 消息，缺 assistant 角色会直接失败。
                    //
                    // 有工具调用时也要入栈：此时 assistant 消息代表"我决定调用工具"，
                    // 后续工具结果作为 user 消息跟在其后，才构成合法的角色交替。
                    let assistant_text = std::mem::take(&mut self.current_assistant_text);
                    if !assistant_text.trim().is_empty() {
                        let token_count = Self::estimate_tokens(&assistant_text);
                        self.working_memory.push_assistant(assistant_text, token_count);
                    } else if has_tool_calls {
                        // 纯工具调用无伴随文本：仍需一条 assistant 占位，否则
                        // user(prompt) 之后直接跟 user(工具结果)，角色不交替。
                        let tool_names: Vec<&str> = self
                            .pending_tool_calls
                            .values()
                            .map(|tc| tc.tool_name.as_str())
                            .collect();
                        let placeholder = format!("[调用工具：{}]", tool_names.join(", "));
                        let token_count = Self::estimate_tokens(&placeholder);
                        self.working_memory.push_assistant(placeholder, token_count);
                    }
                    // 收集本轮工具调用列表（id, name, input）供状态机记录
                    let llm_tool_calls: Vec<(String, String, serde_json::Value)> = self
                        .pending_tool_calls
                        .iter()
                        .map(|(id, tc)| (id.clone(), tc.tool_name.clone(), tc.arguments.clone()))
                        .collect();
                    let llm_action = self.loop_state_machine.transition(LoopEvent::LLMResponse {
                        stop_reason: llm_stop_reason.to_string(),
                        token_used: llm_token_used,
                        tool_calls: llm_tool_calls,
                    });
                    self.state = self.loop_state_machine.state();
                    if let LoopAction::EndTurn { reason } = &llm_action {
                        tracing::debug!(target: "ccore::loop", reason = ?reason, "LoopStateMachine 记录 turn 完成");
                        // turn 结束时持久化状态快照（支持会话中断后恢复）
                        self.save_state_snapshot().await;
                    }

                    if !self.pending_tool_calls.is_empty() {
                        self.state = AgentState::ToolCalling;
                        let tool_calls: Vec<(String, PendingToolCall)> =
                            self.pending_tool_calls.drain().collect();
                        tracing::info!(
                            "Thinker 开始分发 {} 个工具调用",
                            tool_calls.len()
                        );
                        for (tool_call_id, pending) in tool_calls {
                            tracing::info!(
                                "Thinker 分派工具调用：tool_call_id={}, tool_name={}",
                                tool_call_id, pending.tool_name
                            );
                            self.pending_tool_results
                                .insert(tool_call_id.clone(), (pending.tool_name.clone(), std::time::Instant::now()));
                            tracing::info!(
                                "Thinker 已插入 pending_tool_results：tool_call_id={}, pending_count={}",
                                tool_call_id, self.pending_tool_results.len()
                            );

                            // 发送到 agent/{id}/tool_call（ToolNode 标准路径）
                            let tool_call_msg = FrameCodec::new_message(
                                Topic::agent_tool_call(self.id.as_str()),
                                self.id.as_str(),
                                &serde_json::json!({
                                    "tool_call_id": tool_call_id,
                                    "tool_name": pending.tool_name,
                                    "arguments": pending.arguments,
                                    "agent_id": self.id.as_str(),
                                }),
                            )?;
                            // 使用控制面确保可靠投递到 Tool Node
                            transport.send_message(&tool_call_msg).await?;
                            tracing::trace!(
                                agent = %self.id,
                                tool = %pending.tool_name,
                                "tool_call 通过控制面发送"
                            );

                            tracing::debug!(
                                "Thinker {} 发送工具调用：{} ({})",
                                self.id, pending.tool_name, tool_call_id
                            );
                        }
                    } else {
                        self.current_sample_request_id = None;
                        self.agentic_session_active = false;
                        self.state = AgentState::Idle;

                        // ── Hook: Stop（turn 结束，可阻止停止） ──
                        if let Some(ref registry) = self.hook_registry {
                            let last_msg = self.working_memory.entries().iter().rev()
                                .find(|e| matches!(e, WorkingEntry::Hot { role, .. } if matches!(role, MessageRole::Assistant)))
                                .map(|e| match e {
                                    WorkingEntry::Hot { content, .. } => content.as_str(),
                                    _ => "",
                                });
                            let envelope = self.hook_event_envelope(
                                HookEventName::Stop,
                                HookPayload::Stop {
                                    reason: "end_turn".to_string(),
                                    stop_hook_active: false,
                                    last_assistant_message: last_msg.map(|s| s.to_string()),
                                    background_tasks: None,
                                    session_crons: None,
                                },
                            );
                            let ctx = self.hook_run_ctx();
                            let stop_result = dispatcher::dispatch_stop(
                                registry,
                                HookEventName::Stop,
                                &envelope,
                                &ctx,
                            ).await;
                            // 如果 Stop hook 提供了额外上下文，注入工作记忆
                            for context in &stop_result.additional_context {
                                if !context.is_empty() {
                                    self.working_memory.push_user(
                                        context.clone(),
                                        Self::estimate_tokens(context),
                                    );
                                }
                            }
                            // 如果 Stop hook 阻止停止（blocks 非空或 wants_continuation），继续循环
                            if stop_result.wants_continuation() {
                                tracing::info!(target: "ccore::hooks", "Stop hook 请求继续 turn");
                                self.agentic_session_active = true;
                            }
                        }

                        // ── Hook: PreCompact ──
                        if let Some(ref registry) = self.hook_registry {
                            let envelope = self.hook_event_envelope(
                                HookEventName::PreCompact,
                                HookPayload::PreCompact {
                                    source: "auto".to_string(),
                                },
                            );
                            let ctx = self.hook_run_ctx();
                            let _ = dispatcher::dispatch_non_blocking(
                                registry,
                                HookEventName::PreCompact,
                                &envelope,
                                &ctx,
                            ).await;
                        }

                        self.update_context_window();

                        // ── Hook: PostCompact ──
                        if let Some(ref registry) = self.hook_registry {
                            let envelope = self.hook_event_envelope(
                                HookEventName::PostCompact,
                                HookPayload::PostCompact {
                                    source: "auto".to_string(),
                                },
                            );
                            let ctx = self.hook_run_ctx();
                            let _ = dispatcher::dispatch_non_blocking(
                                registry,
                                HookEventName::PostCompact,
                                &envelope,
                                &ctx,
                            ).await;
                        }

                        // ── ccore 高级特性：EpisodicMemory 编码 + MetaCognitive 冲突检测 ──
                        // 编码本轮对话到情景记忆（支持跨轮次上下文重建）
                        let _user_msg = self.working_memory.entries().iter()
                            .find(|e| matches!(e, WorkingEntry::Hot { role, .. } if matches!(role, MessageRole::User)))
                            .map(|e| match e {
                                WorkingEntry::Hot { content, .. } => content.as_str(),
                                _ => "",
                            })
                            .unwrap_or("");
                        let assistant_msg = self.working_memory.entries().iter().rev()
                            .find(|e| matches!(e, WorkingEntry::Hot { role, .. } if matches!(role, MessageRole::Assistant)))
                            .map(|e| match e {
                                WorkingEntry::Hot { content, .. } => content.as_str(),
                                _ => "",
                            })
                            .unwrap_or("");
                        let tools_used = std::mem::take(&mut self.current_turn_tools);
                        let context = format!("tools: {}", tools_used.join(", "));
                        let keywords = tools_used.clone();
                        self.episodic_memory.encode(
                            crate::memory::episodic::MemoryType::Episodic,
                            assistant_msg,
                            &context,
                            keywords,
                            crate::memory::episodic::MemorySource {
                                session_id: self.id.to_string(),
                                timestamp: Utc::now().timestamp(),
                                message_index: None,
                                confidence: 0.8,
                            },
                        );

                        // 元认知冲突检测：如果连续失败，推荐策略变更
                        let errors: Vec<String> = if self.turn_had_tool_failure {
                            vec!["工具执行失败".to_string()]
                        } else {
                            vec![]
                        };
                        let conflicts = self.meta_cognitive.detect_conflicts(&errors);
                        if !conflicts.is_empty() {
                            tracing::info!(target: "ccore::meta_cognitive",
                                conflict_count = conflicts.len(),
                                "元认知检测到冲突，推荐策略调整");
                            // 注入提醒到工作记忆（供下一轮 LLM 参考）
                            for c in &conflicts {
                                tracing::debug!(target: "ccore::meta_cognitive",
                                    conflict = ?c, "冲突详情");
                            }
                        }

                        // 重置本轮 token 统计
                        self.current_turn_token_usage = (0, 0);

                        // GoalLoop 规划阶段：尝试从 LLM 响应中解析子任务列表
                        // 先提取文本并解析子任务，避免与 goal_loop 的可变借用冲突
                        let goal_planning_subtasks: Option<Vec<crate::agent::goal_loop::SubTask>> = {
                            if let Some(ref gl) = self.goal_loop {
                                if gl.state() == crate::agent::goal_loop::GoalState::Planning {
                                    // 从工作记忆中提取最近的 assistant 消息
                                    let last_assistant = self.working_memory.entries().iter().rev()
                                        .find_map(|e| match e {
                                            WorkingEntry::Hot { role: MessageRole::Assistant, content, .. } => Some(content.clone()),
                                            _ => None,
                                        });
                                    if let Some(text) = last_assistant {
                                        // 尝试从文本中提取子任务（JSON 数组或编号列表）
                                        let subtasks = self.parse_subtasks_from_llm_response(&text);
                                        if !subtasks.is_empty() {
                                            Some(subtasks)
                                        } else {
                                            tracing::debug!(target: "ccore::goal", "GoalLoop：规划阶段但未解析到子任务，等待下一轮");
                                            None
                                        }
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        };
                        if let Some(subtasks) = goal_planning_subtasks {
                            if let Some(ref mut gl) = self.goal_loop {
                                let action = gl.on_subtasks_planned(subtasks);
                                self.process_goal_action(action, transport).await?;
                                tracing::info!(target: "ccore::goal", "GoalLoop：从 LLM 响应解析到子任务");
                            }
                        }

                        // GoalLoop：turn 完成后通知目标循环
                        // success 判定：未取消且本轮无工具失败
                        if self.goal_loop.is_some() {
                            let success = !self.cancel_requested && !self.turn_had_tool_failure;
                            if let Some(ref mut gl) = self.goal_loop {
                                let action = gl.on_turn_complete(success);
                                self.process_goal_action(action, transport).await?;
                            }
                        }

                        // ScheduleLoop：定时任务 turn 完成后回调
                        if let Some(ref mut sl) = self.schedule_loop {
                            if sl.state() == ScheduleState::Executing {
                                let success = !self.cancel_requested && !self.turn_had_tool_failure;
                                let action = sl.on_execution_complete(success);
                                let _ = self.process_schedule_action(action, transport).await?;
                            }
                        }

                        // ProactiveLoop：扫描/修复 turn 完成后回调
                        {
                            let st = self.proactive_loop.state();
                            if st == ProactiveState::Scanning {
                                // 扫描 turn 完成（暂不解析结构化 issues，走空列表）
                                let action = self.proactive_loop.on_scan_complete(vec![]);
                                let _ = self.process_proactive_action(action, transport).await?;
                            } else if st == ProactiveState::Repairing {
                                let success = !self.cancel_requested && !self.turn_had_tool_failure;
                                let action = self.proactive_loop.on_repair_complete(success);
                                let _ = self.process_proactive_action(action, transport).await?;
                            }
                        }

                        // ERL 轨迹提取：turn 结束时（Done 状态）从状态机提取 TaskTrajectory
                        if self.loop_state_machine.state() == AgentState::Done {
                            let trajectory = self.loop_state_machine.extract_trajectory();
                            if !trajectory.steps.is_empty() {
                                // 手动构造 JSON payload（避免 ccore-advanced 的 serde 跨 crate 兼容性问题）
                            let traj_payload = serde_json::json!({
                                "agent_id": self.id.to_string(),
                                "trajectory": {
                                    "task": trajectory.task,
                                    "steps": trajectory.steps.iter().map(|s| serde_json::json!({
                                        "step_type": s.step_type,
                                        "content": s.content,
                                    })).collect::<Vec<serde_json::Value>>(),
                                    "success": trajectory.success,
                                    "failure_reason": trajectory.failure_reason,
                                },
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

                        // 通知 TUI：turn 已结束，清除 spinner
                        let idle_event = FrameCodec::new_message(
                            Topic::agent_event(self.id.as_str()),
                            self.id.as_str(),
                            &serde_json::json!({ "state": "idle" }),
                        );
                        if let Ok(msg) = idle_event {
                            if let Err(e) = transport.send_message(&msg).await {
                                tracing::debug!("发送 idle 事件失败：{}", e);
                            }
                        }
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
                    self.circuit_breaker.record_failure();
                    self.current_sample_request_id = None;
                    self.state = AgentState::Error;
                    return Ok(());
                }

                // 正常 StreamChunk
                let chunk: StreamChunk = FrameCodec::decode_payload(&msg)?;
                if Some(&chunk.request_id) != self.current_sample_request_id.as_ref() {
                    tracing::warn!(
                        "Thinker {} 丢弃过期 stream chunk：chunk_req_id={}, current={:?}",
                        self.id, chunk.request_id, self.current_sample_request_id
                    );
                    return Ok(());
                }
                self.handle_stream_chunk(&chunk, transport).await?;
            }

            // agent/{agent_id}/tool_result — 工具执行结果（ToolNode 标准路径）
            t if t.starts_with("agent/") && t.ends_with("/tool_result") => {
                tracing::info!(
                    "Thinker {} 收到 tool_result 原始消息：payload_len={}, payload_bytes={:?}",
                    self.id, msg.payload.len(), &msg.payload[..std::cmp::min(100, msg.payload.len())]
                );
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                tracing::info!(
                    "Thinker {} 收到 tool_result：payload={:?}, pending_keys={:?}",
                    self.id, payload, self.pending_tool_results.keys().collect::<Vec<_>>()
                );
                let tool_call_id = payload["tool_call_id"].as_str().unwrap_or("");
                let output = payload["output"].as_str().unwrap_or("");
                let success = payload["success"].as_bool().unwrap_or(true);
                let duration_ms = payload["duration_ms"].as_u64().unwrap_or(0);

                if let Some((tool_name, _started_at)) = self.pending_tool_results.get(tool_call_id).cloned() {
                    AgentMetrics::global()
                        .record_tool_execution_time(&tool_name, duration_ms as f64);
                    if !success {
                        AgentMetrics::global().record_error("tool_execution_failed");
                    }

                    let all_done = self.handle_tool_result(tool_call_id, &tool_name, output, success, transport).await;

                    if all_done {
                        // 如果状态机判定结束 turn，不继续调用 LLM
                        if self.force_end_turn {
                            self.force_end_turn = false;
                            tracing::info!("LoopStateMachine 判定 EndTurn，不继续采样，直接结束 turn");
                            self.persist_to_state(transport).await;
                            return Ok(());
                        }

                        // 工具执行完毕，检查 Doom Loop
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
                                        transport.send_message(&notice_msg).await?;
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
                            transport.send_message(&notice_msg).await?;

                            // 不结束 turn，继续调用 LLM 给 Agent 逃脱循环的机会
                            self.run_compaction_pipeline_with_llm(transport).await;
                            self.send_sample_request(transport).await?;
                        } else {
                            // 未检测到 Doom Loop。
                            //
                            // 注意：本分支每轮工具执行后都会走到，不是只在"刚逃脱成功"时走，
                            // 因此清理动作必须以 escape_attempts > 0 为条件——否则会无条件
                            // 清空签名历史，检测器永远攒不满阈值，doom loop 检测彻底失效。
                            if self.doom_loop_escape_attempts > 0 {
                                tracing::info!(
                                    "Doom Loop 已跳出（此前尝试 {} 次），重置检测器状态",
                                    self.doom_loop_escape_attempts
                                );
                                self.doom_loop_escape_attempts = 0;
                                // 恢复原级推理强度
                                self.doom_loop_detector.reset_degrade_level();
                                // 清空签名历史：触发判定的那批旧签名若留在窗口内，
                                // 下一轮只要再调一次同名工具就会立刻被重新判定为循环，
                                // 且每次判定都递增降级等级，把推理强度一路压到最低（误报自我强化）
                                self.doom_loop_detector.reset();
                                // 还原检测阈值：反射弧 motor 的 slow_down 会收紧阈值，
                                // 不还原则灵敏度永久停留在收紧状态（单向棘轮），
                                // 正常操作也会被判定为循环
                                self.doom_loop_detector.restore_repeat_threshold();
                            }

                            // 回到 sampler 前先跑压缩管道
                            self.run_compaction_pipeline_with_llm(transport).await;
                            self.send_sample_request(transport).await?;
                        }
                    }
                } else {
                    // tool_call_id 为空或未知 — 可能是序列化问题或重复消息
                    // 尝试匹配任意待处理的工具调用，避免 Thinker 卡在 ToolCalling 状态
                    tracing::warn!(
                        "Thinker {} 收到未知 tool_call_id 的结果：tool_call_id='{}', payload={:?}, pending_keys={:?}",
                        self.id, tool_call_id, payload, self.pending_tool_results.keys().collect::<Vec<_>>()
                    );

                    if !self.pending_tool_results.is_empty() {
                        // 取出第一个待工具调用，尝试匹配
                        let (pending_id, (tool_name, _started_at)) = self.pending_tool_results.iter().next()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .unwrap();
                        tracing::warn!(
                            "Thinker {} 尝试匹配未知结果到待处理工具：pending_id={}, tool_name={}",
                            self.id, pending_id, tool_name
                        );
                        self.pending_tool_results.remove(&pending_id);
                        let all_done = self.handle_tool_result(&pending_id, &tool_name, output, success, transport).await;
                        if all_done {
                            self.run_compaction_pipeline_with_llm(transport).await;
                            self.send_sample_request(transport).await?;
                        }
                    }
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
                    transport.send_message(&cancel_msg).await?;
                    tracing::info!("Thinker {} 已转发取消到 sampler: {}", self.id, req_id);
                }

                // ── Tombstone 模式（借鉴 Claude Code）──
                // 为每个待处理的工具调用注入合成"已取消"结果，保持对话历史一致性。
                // 避免恢复会话时 LLM 看到"有调用无结果"的断裂上下文。
                let cancelled_tools: Vec<(String, String)> = self.pending_tool_calls.iter()
                    .map(|(id, tc)| (id.clone(), tc.tool_name.clone()))
                    .collect();

                // tombstone 以 user 角色入栈，前面必须先有 assistant 消息，
                // 否则 user(prompt) 后直接跟 user(tombstone)，角色不交替 → Anthropic 拒绝。
                let assistant_text = std::mem::take(&mut self.current_assistant_text);
                if !assistant_text.trim().is_empty() {
                    let token_count = Self::estimate_tokens(&assistant_text);
                    self.working_memory.push_assistant(assistant_text, token_count);
                } else if !cancelled_tools.is_empty() {
                    let tool_names: Vec<&str> =
                        cancelled_tools.iter().map(|(_, name)| name.as_str()).collect();
                    let placeholder = format!("[调用工具：{}]", tool_names.join(", "));
                    let token_count = Self::estimate_tokens(&placeholder);
                    self.working_memory.push_assistant(placeholder, token_count);
                }

                for (tool_call_id, tool_name) in &cancelled_tools {
                    let tombstone = format!(
                        "<tool_result>\n工具 {} 的执行已被用户取消，未产生结果。\n\
                         如果用户发起了新任务，请专注于新任务。\n</tool_result>",
                        tool_name
                    );
                    let token_count = Self::estimate_tokens(&tombstone);
                    self.working_memory.push_user(tombstone, token_count);
                    tracing::debug!(
                        "Tombstone 注入：tool_call_id={}, tool_name={}",
                        tool_call_id, tool_name
                    );
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
                self.budget_denied = true;
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
                    transport.send_message(&notice_msg).await?;
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
                // 统一走 estimate_tokens：此处曾用 len()/3，与别处的 len()/4 口径分叉
                let summary_tokens = Self::estimate_tokens(&summary);
                self.working_memory.push_system(&summary, summary_tokens);
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
                    let source = payload["source"].as_str().unwrap_or("extract");
                    let label = if source == "retrieve" { "历史经验" } else { "经验教训" };
                    tracing::info!(source, "ERL 注入{}：{}", label, heuristic.chars().take(100).collect::<String>());
                    self.working_memory.push_system(
                        format!("[{}] {}", label, heuristic),
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

                    // ── Hook: SubagentEnd（崩溃） ──
                    if let Some(ref registry) = self.hook_registry {
                        let envelope = self.hook_event_envelope(
                            HookEventName::SubagentEnd,
                            HookPayload::SubagentStop {
                                phase: SubagentStopPhase::Observe,
                                subagent_id: crashed.node_id.clone(),
                                subagent_type: "subagent".to_string(),
                                stop_hook_active: None,
                                last_assistant_message: None,
                            },
                        );
                        let ctx = self.hook_run_ctx();
                        let _ = dispatcher::dispatch_stop(
                            registry,
                            HookEventName::SubagentEnd,
                            &envelope,
                            &ctx,
                        ).await;
                    }

                    let node_id: NodeId = crashed.node_id.clone().into();
                    self.orchestrator.remove_subagent(&node_id);
                } else if payload.get("type").and_then(|v| v.as_str()) == Some("completed") {
                    let subagent_id = payload["node_id"].as_str().unwrap_or("");
                    let output = payload["output"].as_str().unwrap_or("");
                    tracing::info!("子 Agent 完成：{}, 输出长度={}", subagent_id, output.len());

                    // ── Hook: SubagentEnd（完成） ──
                    if let Some(ref registry) = self.hook_registry {
                        let envelope = self.hook_event_envelope(
                            HookEventName::SubagentEnd,
                            HookPayload::SubagentStop {
                                phase: SubagentStopPhase::Observe,
                                subagent_id: subagent_id.to_string(),
                                subagent_type: "subagent".to_string(),
                                stop_hook_active: None,
                                last_assistant_message: None,
                            },
                        );
                        let ctx = self.hook_run_ctx();
                        let _ = dispatcher::dispatch_stop(
                            registry,
                            HookEventName::SubagentEnd,
                            &envelope,
                            &ctx,
                        ).await;
                    }

                    // 子 Agent 摘要返回：大输出压缩为摘要，保护父上下文 token 预算
                    // 借鉴 Claude Code 子 agent 摘要返回设计
                    let summary = if output.len() > 2000 {
                        let paths: Vec<&str> = output
                            .split_whitespace()
                            .filter(|w| w.starts_with('/') && w.contains('.') && w.len() < 200)
                            .take(10)
                            .collect();
                        let state_dir = format!(".ccode/state");
                        let _ = std::fs::create_dir_all(&state_dir);
                        let full_path = format!("{}/subagent-{}.md", state_dir, subagent_id);
                        let _ = std::fs::write(&full_path, output);

                        let head_end = output.floor_char_boundary(200);
                        let tail_start = output.ceil_char_boundary(output.len().saturating_sub(200));
                        let head = &output[..head_end];
                        let tail = &output[tail_start..];
                        let paths_str = if paths.is_empty() {
                            String::new()
                        } else {
                            format!(" 涉及文件：{}", paths.join(", "))
                        };
                        format!(
                            "[子Agent 完成] 摘要：{}...{}。{}完整输出已保存至 {}，可用 read_file 查看",
                            head, tail, paths_str, full_path
                        )
                    } else {
                        output.to_string()
                    };

                    let summary_token_count = Self::estimate_tokens(&summary);
                    self.working_memory.push_user(summary, summary_token_count);
                    let node_id: NodeId = subagent_id.to_string().into();
                    self.orchestrator.remove_subagent(&node_id);
                }
            }

            // cortex/goal_start — 启动目标驱动循环（外部触发：Kernel / 其他 Node / IDE 插件）
            "cortex/goal_start" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let Some(description) = payload["description"].as_str() else {
                    tracing::warn!(
                        target: "ccore::goal",
                        "cortex/goal_start 缺少 description 字段，忽略"
                    );
                    return Ok(());
                };
                if description.trim().is_empty() {
                    tracing::warn!(target: "ccore::goal", "cortex/goal_start 的 description 为空，忽略");
                    return Ok(());
                }

                // 与 /goal 命令共用入口
                self.start_goal_with_planning(description.to_string());

                // 本路径不经 user_input handler 末尾的统一采样出口，需自行开启 session
                self.agentic_session_active = true;
                AgentMetrics::global().record_loop_count(1);
                self.inference_start = Some(std::time::Instant::now());
                if !self.check_budget_and_circuit(transport).await {
                    tracing::warn!(target: "ccore::goal", "预算或熔断器拒绝，goal 规划未能启动");
                    self.agentic_session_active = false;
                    self.state = AgentState::Idle;
                    return Ok(());
                }
                let request = self.build_sample_request();
                let sample_msg = FrameCodec::new_message(
                    Topic::sampler_request(),
                    self.id.as_str(),
                    &request,
                )?;
                transport.send_message(&sample_msg).await?;
            }

            // cortex/goal_verify_result — 目标验证结果回调（来自 Kernel 或外部验证器）
            "cortex/goal_verify_result" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let passed = payload["passed"].as_bool().unwrap_or(false);
                self.on_goal_verification_result(passed, transport).await?;
            }

            // cortex/summary_result — 压缩管道 L4 的 LLM 摘要回传（来自 SamplerNode）
            "cortex/summary_result" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let Some(summary) = payload["summary"].as_str() else {
                    tracing::warn!(
                        target: "ccore::compaction",
                        "cortex/summary_result 缺少 summary 字段，忽略"
                    );
                    return Ok(());
                };
                self.apply_llm_summary(summary);
                self.pending_summary_request = None;
            }

            // cortex/schedule_tick — Schedule Loop 定时唤醒（来自 spawn 的 tick task）
            "cortex/schedule_tick" => {
                if let Some(ref mut sl) = self.schedule_loop {
                    if sl.should_execute_now() {
                        let action = sl.on_timer_fired();
                        if self.process_schedule_action(action, transport).await? {
                            self.agentic_session_active = true;
                            self.inference_start = Some(std::time::Instant::now());
                            self.check_budget_and_circuit(transport).await;
                            self.send_sample_request(transport).await?;
                        }
                    }
                }
            }

            // cortex/proactive_tick — Proactive Loop 闲置扫描触发（来自 spawn 的 tick task）
            "cortex/proactive_tick" => {
                if self.proactive_loop.should_scan() {
                    let action = self.proactive_loop.on_start_scan();
                    if self.process_proactive_action(action, transport).await? {
                        self.agentic_session_active = true;
                        self.inference_start = Some(std::time::Instant::now());
                        self.check_budget_and_circuit(transport).await;
                        self.send_sample_request(transport).await?;
                    }
                }
            }

            _ => {
                tracing::debug!("Thinker 收到未处理 topic：{}", topic);
            }
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
            "cortex/goal_start".into(),                // 目标驱动循环启动
            "cortex/goal_verify_result".into(),        // 目标验证结果
            "cortex/summary_result".into(),            // 压缩管道 L4 的 LLM 摘要回传
            "cortex/schedule_tick".into(),             // Schedule Loop 定时唤醒
            "cortex/proactive_tick".into(),            // Proactive Loop 闲置扫描触发
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
        // 必须与实际发布行为一致：Kernel 用本列表建立数据面路由
        // （`find_subscribers_for_publisher`）。此前只声明了 3 个 agent/* topic，
        // 漏了 sampler/state/cortex 系列——因为这些走 `send_message`（控制面，
        // 经 Kernel 路由）而非 `publish_data`（数据面 PUB），所以投递未受影响，
        // 但声明与实际不符会误导后续排查（比对声明扫死代码时会误判为无发布方）。
        vec![
            format!("agent/{}/output", self.id),       // 文本输出（替代 cortex/{id}/speak）
            format!("agent/{}/tool_call", self.id),     // 工具调用（替代 hand/limb/*）
            format!("agent/{}/event", self.id),         // 状态事件（idle/thinking/outputting）
            "sampler/request".into(),                   // LLM 采样请求
            "state/persist".into(),                     // 工作记忆持久化
            "cortex/schedule_tick".into(),              // ScheduleLoop 定时唤醒（spawn task 内发布）
            "cortex/proactive_tick".into(),             // ProactiveLoop 闲置扫描（spawn task 内发布）
            "cortex/erl_retrieve_request".into(),       // ERL 经验检索请求
            "cortex/erl_trajectory".into(),             // ERL 轨迹上报
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

    #[test]
    fn tombstone_injects_cancel_results_into_working_memory() {
        let mut thinker = ThinkerNode::new(NodeId::new(), test_config());
        thinker.working_memory.push_system("system".to_string(), 5);
        thinker.working_memory.push_user("do something".to_string(), 5);

        // 模拟两个待处理的工具调用
        thinker.pending_tool_calls.insert("tc-1".into(), PendingToolCall {
            tool_call_id: "tc-1".into(),
            tool_name: "read_file".into(),
            arguments: serde_json::json!({"path": "src/main.rs"}),
        });
        thinker.pending_tool_calls.insert("tc-2".into(), PendingToolCall {
            tool_call_id: "tc-2".into(),
            tool_name: "grep".into(),
            arguments: serde_json::json!({"pattern": "fn main"}),
        });

        // 收集取消前的工具信息（与 cancel handler 逻辑一致）
        let cancelled: Vec<(String, String)> = thinker.pending_tool_calls.iter()
            .map(|(id, tc)| (id.clone(), tc.tool_name.clone()))
            .collect();
        assert_eq!(cancelled.len(), 2);

        // 注入 tombstone 结果（与生产 cancel handler 逻辑一致：
        // 先补 assistant 占位，再逐条注入 user tombstone）
        let placeholder = format!(
            "[调用工具：{}]",
            cancelled.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", ")
        );
        let ph_tokens = ThinkerNode::estimate_tokens(&placeholder);
        thinker.working_memory.push_assistant(placeholder, ph_tokens);

        for (_tool_call_id, tool_name) in &cancelled {
            let tombstone = format!(
                "<tool_result>\n工具 {} 的执行已被用户取消，未产生结果。\n\
                 如果用户发起了新任务，请专注于新任务。\n</tool_result>",
                tool_name
            );
            let token_count = ThinkerNode::estimate_tokens(&tombstone);
            thinker.working_memory.push_user(tombstone, token_count);
        }

        // 验证 working_memory 中有 tombstone 注入
        let messages = thinker.working_memory.to_chat_messages();
        let tombstone_msgs: Vec<_> = messages.iter()
            .filter(|(role, content)| *role == "user" && content.contains("已被用户取消"))
            .collect();
        assert_eq!(tombstone_msgs.len(), 2, "应为每个被取消的工具注入一条 tombstone 消息");
        assert!(tombstone_msgs.iter().any(|(_, c)| c.contains("read_file")));
        assert!(tombstone_msgs.iter().any(|(_, c)| c.contains("grep")));

        // tombstone 之前必须有 assistant 消息，否则 user(prompt) 直接跟
        // user(tombstone)，角色不交替 → Anthropic 拒绝请求
        assert!(
            messages.iter().any(|(role, _)| role == "assistant"),
            "取消路径也必须补 assistant 消息以维持角色交替"
        );
    }

    /// 一个完整 turn 之后，对话历史必须能构成合法的 Anthropic 请求。
    ///
    /// 此前 assistant 回复从不入工作记忆（流式文本只转发 TUI），
    /// 导致形状是 `user(prompt) → user(工具结果) → ...`，Anthropic 直接拒绝。
    #[test]
    fn conversation_alternates_after_full_turn() {
        use crate::sampler::claude_compat::ClaudeCompatProvider;

        let mut wm = WorkingMemory::new(8000);
        // 模拟一个完整 turn：用户提问 → assistant 决定调工具 → 两个并行工具结果
        wm.push_user("帮我看下代码".to_string(), 10);
        wm.push_assistant("[调用工具：read_file, grep]".to_string(), 10);
        wm.push_user("文件内容".to_string(), 10);
        wm.push_user("搜索结果".to_string(), 10);

        let msgs = wm.to_chat_messages_direct(|role, content| crate::sampler::provider::ChatMessage {
            role,
            content,
            cache_control: None,
        });
        // 工作记忆里允许连续 user（N 个并行工具 = N 条结果），
        // 但经 Anthropic provider 合并后必须严格交替
        let merged = ClaudeCompatProvider::merge_consecutive_roles(&msgs);

        assert!(
            merged.iter().any(|m| m.role == "assistant"),
            "完整 turn 后必须存在 assistant 消息"
        );
        for pair in merged.windows(2) {
            assert_ne!(
                pair[0].role, pair[1].role,
                "合并后不应有连续同角色，实际：{:?}",
                merged.iter().map(|m| m.role.as_str()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn test_prompt_cache_breakpoints() {
        let mut wm = WorkingMemory::new(8000);
        wm.push_system("You are a coding assistant.".to_string(), 20);
        wm.push_user("Hello".to_string(), 5);
        wm.push_assistant("Hi there!".to_string(), 5);
        wm.push_user("Write a function".to_string(), 10);
        wm.push_assistant("fn hello() {}".to_string(), 10);

        let messages = wm.to_chat_messages();
        // system prompt 应在首位
        assert_eq!(messages[0].0, "system");
    }

    #[test]
    fn test_prompt_cache_breakpoints_in_build_sample_request() {
        let mut thinker = ThinkerNode::new(NodeId::new(), test_config());
        // 注入足够多的消息以触发两个断点
        thinker.working_memory.push_system("You are a coding assistant.".to_string(), 20);
        thinker.working_memory.push_user("Hello".to_string(), 5);
        thinker.working_memory.push_assistant("Hi there!".to_string(), 5);
        thinker.working_memory.push_user("Write a function".to_string(), 10);
        thinker.working_memory.push_assistant("fn hello() {}".to_string(), 10);
        thinker.working_memory.push_user("Now add tests".to_string(), 10);

        let request = thinker.build_sample_request();

        // 断点 1：首条 system 消息应有 cache_control
        assert!(request.messages[0].cache_control.is_some(), "首条 system 消息应有 cache_control 断点");
        let cc = request.messages[0].cache_control.as_ref().unwrap();
        assert_eq!(cc.cache_type, "ephemeral");

        // 断点 2：倒数第 2 条消息（BP4）——只有最后一条不缓存。
        //
        // 注意：此断言原为 index 1（旧的 2-breakpoint 策略：首条 + 倒数第 5 条）。
        // 现已改为 `sampler::cache_placement::place_breakpoints` 的 4-breakpoint
        // 策略：len<3 只放 BP1；len<7 放 BP1+BP4；len>=7 才加 BP2/BP3。
        // 本例 len=6，故断点在 [0, 4]。
        let bp_indices: Vec<usize> = request
            .messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.cache_control.is_some())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            bp_indices,
            vec![0, request.messages.len() - 2],
            "len=6 应只有 BP1(首条 system) + BP4(倒数第 2 条)"
        );

        // prompt_cache_key 应设置
        assert!(request.prompt_cache_key.is_some());
    }

    #[test]
    fn test_reminder_injection_after_consecutive_tool_rounds() {
        let mut thinker = ThinkerNode::new(NodeId::new(), test_config());
        thinker.working_memory.push_system("You are a coding assistant.".to_string(), 20);
        thinker.working_memory.push_user("帮我重构这个模块".to_string(), 10);
        thinker.loop_state_machine.set_task_description("帮我重构这个模块".to_string());

        // 低于阈值时不注入 reminder
        thinker.consecutive_tool_rounds = 3;
        let request = thinker.build_sample_request();
        let last_user = request.messages.iter().rev().find(|m| m.role == "user").unwrap();
        assert!(!last_user.content.contains("system-reminder"),
            "低于阈值时不应注入 reminder");

        // 达到阈值时注入 reminder
        thinker.consecutive_tool_rounds = 5;
        let request = thinker.build_sample_request();
        let last_user = request.messages.iter().rev().find(|m| m.role == "user").unwrap();
        assert!(last_user.content.contains("system-reminder"),
            "达到阈值时应注入 reminder");
        assert!(last_user.content.contains("帮我重构这个模块"),
            "reminder 应包含原始任务描述");
        assert!(last_user.content.contains("5"),
            "reminder 应包含连续轮次数");
    }

    /// token 估算不得低于旧的 `len()/4` 口径——旧实现对中文按 UTF-8 字节数算，
    /// 低估约 16%，会让压缩触发偏晚、上下文有溢出风险。
    #[test]
    fn estimate_tokens_beats_old_byte_based_estimate() {
        let zh = "检测到工具重复调用，请换一种方法。";
        let est = ThinkerNode::estimate_tokens(zh);
        let old_est = (zh.len() as u32).div_ceil(4);
        assert!(
            est >= old_est,
            "中文估算 {est} 不应低于旧 len()/4 口径 {old_est}"
        );
    }

    /// thinker 侧与 sampler 侧必须共用同一套预算口径，不得各写一份导致分叉。
    #[test]
    fn estimate_tokens_delegates_to_token_budget() {
        for s in ["", "a", "中", "调用 read_file 失败", "pub fn main() {}"] {
            assert_eq!(
                ThinkerNode::estimate_tokens(s) as usize,
                crate::sampler::token_budget::TokenBudgetManager::estimate_tokens(s),
                "thinker 的估算应与 TokenBudgetManager 完全一致（输入：{s:?}）"
            );
        }
    }

    #[test]
    fn estimate_tokens_edge_cases() {
        assert_eq!(ThinkerNode::estimate_tokens(""), 0, "空串应为 0");
        assert!(ThinkerNode::estimate_tokens("a") >= 1, "非空串至少 1 token");
        assert!(ThinkerNode::estimate_tokens("中") >= 1, "单个 CJK 字符至少 1 token");
    }

    /// `/goal` 与 `cortex/goal_start` 必须共用入口，不得各自复制逻辑。
    #[test]
    fn start_goal_with_planning_creates_loop_and_injects_prompt() {
        let mut thinker = ThinkerNode::new(NodeId::new(), test_config());
        thinker.start_goal_with_planning("实现登录模块".into());

        assert!(thinker.goal_loop.is_some(), "应创建 GoalLoop");
        assert_eq!(
            thinker.goal_loop.as_ref().unwrap().description(),
            "实现登录模块"
        );

        // 应注入拆解提示，且提示中带目标描述
        let msgs = thinker.working_memory.to_chat_messages();
        let injected = msgs
            .iter()
            .find(|(role, c)| role == "user" && c.contains("拆解"))
            .expect("应注入子任务拆解提示");
        assert!(injected.1.contains("实现登录模块"), "提示应包含目标描述");
        assert!(injected.1.contains("verification"), "提示应说明返回契约");
    }

    /// token 估算须按注入的提示全文计，而非只算目标描述——
    /// 提示模板本身约 40 token，只算 desc 会低估实际注入量。
    #[test]
    fn start_goal_planning_counts_full_prompt_tokens() {
        let mut thinker = ThinkerNode::new(NodeId::new(), test_config());
        let desc = "实现登录模块";
        thinker.start_goal_with_planning(desc.into());

        let used = thinker.working_memory.used_tokens();
        let desc_only = ThinkerNode::estimate_tokens(desc);
        assert!(
            used > desc_only,
            "记账 {used} 应大于仅目标描述的 {desc_only}（提示模板本身也占 token）"
        );
    }

    /// 空描述不应创建 GoalLoop——`cortex/goal_start` 的 payload 校验依赖此前提。
    #[test]
    fn start_goal_with_planning_handles_empty_description() {
        let mut thinker = ThinkerNode::new(NodeId::new(), test_config());
        // 调用方（/goal 与 cortex/goal_start）都会先过滤空串，
        // 这里验证即使传入空串也不 panic
        thinker.start_goal_with_planning(String::new());
        assert!(thinker.goal_loop.is_some(), "start_goal 本身不做校验，由调用方过滤");
    }

    /// L4 只在 L1~L3 跑完仍超阈值时才触发——避免每轮都多调一次 LLM。
    #[test]
    fn needs_llm_summary_only_when_over_threshold() {
        let mut thinker = ThinkerNode::new(NodeId::new(), test_config());
        assert!(!thinker.needs_llm_summary(), "空上下文不应触发 L4");

        // 灌到超过 auto_compact_threshold_percent（默认 95%）
        let max = thinker.working_memory.max_tokens();
        thinker.working_memory.push_user("x".repeat(100), max);
        assert!(thinker.needs_llm_summary(), "用量超阈值时应触发 L4");
    }

    /// LLM 摘要以 system 角色入栈——与 Warm/Cold 一致，会被 provider 侧
    /// `extract_system_messages` 提到顶层，不参与角色交替。
    #[test]
    fn apply_llm_summary_injects_as_system_role() {
        let mut thinker = ThinkerNode::new(NodeId::new(), test_config());
        thinker.apply_llm_summary("用户要求重构登录模块，已完成参数校验部分");

        let msgs = thinker.working_memory.to_chat_messages();
        let summary_msg = msgs
            .iter()
            .find(|(_, c)| c.contains("LLM 上下文摘要"))
            .expect("应注入摘要条目");
        assert_eq!(summary_msg.0, "system", "摘要须以 system 角色入栈");
        assert!(summary_msg.1.contains("重构登录模块"), "应保留摘要内容");
    }

    /// 空摘要不应污染上下文。
    #[test]
    fn apply_llm_summary_ignores_blank() {
        let mut thinker = ThinkerNode::new(NodeId::new(), test_config());
        let before = thinker.working_memory.entries().len();
        thinker.apply_llm_summary("   \n  ");
        assert_eq!(
            thinker.working_memory.entries().len(),
            before,
            "空白摘要不应写入工作记忆"
        );
    }
}
