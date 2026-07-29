//! Agent 自愈机制
//!
//! 监控 Agent 心跳，超时后触发自动重启。
//! 依赖持久化模块恢复 Agent 状态。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{info, warn};

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

/// 自愈管理器
///
/// 监控所有 Agent 的心跳，超时后触发重启。
pub struct SelfHealingManager {
    /// Agent 健康状态表
    agents: Arc<Mutex<HashMap<String, AgentHealth>>>,
    /// 心跳超时时间
    heartbeat_timeout: Duration,
    /// 最大重启次数
    max_restarts: u32,
}

impl SelfHealingManager {
    /// 创建自愈管理器
    pub fn new(heartbeat_timeout_secs: u64, max_restarts: u32) -> Self {
        Self {
            agents: Arc::new(Mutex::new(HashMap::new())),
            heartbeat_timeout: Duration::from_secs(heartbeat_timeout_secs),
            max_restarts,
        }
    }

    /// 注册新 Agent
    pub async fn register_agent(&self, agent_id: String) {
        let mut agents = self.agents.lock().await;
        agents.insert(agent_id.clone(), AgentHealth::new(agent_id.clone(), self.max_restarts));
        info!(agent_id = %agent_id, "Agent 已注册到自愈管理器");
    }

    /// 更新 Agent 心跳
    pub async fn record_heartbeat(&self, agent_id: &str) {
        let mut agents = self.agents.lock().await;
        if let Some(health) = agents.get_mut(agent_id) {
            health.heartbeat();
        }
    }

    /// 注销 Agent
    pub async fn unregister_agent(&self, agent_id: &str) {
        let mut agents = self.agents.lock().await;
        agents.remove(agent_id);
        info!(agent_id = %agent_id, "Agent 已从自愈管理器注销");
    }

    /// 检查所有 Agent 健康状态
    ///
    /// 返回需要重启的 Agent ID 列表
    pub async fn check_health(&self) -> Vec<String> {
        let mut agents = self.agents.lock().await;
        let mut need_restart = Vec::new();

        for (agent_id, health) in agents.iter_mut() {
            if health.marked_dead {
                continue;
            }

            if health.is_timed_out(self.heartbeat_timeout) {
                warn!(
                    agent_id = %agent_id,
                    elapsed_secs = health.last_heartbeat.elapsed().as_secs(),
                    "Agent 心跳超时，触发自愈"
                );

                if health.can_restart() {
                    health.record_restart();
                    need_restart.push(agent_id.clone());
                    info!(
                        agent_id = %agent_id,
                        restart_count = health.restart_count,
                        "Agent 自愈重启已触发"
                    );
                } else {
                    health.marked_dead = true;
                    warn!(
                        agent_id = %agent_id,
                        max_restarts = health.max_restarts,
                        "Agent 已达最大重启次数，标记为死亡"
                    );
                }
            }
        }

        need_restart
    }

    /// 启动健康检查循环
    ///
    /// 每 10 秒检查一次所有 Agent 健康状态
    pub fn start_health_check_loop(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            loop {
                interval.tick().await;
                let need_restart = self.check_health().await;
                for agent_id in need_restart {
                    // 实际重启逻辑由 Kernel 处理
                    // 这里只负责检测和标记
                    warn!(agent_id = %agent_id, "Agent 需要重启，已通知 Kernel");
                }
            }
        });
    }

    /// 获取 Agent 健康状态
    pub async fn get_health(&self, agent_id: &str) -> Option<AgentHealth> {
        let agents = self.agents.lock().await;
        agents.get(agent_id).cloned()
    }

    /// 获取所有 Agent 健康状态
    pub async fn get_all_health(&self) -> Vec<AgentHealth> {
        let agents = self.agents.lock().await;
        agents.values().cloned().collect()
    }
}