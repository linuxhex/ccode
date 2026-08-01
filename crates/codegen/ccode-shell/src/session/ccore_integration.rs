//! ccore 统一入口模块
//!
//! # 架构融合：双路径 → 单路径
//!
//! 历史问题：ccode 存在两条 Agent 入口：
//! - **旧路径**：SessionActor 自包含循环（shell 内部 goal/compaction/reminders/subagent 各自实现）
//! - **新路径**：ccore Kernel → ThinkerNode 驱动（消息总线 + LoopStateMachine + 4 层循环工程）
//!
//! 旧路径的问题：shell 自有的 goal/reminders/compaction/subagent 与 ccore 的
//! GoalLoop/ScheduleLoop/WorkingMemory/Orchestrator 重复实现，且不互通。
//! 用户实际运行走旧路径，ccore 的 3000+ 行新能力不可达。
//!
//! 融合方案：通过 `KernelSession` 让 shell 启动 ccore Kernel，
//! ThinkerNode 驱动 Agent 循环，shell 只做 UI 层（显示输出 + 接收输入）。
//!
//! ## 数据流
//!
//! ```text
//! 用户输入 → shell → KernelSession.send_input()
//!   → Kernel ROUTER → ThinkerNode → LoopStateMachine 驱动
//!     → WorkingMemory 4 级压缩 + Stable Prefix
//!     → EpisodicMemory + Context Engine (向量/意图/仓库图)
//!     → GoalLoop / ScheduleLoop / ProactiveLoop
//!     → SamplerNode (LLM) → ThinkerNode → ToolNode → ThinkerNode
//!   → ThinkerNode → agent/{id}/output → AcpNode → KernelSession.on_output()
//!   → shell 显示给用户
//! ```
//!
//! ## 桥接策略
//!
//! | shell 自有实现 | ccore 实现 | 策略 |
//! |---|---|---|
//! | goal.rs + goal_support.rs | GoalLoop | ccore 驱动，shell 透传 |
//! | compaction.rs + segments.rs | WorkingMemory 4 级压缩 | ccore 驱动，shell 不再自压缩 |
//! | reminders.rs | ScheduleLoop | ccore 驱动，shell 透传 |
//! | subagent/mod.rs | Orchestrator + SubAgentNode | ccore 驱动，shell 不再 spawn |
//! | model_switch.rs | SkillInfo.model + ThinkerNode | ccore 驱动 |
//! | memory_state.rs + memory_dream.rs | EpisodicMemory + Context Engine | ccore 驱动 |
//! | MCP (mcp.rs 等) | 无 | **shell 独有**，通过 McpBridge 桥接到消息总线 |
//! | permission 模块 | 5 阶段链式 + 14 项 Shell 安全 | ccore ToolNode 执行 |
//!
//! ## 设计原则
//!
//! - 所有会话级对象通过 CcoreSessionState 一次性创建，避免 new-per-call 丢失状态
//! - CircuitBreaker 为全局共享，熔断时降级等待而非直接报错
//! - ReadTracker 使用 Arc<Mutex> 保证跨线程安全
//! - KernelSession 是 shell 与 ccore 消息总线的唯一接口点

use std::sync::Arc;

// ─── 1. CircuitBreaker Bridge ────────────────────────────────────────────────

use ccore::retry::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig};

/// 全局熔断器，跨所有会话共享，防止 LLM API 级联故障。
static CIRCUIT_BREAKER: std::sync::OnceLock<Arc<CircuitBreaker>> = std::sync::OnceLock::new();

fn circuit_breaker() -> &'static Arc<CircuitBreaker> {
    CIRCUIT_BREAKER.get_or_init(|| Arc::new(CircuitBreaker::new(CircuitBreakerConfig::default())))
}

/// 检查熔断器是否允许请求通过。返回 true 表示正常，false 表示熔断开启。
pub fn check_circuit_breaker() -> bool {
    circuit_breaker().allow_request()
}

/// 记录一次成功的 LLM 调用。
pub fn record_circuit_success() {
    circuit_breaker().record_success();
}

/// 记录一次失败的 LLM 调用。
pub fn record_circuit_failure() {
    circuit_breaker().record_failure();
}

/// 获取当前熔断器状态，用于诊断。
pub fn circuit_breaker_state() -> String {
    format!("{:?}", circuit_breaker().current_state())
}

/// 降级式熔断器等待：若熔断开启则等待冷却期后重试。
///
/// 与 check_circuit_breaker() 不同，此方法不会直接返回 false，
/// 而是短暂等待让熔断器冷却，最多等 `max_wait_ms` 毫秒。
/// 返回 true 表示可以继续，false 表示等待后仍未恢复。
pub async fn wait_for_circuit_recovery(max_wait_ms: u64) -> bool {
    if circuit_breaker().allow_request() {
        return true;
    }
    tracing::warn!("ccore: circuit breaker open, waiting for recovery...");
    let start = std::time::Instant::now();
    let max_wait = std::time::Duration::from_millis(max_wait_ms);
    let check_interval = std::time::Duration::from_millis(500);

    while start.elapsed() < max_wait {
        tokio::time::sleep(check_interval).await;
        if circuit_breaker().allow_request() {
            tracing::info!("ccore: circuit breaker recovered after {:?}", start.elapsed());
            return true;
        }
    }
    tracing::warn!("ccore: circuit breaker still open after {:?}", max_wait);
    false
}

// ─── 2. PromptCacheTracker Bridge ────────────────────────────────────────────

use ccore::sampler::cache_break::{CacheEvent, CacheStats, PromptCacheTracker};

/// 会话级 Prompt 缓存追踪器。
pub struct SessionCacheTracker {
    inner: PromptCacheTracker,
    /// 请求计数器，用于生成 request_id
    request_counter: u64,
}

impl SessionCacheTracker {
    pub fn new() -> Self {
        Self {
            inner: PromptCacheTracker::new(),
            request_counter: 0,
        }
    }

    /// 记录一次 prompt 提交并检查缓存是否可能命中。
    pub fn record_prompt(&mut self, system_prompt: &str, tool_schema: &str, cache_hit: bool) -> bool {
        self.request_counter += 1;
        let sys_hash = PromptCacheTracker::simple_hash(system_prompt);
        let tool_hash = PromptCacheTracker::simple_hash(tool_schema);

        let event = CacheEvent {
            request_id: self.request_counter,
            cache_hit,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            break_reason: None,
            system_prompt_hash: sys_hash,
            tool_schema_hash: tool_hash,
        };

        self.inner.record(event);
        cache_hit
    }

    /// 获取缓存命中率。
    pub fn hit_rate(&self) -> f64 {
        self.inner.hit_rate()
    }

    /// 获取完整缓存统计。
    pub fn stats(&self) -> CacheStats {
        self.inner.stats()
    }
}

// ─── 3. ReadTracker Bridge (Arc<Mutex> 跨线程安全) ──────────────────────────

use ccore::tools::read_tracker::ReadTracker;

/// 会话级读取追踪器，使用 Arc<Mutex> 保证跨线程安全。
///
/// 之前使用 thread_local! 导致跨线程工具调度时读取记录丢失，
/// 改为 Arc<Mutex> 后所有线程共享同一份读取记录。
pub struct SessionReadTracker {
    inner: Arc<std::sync::Mutex<ReadTracker>>,
}

impl SessionReadTracker {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(ReadTracker::new())),
        }
    }

    /// 记录文件已被读取。
    pub fn record_read(&self, path: &str) {
        if let Ok(mut tracker) = self.inner.lock() {
            tracker.mark_read(path);
        }
    }

    /// 检查写入前是否已读取目标文件。
    pub fn has_been_read(&self, path: &str) -> bool {
        self.inner
            .lock()
            .map(|t| t.has_been_read(path))
            .unwrap_or(false)
    }

    /// 清除所有读取记录。
    pub fn clear(&self) {
        if let Ok(mut tracker) = self.inner.lock() {
            tracker.clear();
        }
    }
}

impl Clone for SessionReadTracker {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

// ─── 4. TokenBudget Bridge ───────────────────────────────────────────────────

use ccore::sampler::token_budget::{BudgetStatus, TokenBudgetManager};

/// 会话级 Token 预算管理器。
pub struct SessionTokenBudget {
    inner: TokenBudgetManager,
}

impl SessionTokenBudget {
    /// 根据模型名创建 Token 预算管理器。
    pub fn new(model: &str) -> Self {
        Self {
            inner: TokenBudgetManager::new(model),
        }
    }

    /// 切换模型。
    pub fn switch_model(&mut self, model: &str) {
        self.inner.switch_model(model);
    }

    /// 记录 token 使用量。
    pub fn record_usage(&mut self, input_tokens: usize, output_tokens: usize) {
        self.inner.record_usage(input_tokens, output_tokens);
    }

    /// 获取当前预算状态。
    pub fn status(&self) -> BudgetStatus {
        self.inner.status()
    }

    /// 检查 token 使用是否超过自动压缩阈值。
    pub fn should_compact(&self) -> bool {
        self.inner.status().should_compact
    }

    /// 估算文本的 token 数量。
    pub fn estimate_tokens(text: &str) -> usize {
        TokenBudgetManager::estimate_tokens(text)
    }

    /// 检查是否有足够空间添加新内容。
    pub fn can_fit(&self, additional_tokens: usize) -> bool {
        self.inner.can_fit(additional_tokens)
    }

    /// 压缩后重置 token 使用量。
    pub fn reset_after_compact(&mut self, new_usage: usize) {
        self.inner.reset_after_compact(new_usage);
    }
}

// ─── 5. MetaCognitive Bridge ─────────────────────────────────────────────────

use ccore::agent::meta_cognitive::{
    ConflictDetection, DifficultyLevel, ExecutionStrategy, MetaCognitiveController,
};

/// 会话级元认知控制器。
pub struct SessionMetaCognitive {
    inner: MetaCognitiveController,
}

/// 元认知评估结果。
pub struct MetaAssessment {
    /// 难度评分
    pub difficulty: DifficultyLevel,
    /// 推荐策略
    pub strategy: ExecutionStrategy,
}

impl SessionMetaCognitive {
    pub fn new() -> Self {
        Self {
            inner: MetaCognitiveController::new(),
        }
    }

    /// 评估任务难度并获取策略推荐。
    pub fn assess_and_recommend(&self, task_description: &str) -> MetaAssessment {
        let ctx = std::collections::HashMap::new();
        let difficulty = self.inner.assess_difficulty(task_description, &ctx);
        let strategy = self.inner.select_strategy(&difficulty);
        MetaAssessment {
            difficulty,
            strategy,
        }
    }

    /// 检测计划步骤中的冲突。
    pub fn detect_conflicts(&self, plan_steps: &[String]) -> Vec<ConflictDetection> {
        self.inner.detect_conflicts(plan_steps)
    }

    /// 更新策略效果（闭环学习）。
    pub fn update_strategy_effectiveness(
        &self,
        strategy: &ExecutionStrategy,
        task_success: bool,
    ) {
        self.inner.update_strategy_effectiveness(strategy, task_success);
    }
}

// ─── 6. EpisodicMemory Bridge ────────────────────────────────────────────────

use ccore::memory::episodic::{EpisodicMemoryStore, MemorySource, MemoryType};
use ccore::memory::intent_retriever::{IntentRetriever, RetrievalIntent, RetrievalResult};
use ccore::memory::repo_map::RepoMap;
use ccore::memory::embedding::EmbeddingIndex;
use ccore::memory::function_embed::CodeBlock;
use ccore::kernel::reflex::{ReflexRouter, ReflexAction, ReflexLevel, ReflexRule, builtin_reflex_rules};

/// 会话级情景记忆存储。
pub struct SessionEpisodicMemory {
    inner: EpisodicMemoryStore,
    /// 会话 ID（用于 MemorySource）
    session_id: String,
    /// 消息索引计数器
    message_index: u64,
}

impl SessionEpisodicMemory {
    pub fn new(session_id: &str) -> Self {
        Self {
            inner: EpisodicMemoryStore::new(),
            session_id: session_id.to_string(),
            message_index: 0,
        }
    }

    /// 将对话轮次编码为情景记忆。
    pub fn encode_turn(
        &mut self,
        user_msg: &str,
        assistant_msg: &str,
        tools_used: &[String],
    ) -> String {
        self.message_index += 1;
        let content = format!("用户: {}\n助手: {}", user_msg, assistant_msg);
        let context = format!("使用的工具: {:?}", tools_used);
        let keywords = tools_used.to_vec();
        let source = MemorySource {
            session_id: self.session_id.clone(),
            timestamp: chrono::Utc::now().timestamp(),
            message_index: Some(self.message_index as usize),
            confidence: 0.8,
        };
        self.inner
            .encode(MemoryType::Episodic, &content, &context, keywords, source)
    }

    /// 为相似查询重建上下文。
    pub fn reconstruct_context(&self, query: &str, max_depth: usize) -> String {
        self.inner.reconstruct_context(query, max_depth)
    }

    /// 获取记忆统计信息 (语义数, 情景数, 程序数)。
    pub fn stats(&self) -> (usize, usize, usize) {
        self.inner.stats()
    }
}

// ─── 7. CcoreSessionState —— 一次性创建的会话级状态 ─────────────────────────
//
// 解决 P0 问题：之前每个调用点 new() 导致状态无法跨轮次累积。
// 改为在 SessionActor 初始化时创建一次，所有调用点共享。
// ─────────────────────────────────────────────────────────────────────────────

/// ccore 会话级状态容器。
///
/// 在 SessionActor 创建时一次性构造，所有桥接对象在整个会话生命周期内共享，
/// 确保元认知、情景记忆、缓存追踪等功能可以跨轮次累积状态。
///
/// # 用法
/// ```ignore
/// // 在 SessionActor 构造函数中：
/// let ccore_state = CcoreSessionState::new("session-id", "claude-3.5-sonnet");
///
/// // 在 turn 执行中：
/// ccore_state.episodic.encode_turn(user_msg, assistant_msg, &tools);
/// ccore_state.meta_cognitive.assess_and_recommend(task);
/// ccore_state.read_tracker.record_read(path);
/// ```
pub struct CcoreSessionState {
    /// 会话级情景记忆（跨轮次累积）
    pub episodic: parking_lot::Mutex<SessionEpisodicMemory>,
    /// 会话级元认知控制器（跨轮次学习策略效果）
    pub meta_cognitive: SessionMetaCognitive,
    /// 会话级缓存追踪器（跨轮次统计命中率）
    pub cache_tracker: parking_lot::Mutex<SessionCacheTracker>,
    /// 会话级读取追踪器（跨线程安全）
    pub read_tracker: SessionReadTracker,
    /// 会话级 Token 预算管理器（跨轮次累计使用量）
    pub token_budget: parking_lot::Mutex<SessionTokenBudget>,
    /// 意图检索器（Context Engine：意图扩展 + 代码块检索）
    intent_retriever: parking_lot::Mutex<IntentRetriever>,
    /// 仓库地图（文件依赖图 + 上下文注入）
    repo_map: parking_lot::RwLock<Option<RepoMap>>,
    /// 反射路由器（L0 直接反射 + L1 本能反射 + 经验学习）
    reflex_router: parking_lot::Mutex<ReflexRouter>,
}

impl CcoreSessionState {
    /// 创建会话级 ccore 状态。
    ///
    /// 应在 SessionActor 构造时调用一次，而非每次 turn 时 new()。
    pub fn new(session_id: &str, model: &str) -> Self {
        Self {
            episodic: parking_lot::Mutex::new(SessionEpisodicMemory::new(session_id)),
            meta_cognitive: SessionMetaCognitive::new(),
            cache_tracker: parking_lot::Mutex::new(SessionCacheTracker::new()),
            read_tracker: SessionReadTracker::new(),
            token_budget: parking_lot::Mutex::new(SessionTokenBudget::new(model)),
            intent_retriever: parking_lot::Mutex::new(IntentRetriever::new(EmbeddingIndex::new())),
            repo_map: parking_lot::RwLock::new(None),
            reflex_router: parking_lot::Mutex::new(ReflexRouter::with_rules(builtin_reflex_rules())),
        }
    }

    /// 便捷方法：编码对话轮次到情景记忆。
    pub fn encode_episodic_turn(
        &self,
        user_msg: &str,
        assistant_msg: &str,
        tools_used: &[String],
    ) {
        self.episodic
            .lock()
            .encode_turn(user_msg, assistant_msg, tools_used);
    }

    /// 便捷方法：重建情景上下文。
    pub fn reconstruct_episodic_context(&self, query: &str, max_depth: usize) -> String {
        self.episodic.lock().reconstruct_context(query, max_depth)
    }

    /// 便捷方法：元认知评估。
    pub fn meta_assess(&self, task: &str) -> MetaAssessment {
        self.meta_cognitive.assess_and_recommend(task)
    }

    /// 便捷方法：检测冲突。
    pub fn meta_detect_conflicts(&self, steps: &[String]) -> Vec<ConflictDetection> {
        self.meta_cognitive.detect_conflicts(steps)
    }

    /// 便捷方法：记录文件读取。
    pub fn record_read(&self, path: &str) {
        self.read_tracker.record_read(path);
    }

    /// 便捷方法：检查是否已读取。
    pub fn has_been_read(&self, path: &str) -> bool {
        self.read_tracker.has_been_read(path)
    }

    /// 便捷方法：写前检查。
    pub fn check_write_without_read(&self, tool_name: &str, args: &serde_json::Value) -> Option<String> {
        if !matches!(
            tool_name,
            "write" | "edit" | "search_replace" | "hashline_edit"
        ) {
            return None;
        }

        let path = args
            .get("file_path")
            .or_else(|| args.get("path"))
            .or_else(|| args.get("target_file"))
            .and_then(|v| v.as_str());

        let Some(path) = path else {
            return None;
        };

        if !self.read_tracker.has_been_read(path) {
            Some(format!(
                "Warning: writing to '{}' without reading it first. Consider reading the file before editing.",
                path
            ))
        } else {
            None
        }
    }

    /// 便捷方法：记录 Token 使用量。
    pub fn record_token_usage(&self, input_tokens: usize, output_tokens: usize) {
        self.token_budget.lock().record_usage(input_tokens, output_tokens);
    }

    /// 便捷方法：记录 Prompt 缓存事件。
    pub fn record_cache(&self, system_prompt: &str, tool_schema: &str, cache_hit: bool) {
        self.cache_tracker.lock().record_prompt(system_prompt, tool_schema, cache_hit);
    }

    /// 便捷方法：获取缓存命中率。
    pub fn cache_hit_rate(&self) -> f64 {
        self.cache_tracker.lock().hit_rate()
    }

    /// 便捷方法：意图检索——将用户查询展开为多维检索意图并检索相关代码块。
    ///
    /// 返回检索结果列表，每个结果包含文件路径、符号名、相关度分数和代码预览。
    /// 可用于在 turn 开始时注入上下文，类似 Claude Code 的 repo-map 工具。
    pub fn search_by_intent(&self, query: &str, top_k: usize) -> Vec<RetrievalResult> {
        let intents = IntentRetriever::expand_intents(query);
        let query_embedding: Vec<f32> = vec![0.0; 64]; // 占位：真实场景需调用 embedding 模型
        self.intent_retriever
            .lock()
            .search_by_intents(&intents, &query_embedding, top_k)
    }

    /// 便捷方法：注册代码块到意图检索器。
    ///
    /// 在文件读取后调用，将文件中的函数/类/方法注册到检索索引。
    pub fn register_code_blocks(&self, blocks: Vec<CodeBlock>) {
        self.intent_retriever.lock().register_blocks(blocks);
    }

    /// 便捷方法：初始化仓库地图（首次使用时延迟创建）。
    pub fn init_repo_map(&self, root: std::path::PathBuf) {
        let mut guard = self.repo_map.write();
        if guard.is_none() {
            *guard = Some(RepoMap::new(root));
        }
    }

    /// 便捷方法：获取仓库地图的文件列表摘要。
    pub fn repo_map_summary(&self) -> String {
        let guard = self.repo_map.read();
        match guard.as_ref() {
            Some(_rm) => {
                // RepoMap 文件信息暂不暴露公共接口，返回占位
                "RepoMap 已加载".to_string()
            }
            None => String::new(),
        }
    }

    /// 便捷方法：获取仓库地图中指定文件的依赖。
    pub fn file_dependencies(&self, path: &std::path::Path) -> Vec<std::path::PathBuf> {
        let guard = self.repo_map.read();
        match guard.as_ref() {
            Some(rm) => rm.all_dependencies(path).into_iter().collect(),
            None => Vec::new(),
        }
    }

    /// 便捷方法：检索结果格式化为可注入 system prompt 的文本。
    pub fn format_retrieval_context(results: &[RetrievalResult]) -> String {
        if results.is_empty() {
            return String::new();
        }
        let mut ctx = String::from("[Context Engine] 相关代码块：\n");
        for r in results.iter().take(10) {
            ctx.push_str(&format!(
                "- {} ({}, 相关度: {:.2}): {}\n",
                r.name, r.file_path.display(), r.relevance_score, r.preview
            ));
        }
        ctx
    }

    // ─── 反射弧 ──────────────────────────────────────────────────────────────

    /// 便捷方法：处理工具结果的反射路由。
    ///
    /// 将工具执行结果转化为感官信号，匹配反射规则。
    /// - 文件类工具（read/write/edit）→ sensory/eye 信号
    /// - 编译/检查工具失败 → sensory/nose/compile_error 信号
    /// - 工具执行反馈 → sensory/skin 信号
    ///
    /// 返回匹配的反射动作（如有），供 turn.rs 注入为 system reminder。
    pub fn route_reflex(&self, tool_name: &str, output: &str, success: bool) -> Option<String> {
        let (topic, payload) = Self::classify_tool_signal(tool_name, output, success);
        let action = self.reflex_router.lock().route(&topic, &payload);
        match action {
            Some(ReflexAction::Direct { action, params }) => {
                Some(format!("[Reflex L0] {} → {}", action, params))
            }
            Some(ReflexAction::Instinct { action, params }) => {
                Some(format!("[Reflex L1 本能] {} → {}", action, params))
            }
            Some(ReflexAction::Trial { action, params }) => {
                Some(format!("[Reflex L1 试验] {} → {}", action, params))
            }
            None => None,
        }
    }

    /// 感觉系统：将工具结果分类为感官信号。
    ///
    /// feel（触觉）：感知工具执行成功/失败
    /// sniff（嗅觉）：解析编译错误/代码异味
    /// observe（视觉）：观察文件内容变化
    fn classify_tool_signal(tool_name: &str, output: &str, success: bool) -> (String, String) {
        match tool_name {
            "read" | "glob" | "grep" | "search" => {
                (format!("sensory/eye/{}", tool_name), output.to_string())
            }
            "write" | "edit" | "search_replace" | "hashline_edit" => {
                (format!("sensory/eye/{}", tool_name), output.to_string())
            }
            "bash" | "shell" if !success => {
                // Shell 失败 → 检查是否编译错误
                if output.contains("error[E") || output.contains("error:") {
                    ("sensory/nose/compile_error".to_string(), output.to_string())
                } else {
                    ("sensory/skin/tool_failure".to_string(), output.to_string())
                }
            }
            _ if !success => {
                ("sensory/skin/tool_failure".to_string(), format!("{}: {}", tool_name, output))
            }
            _ => {
                ("sensory/skin/tool_success".to_string(), format!("{}: ok", tool_name))
            }
        }
    }

    /// 便捷方法：添加自定义反射规则（运行时学习）。
    pub fn add_reflex_rule(&self, rule: ReflexRule) {
        self.reflex_router.lock().add_rule(rule);
    }

    /// 便捷方法：记录反射规则执行结果（用于经验学习）。
    pub fn record_reflex_result(&self, rule_id: &str, success: bool) -> bool {
        self.reflex_router.lock().record_result(rule_id, success)
    }
}

// ─── 8. KernelSession —— 已删除 ─────────────────────────────────────────────
//
// KernelSession 曾设计为 shell → ccore Kernel 统一入口，但实际实现为空壳：
// - send_input() 只打日志，不转发消息
// - 缺少真实 LLM HTTP 调用、MCP 工具、权限 UI 交互
// - SessionActor 是事实主线，CcoreSessionState 桥接已验证可行
//
// 收敛决策：保留 SessionActor + CcoreSessionState 桥接，删除 KernelSession。
// SessionActor 直接调用 LLM API，通过 CcoreSessionState 桥接 ccore 高级能力
// （episodic memory、meta cognitive、cache tracker、token budget、read tracker）。
// ─────────────────────────────────────────────────────────────────────────────

// ─── 9. McpBridge —— shell 独有的 MCP 能力桥接到消息总线 ─────────────────────
//
// MCP（Model Context Protocol）是 ccode-shell 独有的能力，
// ccore 没有 MCP 实现。通过 McpBridge 将 MCP 工具暴露到消息总线上，
// 让 ToolNode 可以调用 MCP 工具。
//
// 桥接方式：
// - McpBridge 启动后作为消息总线上的一个虚拟 Node
// - ToolNode 执行 MCP 工具时，发送 mcp/{server}/call 消息
// - McpBridge 收到后调用 MCP Server，返回结果
// ─────────────────────────────────────────────────────────────────────────────

/// MCP 服务器配置
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    /// 服务器名称
    pub name: String,
    /// 传输方式（stdio / sse / streamable-http）
    pub transport: String,
    /// 启动命令（stdio 模式）
    pub command: Option<String>,
    /// 启动参数
    pub args: Vec<String>,
    /// 环境变量
    pub env: std::collections::HashMap<String, String>,
    /// 服务器 URL（sse / streamable-http 模式）
    pub url: Option<String>,
}

/// MCP 桥接：将 shell 的 MCP 能力暴露到消息总线
///
/// McpBridge 不是独立的 ZMQ Node，而是通过 AcpNode 间接接入消息总线：
/// - AcpNode 收到 mcp/{server}/call 消息时，转发给 McpBridge
/// - McpBridge 调用 MCP Server，返回结果
/// - 这样 ToolNode 可以通过消息总线调用 MCP 工具
pub struct McpBridge {
    /// 已连接的 MCP 服务器
    servers: Vec<McpServerConfig>,
    /// MCP 工具注册表（server → tools 映射）
    tool_registry: std::collections::HashMap<String, Vec<McpToolDef>>,
}

/// MCP 工具定义
#[derive(Debug, Clone)]
pub struct McpToolDef {
    /// 工具名称
    pub name: String,
    /// 工具描述
    pub description: String,
    /// 输入 schema (JSON Schema)
    pub input_schema: serde_json::Value,
}

impl McpBridge {
    /// 创建 MCP 桥接
    pub fn new(servers: Vec<McpServerConfig>) -> Self {
        Self {
            servers,
            tool_registry: std::collections::HashMap::new(),
        }
    }

    /// 注册 MCP 服务器提供的工具
    pub fn register_tools(&mut self, server_name: &str, tools: Vec<McpToolDef>) {
        self.tool_registry.insert(server_name.to_string(), tools);
    }

    /// 获取所有 MCP 工具定义（用于 ThinkerNode 的 SampleRequest.tools）
    pub fn all_tools(&self) -> Vec<McpToolDef> {
        self.tool_registry.values().flatten().cloned().collect()
    }

    /// 调用 MCP 工具
    ///
    /// 返回工具执行结果。实际实现通过 MCP 协议与 Server 通信。
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        tracing::info!(server = server_name, tool = tool_name, "调用 MCP 工具");
        // 实际实现通过 MCP 协议（stdio/SSE）与 Server 通信
        // 这里是桥接层的接口定义，具体实现在 shell 侧的 mcp_servers.rs 中
        Err(format!("MCP 工具调用需要 shell 侧 mcp_servers.rs 实现: {}/{}", server_name, tool_name))
    }

    /// 获取已注册的 MCP 服务器列表
    pub fn servers(&self) -> &[McpServerConfig] {
        &self.servers
    }

    /// 检查指定服务器是否已注册
    pub fn has_server(&self, name: &str) -> bool {
        self.servers.iter().any(|s| s.name == name)
    }
}

// ─── 10. 架构收敛决策 ──────────────────────────────────────────────────────
//
// 最终架构：SessionActor 是唯一引擎，CcoreSessionState 桥接 ccore 能力。
//
// | SessionActor 能力 | ccore 桥接 |
// |---|---|
// | goal.rs → GoalTracker | CcoreSessionState.meta_cognitive 评估 |
// | compaction.rs → 自有压缩 | CcoreSessionState.token_budget 预算 |
// | reminders.rs → 自有定时 | 无（shell 自有足够） |
// | subagent/mod.rs → 自有 spawn | 无（shell 自有足够） |
// | model_switch.rs → 自有切换 | CcoreSessionState.cache_tracker |
// | memory_state.rs → 自有记忆 | CcoreSessionState.episodic 记忆 |
// | mcp.rs → 自有 MCP | McpBridge 接口（shell 自有实现） |
// | turn.rs → LoopStateMachine 驱动 | 无（LoopStateMachine 真驱动主循环） |
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_bridge() {
        assert!(check_circuit_breaker());
        record_circuit_success();
        assert!(check_circuit_breaker());
    }

    #[test]
    fn test_cache_tracker_bridge() {
        let mut tracker = SessionCacheTracker::new();
        assert_eq!(tracker.hit_rate(), 0.0);

        tracker.record_prompt("system prompt", "tool schema", true);
        assert_eq!(tracker.hit_rate(), 1.0);

        tracker.record_prompt("system prompt", "tool schema", false);
        assert!((tracker.hit_rate() - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_session_read_tracker_thread_safe() {
        let tracker = SessionReadTracker::new();
        tracker.record_read("/tmp/test_thread_safe.rs");
        assert!(tracker.has_been_read("/tmp/test_thread_safe.rs"));
        assert!(!tracker.has_been_read("/tmp/other.rs"));

        // 跨线程共享
        let tracker_clone = tracker.clone();
        let handle = std::thread::spawn(move || {
            assert!(tracker_clone.has_been_read("/tmp/test_thread_safe.rs"));
            tracker_clone.record_read("/tmp/from_other_thread.rs");
        });
        handle.join().unwrap();
        assert!(tracker.has_been_read("/tmp/from_other_thread.rs"));
    }

    #[test]
    fn test_token_budget_bridge() {
        let mut budget = SessionTokenBudget::new("gpt-4o");
        assert!(!budget.should_compact());

        let tokens = SessionTokenBudget::estimate_tokens("Hello world");
        assert!(tokens > 0);
    }

    #[test]
    fn test_meta_cognitive_bridge() {
        let meta = SessionMetaCognitive::new();
        let assessment = meta.assess_and_recommend("读取文件内容");
        assert_eq!(assessment.difficulty, DifficultyLevel::Trivial);

        let conflicts = meta.detect_conflicts(&[
            "添加新功能".to_string(),
            "删除旧功能".to_string(),
        ]);
        assert!(!conflicts.is_empty());
    }

    #[test]
    fn test_episodic_memory_bridge() {
        let mut memory = SessionEpisodicMemory::new("test_session");
        let id = memory.encode_turn("你好", "你好！", &[]);
        assert!(id.starts_with("mem_"));

        // 编码第二条记忆，验证跨轮次累积
        let id2 = memory.encode_turn("排序算法", "这是快速排序...", &["read_file".to_string()]);
        assert!(id2.starts_with("mem_"));

        let context = memory.reconstruct_context("排序", 2);
        assert!(!context.is_empty());

        let stats = memory.stats();
        assert!(stats.1 >= 2, "应至少有 2 条情景记忆");
    }

    #[test]
    fn test_ccore_session_state_persistence() {
        let state = CcoreSessionState::new("test_session", "gpt-4o");

        // 情景记忆累积
        state.encode_episodic_turn("你好", "你好！", &[]);
        state.encode_episodic_turn("排序算法", "快速排序实现", &["write".to_string()]);
        let ctx = state.reconstruct_episodic_context("排序", 2);
        assert!(!ctx.is_empty());

        // 读取追踪跨操作累积
        state.record_read("/tmp/test_a.rs");
        assert!(state.has_been_read("/tmp/test_a.rs"));
        assert!(!state.has_been_read("/tmp/test_b.rs"));

        // Token 预算累积
        state.record_token_usage(1000, 500);
        state.record_token_usage(2000, 800);
        // 两次调用应被累加

        // 缓存追踪累积
        state.record_cache("sys", "tools", true);
        state.record_cache("sys", "tools", false);
        assert!((state.cache_hit_rate() - 0.5).abs() < 0.01);

        // 元认知累积
        let assessment = state.meta_assess("简单查询");
        assert_eq!(assessment.difficulty, DifficultyLevel::Trivial);
    }

    #[test]
    fn test_check_write_without_read() {
        let state = CcoreSessionState::new("test_session", "gpt-4o");

        let args = serde_json::json!({"file_path": "/tmp/test_write_check.rs"});
        let warning = state.check_write_without_read("write", &args);
        assert!(warning.is_some());

        state.record_read("/tmp/test_write_check.rs");
        let no_warning = state.check_write_without_read("write", &args);
        assert!(no_warning.is_none());

        let no_check = state.check_write_without_read("bash", &args);
        assert!(no_check.is_none());
    }

    #[tokio::test]
    async fn test_circuit_breaker_recovery() {
        // 正常情况应立即返回 true
        assert!(wait_for_circuit_recovery(100).await);
    }
}
