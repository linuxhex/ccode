//! State Node - 对话持久化、记忆管理、token 计数
//!
//! State Node 的职责：
//! 1. 接收 state/persist 消息 → 持久化对话到磁盘
//! 2. 接收 state/query 消息 → 查询对话状态并回复
//! 3. 接收 agent/{id}/event → 监听 Agent 状态变化，触发滑动窗口更新

use async_trait::async_trait;
use std::path::PathBuf;

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::node::{Node, NodeId, NodeType, NodeContext};
use crate::node::transport::NodeTransportHandle;
use crate::memory::short_term::ShortTermMemory;
use crate::memory::long_term::LongTermMemory;
use crate::memory::window::SlidingWindow;
use crate::memory::dream::DreamOrganizer;
use crate::memory::long_term::KnowledgeEntry;

/// 对话持久化格式
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedConversation {
    pub session_id: String,
    pub agent_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub messages: Vec<PersistedMessage>,
    pub token_count: u32,
    pub turn_count: u32,
}

/// 单条持久化消息
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedMessage {
    pub id: String,
    pub turn: u32,
    pub role: String,
    pub content: String,
    pub token_count: u32,
    pub is_tool_call: bool,
}

/// 对话状态查询响应
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConversationState {
    pub session_id: String,
    pub total_messages: usize,
    pub total_tokens: u32,
    pub turn_count: u32,
    pub l0_used_tokens: u32,
    pub l0_max_tokens: u32,
}

/// State Node 实现
pub struct StateNode {
    id: NodeId,
    /// L1 短期记忆
    short_term: ShortTermMemory,
    /// L2 长期记忆
    long_term: Option<LongTermMemory>,
    /// 滑动窗口更新器
    sliding_window: SlidingWindow,
    /// 持久化根目录
    storage_root: PathBuf,
    /// 当前会话 ID
    session_id: String,
}

impl StateNode {
    pub fn new(id: NodeId) -> Self {
        let storage_root = dirs();
        let session_id = uuid::Uuid::new_v4().to_string();

        Self {
            id,
            short_term: ShortTermMemory::new(),
            long_term: None,
            sliding_window: SlidingWindow::new(128_000),
            storage_root,
            session_id,
        }
    }

    /// 获取默认存储目录
    fn dirs() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".ccode").join("sessions")
    }

    /// 持久化当前对话到磁盘
    async fn persist(&self) -> anyhow::Result<()> {
        let session_dir = self.storage_root.join(&self.session_id);
        std::fs::create_dir_all(&session_dir)?;

        let entries = self.short_term.all_entries();
        let messages: Vec<PersistedMessage> = entries
            .iter()
            .map(|e| PersistedMessage {
                id: e.id.clone(),
                turn: e.turn,
                role: e.role.clone(),
                content: e.content.clone(),
                token_count: e.token_count,
                is_tool_call: e.is_tool_call,
            })
            .collect();

        let conversation = PersistedConversation {
            session_id: self.session_id.clone(),
            agent_id: String::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            token_count: messages.iter().map(|m| m.token_count).sum(),
            turn_count: entries.last().map(|e| e.turn).unwrap_or(0),
            messages,
        };

        let json = serde_json::to_string_pretty(&conversation)?;
        let path = session_dir.join("conversation.json");
        std::fs::write(path, json)?;

        tracing::debug!("对话已持久化：{}", self.session_id);
        Ok(())
    }

    /// 查询当前对话状态
    fn query_state(&self) -> ConversationState {
        let entries = self.short_term.all_entries();
        ConversationState {
            session_id: self.session_id.clone(),
            total_messages: entries.len(),
            total_tokens: entries.iter().map(|e| e.token_count).sum(),
            turn_count: entries.last().map(|e| e.turn).unwrap_or(0),
            l0_used_tokens: 0,
            l0_max_tokens: 128_000,
        }
    }

    /// 从磁盘加载历史对话
    pub async fn load_session(&mut self, session_id: &str) -> anyhow::Result<()> {
        let path = self.storage_root.join(session_id).join("conversation.json");
        if !path.exists() {
            return Err(anyhow::anyhow!("会话不存在：{}", session_id));
        }

        let json = std::fs::read_to_string(&path)?;
        let conversation: PersistedConversation = serde_json::from_str(&json)?;

        for msg in conversation.messages {
            self.short_term.store(
                msg.role,
                msg.content,
                msg.token_count,
                msg.is_tool_call,
            );
        }

        self.session_id = session_id.to_string();
        tracing::info!("加载历史会话：{} ({}条消息)", session_id, self.short_term.len());
        Ok(())
    }
}

#[async_trait]
impl Node for StateNode {
    fn node_type(&self) -> NodeType {
        NodeType::State
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!(
            "State Node 启动：{} (session={}, storage={})",
            self.id,
            self.session_id,
            self.storage_root.display()
        );
        std::fs::create_dir_all(&self.storage_root)?;
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        match msg.topic.as_str() {
            "state/persist" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let role = payload["role"].as_str().unwrap_or("user");
                let content = payload["content"].as_str().unwrap_or("");
                let token_count = payload["token_count"].as_u64().unwrap_or(0) as u32;
                let is_tool = payload["is_tool_call"].as_bool().unwrap_or(false);
                self.short_term.store(
                    role.to_string(),
                    content.to_string(),
                    token_count,
                    is_tool,
                );
                self.persist().await?;
            }
            "state/query" => {
                let state = self.query_state();
                let reply = FrameCodec::new_reply(
                    Topic::state_response(),
                    self.id.to_string(),
                    &msg.header.msg_id,
                    &state,
                )?;
                transport.send_message(&reply).await?;
            }
            _ => {}
        }
        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec![
            "state/persist".into(),
            "state/query".into(),
            "agent/*/event".into(),
            "sys/shutdown".into(),
        ]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        self.persist().await?;
        tracing::info!("State Node 关闭：{} (最终持久化完成)", self.id);
        Ok(())
    }
}
