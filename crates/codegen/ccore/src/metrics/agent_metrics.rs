//! Agent Metrics 定义
//!
//! 使用 metrics crate 注册 Histogram/Counter/Gauge，
//! 记录推理延迟、工具执行时间、循环次数、内存使用等指标。

use std::sync::OnceLock;

/// Agent 运行时指标
///
/// 所有指标在首次访问时注册，全局共享。
pub struct AgentMetrics;

/// 全局指标单例
static METRICS: OnceLock<AgentMetrics> = OnceLock::new();

impl AgentMetrics {
    /// 获取全局指标实例
    pub fn global() -> &'static AgentMetrics {
        METRICS.get_or_init(|| AgentMetrics)
    }

    /// 记录推理延迟（毫秒）
    pub fn record_inference_latency(&self, latency_ms: f64) {
        metrics::gauge!("agent_inference_latency_ms").set(latency_ms);
    }

    /// 记录工具执行时间（毫秒）
    pub fn record_tool_execution_time(&self, tool_name: &str, time_ms: f64) {
        metrics::histogram!("agent_tool_execution_time_ms", "tool" => tool_name.to_string())
            .record(time_ms);
    }

    /// 记录循环次数
    pub fn record_loop_count(&self, count: u64) {
        metrics::counter!("agent_loop_count").increment(count);
    }

    /// 记录内存使用（字节）
    pub fn record_memory_usage(&self, bytes: u64) {
        metrics::gauge!("agent_memory_usage_bytes").set(bytes as f64);
    }

    /// 记录错误次数
    pub fn record_error(&self, error_type: &str) {
        metrics::counter!("agent_errors", "type" => error_type.to_string())
            .increment(1);
    }

    /// 记录工具调用次数
    pub fn record_tool_call(&self, tool_name: &str, success: bool) {
        metrics::counter!(
            "agent_tool_calls",
            "tool" => tool_name.to_string(),
            "status" => if success { "success" } else { "failure" }.to_string()
        )
        .increment(1);
    }

    /// 记录 token 使用量
    pub fn record_token_usage(&self, input_tokens: u64, output_tokens: u64) {
        metrics::counter!("agent_input_tokens").increment(input_tokens);
        metrics::counter!("agent_output_tokens").increment(output_tokens);
    }

    /// 记录子代理创建数
    pub fn record_subagent_spawn(&self) {
        metrics::counter!("agent_subagent_spawns").increment(1);
    }

    /// 记录 Doom Loop 触发次数
    pub fn record_doom_loop(&self) {
        metrics::counter!("agent_doom_loop_triggers").increment(1);
    }

    /// 记录 ACK 超时次数
    pub fn record_ack_timeout(&self) {
        metrics::counter!("agent_ack_timeouts").increment(1);
    }

    /// 记录 Agent 启动
    pub fn record_agent_started(&self) {
        metrics::counter!("agent_started").increment(1);
    }

    /// 记录 Agent 停止
    pub fn record_agent_stopped(&self) {
        metrics::counter!("agent_stopped").increment(1);
    }
}