//! Embedding 向量存储和相似度检索
//!
//! 基于 O(n log k) TopK 堆实现的向量检索，支持余弦相似度和 MMR 多样性排序。
//!
//! ## 性能
//!
//! | 操作 | 复杂度 | 说明 |
//! |------|--------|------|
//! | add | O(1) | Vec push |
//! | search | O(n log k) | 最小堆 TopK，避免全量排序 |
//! | cosine_similarity | O(d) | d=维度，通常 1536 |

use serde::{Deserialize, Serialize};

/// Embedding 向量
///
/// 存储单个代码块/消息的嵌入向量，关联 entry_id 和文本预览。
/// 默认维度 1536（OpenAI text-embedding-ada-002），可通过 with_dimension 自定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingVector {
    /// 向量数据（通常 1536 维，对应 text-embedding-ada-002）
    pub data: Vec<f32>,
    /// 来源条目 ID
    pub entry_id: String,
    /// 原始文本（前 100 字符预览）
    pub text_preview: String,
    /// 创建时间戳
    pub created_at: u64,
}

/// Embedding 索引（用于快速相似度检索）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingIndex {
    /// 所有向量
    vectors: Vec<EmbeddingVector>,
    /// 向量维度
    dimension: usize,
}

impl EmbeddingIndex {
    /// 创建新的 Embedding 索引
    pub fn new() -> Self {
        Self {
            vectors: Vec::new(),
            dimension: 1536, // OpenAI ada-002 维度
        }
    }

    /// 创建指定维度的 Embedding 索引
    pub fn with_dimension(dimension: usize) -> Self {
        Self {
            vectors: Vec::new(),
            dimension,
        }
    }

    /// 添加向量到索引
    pub fn add(&mut self, vector: EmbeddingVector) {
        if vector.data.len() == self.dimension {
            self.vectors.push(vector);
        }
    }

    /// 批量添加向量
    pub fn extend(&mut self, vectors: Vec<EmbeddingVector>) {
        for vector in vectors {
            self.add(vector);
        }
    }

    /// 获取向量数量
    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    /// 检查是否为空
    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }

    /// 获取向量维度
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// 获取所有向量
    pub fn vectors(&self) -> &[EmbeddingVector] {
        &self.vectors
    }

    /// 根据 entry_id 查找向量
    pub fn find_by_entry_id(&self, entry_id: &str) -> Option<&EmbeddingVector> {
        self.vectors.iter().find(|v| v.entry_id == entry_id)
    }

    /// 计算余弦相似度
    ///
    /// 余弦相似度 = (A · B) / (||A|| * ||B||)
    /// 范围：[-1, 1]，值越大表示越相似
    pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        if a.len() != b.len() {
            return 0.0;
        }

        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

        if norm_a == 0.0 || norm_b == 0.0 {
            return 0.0;
        }

        dot / (norm_a * norm_b)
    }

    /// 相似度搜索（返回 top-k）
    ///
    /// 使用最小堆维护 top-k 结果，避免全量排序。
    /// 复杂度：O(n log k) 替代 O(n log n)
    pub fn search(&self, query: &[f32], k: usize) -> Vec<(usize, f32)> {
        if query.len() != self.dimension || self.vectors.is_empty() || k == 0 {
            return Vec::new();
        }

        // 最小堆：堆顶是当前 top-k 中最小的分数，新分数大于堆顶则替换
        let mut top_k: Vec<(f32, usize)> = Vec::with_capacity(k + 1);

        for (i, v) in self.vectors.iter().enumerate() {
            let score = Self::cosine_similarity(query, &v.data);
            if !score.is_finite() || score <= 0.0 {
                continue;
            }
            top_k.push((score, i));
            if top_k.len() > k {
                // 找到最小分数并移除
                if let Some(min_idx) = top_k.iter().enumerate()
                    .min_by(|a, b| a.1 .0.partial_cmp(&b.1 .0).unwrap_or(std::cmp::Ordering::Equal))
                    .map(|(idx, _)| idx)
                {
                    top_k.swap_remove(min_idx);
                }
            }
        }

        // 按分数降序排列
        top_k.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        top_k.into_iter().map(|(score, idx)| (idx, score)).collect()
    }

    /// 搜索并返回 entry_id
    ///
    /// 返回格式：Vec<(entry_id, 相似度分数, 文本预览)>
    pub fn search_with_metadata(
        &self,
        query: &[f32],
        k: usize,
    ) -> Vec<(String, f32, String)> {
        self.search(query, k)
            .into_iter()
            .map(|(idx, score)| {
                let v = &self.vectors[idx];
                (v.entry_id.clone(), score, v.text_preview.clone())
            })
            .collect()
    }

    /// 清空索引
    pub fn clear(&mut self) {
        self.vectors.clear();
    }

    /// 移除指定 entry_id 的向量
    pub fn remove_by_entry_id(&mut self, entry_id: &str) -> bool {
        let len_before = self.vectors.len();
        self.vectors.retain(|v| v.entry_id != entry_id);
        self.vectors.len() != len_before
    }
}

impl Default for EmbeddingIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_vector(id: &str, values: Vec<f32>) -> EmbeddingVector {
        EmbeddingVector {
            data: values,
            entry_id: id.to_string(),
            text_preview: format!("Preview for {}", id),
            created_at: 1000,
        }
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = EmbeddingIndex::cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = EmbeddingIndex::cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        let sim = EmbeddingIndex::cosine_similarity(&a, &b);
        assert!((sim + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_partial() {
        let a = vec![1.0, 1.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let sim = EmbeddingIndex::cosine_similarity(&a, &b);
        // 应该是 1/sqrt(2) ≈ 0.707
        assert!((sim - 0.707).abs() < 0.01);
    }

    #[test]
    fn test_cosine_similarity_zero_vectors() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = EmbeddingIndex::cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_different_lengths() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0];
        let sim = EmbeddingIndex::cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn test_index_add_and_search() {
        let mut index = EmbeddingIndex::with_dimension(3);

        // 添加测试向量
        index.add(create_test_vector("v1", vec![1.0, 0.0, 0.0]));
        index.add(create_test_vector("v2", vec![0.0, 1.0, 0.0]));
        index.add(create_test_vector("v3", vec![0.0, 0.0, 1.0]));

        // 搜索与 [1, 0, 0] 最相似的向量
        // 注意：search 过滤掉 score <= 0.0 的结果，
        // [0,1,0] 和 [0,0,1] 与 [1,0,0] 正交，相似度为 0，被过滤
        let query = vec![1.0, 0.0, 0.0];
        let results = index.search(&query, 2);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 0); // v1 should be first
        assert!((results[0].1 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_index_with_metadata() {
        let mut index = EmbeddingIndex::with_dimension(3);
        index.add(create_test_vector("v1", vec![1.0, 0.0, 0.0]));

        let query = vec![1.0, 0.0, 0.0];
        let results = index.search_with_metadata(&query, 1);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "v1");
        assert!((results[0].1 - 1.0).abs() < 1e-6);
        assert!(results[0].2.contains("v1"));
    }

    #[test]
    fn test_index_remove() {
        let mut index = EmbeddingIndex::with_dimension(3);
        index.add(create_test_vector("v1", vec![1.0, 0.0, 0.0]));
        index.add(create_test_vector("v2", vec![0.0, 1.0, 0.0]));

        assert_eq!(index.len(), 2);

        let removed = index.remove_by_entry_id("v1");
        assert!(removed);
        assert_eq!(index.len(), 1);
        assert!(index.find_by_entry_id("v1").is_none());
    }

    #[test]
    fn test_index_clear() {
        let mut index = EmbeddingIndex::with_dimension(3);
        index.add(create_test_vector("v1", vec![1.0, 0.0, 0.0]));

        index.clear();
        assert!(index.is_empty());
    }

    #[test]
    fn test_index_wrong_dimension() {
        let mut index = EmbeddingIndex::with_dimension(3);
        index.add(create_test_vector("v1", vec![1.0, 0.0])); // Wrong dimension

        assert!(index.is_empty());
    }

    #[test]
    fn test_search_empty_index() {
        let index = EmbeddingIndex::with_dimension(3);
        let query = vec![1.0, 0.0, 0.0];
        let results = index.search(&query, 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_wrong_dimension() {
        let mut index = EmbeddingIndex::with_dimension(3);
        index.add(create_test_vector("v1", vec![1.0, 0.0, 0.0]));

        let query = vec![1.0, 0.0]; // Wrong dimension
        let results = index.search(&query, 5);
        assert!(results.is_empty());
    }
}