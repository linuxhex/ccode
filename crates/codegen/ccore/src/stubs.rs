//! 进阶模块 stub 实现
//!
//! 当 `advanced` feature 关闭时（main 分支），
//! 这些 stub 提供与 ccore-advanced 相同的公共 API，
//! 但实现为空操作，确保核心代码编译通过且功能优雅降级。
//!
//! 类型定义与 ccore-advanced 中的真实定义完全一致。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// agent::experiential stubs
// ============================================================================

pub mod experiential {
    use super::*;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Heuristic {
        pub id: String,
        pub content: String,
        pub source_task_type: String,
        pub from_success: bool,
        pub applies_to: String,
        pub relevance_score: f64,
        pub usage_count: u64,
        pub effectiveness: f64,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
    pub enum ReflectionPersona {
        Verifier,
        Planner,
        Skeptic,
        Logician,
        MetaReflector,
    }

    impl ReflectionPersona {
        pub fn prompt_template(&self) -> &str {
            match self {
                Self::Verifier => "你是一个验证者(Verifier)。",
                Self::Planner => "你是一个规划者(Planner)。",
                Self::Skeptic => "你是一个怀疑者(Skeptic)。",
                Self::Logician => "你是一个逻辑者(Logician)。",
                Self::MetaReflector => "你是一个元反思者(MetaReflector)。",
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct TaskTrajectory {
        pub task: String,
        pub steps: Vec<TrajectoryStep>,
        pub success: bool,
        pub failure_reason: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct TrajectoryStep {
        pub step_type: String,
        pub content: String,
    }

    pub struct ExperientialReflectiveLearner;

    impl ExperientialReflectiveLearner {
        pub fn new(_max_heuristics: usize) -> Self { Self }

        pub fn extract_heuristics(&self, _trajectory: &TaskTrajectory) -> Vec<Heuristic> {
            vec![]
        }

        pub fn retrieve_relevant(&self, _task: &str, _top_k: usize) -> Vec<Heuristic> {
            vec![]
        }

        pub fn update_effectiveness(&self, _heuristic_id: &str, _was_helpful: bool) {}

        pub fn multi_persona_reflect(
            &self,
            _trajectory: &TaskTrajectory,
        ) -> HashMap<ReflectionPersona, String> {
            HashMap::new()
        }

        pub fn format_for_injection(&self, heuristics: &[Heuristic]) -> String {
            if heuristics.is_empty() {
                return String::new();
            }
            String::from("[stub: 进阶模块未加载]")
        }

        pub fn stats(&self) -> (usize, usize, f64) {
            (0, 0, 0.0)
        }
    }
}

// ============================================================================
// agent::meta_cognitive stubs
// ============================================================================

pub mod meta_cognitive {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum DifficultyLevel {
        Trivial,
        Moderate,
        Complex,
        Extreme,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ExecutionStrategy {
        Direct,
        PlanThenExecute,
        ReflectiveExecution,
        MultiAgent,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ConflictType {
        GoalConflict,
        ResourceConflict,
        TemporalConflict,
        LogicalConflict,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ConflictDetection {
        pub conflict_type: ConflictType,
        pub description: String,
        pub severity: f64,
        pub suggestion: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct StatePrediction {
        pub action: String,
        pub predicted_outcome: String,
        pub confidence: f64,
        pub risk_level: f64,
    }

    pub struct MetaCognitiveController;

    impl MetaCognitiveController {
        pub fn new() -> Self { Self }

        pub fn assess_difficulty(
            &self,
            _task: &str,
            _context: &HashMap<String, String>,
        ) -> DifficultyLevel {
            DifficultyLevel::Moderate
        }

        pub fn select_strategy(&self, _difficulty: &DifficultyLevel) -> ExecutionStrategy {
            ExecutionStrategy::PlanThenExecute
        }

        pub fn detect_conflicts(&self, _plan: &[String]) -> Vec<ConflictDetection> {
            vec![]
        }

        pub fn predict_outcome(&self, _action: &str, _current_state: &str) -> StatePrediction {
            StatePrediction {
                action: String::new(),
                predicted_outcome: String::new(),
                confidence: 0.0,
                risk_level: 0.0,
            }
        }

        pub fn evaluate_state(&self, _predicted_state: &str, _goal: &str) -> f64 {
            0.0
        }

        pub fn update_strategy_effectiveness(&self, _strategy: &ExecutionStrategy, _task_success: bool) {}

        pub fn stats(&self) -> (usize, HashMap<String, f64>) {
            (0, HashMap::new())
        }
    }
}

// ============================================================================
// agent::decentralized stubs
// ============================================================================

pub mod decentralized {
    use super::*;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct AgentProfile {
        pub agent_id: String,
        pub capabilities: Vec<String>,
        pub workload: usize,
        pub reliability: f64,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct TaskStats {
        pub total_assigned: usize,
        pub completed: usize,
        pub failed: usize,
        pub avg_completion_ms: u64,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct DagEdge {
        pub from: String,
        pub to: String,
        pub dependency_type: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct RoutingRequest {
        pub task_id: String,
        pub task_type: String,
        pub priority: u8,
        pub required_capabilities: Vec<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct RoutingDecision {
        pub assigned_agent: Option<String>,
        pub reason: String,
        pub alternatives: Vec<String>,
    }

    pub struct DecentralizedCoordinator;

    impl DecentralizedCoordinator {
        pub fn new(_max_agents: usize) -> Self { Self }

        pub fn register_agent(&self, _profile: AgentProfile) {}

        pub fn route_task(&self, _request: &RoutingRequest) -> RoutingDecision {
            RoutingDecision {
                assigned_agent: None,
                reason: "stub: no agents registered".into(),
                alternatives: vec![],
            }
        }

        pub fn get_dag(&self, _task_id: &str) -> Vec<DagEdge> {
            vec![]
        }
    }
}

// ============================================================================
// agent::goal_verifier stubs
// ============================================================================

pub mod goal_verifier {
    use super::*;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct GoalVerifyRequest {
        pub agent_id: String,
        pub subtask_description: String,
        pub verification: String,
    }

    impl GoalVerifyRequest {
        pub fn to_verify_prompt(&self) -> String {
            format!(
                "你是一个任务验证评估器。请判断以下子任务是否已完成。\n\n\
                 子任务：{}\n\
                 验证标准：{}\n\n\
                 请仅回答 JSON：{{\"passed\": true/false, \"reasoning\": \"评估理由\"}}",
                self.subtask_description, self.verification
            )
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct GoalVerifyResult {
        pub is_achieved: bool,
        pub confidence: f64,
        pub remaining_steps: Vec<String>,
        pub summary: String,
    }

    pub struct GoalVerifier;

    impl GoalVerifier {
        pub fn new() -> Self { Self }

        pub fn verify(&self, _request: &GoalVerifyRequest) -> GoalVerifyResult {
            GoalVerifyResult {
                is_achieved: false,
                confidence: 0.0,
                remaining_steps: vec![],
                summary: "stub: verification not available".into(),
            }
        }
    }
}

// ============================================================================
// retry::circuit_breaker stubs
// ============================================================================

pub mod circuit_breaker {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CircuitState { Closed, Open, HalfOpen }

    #[derive(Debug, Clone)]
    pub struct CircuitBreakerConfig {
        pub failure_threshold: usize,
        pub recovery_timeout_secs: u64,
        pub half_open_max_requests: usize,
    }

    impl Default for CircuitBreakerConfig {
        fn default() -> Self {
            Self { failure_threshold: 5, recovery_timeout_secs: 30, half_open_max_requests: 3 }
        }
    }

    pub struct CircuitBreaker;

    impl CircuitBreaker {
        pub fn new(_config: CircuitBreakerConfig) -> Self { Self }
        pub fn allow_request(&self) -> bool { true }
        pub fn record_success(&self) {}
        pub fn record_failure(&self) {}
        pub fn state(&self) -> CircuitState { CircuitState::Closed }
        pub fn current_state(&self) -> CircuitState { CircuitState::Closed }
    }
}

// ============================================================================
// mcp_server stubs
// ============================================================================

pub mod mcp_server {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub enum McpTransportKind {
        Stdio,
        Sse { port: u16 },
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct McpServerConfig {
        pub transport: McpTransportKind,
        pub name: String,
        pub version: String,
    }

    impl Default for McpServerConfig {
        fn default() -> Self {
            Self {
                transport: McpTransportKind::Stdio,
                name: "ccode-mcp".into(),
                version: "0.1.0".into(),
            }
        }
    }

    pub struct McpServerHandle;

    impl McpServerHandle {
        pub fn shutdown(self) {}
        pub fn is_running(&self) -> bool { false }
    }

    pub struct McpServer;

    impl McpServer {
        pub fn new(_config: McpServerConfig) -> Self { Self }
        pub fn run(self) -> McpServerHandle { McpServerHandle }
    }
}

// ============================================================================
// 其他 A+ 模块 stub（空模块）
// ============================================================================

pub mod metrics {
    pub struct AgentMetrics;

    impl AgentMetrics {
        pub fn global() -> &'static AgentMetrics {
            static METRICS: std::sync::OnceLock<AgentMetrics> = std::sync::OnceLock::new();
            METRICS.get_or_init(|| AgentMetrics)
        }

        pub fn record_inference_latency(&self, _latency_ms: f64) {}
        pub fn record_tool_execution_time(&self, _tool_name: &str, _time_ms: f64) {}
        pub fn record_loop_count(&self, _count: u64) {}
        pub fn record_memory_usage(&self, _bytes: u64) {}
        pub fn record_error(&self, _error_type: &str) {}
        pub fn record_tool_call(&self, _tool_name: &str, _success: bool) {}
        pub fn record_token_usage(&self, _input_tokens: u64, _output_tokens: u64) {}
        pub fn record_subagent_spawn(&self) {}
        pub fn record_doom_loop(&self) {}
        pub fn record_ack_timeout(&self) {}
        pub fn record_agent_started(&self) {}
        pub fn record_agent_stopped(&self) {}
    }
}
pub mod telemetry {}
pub mod retry {
    pub use super::circuit_breaker;

    pub struct RetryPolicy {
        pub max_retries: u32,
        pub initial_backoff_ms: u64,
        pub max_backoff_ms: u64,
        pub retryable_codes: Vec<u16>,
    }

    pub mod backoff {
        use std::future::Future;

        pub async fn retry_with_error_check<F, Fut, T, E>(
            _policy: &super::RetryPolicy,
            mut f: F,
            _error_code_fn: impl Fn(&E) -> Option<u16>,
        ) -> std::result::Result<T, E>
        where
            F: FnMut() -> Fut,
            Fut: Future<Output = std::result::Result<T, E>>,
        {
            f().await
        }
    }
}
pub mod degradation {}
pub mod performance {
    pub mod memory_pool {
        pub struct MessagePool;

        impl MessagePool {
            pub fn new(_capacity: usize, _buffer_size: usize) -> Self { Self }
            pub fn acquire(&self) -> Vec<u8> { Vec::new() }
            pub fn release(&self, _buf: Vec<u8>) {}
            pub fn usage(&self) -> f32 { 0.0 }
            pub fn capacity(&self) -> usize { 0 }
            pub fn available(&self) -> usize { 0 }
        }

        pub struct BufferGuard<'a> {
            buf: &'a mut Vec<u8>,
        }

        impl<'a> BufferGuard<'a> {
            pub fn new(_pool: &'a MessagePool) -> Self {
                // 使用 leaked 静态缓冲区避免生命周期问题
                static mut STUB_BUF: Vec<u8> = Vec::new();
                Self {
                    // SAFETY: STUB_BUF is only accessed through this single BufferGuard instance
                    buf: unsafe { &mut *(&raw mut STUB_BUF) },
                }
            }

            pub fn as_mut(&mut self) -> &mut Vec<u8> {
                self.buf
            }

            pub fn as_ref(&self) -> &Vec<u8> {
                self.buf
            }
        }
    }
}
pub mod error {}
pub mod utils {
    /// trigram 相似度计算（stub 版本，始终返回 0）
    pub fn trigram_similarity(_a: &str, _b: &str) -> f64 {
        0.0
    }
}