//! 记忆桥接 — 连接外部记忆系统（如 ccode-memory）到 ThinkerNode

use crate::memory::episodic::EpisodicMemoryStore;

/// 记忆桥接 trait — 连接外部记忆系统（如 ccode-memory）到 ThinkerNode
///
/// 实现此 trait 的模块负责：
/// - 根据用户输入搜索相关长期记忆（冷区→热区注入）
/// - 在会话结束时提取关键知识（热区→冷区持久化）
pub trait MemoryBridge: Send + Sync {
    /// 根据查询文本搜索相关记忆，返回注入工作记忆的文本片段
    fn search_relevant(&self, query: &str, top_k: usize) -> Vec<String> { let _ = (query, top_k); Vec::new() }
    /// 会话结束时提取关键知识并持久化
    fn extract_and_store(&self, _messages: &[(String, String)]) {}
}

/// 空记忆桥接（默认实现，不做任何事）
pub(crate) struct NoopMemoryBridge;
impl MemoryBridge for NoopMemoryBridge {}

/// 情景记忆桥接 — 连接 EpisodicMemoryStore 到 ThinkerNode
pub struct EpisodicMemoryBridge {
    store: std::sync::Arc<EpisodicMemoryStore>,
}

impl EpisodicMemoryBridge {
    pub fn new(store: std::sync::Arc<EpisodicMemoryStore>) -> Self {
        Self { store }
    }
}

impl MemoryBridge for EpisodicMemoryBridge {
    fn search_relevant(&self, query: &str, top_k: usize) -> Vec<String> {
        let context = self.store.reconstruct_context(query, top_k);
        if context.is_empty() {
            Vec::new()
        } else {
            vec![context]
        }
    }

    fn extract_and_store(&self, messages: &[(String, String)]) {
        use crate::memory::episodic::{MemoryType, MemorySource};
        for (i, (role, content)) in messages.iter().enumerate() {
            // 语义切分：按段落/句子边界分割，保留完整语义单元
            let chunks = Self::semantic_chunks(content, 500);
            for chunk in chunks {
                if chunk.len() < 20 { continue; } // 跳过过短的片段
                // 从每个语义单元提取关键词（取名词性词汇，过滤停用词）
                let keywords: Vec<String> = chunk.split_whitespace()
                    .filter(|w| w.len() > 3 && !Self::is_stop_word(w))
                    .take(10)
                    .map(|s| s.to_lowercase())
                    .collect();
                self.store.encode(
                    if role == "assistant" { MemoryType::Episodic } else { MemoryType::Semantic },
                    &chunk,
                    role,
                    keywords,
                    MemorySource {
                        session_id: String::new(),
                        timestamp: chrono::Utc::now().timestamp(),
                        message_index: Some(i),
                        confidence: 0.7,
                    },
                );
            }
        }
    }
}

impl EpisodicMemoryBridge {
    /// 语义切分：在句子/段落边界分割，不截断句子
    pub(crate) fn semantic_chunks(text: &str, max_chunk: usize) -> Vec<String> {
        if text.len() <= max_chunk {
            return vec![text.to_string()];
        }
        let mut chunks = Vec::new();
        let mut current = String::new();
        for sentence in text.split_inclusive(|c: char| c == '\n' || c == '。' || c == '.' || c == '！' || c == '？') {
            if current.len() + sentence.len() > max_chunk && !current.is_empty() {
                chunks.push(current.trim().to_string());
                current.clear();
            }
            current.push_str(sentence);
        }
        if !current.trim().is_empty() {
            chunks.push(current.trim().to_string());
        }
        chunks
    }

    /// 停用词检查
    pub(crate) fn is_stop_word(w: &str) -> bool {
        matches!(w.to_lowercase().as_str(),
            "the" | "a" | "an" | "is" | "are" | "was" | "were" | "be" | "been" | "being" |
            "have" | "has" | "had" | "do" | "does" | "did" | "will" | "would" | "could" |
            "should" | "may" | "might" | "shall" | "can" | "need" | "dare" | "ought" |
            "used" | "to" | "of" | "in" | "for" | "on" | "with" | "at" | "by" | "from" |
            "as" | "into" | "through" | "during" | "before" | "after" | "above" | "below" |
            "between" | "out" | "off" | "over" | "under" | "again" | "further" | "then" |
            "once" | "here" | "there" | "when" | "where" | "why" | "how" | "all" | "both" |
            "each" | "few" | "more" | "most" | "other" | "some" | "such" | "no" | "nor" |
            "not" | "only" | "own" | "same" | "so" | "than" | "too" | "very" | "just" |
            "because" | "but" | "and" | "or" | "if" | "while" | "about" | "up" |
            "的" | "了" | "在" | "是" | "我" | "有" | "和" | "就" | "不" | "人" | "都" |
            "一" | "个" | "上" | "也" | "很" | "到" | "说" | "要" | "去" | "你" | "会" |
            "着" | "没" | "看" | "好" | "自" | "这" | "他" | "她" | "它" | "们" | "那"
        )
    }
}
