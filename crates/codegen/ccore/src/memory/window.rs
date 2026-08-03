//! 滑动窗口更新
//!
//! 每 agent turn 结束时，重算冷热评分，从热到冷填充 L0

use crate::memory::heat::{self, HeatInput, HeatWeights, HeatThresholds, Temperature};
use crate::memory::working::{WorkingEntry, MessageRole};

/// 滑动窗口更新器
pub struct SlidingWindow {
    weights: HeatWeights,
    thresholds: HeatThresholds,
    max_tokens: u32,
}

/// 消息元数据，用于冷热评分
#[derive(Debug, Clone)]
pub struct MessageMeta {
    pub elapsed_turns: u32,
    pub relevance: f64,
    pub recall_count: u32,
    pub is_tool_result: bool,
    pub tool_importance: f64,
    pub role: String,
    pub content: String,
    pub token_count: u32,
    pub source_range: (usize, usize),
}

impl SlidingWindow {
    pub fn new(max_tokens: u32) -> Self {
        Self {
            weights: HeatWeights::default(),
            thresholds: HeatThresholds::default(),
            max_tokens,
        }
    }

    /// 执行滑动窗口更新：对所有消息计算冷热，填充 L0
    pub fn update(&self, messages: &[MessageMeta]) -> Vec<WorkingEntry> {
        // 1. 计算每条消息的冷热评分和温度等级
        let mut scored: Vec<(f64, Temperature, &MessageMeta)> = messages
            .iter()
            .map(|msg| {
                let input = HeatInput {
                    elapsed_turns: msg.elapsed_turns,
                    relevance: msg.relevance,
                    recall_count: msg.recall_count,
                    is_tool_result: msg.is_tool_result,
                    tool_importance: msg.tool_importance,
                };
                let heat = heat::compute_heat(&input, &self.weights);
                let temp = heat::classify(heat, &self.thresholds);
                (heat, temp, msg)
            })
            .collect();

        // 2. 按热度从高到低排序
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // 3. 贪心填充 L0，直到 token 预算用满
        let mut entries = Vec::new();
        let mut used_tokens = 0u32;

        for (_heat_score, temp, msg) in &scored {
            let entry = match temp {
                Temperature::Hot => WorkingEntry::Hot {
                    role: MessageRole::try_from(msg.role.as_str()).unwrap_or(MessageRole::User),
                    content: msg.content.clone(),
                    token_count: msg.token_count,
                    created_at: std::time::Instant::now(),
                },
                Temperature::Warm => {
                    // 温消息：生成简短摘要
                    // 实际场景中由 LLM 生成摘要，此处用简化逻辑
                    let summary = if msg.content.len() > 100 {
                        // 找到不超过 100 字节的最后一个有效 UTF-8 字符边界
                        let truncate_at = msg.content.char_indices()
                            .take_while(|(idx, _)| *idx < 100)
                            .last()
                            .map(|(idx, c)| idx + c.len_utf8())
                            .unwrap_or(0);
                        if truncate_at > 0 && truncate_at <= msg.content.len() {
                            format!("{}...", &msg.content[..truncate_at])
                        } else {
                            msg.content.clone()
                        }
                    } else {
                        msg.content.clone()
                    };
                    let summary_tokens = (summary.len() as f32 / 4.0) as u32; // 粗略估算
                    WorkingEntry::Warm {
                        summary,
                        token_count: summary_tokens,
                        source_range: msg.source_range,
                    }
                }
                Temperature::Cold => {
                    // 冷消息：替换为占位符
                    let placeholder = format!(
                        "[冷记忆: 第{}-{}轮, 主题概要]",
                        msg.source_range.0, msg.source_range.1
                    );
                    WorkingEntry::Cold {
                        placeholder,
                        source_range: msg.source_range,
                        token_count: 30, // 占位符固定约 30 tokens
                    }
                }
            };

            let entry_tokens = entry.token_count();
            if used_tokens + entry_tokens <= self.max_tokens {
                used_tokens += entry_tokens;
                entries.push(entry);
            } else {
                // 预算用满，剩余消息标记为冷
                entries.push(WorkingEntry::Cold {
                    placeholder: format!("[冷记忆: 第{}-{}轮]", msg.source_range.0, msg.source_range.1),
                    source_range: msg.source_range,
                    token_count: 30,
                });
            }
        }

        // 4. 按原始顺序排列（不按热度排列，保持对话时序）
        entries.sort_by_key(|e| match e {
            WorkingEntry::Hot { .. } => 0,
            WorkingEntry::Warm { source_range, .. } => source_range.0,
            WorkingEntry::Cold { source_range, .. } => source_range.0,
        });

        entries
    }
}
