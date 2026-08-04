//! Metrics 模块入口
//!
//! 提供 Agent 运行时指标的收集和导出。

pub mod agent_metrics;
pub mod prometheus_exporter;

pub use agent_metrics::AgentMetrics;
pub use prometheus_exporter::start_prometheus_exporter;