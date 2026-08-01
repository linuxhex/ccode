//! 监控和健康检查
//!
//! 提供系统运行时指标和健康状态监控。
//!
//! 功能：
//! - 性能指标（吞吐量、延迟、错误率）
//! - 资源使用（内存、连接数）
//! - 健康检查（Node 心跳、队列状态）
//! - 告警规则（阈值检测）

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::sync::RwLock;

/// 系统指标
#[derive(Debug, Clone, Default)]
pub struct SystemMetrics {
    // ---- 消息指标 ----
    /// 总发送消息数
    pub messages_sent: u64,
    /// 总接收消息数
    pub messages_received: u64,
    /// 总成功消息数
    pub messages_success: u64,
    /// 总失败消息数
    pub messages_failed: u64,
    /// 总重试消息数
    pub messages_retried: u64,
    
    // ---- 性能指标 ----
    /// 平均消息延迟（毫秒）
    pub avg_latency_ms: f64,
    /// 最大消息延迟（毫秒）
    pub max_latency_ms: f64,
    /// P99 延迟（毫秒）
    pub p99_latency_ms: f64,
    
    // ---- 连接指标 ----
    /// 活跃 Node 数量
    pub active_nodes: u64,
    /// 连接失败次数
    pub connection_failures: u64,
    
    // ---- 错误指标 ----
    /// 序列号错误次数
    pub sequence_errors: u64,
    /// 解码错误次数
    pub decode_errors: u64,
    /// 路由错误次数
    pub routing_errors: u64,
}

/// 监控收集器
pub struct MetricsCollector {
    // ---- 计数器 ----
    messages_sent: AtomicU64,
    messages_received: AtomicU64,
    messages_success: AtomicU64,
    messages_failed: AtomicU64,
    messages_retried: AtomicU64,
    
    // ---- 错误计数 ----
    sequence_errors: AtomicU64,
    decode_errors: AtomicU64,
    routing_errors: AtomicU64,
    connection_failures: AtomicU64,
    
    // ---- 延迟记录 ----
    /// 使用 RwLock 替代 Mutex：写入互斥但读取可并发，减少热路径锁竞争。
    /// record_success 写入频率高但持锁时间短（push 一次 f64），
    /// collect 读取频率低但持锁时间长（遍历 + 排序），RwLock 更适合这种模式。
    latencies: Arc<RwLock<Vec<f64>>>,
    
    // ---- 时间窗口 ----
    window_start: RwLock<Instant>,
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            messages_sent: AtomicU64::new(0),
            messages_received: AtomicU64::new(0),
            messages_success: AtomicU64::new(0),
            messages_failed: AtomicU64::new(0),
            messages_retried: AtomicU64::new(0),
            sequence_errors: AtomicU64::new(0),
            decode_errors: AtomicU64::new(0),
            routing_errors: AtomicU64::new(0),
            connection_failures: AtomicU64::new(0),
            latencies: Arc::new(RwLock::new(Vec::new())),
            window_start: RwLock::new(Instant::now()),
        }
    }

    /// 记录发送消息
    pub fn record_sent(&self) {
        self.messages_sent.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录接收消息
    pub fn record_received(&self) {
        self.messages_received.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录成功消息
    pub fn record_success(&self, latency_ms: f64) {
        self.messages_success.fetch_add(1, Ordering::Relaxed);
        let mut latencies = self.latencies.write().unwrap_or_else(|e| e.into_inner());
        latencies.push(latency_ms);
        // 限制延迟记录数量，防止无界增长导致 OOM
        const MAX_LATENCIES: usize = 10000;
        if latencies.len() > MAX_LATENCIES {
            let drain_count = latencies.len() - MAX_LATENCIES;
            latencies.drain(0..drain_count);
        }
    }

    /// 记录失败消息
    pub fn record_failed(&self) {
        self.messages_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录重试消息
    pub fn record_retried(&self) {
        self.messages_retried.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录序列号错误
    pub fn record_sequence_error(&self) {
        self.sequence_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录解码错误
    pub fn record_decode_error(&self) {
        self.decode_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录路由错误
    pub fn record_routing_error(&self) {
        self.routing_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录连接失败
    pub fn record_connection_failure(&self) {
        self.connection_failures.fetch_add(1, Ordering::Relaxed);
    }

    /// 收集当前指标
    pub fn collect(&self, active_nodes: u64) -> SystemMetrics {
        let latencies = self.latencies.read().unwrap_or_else(|e| e.into_inner());
        
        let avg_latency = if !latencies.is_empty() {
            latencies.iter().sum::<f64>() / latencies.len() as f64
        } else {
            0.0
        };
        
        let max_latency = latencies.iter().fold(0.0_f64, |a, &b| a.max(b));
        
        let p99_latency = if !latencies.is_empty() {
            let sorted = {
                let mut v = latencies.clone();
                v.sort_by(|a, b| a.partial_cmp(b).expect("f64 排序不应出现 NaN"));
                v
            };
            let p99_idx = ((sorted.len() as f64 * 0.99) as usize).min(sorted.len() - 1);
            sorted[p99_idx]
        } else {
            0.0
        };

        SystemMetrics {
            messages_sent: self.messages_sent.load(Ordering::Relaxed),
            messages_received: self.messages_received.load(Ordering::Relaxed),
            messages_success: self.messages_success.load(Ordering::Relaxed),
            messages_failed: self.messages_failed.load(Ordering::Relaxed),
            messages_retried: self.messages_retried.load(Ordering::Relaxed),
            avg_latency_ms: avg_latency,
            max_latency_ms: max_latency,
            p99_latency_ms: p99_latency,
            active_nodes,
            connection_failures: self.connection_failures.load(Ordering::Relaxed),
            sequence_errors: self.sequence_errors.load(Ordering::Relaxed),
            decode_errors: self.decode_errors.load(Ordering::Relaxed),
            routing_errors: self.routing_errors.load(Ordering::Relaxed),
        }
    }

    /// 重置计数器（用于时间窗口）
    pub fn reset(&self) {
        self.messages_sent.store(0, Ordering::Relaxed);
        self.messages_received.store(0, Ordering::Relaxed);
        self.messages_success.store(0, Ordering::Relaxed);
        self.messages_failed.store(0, Ordering::Relaxed);
        self.messages_retried.store(0, Ordering::Relaxed);
        self.sequence_errors.store(0, Ordering::Relaxed);
        self.decode_errors.store(0, Ordering::Relaxed);
        self.routing_errors.store(0, Ordering::Relaxed);
        self.connection_failures.store(0, Ordering::Relaxed);
        self.latencies.write().unwrap_or_else(|e| e.into_inner()).clear();
        *self.window_start.write().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }
}

/// 健康检查配置
#[derive(Debug, Clone)]
pub struct HealthCheckConfig {
    /// 心跳超时（秒）
    pub heartbeat_timeout_secs: u64,
    /// 最大错误率（0.0-1.0）
    pub max_error_rate: f64,
    /// 最大延迟（毫秒）
    pub max_latency_ms: f64,
    /// 最小吞吐量（消息/秒）
    pub min_throughput: u64,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            heartbeat_timeout_secs: 30,
            max_error_rate: 0.1,  // 10%
            max_latency_ms: 5000.0,  // 5 秒
            min_throughput: 1,  // 至少 1 msg/s
        }
    }
}

/// 健康状态
#[derive(Debug, Clone, PartialEq)]
pub enum HealthStatus {
    /// 健康
    Healthy,
    /// 警告
    Warning(String),
    /// 不健康
    Unhealthy(String),
}

/// 健康检查器
pub struct HealthChecker {
    config: HealthCheckConfig,
    /// Node 心跳时间
    /// 使用 RwLock 替代 Mutex：record_heartbeat 是高频写入（短持锁），
    /// check/get_dead_nodes 是低频读取（长持锁），RwLock 允许并发读取。
    node_heartbeats: Arc<RwLock<HashMap<String, Instant>>>,
}

impl HealthChecker {
    pub fn new(config: HealthCheckConfig) -> Self {
        Self {
            config,
            node_heartbeats: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 记录 Node 心跳
    pub fn record_heartbeat(&self, node_id: &str) {
        self.node_heartbeats
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(node_id.to_string(), Instant::now());
    }

    /// 检查系统健康状态
    pub fn check(&self, metrics: &SystemMetrics) -> HealthStatus {
        // 检查错误率
        let total = metrics.messages_success + metrics.messages_failed;
        if total > 0 {
            let error_rate = metrics.messages_failed as f64 / total as f64;
            if error_rate > self.config.max_error_rate {
                return HealthStatus::Unhealthy(format!(
                    "错误率过高：{:.2}% (阈值 {:.2}%)",
                    error_rate * 100.0,
                    self.config.max_error_rate * 100.0
                ));
            }
        }

        // 检查延迟
        if metrics.p99_latency_ms > self.config.max_latency_ms {
            return HealthStatus::Warning(format!(
                "P99 延迟过高：{:.2}ms (阈值 {:.2}ms)",
                metrics.p99_latency_ms, self.config.max_latency_ms
            ));
        }

        // 检查心跳
        let now = Instant::now();
        let timeout = Duration::from_secs(self.config.heartbeat_timeout_secs);
        let mut dead_nodes = Vec::new();
        
        for (node_id, last_heartbeat) in self.node_heartbeats.read().unwrap_or_else(|e| e.into_inner()).iter() {
            if now.duration_since(*last_heartbeat) > timeout {
                dead_nodes.push(node_id.clone());
            }
        }

        if !dead_nodes.is_empty() {
            return HealthStatus::Unhealthy(format!(
                "Node 心跳超时：{:?}",
                dead_nodes
            ));
        }

        // 检查序列号错误
        if metrics.sequence_errors > 0 {
            return HealthStatus::Warning(format!(
                "发现 {} 次序列号错误",
                metrics.sequence_errors
            ));
        }

        HealthStatus::Healthy
    }

    /// 获取不健康的 Node
    pub fn get_dead_nodes(&self) -> Vec<String> {
        let now = Instant::now();
        let timeout = Duration::from_secs(self.config.heartbeat_timeout_secs);
        
        self.node_heartbeats
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, last)| now.duration_since(**last) > timeout)
            .map(|(id, _)| id.clone())
            .collect()
    }
}

/// 监控服务
pub struct MonitoringService {
    collector: Arc<MetricsCollector>,
    checker: HealthChecker,
}

impl MonitoringService {
    pub fn new(config: HealthCheckConfig) -> Self {
        Self {
            collector: Arc::new(MetricsCollector::new()),
            checker: HealthChecker::new(config),
        }
    }

    /// 获取指标收集器
    pub fn collector(&self) -> Arc<MetricsCollector> {
        self.collector.clone()
    }

    /// 记录 Node 心跳
    pub fn record_heartbeat(&self, node_id: &str) {
        self.checker.record_heartbeat(node_id);
    }

    /// 执行健康检查
    pub fn health_check(&self, active_nodes: u64) -> HealthStatus {
        let metrics = self.collector.collect(active_nodes);
        self.checker.check(&metrics)
    }

    /// 获取当前指标
    pub fn get_metrics(&self, active_nodes: u64) -> SystemMetrics {
        self.collector.collect(active_nodes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_collector() {
        let collector = MetricsCollector::new();
        
        collector.record_sent();
        collector.record_received();
        collector.record_success(10.0);
        
        let metrics = collector.collect(1);
        assert_eq!(metrics.messages_sent, 1);
        assert_eq!(metrics.messages_received, 1);
        assert_eq!(metrics.messages_success, 1);
        assert_eq!(metrics.avg_latency_ms, 10.0);
    }

    #[test]
    fn test_health_checker() {
        let checker = HealthChecker::new(HealthCheckConfig::default());
        
        let healthy_metrics = SystemMetrics::default();
        let status = checker.check(&healthy_metrics);
        assert_eq!(status, HealthStatus::Healthy);
        
        let error_metrics = SystemMetrics {
            messages_failed: 100,
            messages_success: 0,
            ..Default::default()
        };
        let status = checker.check(&error_metrics);
        assert!(matches!(status, HealthStatus::Unhealthy(_)));
    }
}