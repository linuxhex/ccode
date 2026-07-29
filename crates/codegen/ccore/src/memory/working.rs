//! L0 工作记忆 - 当前 LLM context window 内的内容
//!
//! 管理当前上下文窗口中的消息，按冷热评分动态更新

use serde::{Deserialize, Serialize};

/// 上下文压缩策略（借鉴 Claude Code CompactionPolicy）
#[derive(Debug, Clone)]
pub struct CompactionPolicy {
    /// 触发自动压缩的上下文使用百分比
    pub auto_compact_threshold_percent: u32,
    /// 压缩时使用的模型（None = 使用当前模型）
    pub compact_model: Option<String>,
    /// 是否在压缩前运行记忆刷写
    pub memory_flush_enabled: bool,
    /// 压缩挂钟时间预算（秒）
    pub wall_clock_budget_secs: u64,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            auto_compact_threshold_percent: 85,
            compact_model: None,
            memory_flush_enabled: false,
            wall_clock_budget_secs: 300,
        }
    }
}

/// 压缩结果
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// 压缩前 token 数
    pub tokens_before: u32,
    /// 压缩后 token 数
    pub tokens_after: u32,
    /// 被压缩的条目数
    pub entries_compacted: usize,
    /// 压缩的条目索引范围 (start, end)
    pub compacted_range: (usize, usize),
}

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
    /// 压缩策略
    compaction_policy: CompactionPolicy,
}

impl WorkingMemory {
    pub fn new(max_tokens: u32) -> Self {
        Self {
            entries: Vec::new(),
            max_tokens,
            compaction_policy: CompactionPolicy::default(),
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

    /// 获取最大 token 预算
    pub fn max_tokens(&self) -> u32 {
        self.max_tokens
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

    /// 设置压缩策略
    pub fn set_compaction_policy(&mut self, policy: CompactionPolicy) {
        self.compaction_policy = policy;
    }

    /// 判断是否需要压缩
    pub fn should_compact(&self) -> bool {
        if self.max_tokens == 0 {
            return false;
        }
        let usage_percent = (self.used_tokens() as u64 * 100) / self.max_tokens as u64;
        usage_percent >= self.compaction_policy.auto_compact_threshold_percent as u64
    }

    /// 使用当前策略执行压缩
    pub fn compact(&mut self) -> CompactionResult {
        self.compact_with_policy(&self.compaction_policy.clone())
    }

    /// 使用指定策略执行压缩
    ///
    /// 策略：
    /// 1. 找到最旧的连续 Hot 条目块（排除最近 4 条保持 Hot）
    /// 2. 将这些 Hot→Warm（截断内容至约 50% 并加 `[已压缩]` 前缀）
    /// 3. 将已有的 Warm→Cold（占位符替换）
    pub fn compact_with_policy(&mut self, _policy: &CompactionPolicy) -> CompactionResult {
        let tokens_before = self.used_tokens();
        let total = self.entries.len();

        // 保持最近的条目为 Hot 的安全边界
        let keep_hot_recent = 4;

        if total <= keep_hot_recent {
            return CompactionResult {
                tokens_before,
                tokens_after: tokens_before,
                entries_compacted: 0,
                compacted_range: (0, 0),
            };
        }

        // 第一步：将已有的 Warm 条目降级为 Cold
        for entry in &mut self.entries {
            if let WorkingEntry::Warm {
                summary,
                token_count,
                source_range,
            } = entry
            {
                let placeholder = format!("[冷缓存] {}", &summary[..summary.len().min(40)]);
                let cold_tokens = (*token_count / 4).max(1);
                *entry = WorkingEntry::Cold {
                    placeholder,
                    source_range: *source_range,
                    token_count: cold_tokens,
                };
            }
        }

        // 第二步：将较旧的 Hot 条目降级为 Warm（保留最近 keep_hot_recent 条为 Hot）
        let hot_cutoff = total.saturating_sub(keep_hot_recent);
        let mut compacted_start = total;
        let mut compacted_end = 0;
        let mut entries_compacted = 0usize;

        for i in 0..hot_cutoff {
            if let WorkingEntry::Hot {
                content,
                token_count,
                ..
            } = &self.entries[i]
            {
                let content = content.clone();
                let token_count = *token_count;

                // 截断内容至约 50% 并加 [已压缩] 前缀
                let truncate_len = (content.len() / 2).max(1);
                let summary = format!("[已压缩] {}...", &content[..truncate_len]);
                let warm_tokens = (token_count / 2).max(1);

                self.entries[i] = WorkingEntry::Warm {
                    summary,
                    token_count: warm_tokens,
                    source_range: (i, i),
                };

                compacted_start = compacted_start.min(i);
                compacted_end = compacted_end.max(i);
                entries_compacted += 1;
            }
        }

        let tokens_after = self.used_tokens();

        CompactionResult {
            tokens_before,
            tokens_after,
            entries_compacted,
            compacted_range: (compacted_start, compacted_end),
        }
    }
}
