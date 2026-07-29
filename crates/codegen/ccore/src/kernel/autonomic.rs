//! 自主神经系统（Autonomic Nervous System）
//!
//! 仿生架构中的核心协调层，类似于生物体的自主神经系统，
//! 在无意识层面自动调节"心跳-呼吸-血液循环"三大生命体征：
//!
//! - **心跳监控**（Sympathetic）：监控 Agent 心跳，超时触发自愈重启
//! - **呼吸控制**（Parasympathetic）：Semaphore 限流，调节并发节奏
//! - **血液循环**（Circulatory）：MessagePool 内存池，缓冲区复用与回收
//!
//! 将原先分散的 SelfHealingManager + ConcurrencyController + MessagePool
//! 合并为统一的自主神经中枢，Kernel 只需持有一个 ANS 实例即可获得
//! 全套自调节能力。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Mutex, Semaphore, SemaphorePermit};
use tracing::{info, warn};

use crate::kernel::self_healing::AgentHealth;
use crate::performance::memory_pool::MessagePool;

/// 心跳超时事件：通知 Kernel 主循环需要重启的 Agent
#[derive(Debug, Clone)]
pub struct HeartbeatEvent {
    /// 需要重启的 Agent ID
    pub agent_id: String,
    /// 已重启次数
    pub restart_count: u32,
}

/// 自主神经系统
///
/// 统一管理 Agent 心跳监控、并发控制和内存池，
/// 提供自动健康检查与内存压力处理的自主循环。
pub struct AutonomicNervousSystem {
    /// Agent 健康状态表（心跳监控）
    agents: Arc<Mutex<HashMap<String, AgentHealth>>>,
    /// Agent 并发信号量（呼吸控制，默认 10）
    agent_semaphore: Arc<Semaphore>,
    /// 工具并发信号量（呼吸控制，默认 20）
    tool_semaphore: Arc<Semaphore>,
    /// 消息内存池（血液循环，默认 capacity=64, buffer_size=4096）
    memory_pool: Arc<MessagePool>,
    /// 心跳超时时间（默认 30s）
    heartbeat_timeout: Duration,
    /// 最大重启次数（默认 3）
    max_restarts: u32,
    /// 心跳事件发送端（通知 Kernel 主循环重启 Agent）
    heartbeat_tx: Option<mpsc::Sender<HeartbeatEvent>>,
}

impl AutonomicNervousSystem {
    /// 创建自主神经系统
    ///
    /// # 参数
    /// - `heartbeat_timeout_secs`: 心跳超时秒数
    /// - `max_restarts`: 最大重启次数
    pub fn new(heartbeat_timeout_secs: u64, max_restarts: u32) -> Self {
        Self {
            agents: Arc::new(Mutex::new(HashMap::new())),
            agent_semaphore: Arc::new(Semaphore::new(10)),
            tool_semaphore: Arc::new(Semaphore::new(20)),
            memory_pool: Arc::new(MessagePool::new(64, 4096)),
            heartbeat_timeout: Duration::from_secs(heartbeat_timeout_secs),
            max_restarts,
            heartbeat_tx: None,
        }
    }

    /// 设置心跳事件通道（Kernel 主循环调用）
    ///
    /// Kernel 创建 channel 后，将 tx 传给 ANS，rx 由 Kernel 主循环消费。
    /// 当 ANS 检测到 Agent 心跳超时时，通过 tx 发送 HeartbeatEvent，
    /// Kernel 主循环收到后实际执行重启。
    pub fn set_heartbeat_channel(&mut self, tx: mpsc::Sender<HeartbeatEvent>) {
        self.heartbeat_tx = Some(tx);
    }

    // ── 心跳监控（Sympathetic） ──────────────────────────────

    /// 注册新 Agent 到心跳监控
    pub async fn register_agent(&self, agent_id: String) {
        let mut agents = self.agents.lock().await;
        agents.insert(
            agent_id.clone(),
            AgentHealth::new(agent_id.clone(), self.max_restarts),
        );
        info!(agent_id = %agent_id, "Agent 已注册到自主神经系统");
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
        info!(agent_id = %agent_id, "Agent 已从自主神经系统注销");
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
                let status = format!(
                    "timed_out (elapsed={:?}, threshold={:?})",
                    health.last_heartbeat.elapsed(),
                    self.heartbeat_timeout
                );
                tracing::debug!(
                    target: "ccore::autonomic",
                    agent_id = %agent_id,
                    status = %status,
                    "heartbeat check: anomaly detected"
                );

                warn!(
                    target: "ccore::autonomic",
                    anomaly = "heartbeat_timeout",
                    agent_id = %agent_id,
                    elapsed_secs = health.last_heartbeat.elapsed().as_secs(),
                    "anomaly detected"
                );

                if health.can_restart() {
                    health.record_restart();
                    need_restart.push(agent_id.clone());
                    tracing::info!(
                        target: "ccore::autonomic",
                        action = "self_healing_restart",
                        agent_id = %agent_id,
                        restart_count = health.restart_count,
                        "self-healing triggered"
                    );
                } else {
                    health.marked_dead = true;
                    tracing::error!(
                        target: "ccore::autonomic",
                        agent_id = %agent_id,
                        max_restarts = health.max_restarts,
                        "self-healing failed: max restarts reached, agent marked dead"
                    );
                }
            } else {
                tracing::debug!(
                    target: "ccore::autonomic",
                    agent_id = %agent_id,
                    status = "healthy",
                    "heartbeat check: status=healthy"
                );
            }
        }

        need_restart
    }

    // ── 并发控制（Parasympathetic / 呼吸） ──────────────────

    /// 获取 Agent 并发许可
    ///
    /// 超过最大并发数时等待
    pub async fn acquire_agent_permit(&self) -> Result<SemaphorePermit<'_>, ()> {
        match self.agent_semaphore.acquire().await {
            Ok(permit) => {
                info!(
                    available = self.agent_semaphore.available_permits(),
                    "获取 Agent 并发许可"
                );
                Ok(permit)
            }
            Err(e) => {
                warn!("获取 Agent 并发许可失败：{}", e);
                Err(())
            }
        }
    }

    /// 获取工具并发许可
    ///
    /// 超过最大并发数时等待
    pub async fn acquire_tool_permit(&self) -> Result<SemaphorePermit<'_>, ()> {
        match self.tool_semaphore.acquire().await {
            Ok(permit) => {
                info!(
                    available = self.tool_semaphore.available_permits(),
                    "获取工具并发许可"
                );
                Ok(permit)
            }
            Err(e) => {
                warn!("获取工具并发许可失败：{}", e);
                Err(())
            }
        }
    }

    // ── 内存池（Circulatory / 血液循环） ─────────────────────

    /// 从内存池获取一个缓冲区
    pub fn acquire_buffer(&self) -> Vec<u8> {
        self.memory_pool.acquire()
    }

    /// 将缓冲区归还到内存池
    pub fn release_buffer(&self, buf: Vec<u8>) {
        self.memory_pool.release(buf);
    }

    // ── 自主循环 ─────────────────────────────────────────────

    /// 启动自主神经循环
    ///
    /// spawn 一个 tokio task，每 10 秒执行：
    /// 1. 健康检查（心跳超时 → 通过 channel 通知 Kernel 重启）
    /// 2. 内存压力处理（使用率过高 → 触发回收）
    pub fn start_autonomic_loop(self: Arc<Self>) -> mpsc::Receiver<HeartbeatEvent> {
        let (tx, rx) = mpsc::channel::<HeartbeatEvent>(32);
        // 注意：这里需要修改 self.heartbeat_tx，但 Arc<Self> 不可变引用
        // 所以通过内部 Mutex 传递 tx
        let tx_clone = tx.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            loop {
                interval.tick().await;

                // 健康检查：通过 channel 通知 Kernel 主循环
                let need_restart = self.check_health().await;
                for agent_id in need_restart {
                    warn!(agent_id = %agent_id, "Agent 心跳超时，通过 channel 通知 Kernel 重启");
                    if let Err(e) = tx_clone.send(HeartbeatEvent {
                        agent_id: agent_id.clone(),
                        restart_count: 0, // 具体次数由 Kernel 查询
                    }).await {
                        warn!("发送心跳事件失败：{}", e);
                    }
                }

                // 内存压力处理
                self.handle_memory_pressure();
            }
        });

        rx
    }

    /// 处理内存压力
    ///
    /// 检查内存池使用率，必要时触发回收。
    /// 当使用率超过 80% 时记录警告。
    pub fn handle_memory_pressure(&self) {
        let usage = self.memory_pool.usage();
        if usage > 0.8 {
            warn!(
                usage_pct = (usage * 100.0) as u32,
                available = self.memory_pool.available(),
                capacity = self.memory_pool.capacity(),
                "内存池使用率过高，建议检查缓冲区泄漏"
            );
        }
    }
}
