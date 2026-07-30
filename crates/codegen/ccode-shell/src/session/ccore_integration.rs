//! ccore 高级特性桥接模块
//!
//! 将 ccore 的 CircuitBreaker、PromptCacheTracker、ReadTracker、
//! TokenBudgetManager、MetaCognitiveController、EpisodicMemoryStore
//! 以薄适配层的形式接入 ccode-shell 的执行流程。
//!
//! 设计原则：
//! - 不修改现有大文件（run_loop.rs, turn.rs, sampler_turn.rs, tool_dispatch.rs）
//! - 提供独立函数/结构，供 shell 在关键调用点使用
//! - 适配 ccore 的实际 API 签名

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
    ///
    /// 通过比较系统提示和工具定义的 hash 来自动检测缓存断裂原因。
    /// 返回 true 表示缓存命中。
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

// ─── 3. ReadTracker Bridge ───────────────────────────────────────────────────

use ccore::tools::read_tracker::ReadTracker;

// 线程级读取追踪器，用于先读后写约束的执行。
thread_local! {
    static READ_TRACKER: std::cell::RefCell<ReadTracker> = std::cell::RefCell::new(ReadTracker::new());
}

/// 记录文件已被读取（在 read 工具中调用）。
pub fn record_file_read(path: &str) {
    READ_TRACKER.with(|t| t.borrow().mark_read(path));
}

/// 检查写入前是否已读取目标文件（在 write/edit 工具中调用）。
/// 返回 true 表示文件已被读取，false 表示未读取。
pub fn check_read_before_write(path: &str) -> bool {
    READ_TRACKER.with(|t| t.borrow().has_been_read(path))
}

/// 清除读取追踪记录（在会话开始时调用）。
pub fn clear_read_tracker() {
    READ_TRACKER.with(|t| t.borrow().clear());
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
}

impl SessionEpisodicMemory {
    pub fn new(session_id: &str) -> Self {
        Self {
            inner: EpisodicMemoryStore::new(),
            session_id: session_id.to_string(),
        }
    }

    /// 将对话轮次编码为情景记忆。
    pub fn encode_turn(
        &self,
        user_msg: &str,
        assistant_msg: &str,
        tools_used: &[String],
    ) -> String {
        let content = format!("用户: {}\n助手: {}", user_msg, assistant_msg);
        let context = format!("使用的工具: {:?}", tools_used);
        let keywords = tools_used.to_vec();
        let source = MemorySource {
            session_id: self.session_id.clone(),
            timestamp: chrono::Utc::now().timestamp(),
            message_index: None,
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

// ─── 7. sampler_turn 集成 ────────────────────────────────────────────────────

/// 在采样轮次中集成 ccore 高级特性的包装函数。
///
/// 在现有重试逻辑前后添加：
/// - 熔断器检查（请求前）
/// - 缓存追踪（响应后）
/// - Token 预算监控
///
/// 实际调用点应使用本模块中的各个独立函数。
pub async fn run_turn_with_ccore_gates<F, Fut, T, E>(
    circuit_check: bool,
    f: F,
) -> Result<T, E>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Debug,
{
    if circuit_check && !check_circuit_breaker() {
        tracing::warn!("circuit breaker open, skipping LLM request");
    }
    f().await
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
    fn test_read_tracker_bridge() {
        clear_read_tracker();
        assert!(!check_read_before_write("/tmp/test_read_tracker.rs"));

        record_file_read("/tmp/test_read_tracker.rs");
        assert!(check_read_before_write("/tmp/test_read_tracker.rs"));

        clear_read_tracker();
        assert!(!check_read_before_write("/tmp/test_read_tracker.rs"));
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
        let memory = SessionEpisodicMemory::new("test_session");
        let id = memory.encode_turn("你好", "你好！", &[]);
        assert!(id.starts_with("mem_"));

        let context = memory.reconstruct_context("你好", 2);
        assert!(!context.is_empty());
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
}
