//! recall 工具 - 从 L1/L2 按需取回冷记忆

use crate::memory::short_term::{ShortTermMemory, cosine_similarity};
use crate::memory::long_term::LongTermMemory;

/// recall 请求
#[derive(Debug, Clone)]
pub struct RecallRequest {
    /// 查询文本
    pub query: String,
    /// 查询 embedding
    pub query_embedding: Vec<f32>,
    /// 最大返回条目数
    pub top_k: usize,
    /// 是否搜索 L2（跨会话）
    pub include_long_term: bool,
}

/// recall 结果
#[derive(Debug, Clone)]
pub struct RecallResult {
    /// 召回的条目列表
    pub entries: Vec<RecalledEntry>,
    /// 来源
    pub source: RecallSource,
}

/// 召回的条目
#[derive(Debug, Clone)]
pub struct RecalledEntry {
    pub id: String,
    pub role: String,
    pub content: String,
    pub turn: Option<u32>,
    pub score: f64,
}

/// 召回来源
#[derive(Debug, Clone, Copy)]
pub enum RecallSource {
    ShortTerm,
    LongTerm,
}

/// 执行 recall
pub async fn recall(
    request: &RecallRequest,
    short_term: &mut ShortTermMemory,
    long_term: &Option<LongTermMemory>,
) -> anyhow::Result<RecallResult> {
    // 1. 先搜索 L1
    let l1_results = short_term.search_by_embedding(&request.query_embedding, request.top_k);

    let mut entries: Vec<RecalledEntry> = l1_results
        .iter()
        .map(|e| {
            let score = if e.embedding.is_empty() {
                0.0
            } else {
                cosine_similarity(&request.query_embedding, &e.embedding) as f64
            };
            RecalledEntry {
                id: e.id.clone(),
                role: e.role.clone(),
                content: e.content.clone(),
                turn: Some(e.turn),
                score,
            }
        })
        .collect();

    // 2. 如果需要，搜索 L2
    if request.include_long_term {
        if let Some(lt) = long_term {
            let l2_results = lt.search(&request.query, &request.query_embedding, request.top_k).await?;
            for e in l2_results {
                entries.push(RecalledEntry {
                    id: e.id,
                    role: "system".into(),
                    content: e.content,
                    turn: None,
                    score: 0.0,
                });
            }
        }
    }

    // 3. 标记被召回的条目
    for entry in &entries {
        short_term.mark_recalled(&entry.id);
    }

    Ok(RecallResult {
        entries,
        source: RecallSource::ShortTerm,
    })
}
