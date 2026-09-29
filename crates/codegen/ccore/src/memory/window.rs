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
    /// 原始工具名（仅工具结果有值）
    pub tool_name: Option<String>,
    /// 是否为用户纠正消息
    pub is_correction: bool,
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
        self.fill_entries(messages, |msg| fallback_truncate(&msg.content, 200))
    }

    /// Async variant that uses LLM summarization for Warm entries.
    ///
    /// Falls back to truncation if the summarizer fails or is not provided.
    /// Correction messages get an enhanced prompt to preserve the correction pair.
    pub async fn update_with_summarizer(
        &self,
        messages: &[MessageMeta],
        summarizer: Option<&dyn crate::memory::working::LlmSummarizer>,
    ) -> Vec<WorkingEntry> {
        if let Some(summarizer) = summarizer {
            let summarize = |msg: &MessageMeta| async {
                let prompt = if msg.is_correction {
                    "Summarize preserving the user's corrections. \
                     Note what was wrong and what the correct information is:"
                } else {
                    "Summarize this conversation message in 2-3 sentences, \
                     preserving key facts and decisions:"
                };
                match summarizer.summarize(prompt, &msg.content).await {
                    Ok(s) => s,
                    Err(_) => fallback_truncate(&msg.content, 200),
                }
            };
            self.fill_entries_async(messages, summarize).await
        } else {
            self.fill_entries(messages, |msg| fallback_truncate(&msg.content, 200))
        }
    }

    /// 公共逻辑：评分 → 排序 → 贪心填充 → 按时序排列
    fn fill_entries(
        &self,
        messages: &[MessageMeta],
        warm_summary: impl Fn(&MessageMeta) -> String,
    ) -> Vec<WorkingEntry> {
        let mut scored = self.score_and_sort(messages);
        let mut entries = Vec::new();
        let mut used_tokens = 0u32;

        for (_heat_score, temp, msg) in &scored {
            let entry = self.build_entry(temp, msg, &warm_summary);
            let entry_tokens = entry.token_count();
            if used_tokens + entry_tokens <= self.max_tokens {
                used_tokens += entry_tokens;
                entries.push(entry);
            } else {
                entries.push(Self::cold_placeholder(msg.source_range));
            }
        }

        Self::sort_by_chronology(&mut entries);
        entries
    }

    /// Async 版本，warm_summary 为 async 闭包
    async fn fill_entries_async<F, Fut>(
        &self,
        messages: &[MessageMeta],
        warm_summary: F,
    ) -> Vec<WorkingEntry>
    where
        F: Fn(&MessageMeta) -> Fut,
        Fut: std::future::Future<Output = String>,
    {
        let scored = self.score_and_sort(messages);
        let mut entries = Vec::new();
        let mut used_tokens = 0u32;

        for (_heat_score, temp, msg) in &scored {
            let entry = match temp {
                Temperature::Hot => WorkingEntry::Hot {
                    role: MessageRole::try_from(msg.role.as_str()).unwrap_or(MessageRole::User),
                    content: msg.content.clone(),
                    token_count: msg.token_count,
                    tool_name: msg.tool_name.clone(),
                    created_at: std::time::Instant::now(),
                },
                Temperature::Warm => {
                    let summary = warm_summary(msg).await;
                    WorkingEntry::Warm {
                        summary,
                        token_count: estimate_tokens(&summary),
                        source_range: msg.source_range,
                    }
                }
                Temperature::Cold => Self::cold_placeholder(msg.source_range),
            };
            let entry_tokens = entry.token_count();
            if used_tokens + entry_tokens <= self.max_tokens {
                used_tokens += entry_tokens;
                entries.push(entry);
            } else {
                entries.push(Self::cold_placeholder(msg.source_range));
            }
        }

        Self::sort_by_chronology(&mut entries);
        entries
    }

    fn score_and_sort(
        &self,
        messages: &[MessageMeta],
    ) -> Vec<(f64, Temperature, &MessageMeta)> {
        let mut scored: Vec<(f64, Temperature, &MessageMeta)> = messages
            .iter()
            .map(|msg| {
                let input = HeatInput {
                    elapsed_turns: msg.elapsed_turns,
                    relevance: msg.relevance,
                    recall_count: msg.recall_count,
                    is_tool_result: msg.is_tool_result,
                    tool_importance: msg.tool_importance,
                    is_correction: msg.is_correction,
                };
                let heat = heat::compute_heat(&input, &self.weights);
                let temp = heat::classify(heat, &self.thresholds);
                (heat, temp, msg)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored
    }

    fn build_entry(
        &self,
        temp: &Temperature,
        msg: &MessageMeta,
        warm_summary: &impl Fn(&MessageMeta) -> String,
    ) -> WorkingEntry {
        match temp {
            Temperature::Hot => WorkingEntry::Hot {
                role: MessageRole::try_from(msg.role.as_str()).unwrap_or(MessageRole::User),
                content: msg.content.clone(),
                token_count: msg.token_count,
                tool_name: msg.tool_name.clone(),
                created_at: std::time::Instant::now(),
            },
            Temperature::Warm => {
                let summary = warm_summary(msg);
                WorkingEntry::Warm {
                    summary,
                    token_count: estimate_tokens(&summary),
                    source_range: msg.source_range,
                }
            }
            Temperature::Cold => Self::cold_placeholder(msg.source_range),
        }
    }

    fn cold_placeholder(source_range: (usize, usize)) -> WorkingEntry {
        WorkingEntry::Cold {
            placeholder: format!("[冷记忆: 第{}-{}轮]", source_range.0, source_range.1),
            source_range,
            token_count: 30,
        }
    }

    fn sort_by_chronology(entries: &mut [WorkingEntry]) {
        entries.sort_by_key(|e| match e {
            WorkingEntry::Hot { .. } => 0,
            WorkingEntry::Warm { source_range, .. } => source_range.0,
            WorkingEntry::Cold { source_range, .. } => source_range.0,
        });
    }
}

impl SlidingWindow {
    /// Compute semantic relevance scores for messages using an embedding provider.
    ///
    /// Uses the last user message as the anchor and computes cosine similarity
    /// between each message's embedding and the anchor. Updates `messages` in place.
    /// Silently degrades to default relevance on failure.
    pub async fn compute_relevance(
        messages: &mut [super::window::MessageMeta],
        embedding_provider: &dyn crate::memory::working::EmbeddingProvider,
    ) {
        if messages.is_empty() {
            return;
        }

        let texts: Vec<&str> = messages.iter().map(|m| m.content.as_str()).collect();
        let vectors = match embedding_provider.embed_batch(&texts).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("embedding computation failed, using default relevance: {}", e);
                return;
            }
        };

        if vectors.len() != messages.len() {
            return;
        }

        let anchor_idx = messages.len() - 1;
        let anchor = &vectors[anchor_idx];

        for (i, msg) in messages.iter_mut().enumerate() {
            msg.relevance = crate::memory::working::cosine_similarity(&vectors[i], anchor);
        }
    }
}

/// Truncate content to a maximum number of characters, respecting UTF-8 boundaries.
fn fallback_truncate(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        return content.to_string();
    }
    let truncate_at = content
        .char_indices()
        .take_while(|(idx, _)| *idx < max_chars)
        .last()
        .map(|(idx, c)| idx + c.len_utf8())
        .unwrap_or(0);
    if truncate_at > 0 && truncate_at <= content.len() {
        format!("{}...", &content[..truncate_at])
    } else {
        content.to_string()
    }
}

/// Rough token estimate (CJK-aware).
fn estimate_tokens(text: &str) -> u32 {
    let cjk_count = text.chars().filter(|c| {
        matches!(*c, '\u{4e00}'..='\u{9fff}' | '\u{3000}'..='\u{303f}' | '\u{ff00}'..='\u{ffef}')
    }).count();
    let ascii_count = text.len() - cjk_count * 3;
    ((ascii_count + cjk_count * 2) as f32 / 4.0).ceil() as u32
}
