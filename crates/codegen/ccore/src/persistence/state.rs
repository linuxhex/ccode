//! 状态持久化逻辑
//!
//! 保存和恢复 LoopState/AgentState 快照。

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::agent::loop_state::DoneReason;
use crate::agent::goal_loop::GoalLoopSnapshot;
use crate::agent::AgentState;

use super::storage::StorageBackend;

/// 循环状态快照
///
/// 覆盖 LoopStateMachine（Turn 级）和 GoalLoop（Goal 级，可选）的完整可序列化状态。
/// 由 LoopStateMachine::to_snapshot() 生成，restore_from_snapshot() 消费。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopStateSnapshot {
    // ---- Turn 级（LoopStateMachine）----
    /// 当前 Agent 状态
    pub state: AgentState,
    /// 已执行轮次数
    pub turn_count: u32,
    /// 已使用 token 数
    pub tokens_used: u64,
    /// 连续失败次数
    pub consecutive_failures: u32,
    /// 已运行时间（秒，由 Instant::elapsed() 计算）
    pub elapsed_secs: u64,
    /// 结束原因（Done/Error 状态时有值）
    pub done_reason: Option<DoneReason>,
    // ---- Goal 级（GoalLoop，/goal 命令触发时才有）----
    /// GoalLoop 快照（None 表示未启用 GoalLoop）
    pub goal: Option<GoalLoopSnapshot>,
}

/// 状态持久化器
///
/// 保存和恢复 Agent 的运行状态。去泛型化持有 trait object，
/// 使 ThinkerNode 可在运行时注入（FileStorage::new 是 async，无法在同步构造中创建）。
pub struct StatePersister {
    backend: Arc<dyn StorageBackend>,
    agent_id: String,
}

impl StatePersister {
    /// 创建状态持久化器
    pub fn new(backend: Arc<dyn StorageBackend>, agent_id: String) -> Self {
        Self { backend, agent_id }
    }

    /// 保存状态快照
    pub async fn save_state(&self, snapshot: &LoopStateSnapshot) -> Result<()> {
        let key = self.key();
        let data = serde_json::to_vec(snapshot)?;
        self.backend.save(&key, &data).await
    }

    /// 加载状态快照
    pub async fn load_state(&self) -> Result<Option<LoopStateSnapshot>> {
        let key = self.key();
        match self.backend.load(&key).await? {
            Some(data) => {
                let snapshot = serde_json::from_slice(&data)?;
                Ok(Some(snapshot))
            }
            None => Ok(None),
        }
    }

    /// 获取存储键
    fn key(&self) -> String {
        format!("state/{}", self.agent_id)
    }
}
