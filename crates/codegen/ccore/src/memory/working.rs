//! L0 工作记忆 - 当前 LLM context window 内的内容
//!
//! 管理当前上下文窗口中的消息，按冷热评分动态更新

use serde::{Deserialize, Serialize};

/// 工作记忆中的条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkingEntry {
    /// 热消息：完整原文
    Hot {
        role: String,
        content: String,
        token_count: u32,
    },
    /// 温消息：压缩摘要
    Warm {
        summary: String,
        token_count: u32,
        source_range: (usize, usize),
    },
    /// 冷消息：占位符，可从 L1 召回
    Cold {
        placeholder: String,
        source_range: (usize, usize),
        token_count: u32,
    },
}

impl WorkingEntry {
    /// 获取此条目占用的 token 数
    pub fn token_count(&self) -> u32 {
        match self {
            Self::Hot { token_count, .. } => *token_count,
            Self::Warm { token_count, .. } => *token_count,
            Self::Cold { token_count, .. } => *token_count,
        }
    }

    /// 是否为冷消息
    pub fn is_cold(&self) -> bool {
        matches!(self, Self::Cold { .. })
    }
}

/// L0 工作记忆
pub struct WorkingMemory {
    /// 当前条目列表
    entries: Vec<WorkingEntry>,
    /// 最大 token 预算
    max_tokens: u32,
}

impl WorkingMemory {
    pub fn new(max_tokens: u32) -> Self {
        Self {
            entries: Vec::new(),
            max_tokens,
        }
    }

    /// 当前已用 token 数
    pub fn used_tokens(&self) -> u32 {
        self.entries.iter().map(|e| e.token_count()).sum()
    }

    /// 剩余可用 token 数
    pub fn available_tokens(&self) -> u32 {
        self.max_tokens.saturating_sub(self.used_tokens())
    }

    /// 添加热消息
    pub fn push_hot(&mut self, role: String, content: String, token_count: u32) {
        self.entries.push(WorkingEntry::Hot {
            role,
            content,
            token_count,
        });
    }

    /// 获取所有条目
    pub fn entries(&self) -> &[WorkingEntry] {
        &self.entries
    }

    /// 获取可发送给 LLM 的消息列表（过滤冷占位符，或按需替换为召回内容）
    pub fn to_chat_messages(&self) -> Vec<(String, String)> {
        self.entries
            .iter()
            .map(|entry| match entry {
                WorkingEntry::Hot { role, content, .. } => {
                    (role.clone(), content.clone())
                }
                WorkingEntry::Warm { summary, .. } => {
                    ("system".into(), format!("[上下文摘要] {}", summary))
                }
                WorkingEntry::Cold { placeholder, .. } => {
                    ("system".into(), placeholder.clone())
                }
            })
            .collect()
    }

    /// 用滑动窗口结果替换所有条目
    pub fn replace_entries(&mut self, entries: Vec<WorkingEntry>) {
        self.entries = entries;
    }
}
