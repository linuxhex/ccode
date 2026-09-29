//! Sub-agent mailbox — 代理间直接通信
//!
//! 借鉴 Codex V2 的 mailbox 模式。代理可以通过邮箱向任意其他代理发送消息，
//! 无需经过父代理中转。支持：
//! - 有界邮箱（防止内存泄漏）
//! - 多种消息类型（进度、结果、取消、自定义）
//! - 非阻塞发送 + 异步接收
//! - 邮箱清理（代理终止时）

use std::collections::{HashMap, VecDeque};
use serde::{Deserialize, Serialize};

/// 邮箱消息类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MailboxMessage {
    /// 进度更新（子代理 → 父代理或监控者）
    Progress {
        from: String,
        message: String,
        percent: Option<f32>,
    },
    /// 中间结果
    PartialResult {
        from: String,
        content: String,
    },
    /// 取消请求
    Cancel {
        from: String,
        reason: Option<String>,
    },
    /// 自定义消息
    Custom {
        from: String,
        tag: String,
        payload: String,
    },
}

impl MailboxMessage {
    pub fn from_agent(&self) -> &str {
        match self {
            Self::Progress { from, .. } => from,
            Self::PartialResult { from, .. } => from,
            Self::Cancel { from, .. } => from,
            Self::Custom { from, .. } => from,
        }
    }
}

/// 单个代理的邮箱
#[derive(Debug, Clone)]
struct Mailbox {
    messages: VecDeque<MailboxMessage>,
    max_capacity: usize,
}

impl Mailbox {
    fn new(max_capacity: usize) -> Self {
        Self {
            messages: VecDeque::new(),
            max_capacity,
        }
    }

    fn push(&mut self, msg: MailboxMessage) -> bool {
        if self.messages.len() >= self.max_capacity {
            return false;
        }
        self.messages.push_back(msg);
        true
    }

    fn pop(&mut self) -> Option<MailboxMessage> {
        self.messages.pop_front()
    }

    fn drain(&mut self) -> Vec<MailboxMessage> {
        self.messages.drain(..).collect()
    }

    fn len(&self) -> usize {
        self.messages.len()
    }

    fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// 邮箱系统 — 管理所有代理的邮箱
///
/// 提供非阻塞发送和批量接收。代理终止时应调用 `remove_mailbox`
/// 清理资源。
#[derive(Debug)]
pub struct MailboxSystem {
    mailboxes: HashMap<String, Mailbox>,
    default_capacity: usize,
    /// 消息统计
    total_sent: u64,
    total_dropped: u64,
}

impl Default for MailboxSystem {
    fn default() -> Self {
        Self::new(64)
    }
}

impl MailboxSystem {
    pub fn new(default_capacity: usize) -> Self {
        Self {
            mailboxes: HashMap::new(),
            default_capacity,
            total_sent: 0,
            total_dropped: 0,
        }
    }

    /// 为代理创建邮箱。如果已存在则不覆盖。
    pub fn create_mailbox(&mut self, agent_id: &str) {
        self.mailboxes.entry(agent_id.to_string()).or_insert_with(|| {
            Mailbox::new(self.default_capacity)
        });
    }

    /// 创建有自定义容量的邮箱
    pub fn create_mailbox_with_capacity(&mut self, agent_id: &str, capacity: usize) {
        self.mailboxes.entry(agent_id.to_string()).or_insert_with(|| {
            Mailbox::new(capacity)
        });
    }

    /// 发送消息到目标代理的邮箱。
    /// 返回 `Ok(())` 如果成功，`Err(msg)` 如果邮箱不存在或已满。
    pub fn send(&mut self, to: &str, msg: MailboxMessage) -> Result<(), MailboxError> {
        let mailbox = self.mailboxes.get_mut(to).ok_or(MailboxError::NoSuchMailbox(to.to_string()))?;
        if mailbox.push(msg) {
            self.total_sent += 1;
            Ok(())
        } else {
            self.total_dropped += 1;
            Err(MailboxError::MailboxFull(to.to_string()))
        }
    }

    /// 接收一条消息（FIFO）
    pub fn receive(&mut self, agent_id: &str) -> Option<MailboxMessage> {
        self.mailboxes.get_mut(agent_id).and_then(|m| m.pop())
    }

    /// 批量接收所有待处理消息
    pub fn receive_all(&mut self, agent_id: &str) -> Vec<MailboxMessage> {
        self.mailboxes.get_mut(agent_id)
            .map(|m| m.drain())
            .unwrap_or_default()
    }

    /// 查看邮箱中的消息数量
    pub fn pending_count(&self, agent_id: &str) -> usize {
        self.mailboxes.get(agent_id).map(|m| m.len()).unwrap_or(0)
    }

    /// 检查邮箱是否为空
    pub fn is_empty(&self, agent_id: &str) -> bool {
        self.mailboxes.get(agent_id).map(|m| m.is_empty()).unwrap_or(true)
    }

    /// 移除代理的邮箱（代理终止时调用）
    pub fn remove_mailbox(&mut self, agent_id: &str) -> Vec<MailboxMessage> {
        self.mailboxes.remove(agent_id)
            .map(|mut m| m.drain())
            .unwrap_or_default()
    }

    /// 检查邮箱是否存在
    pub fn has_mailbox(&self, agent_id: &str) -> bool {
        self.mailboxes.contains_key(agent_id)
    }

    /// 获取所有有待处理消息的代理 ID
    pub fn agents_with_pending(&self) -> Vec<&str> {
        self.mailboxes.iter()
            .filter(|(_, m)| !m.is_empty())
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// 统计信息
    pub fn stats(&self) -> MailboxStats {
        MailboxStats {
            active_mailboxes: self.mailboxes.len(),
            total_pending: self.mailboxes.values().map(|m| m.len()).sum(),
            total_sent: self.total_sent,
            total_dropped: self.total_dropped,
        }
    }
}

/// 邮箱错误
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailboxError {
    NoSuchMailbox(String),
    MailboxFull(String),
}

impl std::fmt::Display for MailboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchMailbox(id) => write!(f, "no mailbox for agent: {id}"),
            Self::MailboxFull(id) => write!(f, "mailbox full for agent: {id}"),
        }
    }
}

impl std::error::Error for MailboxError {}

/// 邮箱统计
#[derive(Debug, Clone)]
pub struct MailboxStats {
    pub active_mailboxes: usize,
    pub total_pending: usize,
    pub total_sent: u64,
    pub total_dropped: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(from: &str, msg: &str) -> MailboxMessage {
        MailboxMessage::Progress {
            from: from.to_string(),
            message: msg.to_string(),
            percent: None,
        }
    }

    #[test]
    fn create_and_send() {
        let mut sys = MailboxSystem::default();
        sys.create_mailbox("agent-a");
        sys.create_mailbox("agent-b");

        assert!(sys.send("agent-a", progress("agent-b", "hello")).is_ok());
        assert_eq!(sys.pending_count("agent-a"), 1);
        assert!(sys.is_empty("agent-b"));
    }

    #[test]
    fn send_to_nonexistent() {
        let mut sys = MailboxSystem::default();
        let result = sys.send("ghost", progress("a", "hi"));
        assert!(matches!(result, Err(MailboxError::NoSuchMailbox(_))));
    }

    #[test]
    fn receive_fifo() {
        let mut sys = MailboxSystem::default();
        sys.create_mailbox("a");

        sys.send("a", progress("b", "first")).unwrap();
        sys.send("a", progress("b", "second")).unwrap();

        let msg1 = sys.receive("a").unwrap();
        assert!(matches!(msg1, MailboxMessage::Progress { message, .. } if message == "first"));

        let msg2 = sys.receive("a").unwrap();
        assert!(matches!(msg2, MailboxMessage::Progress { message, .. } if message == "second"));

        assert!(sys.receive("a").is_none());
    }

    #[test]
    fn receive_all() {
        let mut sys = MailboxSystem::default();
        sys.create_mailbox("a");

        sys.send("a", progress("b", "one")).unwrap();
        sys.send("a", progress("b", "two")).unwrap();
        sys.send("a", progress("b", "three")).unwrap();

        let all = sys.receive_all("a");
        assert_eq!(all.len(), 3);
        assert!(sys.is_empty("a"));
    }

    #[test]
    fn mailbox_full() {
        let mut sys = MailboxSystem::new(2);
        sys.create_mailbox("a");

        sys.send("a", progress("b", "one")).unwrap();
        sys.send("a", progress("b", "two")).unwrap();
        let result = sys.send("a", progress("b", "three"));
        assert!(matches!(result, Err(MailboxError::MailboxFull(_))));

        let stats = sys.stats();
        assert_eq!(stats.total_sent, 2);
        assert_eq!(stats.total_dropped, 1);
    }

    #[test]
    fn remove_mailbox() {
        let mut sys = MailboxSystem::default();
        sys.create_mailbox("a");
        sys.send("a", progress("b", "msg")).unwrap();

        let drained = sys.remove_mailbox("a");
        assert_eq!(drained.len(), 1);
        assert!(!sys.has_mailbox("a"));
    }

    #[test]
    fn agents_with_pending() {
        let mut sys = MailboxSystem::default();
        sys.create_mailbox("a");
        sys.create_mailbox("b");
        sys.create_mailbox("c");

        sys.send("a", progress("x", "msg")).unwrap();
        sys.send("c", progress("x", "msg")).unwrap();

        let pending = sys.agents_with_pending();
        assert_eq!(pending.len(), 2);
        assert!(pending.contains(&"a"));
        assert!(pending.contains(&"c"));
    }

    #[test]
    fn cancel_message() {
        let mut sys = MailboxSystem::default();
        sys.create_mailbox("worker");

        sys.send("worker", MailboxMessage::Cancel {
            from: "parent".to_string(),
            reason: Some("timeout".to_string()),
        }).unwrap();

        let msg = sys.receive("worker").unwrap();
        match msg {
            MailboxMessage::Cancel { from, reason } => {
                assert_eq!(from, "parent");
                assert_eq!(reason.as_deref(), Some("timeout"));
            }
            _ => panic!("expected Cancel"),
        }
    }

    #[test]
    fn custom_message() {
        let mut sys = MailboxSystem::default();
        sys.create_mailbox("a");

        sys.send("a", MailboxMessage::Custom {
            from: "b".to_string(),
            tag: "file_changed".to_string(),
            payload: "/path/to/file.rs".to_string(),
        }).unwrap();

        let msg = sys.receive("a").unwrap();
        match msg {
            MailboxMessage::Custom { from, tag, payload } => {
                assert_eq!(from, "b");
                assert_eq!(tag, "file_changed");
                assert_eq!(payload, "/path/to/file.rs");
            }
            _ => panic!("expected Custom"),
        }
    }

    #[test]
    fn from_agent_extractor() {
        let msg = progress("agent-x", "hello");
        assert_eq!(msg.from_agent(), "agent-x");
    }

    #[test]
    fn duplicate_create_is_noop() {
        let mut sys = MailboxSystem::new(4);
        sys.create_mailbox("a");
        sys.send("a", progress("b", "one")).unwrap();

        // Second create should not reset the mailbox
        sys.create_mailbox("a");
        assert_eq!(sys.pending_count("a"), 1);
    }
}
