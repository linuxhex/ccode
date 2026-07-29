//! SessionActor 持久化集成
//!
//! 提供 SessionActor 与持久化模块的桥接，支持：
//! - turn 结束后异步持久化会话状态
//! - 会话恢复时重建 Conversation
//! - 状态快照保存与恢复

use anyhow::Result;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock;
use tracing::warn;

use ccore::persistence::{FileStorage, SessionPersister, StatePersister, LoopStateSnapshot};

/// 会话持久化桥接器
///
/// 封装 SessionPersister 和 StatePersister，提供统一的持久化接口。
/// 所有持久化操作都是异步的，不阻塞主循环。
pub struct SessionPersistBridge {
    session_persister: Arc<SessionPersister<FileStorage>>,
    state_persister: Arc<StatePersister<FileStorage>>,
}

impl SessionPersistBridge {
    /// 创建持久化桥接器
    ///
    /// # 参数
    /// - base_dir: 存储根目录
    /// - session_id: 会话 ID
    /// - agent_id: Agent ID
    pub async fn new(
        base_dir: std::path::PathBuf,
        session_id: String,
        agent_id: String,
    ) -> Result<Self> {
        let storage = Arc::new(FileStorage::new(base_dir).await?);
        let session_persister = Arc::new(SessionPersister::new(storage.clone(), session_id));
        let state_persister = Arc::new(StatePersister::new(storage, agent_id));
        Ok(Self {
            session_persister,
            state_persister,
        })
    }

    /// 异步保存会话数据
    ///
    /// 在 turn 结束后调用，不阻塞主循环。
    /// 序列化后的会话数据通过 tokio::spawn 异步保存。
    pub fn save_session_async(&self, conversation_data: Vec<u8>) {
        let persister = self.session_persister.clone();
        tokio::spawn(async move {
            if let Err(e) = persister.save_session(&conversation_data).await {
                warn!("会话持久化失败：{}", e);
            }
        });
    }

    /// 异步保存状态快照
    ///
    /// 在 LoopState 转换后调用，不阻塞主循环。
    pub fn save_state_async(&self, snapshot: LoopStateSnapshot) {
        let persister = self.state_persister.clone();
        tokio::spawn(async move {
            if let Err(e) = persister.save_state(&snapshot).await {
                warn!("状态快照持久化失败：{}", e);
            }
        });
    }

    /// 加载会话数据（同步等待）
    ///
    /// 在会话恢复时调用，必须等待数据加载完成。
    pub async fn load_session(&self) -> Result<Option<Vec<u8>>> {
        self.session_persister.load_session().await
    }

    /// 加载状态快照（同步等待）
    ///
    /// 在 Agent 恢复时调用，必须等待数据加载完成。
    pub async fn load_state(&self) -> Result<Option<LoopStateSnapshot>> {
        self.state_persister.load_state().await
    }
}

impl Clone for SessionPersistBridge {
    fn clone(&self) -> Self {
        Self {
            session_persister: self.session_persister.clone(),
            state_persister: self.state_persister.clone(),
        }
    }
}

// ============================================================================
// 会话级桥接注册表
//
// SessionActor 的结构体定义与构造函数不在本模块可改范围内，因此以会话 ID
// 为键缓存 `SessionPersistBridge`：首次保存时懒初始化（基于 `FileStorage`，
// 原子写入），后续复用。这在语义上等价于“在 SessionActor 初始化时创建
// SessionPersister”，同时保证持久化失败仅告警、不阻塞主流程。
// ============================================================================

static BRIDGES: OnceLock<RwLock<HashMap<String, SessionPersistBridge>>> = OnceLock::new();

fn bridges() -> &'static RwLock<HashMap<String, SessionPersistBridge>> {
    BRIDGES.get_or_init(|| RwLock::new(HashMap::new()))
}

/// 获取或懒初始化某会话的持久化桥接器。
///
/// 读锁命中则直接返回克隆；否则在写锁内创建并缓存，避免并发重复初始化。
/// 创建失败时 `warn` 记录并返回 `None`（该会话持久化禁用）。
async fn get_or_create_bridge(
    session_id: &str,
    base_dir: std::path::PathBuf,
    agent_id: String,
) -> Option<SessionPersistBridge> {
    {
        let map = bridges().read().await;
        if let Some(bridge) = map.get(session_id) {
            return Some(bridge.clone());
        }
    }
    let mut map = bridges().write().await;
    // 双检：另一并发任务可能已在本任务取写锁前完成初始化。
    if let Some(bridge) = map.get(session_id) {
        return Some(bridge.clone());
    }
    match SessionPersistBridge::new(base_dir, session_id.to_string(), agent_id).await {
        Ok(bridge) => {
            map.insert(session_id.to_string(), bridge.clone());
            Some(bridge)
        }
        Err(e) => {
            warn!(
                session_id = session_id,
                error = %e,
                "SessionPersister 初始化失败，本会话持久化禁用"
            );
            None
        }
    }
}

/// 异步保存会话快照（best-effort，不阻塞主流程）。
///
/// 在关键状态变更点（用户消息加入、Agent 响应完成等）调用。首次调用按
/// `base_dir` 懒初始化 `SessionPersister`；序列化数据经 `FileStorage` 原子
/// 写入（先写临时文件再 rename）。任何失败仅 `tracing::warn`，绝不影响主循环。
pub fn save_session_snapshot(
    session_id: String,
    base_dir: std::path::PathBuf,
    agent_id: String,
    data: Vec<u8>,
) {
    tokio::spawn(async move {
        match get_or_create_bridge(&session_id, base_dir, agent_id).await {
            Some(bridge) => bridge.save_session_async(data),
            None => warn!(session_id = %session_id, "会话快照未持久化：桥接器不可用"),
        }
    });
}