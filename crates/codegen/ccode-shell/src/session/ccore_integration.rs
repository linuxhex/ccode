//! ccore 高级特性桥接模块
//!
//! 将 ccore 的 CircuitBreaker、PromptCacheTracker、ReadTracker、
//! TokenBudgetManager、MetaCognitiveController、EpisodicMemoryStore
//! 以薄适配层的形式接入 ccode-shell 的执行流程。
//!
//! 设计原则：
//! - 所有会话级对象通过 CcoreSessionState 一次性创建，避免 new-per-call 丢失状态
//! - CircuitBreaker 为全局共享，熔断时降级等待而非直接报错
//! - ReadTracker 使用 Arc<Mutex> 保证跨线程安全
//! - 不修改现有大文件，提供独立函数/结构供 shell 关键调用点使用

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

// 保留旧的全局 API 做兼容，内部委托到 thread_local（仅用于无 SessionReadTracker 的场景）
thread_local! {
    static FALLBACK_READ_TRACKER: std::cell::RefCell<ReadTracker> = std::cell::RefCell::new(ReadTracker::new());
}

/// 记录文件已被读取（兼容旧调用，推荐改用 SessionReadTracker）。
pub fn record_file_read(path: &str) {
    FALLBACK_READ_TRACKER.with(|t| t.borrow_mut().mark_read(path));
}

/// 检查写入前是否已读取目标文件（兼容旧调用，推荐改用 SessionReadTracker）。
pub fn check_read_before_write(path: &str) -> bool {
    FALLBACK_READ_TRACKER.with(|t| t.borrow().has_been_read(path))
}

/// 清除读取追踪记录（兼容旧调用）。
pub fn clear_read_tracker() {
    FALLBACK_READ_TRACKER.with(|t| t.borrow_mut().clear());
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
}

// ─── 8. tool_dispatch 集成 ───────────────────────────────────────────────────

/// 预调度钩子：检查 write/edit 工具是否已先读取目标文件。
///
/// 返回 Some(warning) 表示文件未被读取，返回 None 表示正常。
pub fn check_write_without_read(tool_name: &str, args: &serde_json::Value) -> Option<String> {
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

    if !check_read_before_write(path) {
        Some(format!(
            "Warning: writing to '{}' without reading it first. Consider reading the file before editing.",
            path
        ))
    } else {
        None
    }
}

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
        clear_read_tracker();

        let args = serde_json::json!({"file_path": "/tmp/test_write_check.rs"});
        let warning = check_write_without_read("write", &args);
        assert!(warning.is_some());

        record_file_read("/tmp/test_write_check.rs");
        let no_warning = check_write_without_read("write", &args);
        assert!(no_warning.is_none());

        let no_check = check_write_without_read("bash", &args);
        assert!(no_check.is_none());
    }

    #[tokio::test]
    async fn test_circuit_breaker_recovery() {
        // 正常情况应立即返回 true
        assert!(wait_for_circuit_recovery(100).await);
    }
}
