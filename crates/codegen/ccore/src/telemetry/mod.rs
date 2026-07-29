//! 遥测仪表盘和指标端点（借鉴 Claude Code 设计）
//!
//! 提供轻量级的遥测数据收集和 HTTP 端点：
//! - 内存中收集指标（无需外部服务）
//! - HTTP 端点暴露指标（Prometheus 格式和 JSON）
//! - 实时快照导出

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

/// Agent 运行指标（借鉴 Claude Code telemetry）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMetrics {
    /// 总轮次
    pub total_turns: u64,
    /// 总 Token 消耗
    pub total_tokens: u64,
    /// 工具调用次数
    pub tool_calls: u64,
    /// 工具成功率（百分比）
    pub tool_success_rate: f64,
    /// 平均响应时间（毫秒）
    pub avg_response_time_ms: u64,
    /// 错误次数
    pub error_count: u64,
    /// 子 Agent 次数
    pub subagent_count: u64,
    /// 运行时长（秒）
    pub runtime_secs: u64,
}

impl Default for AgentMetrics {
    fn default() -> Self {
        Self {
            total_turns: 0,
            total_tokens: 0,
            tool_calls: 0,
            tool_success_rate: 0.0,
            avg_response_time_ms: 0,
            error_count: 0,
            subagent_count: 0,
            runtime_secs: 0,
        }
    }
}

/// 遥测数据收集器
///
/// 借鉴 Claude Code 的遥测设计，但更轻量：
/// - 内存中收集指标（无需外部服务）
/// - HTTP 端点暴露指标（Prometheus 格式）
/// - 实时快照导出（JSON）
pub struct TelemetryCollector {
    metrics: Arc<RwLock<AgentMetrics>>,
    start_time: Instant,
}

impl TelemetryCollector {
    /// 创建新的遥测收集器
    pub fn new() -> Self {
        Self {
            metrics: Arc::new(RwLock::new(AgentMetrics::default())),
            start_time: Instant::now(),
        }
    }

    /// 记录一轮完成
    pub async fn record_turn(&self, tokens: u64, response_time_ms: u64) {
        let mut m = self.metrics.write().await;
        m.total_turns += 1;
        m.total_tokens += tokens;

        // 计算平均响应时间
        if m.total_turns > 0 {
            let total_time = m.avg_response_time_ms * (m.total_turns - 1) + response_time_ms;
            m.avg_response_time_ms = total_time / m.total_turns;
        }

        m.runtime_secs = self.start_time.elapsed().as_secs();
    }

    /// 记录工具调用
    pub async fn record_tool_call(&self, success: bool) {
        let mut m = self.metrics.write().await;
        m.tool_calls += 1;

        // 更新成功率
        if m.tool_calls > 0 {
            let success_count =
                (m.tool_success_rate * (m.tool_calls - 1) as f64 / 100.0).round() as u64;
            let new_success_count = if success {
                success_count + 1
            } else {
                success_count
            };
            m.tool_success_rate = (new_success_count as f64 / m.tool_calls as f64) * 100.0;
        }
    }

    /// 记录错误
    pub async fn record_error(&self) {
        self.metrics.write().await.error_count += 1;
    }

    /// 记录子 Agent
    pub async fn record_subagent(&self) {
        self.metrics.write().await.subagent_count += 1;
    }

    /// 获取当前指标快照
    pub async fn snapshot(&self) -> AgentMetrics {
        let mut m = self.metrics.read().await.clone();
        m.runtime_secs = self.start_time.elapsed().as_secs();
        m
    }

    /// 导出为 Prometheus 格式
    pub async fn export_prometheus(&self) -> String {
        let m = self.snapshot().await;
        format!(
            "# HELP agent_total_turns Total number of turns\n# TYPE agent_total_turns counter\nagent_total_turns {}\n\
             # HELP agent_total_tokens Total tokens consumed\n# TYPE agent_total_tokens counter\nagent_total_tokens {}\n\
             # HELP agent_tool_calls Total tool calls\n# TYPE agent_tool_calls counter\nagent_tool_calls {}\n\
             # HELP agent_tool_success_rate Tool success rate (percent)\n# TYPE agent_tool_success_rate gauge\nagent_tool_success_rate {}\n\
             # HELP agent_avg_response_time_ms Average response time in milliseconds\n# TYPE agent_avg_response_time_ms gauge\nagent_avg_response_time_ms {}\n\
             # HELP agent_error_count Total errors\n# TYPE agent_error_count counter\nagent_error_count {}\n\
             # HELP agent_subagent_count Total subagents spawned\n# TYPE agent_subagent_count counter\nagent_subagent_count {}\n\
             # HELP agent_runtime_secs Runtime in seconds\n# TYPE agent_runtime_secs gauge\nagent_runtime_secs {}\n",
            m.total_turns,
            m.total_tokens,
            m.tool_calls,
            m.tool_success_rate,
            m.avg_response_time_ms,
            m.error_count,
            m.subagent_count,
            m.runtime_secs
        )
    }
}

impl Default for TelemetryCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// Dashboard HTTP 服务器（超越 Claude Code）
///
/// 提供两个端点：
/// - GET /metrics: Prometheus 格式指标
/// - GET /dashboard: JSON 格式仪表盘数据
pub struct DashboardServer {
    collector: Arc<TelemetryCollector>,
    addr: SocketAddr,
}

impl DashboardServer {
    /// 创建新的 Dashboard 服务器
    pub fn new(collector: Arc<TelemetryCollector>, port: u16) -> Self {
        Self {
            collector,
            addr: SocketAddr::from(([0, 0, 0, 0], port)),
        }
    }

    /// 运行 HTTP 服务器
    #[cfg(feature = "dashboard")]
    pub async fn run(&self) -> Result<(), anyhow::Error> {
        use axum::{
            http::StatusCode,
            response::IntoResponse,
            routing::get,
            Router,
        };

        let collector = Arc::clone(&self.collector);

        let app = Router::new()
            .route("/metrics", get({
                let collector = Arc::clone(&collector);
                move || {
                    let collector = Arc::clone(&collector);
                    async move {
                        let prometheus_output = collector.export_prometheus().await;
                        (StatusCode::OK, prometheus_output)
                    }
                }
            }))
            .route("/dashboard", get({
                let collector = Arc::clone(&collector);
                move || {
                    let collector = Arc::clone(&collector);
                    async move {
                        let snapshot = collector.snapshot().await;
                        (StatusCode::OK, axum::Json(snapshot))
                    }
                }
            }));

        let listener = tokio::net::TcpListener::bind(self.addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_record_turn() {
        let collector = TelemetryCollector::new();

        collector.record_turn(100, 50).await;
        let metrics = collector.snapshot().await;
        assert_eq!(metrics.total_turns, 1);
        assert_eq!(metrics.total_tokens, 100);
        assert_eq!(metrics.avg_response_time_ms, 50);

        collector.record_turn(200, 100).await;
        let metrics = collector.snapshot().await;
        assert_eq!(metrics.total_turns, 2);
        assert_eq!(metrics.total_tokens, 300);
        assert_eq!(metrics.avg_response_time_ms, 75); // (50 + 100) / 2
    }

    #[tokio::test]
    async fn test_record_tool_call() {
        let collector = TelemetryCollector::new();

        // 第一次调用成功
        collector.record_tool_call(true).await;
        let metrics = collector.snapshot().await;
        assert_eq!(metrics.tool_calls, 1);
        assert_eq!(metrics.tool_success_rate, 100.0);

        // 第二次调用失败
        collector.record_tool_call(false).await;
        let metrics = collector.snapshot().await;
        assert_eq!(metrics.tool_calls, 2);
        assert_eq!(metrics.tool_success_rate, 50.0);

        // 第三次调用成功
        collector.record_tool_call(true).await;
        let metrics = collector.snapshot().await;
        assert_eq!(metrics.tool_calls, 3);
        assert!((metrics.tool_success_rate - 66.66666666666666).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_record_error() {
        let collector = TelemetryCollector::new();

        collector.record_error().await;
        collector.record_error().await;
        let metrics = collector.snapshot().await;
        assert_eq!(metrics.error_count, 2);
    }

    #[tokio::test]
    async fn test_record_subagent() {
        let collector = TelemetryCollector::new();

        collector.record_subagent().await;
        collector.record_subagent().await;
        collector.record_subagent().await;
        let metrics = collector.snapshot().await;
        assert_eq!(metrics.subagent_count, 3);
    }

    #[tokio::test]
    async fn test_export_prometheus() {
        let collector = TelemetryCollector::new();

        collector.record_turn(100, 50).await;
        collector.record_tool_call(true).await;
        collector.record_error().await;

        let prometheus_output = collector.export_prometheus().await;

        assert!(prometheus_output.contains("agent_total_turns 1"));
        assert!(prometheus_output.contains("agent_total_tokens 100"));
        assert!(prometheus_output.contains("agent_tool_calls 1"));
        assert!(prometheus_output.contains("agent_error_count 1"));
        assert!(prometheus_output.contains("# TYPE agent_total_turns counter"));
    }

    #[tokio::test]
    async fn test_snapshot_updates_runtime() {
        let collector = TelemetryCollector::new();

        // 等待至少一秒
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        let metrics = collector.snapshot().await;
        assert!(metrics.runtime_secs >= 1);
    }

    #[tokio::test]
    async fn test_default_metrics() {
        let metrics = AgentMetrics::default();

        assert_eq!(metrics.total_turns, 0);
        assert_eq!(metrics.total_tokens, 0);
        assert_eq!(metrics.tool_calls, 0);
        assert_eq!(metrics.tool_success_rate, 0.0);
        assert_eq!(metrics.avg_response_time_ms, 0);
        assert_eq!(metrics.error_count, 0);
        assert_eq!(metrics.subagent_count, 0);
        assert_eq!(metrics.runtime_secs, 0);
    }
}