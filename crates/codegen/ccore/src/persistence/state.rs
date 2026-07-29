//! 状态持久化逻辑
//!
//! 保存和恢复 LoopState/AgentState 快照。

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::storage::StorageBackend;

/// 循环状态快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopStateSnapshot {
    /// 当前状态名（如 "Idle", "CallingLLM", "Done"）
    pub state: String,
    /// 已执行轮次数
    pub turn_count: u32,
    /// 已使用 token 数
    pub tokens_used: u64,
    /// 连续失败次数
    pub consecutive_failures: u32,
    /// 已运行时间（秒）
    pub elapsed_secs: u64,
}

/// 状态持久化器
///
/// 保存和恢复 Agent 的运行状态。
pub struct StatePersister<B: StorageBackend> {
    backend: Arc<B>,
    agent_id: String,
}

impl<B: StorageBackend> StatePersister<B> {
    /// 创建状态持久化器
    pub fn new(backend: Arc<B>, agent_id: String) -> Self {
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