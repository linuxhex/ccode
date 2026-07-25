//! 健康检查 - 心跳监控和 Node 故障检测

use crate::node::NodeId;

/// 健康检查结果
#[derive(Debug)]
pub enum HealthStatus {
    /// Node 正常
    Healthy,
    /// Node 心跳超时
    Timeout,
    /// Node 已注销
    Deregistered,
}

/// Node 故障事件
#[derive(Debug)]
pub struct NodeFailureEvent {
    pub node_id: NodeId,
    pub status: HealthStatus,
    pub message: String,
}

/// 健康检查器
pub struct HealthChecker {
    timeout_secs: u64,
}

impl HealthChecker {
    pub fn new(timeout_secs: u64) -> Self {
        Self { timeout_secs }
    }

    /// 检查单个 Node 的健康状态
    pub fn check(&self, last_heartbeat: chrono::DateTime<chrono::Utc>) -> HealthStatus {
        let elapsed = chrono::Utc::now()
            .signed_duration_since(last_heartbeat)
            .num_seconds() as u64;
        if elapsed > self.timeout_secs {
            HealthStatus::Timeout
        } else {
            HealthStatus::Healthy
        }
    }
}
