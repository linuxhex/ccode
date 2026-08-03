//! Agent 健康状态类型
//!
//! 原自愈管理器（SelfHealingManager）已由 AutonomicNervousSystem 接管，
//! 此模块仅保留 AgentHealth 类型供 autonomic 复用。

use std::time::{Duration, Instant};

/// Agent 健康状态
#[derive(Debug, Clone)]
pub struct AgentHealth {
    /// Agent ID
    pub agent_id: String,
    /// 最后心跳时间
    pub last_heartbeat: Instant,
    /// 重启次数
    pub restart_count: u32,
    /// 最大重启次数
    pub max_restarts: u32,
    /// 是否已标记为死亡
    pub marked_dead: bool,
}

impl AgentHealth {
    pub fn new(agent_id: String, max_restarts: u32) -> Self {
        Self {
            agent_id,
            last_heartbeat: Instant::now(),
            restart_count: 0,
            max_restarts,
            marked_dead: false,
        }
    }

    /// 更新心跳
    pub fn heartbeat(&mut self) {
        self.last_heartbeat = Instant::now();
        self.marked_dead = false;
    }

    /// 检查是否超时
    pub fn is_timed_out(&self, timeout: Duration) -> bool {
        self.last_heartbeat.elapsed() > timeout
    }

    /// 检查是否可以重启
    pub fn can_restart(&self) -> bool {
        self.restart_count < self.max_restarts
    }

    /// 记录重启
    pub fn record_restart(&mut self) {
        self.restart_count += 1;
        self.last_heartbeat = Instant::now();
        self.marked_dead = false;
    }
}
