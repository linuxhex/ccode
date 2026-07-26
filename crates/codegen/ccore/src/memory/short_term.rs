//! L1 短期记忆 - 内存级向量库（纯 Rust 余弦相似度）
//!
//! 存储当前会话的完整对话历史（永不丢弃），支持语义检索
//! 向量索引使用纯内存余弦相似度搜索，不依赖外部向量库

use serde::{Deserialize, Serialize};

/// 伪向量的维度
const PSEUDO_VECTOR_DIM: usize = 64;

/// L1 中的记忆条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShortTermEntry {
    /// 条目唯一 ID
    pub id: String,
    /// 对话轮次
    pub turn: u32,
    /// 消息角色
    pub role: String,
    /// 消息完整原文
    pub content: String,
    /// 消息 embedding 向量
    pub embedding: Vec<f32>,
    /// 消息 token 数
    pub token_count: u32,
    /// 是否为工具调用
    pub is_tool_call: bool,
    /// 被召回次数
    pub recall_count: u32,
}

/// 根据文本内容生成伪向量
///
/// 在没有真正 embedding 模型时，使用文本的字节特征生成固定维度的伪向量，
/// 以便进行粗粒度的相似度检索。相同或相似文本会产生相近的伪向量。
fn pseudo_vector_from_text(text: &str) -> Vec<f32> {
    let bytes = text.as_bytes();
    let mut vec = vec![0.0f32; PSEUDO_VECTOR_DIM];

    // 将文本字节循环映射到各维度，模拟哈希散列
    for (i, &b) in bytes.iter().enumerate() {
        let dim = i % PSEUDO_VECTOR_DIM;
        // 用字节值作为基础，加上位置偏移使不同位置的同字节产生差异
        vec[dim] += (b as f32) * (1.0 + (i as f32 / bytes.len().max(1) as f32));
    }

    // 归一化，使向量模长为 1（余弦相似度要求）
    let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in vec.iter_mut() {
            *v /= norm;
        }
    }

    vec
}

/// 计算两个向量的余弦相似度
///
/// 返回值范围 [-1.0, 1.0]，1.0 表示完全相同方向
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|v| v * v).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|v| v * v).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a * norm_b)
}

/// L1 短期记忆（会话级）
pub struct ShortTermMemory {
    /// 完整对话历史
    entries: Vec<ShortTermEntry>,
    /// 当前轮次
    current_turn: u32,
}

impl Default for ShortTermMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl ShortTermMemory {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            current_turn: 0,
        }
    }

    /// 存入新消息（永不丢弃）
    pub fn store(
        &mut self,
        role: String,
        content: String,
        token_count: u32,
        is_tool_call: bool,
    ) -> String {
        self.current_turn += 1;
        let id = uuid::Uuid::new_v4().to_string();

        // 生成伪向量，后续可通过 update_embedding 替换为真实向量
        let embedding = pseudo_vector_from_text(&content);

        let entry = ShortTermEntry {
            id: id.clone(),
            turn: self.current_turn,
            role,
            content,
            embedding,
            token_count,
            is_tool_call,
            recall_count: 0,
        };
        self.entries.push(entry);
        id
    }

    /// 更新条目的 embedding（用真实向量替换伪向量）
    pub fn update_embedding(&mut self, id: &str, embedding: Vec<f32>) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.embedding = embedding;
        }
    }

    /// 标记条目被召回
    pub fn mark_recalled(&mut self, id: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.recall_count += 1;
        }
    }

    /// 语义检索：按余弦相似度搜索最相似的条目
    ///
    /// 对每个条目计算查询向量与条目 embedding 的余弦相似度，
    /// 按相似度降序排列，返回前 top_k 条。如果条目的 embedding
    /// 为空（维度为 0），则跳过该条目。
    pub fn search_by_embedding(
        &self,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Vec<&ShortTermEntry> {
        if query_embedding.is_empty() || top_k == 0 {
            return Vec::new();
        }

        let mut scored: Vec<(f32, &ShortTermEntry)> = self
            .entries
            .iter()
            .filter_map(|entry| {
                let emb: &[f32] = if entry.embedding.is_empty() {
                    // 没有 embedding 的条目无法做向量搜索，跳过
                    // （伪向量搜索质量太差，不如跳过）
                    return None;
                } else {
                    &entry.embedding
                };
                let sim = cosine_similarity(query_embedding, emb);
                Some((sim, entry))
            })
            .collect();

        // 按相似度降序排序
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // 取前 top_k
        scored.into_iter().take(top_k).map(|(_, e)| e).collect()
    }

    /// 按文本内容进行伪向量搜索（便捷方法）
    ///
    /// 当没有现成 embedding 时，可直接用文本查询。
    /// 内部将查询文本转为伪向量后调用 search_by_embedding。
    pub fn search_by_text(&self, query: &str, top_k: usize) -> Vec<&ShortTermEntry> {
        let query_vec = pseudo_vector_from_text(query);
        self.search_by_embedding(&query_vec, top_k)
    }

    /// 按轮次范围获取条目
    pub fn get_by_range(&self, start_turn: u32, end_turn: u32) -> Vec<&ShortTermEntry> {
        self.entries
            .iter()
            .filter(|e| e.turn >= start_turn && e.turn <= end_turn)
            .collect()
    }

    /// 获取所有条目
    pub fn all_entries(&self) -> &[ShortTermEntry] {
        &self.entries
    }

    /// 总条目数
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 获取当前轮次
    pub fn current_turn(&self) -> u32 {
        self.current_turn
    }

    /// 按指定 ID 列表移除条目，返回实际移除的数量
    pub fn remove_entries(&mut self, ids: &[String]) -> usize {
        let id_set: std::collections::HashSet<&String> = ids.iter().collect();
        let before = self.entries.len();
        self.entries.retain(|e| !id_set.contains(&e.id));
        before - self.entries.len()
    }
}
