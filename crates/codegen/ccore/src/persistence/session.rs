//! 会话持久化逻辑
//!
//! 保存和恢复 Conversation 状态。

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::storage::StorageBackend;

/// 会话持久化元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPersistMeta {
    pub session_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub turn_count: u32,
    pub token_count: u64,
}

/// 会话持久化器
///
/// 封装会话持久化逻辑，支持增量保存。
pub struct SessionPersister<B: StorageBackend> {
    backend: Arc<B>,
    session_id: String,
}

impl<B: StorageBackend> SessionPersister<B> {
    /// 创建会话持久化器
    pub fn new(backend: Arc<B>, session_id: String) -> Self {
        Self { backend, session_id }
    }

    /// 保存会话数据
    ///
    /// # 参数
    /// - conversation: 会话数据（简化为 Vec<u8>，实际应为 Conversation 类型）
    ///
    /// # 返回
    /// - Ok(()) 保存成功
    /// - Err(e) 保存失败
    pub async fn save_session(&self, conversation: &[u8]) -> Result<()> {
        let key = self.key();
        self.backend.save(&key, conversation).await
    }

    /// 加载会话数据
    ///
    /// # 返回
    /// - Ok(Some(data)) 会话数据存在
    /// - Ok(None) 会话数据不存在
    /// - Err(e) 加载失败
    pub async fn load_session(&self) -> Result<Option<Vec<u8>>> {
        let key = self.key();
        self.backend.load(&key).await
    }

    /// 获取存储键
    fn key(&self) -> String {
        format!("session/{}", self.session_id)
    }
}