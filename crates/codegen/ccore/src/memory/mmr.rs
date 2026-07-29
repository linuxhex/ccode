//! MMR (Maximal Marginal Relevance) 多样化检索
//!
//! 借鉴 Claude Code 的 mmr.rs，平衡相关性和多样性：
//! MMR = λ * Sim(query, doc) - (1-λ) * max(Sim(doc, selected))
//!
//! 与 ccode-memory/mmr.rs 不同，这里使用向量余弦相似度而非 Jaccard 相似度

use super::embedding::EmbeddingIndex;

/// MMR (Maximal Marginal Relevance) 多样化检索
///
/// 通过平衡相关性和多样性，避免返回过于相似的结果
///
/// # Arguments
/// * `index` - Embedding 索引
/// * `query` - 查询向量
/// * `k` - 返回结果数量
/// * `lambda` - 相关性权重（0.0-1.0），通常 0.5-0.8
///     - λ 越高，越偏向相关性
///     - λ 越低，越偏向多样性
///
/// # Returns
/// 返回选中的向量索引列表（按 MMR 分数排序）
///
/// # Example
/// ```ignore
/// let selected = mmr_select(&index, &query_vec, 5, 0.7);
/// ```
pub fn mmr_select(index: &EmbeddingIndex, query: &[f32], k: usize, lambda: f32) -> Vec<usize> {
    if index.is_empty() || k == 0 {
        return Vec::new();
    }

    let vectors = index.vectors();
    let actual_k = k.min(vectors.len());

    let mut selected: Vec<usize> = Vec::with_capacity(actual_k);
    let mut remaining: Vec<usize> = (0..vectors.len()).collect();

    while selected.len() < actual_k && !remaining.is_empty() {
        let mut best_score = f32::NEG_INFINITY;
        let mut best_idx = 0;
        let mut best_pos = 0;

        for (pos, &doc_idx) in remaining.iter().enumerate() {
            let doc_vec = &vectors[doc_idx].data;

            // 相关性分数：查询向量与文档向量的相似度
            let relevance = EmbeddingIndex::cosine_similarity(query, doc_vec);

            // 多样性惩罚：与已选文档的最大相似度
            let diversity_penalty = if selected.is_empty() {
                0.0
            } else {
                selected
                    .iter()
                    .map(|&s| {
                        EmbeddingIndex::cosine_similarity(doc_vec, &vectors[s].data)
                    })
                    .fold(0.0_f32, |max, sim| sim.max(max))
            };

            // MMR 分数 = λ * 相关性 - (1-λ) * 多样性惩罚
            let mmr_score = lambda * relevance - (1.0 - lambda) * diversity_penalty;

            if mmr_score > best_score {
                best_score = mmr_score;
                best_idx = doc_idx;
                best_pos = pos;
            }
        }

        selected.push(best_idx);
        remaining.remove(best_pos);
    }

    selected
}

/// MMR 配置参数
#[derive(Debug, Clone, Copy)]
pub struct MmrConfig {
    /// 是否启用 MMR
    pub enabled: bool,
    /// 相关性权重（0.0-1.0）
    pub lambda: f32,
}

impl Default for MmrConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            lambda: 0.7,
        }
    }
}

impl MmrConfig {
    /// 创建新的 MMR 配置
    pub fn new(enabled: bool, lambda: f32) -> Self {
        Self {
            enabled,
            lambda: lambda.clamp(0.0, 1.0),
        }
    }

    /// 使用 MMR 选择结果
    ///
    /// 如果禁用或只有一个结果，直接返回普通搜索结果
    pub fn select(
        &self,
        index: &EmbeddingIndex,
        query: &[f32],
        k: usize,
    ) -> Vec<usize> {
        if !self.enabled || index.len() <= 1 || self.lambda >= 1.0 {
            // 回退到普通相似度搜索
            return index.search(query, k).into_iter().map(|(i, _)| i).collect();
        }

        mmr_select(index, query, k, self.lambda)
    }
}

#[cfg(test)]
mod tests {
    use super::super::embedding::EmbeddingVector;
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
    fn test_mmr_basic() {
        let mut index = EmbeddingIndex::with_dimension(3);

        // 三个向量：两个相似，一个不同
        index.add(create_test_vector("v1", vec![1.0, 0.0, 0.0]));
        index.add(create_test_vector("v2", vec![0.99, 0.1, 0.0])); // 与 v1 相似
        index.add(create_test_vector("v3", vec![0.0, 1.0, 0.0])); // 与 v1 不同

        let query = vec![1.0, 0.0, 0.0];

        // 使用 MMR 选择 2 个结果
        let selected = mmr_select(&index, &query, 2, 0.7);

        assert_eq!(selected.len(), 2);
        // v1 应该首先被选中（最高相关性）
        assert_eq!(selected[0], 0);
        // 第二个选中的应该是 v2 或 v3（取决于 lambda 和向量相似度）
        // 在 lambda=0.7 时，相关性权重大于多样性权重
        // v2 的相关性更高（0.99），虽然与 v1 相似，但可能被选中
        assert!(selected.contains(&1) || selected.contains(&2));
    }

    #[test]
    fn test_mmr_empty_index() {
        let index = EmbeddingIndex::with_dimension(3);
        let query = vec![1.0, 0.0, 0.0];

        let selected = mmr_select(&index, &query, 5, 0.7);
        assert!(selected.is_empty());
    }

    #[test]
    fn test_mmr_zero_k() {
        let mut index = EmbeddingIndex::with_dimension(3);
        index.add(create_test_vector("v1", vec![1.0, 0.0, 0.0]));

        let query = vec![1.0, 0.0, 0.0];
        let selected = mmr_select(&index, &query, 0, 0.7);

        assert!(selected.is_empty());
    }

    #[test]
    fn test_mmr_k_larger_than_index() {
        let mut index = EmbeddingIndex::with_dimension(3);
        index.add(create_test_vector("v1", vec![1.0, 0.0, 0.0]));
        index.add(create_test_vector("v2", vec![0.0, 1.0, 0.0]));

        let query = vec![1.0, 0.0, 0.0];
        let selected = mmr_select(&index, &query, 10, 0.7);

        // 应该返回所有可用结果
        assert_eq!(selected.len(), 2);
    }

    #[test]
    fn test_mmr_lambda_high() {
        let mut index = EmbeddingIndex::with_dimension(3);

        // 两个相似向量，一个不同向量
        index.add(create_test_vector("v1", vec![1.0, 0.0, 0.0]));
        index.add(create_test_vector("v2", vec![0.99, 0.1, 0.0]));
        index.add(create_test_vector("v3", vec![0.0, 1.0, 0.0]));

        let query = vec![1.0, 0.0, 0.0];

        // λ = 0.95，几乎只看相关性
        let selected = mmr_select(&index, &query, 2, 0.95);

        assert_eq!(selected.len(), 2);
        // 在高 λ 值下，v2（相关但相似）可能被选中而不是 v3（不同但相关性低）
        assert_eq!(selected[0], 0); // v1 总是第一个
    }

    #[test]
    fn test_mmr_lambda_low() {
        let mut index = EmbeddingIndex::with_dimension(3);

        // 两个相似向量，一个不同向量
        index.add(create_test_vector("v1", vec![1.0, 0.0, 0.0]));
        index.add(create_test_vector("v2", vec![0.99, 0.1, 0.0]));
        index.add(create_test_vector("v3", vec![0.0, 1.0, 0.0]));

        let query = vec![1.0, 0.0, 0.0];

        // λ = 0.3，强烈偏向多样性
        let selected = mmr_select(&index, &query, 2, 0.3);

        assert_eq!(selected.len(), 2);
        // 在低 λ 值下，v3 更有可能被选中
        assert_eq!(selected[0], 0); // v1 总是第一个（最高相关性）
        // v3 应该被选中以增加多样性
        assert!(selected.contains(&2));
    }

    #[test]
    fn test_mmr_config_disabled() {
        let mut index = EmbeddingIndex::with_dimension(3);
        index.add(create_test_vector("v1", vec![1.0, 0.0, 0.0]));
        index.add(create_test_vector("v2", vec![0.0, 1.0, 0.0]));

        let config = MmrConfig::new(false, 0.7);
        let query = vec![1.0, 0.0, 0.0];

        let selected = config.select(&index, &query, 2);

        // 禁用时，应该使用普通搜索（按相似度排序）
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0], 0); // v1 最相关
    }

    #[test]
    fn test_mmr_config_lambda_one() {
        let mut index = EmbeddingIndex::with_dimension(3);
        index.add(create_test_vector("v1", vec![1.0, 0.0, 0.0]));
        index.add(create_test_vector("v2", vec![0.0, 1.0, 0.0]));

        let config = MmrConfig::new(true, 1.0);
        let query = vec![1.0, 0.0, 0.0];

        let selected = config.select(&index, &query, 2);

        // λ=1 时，应该使用普通搜索
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0], 0);
    }

    #[test]
    fn test_mmr_preserves_all_results() {
        let mut index = EmbeddingIndex::with_dimension(3);
        index.add(create_test_vector("v1", vec![1.0, 0.0, 0.0]));
        index.add(create_test_vector("v2", vec![0.0, 1.0, 0.0]));
        index.add(create_test_vector("v3", vec![0.0, 0.0, 1.0]));

        let query = vec![1.0, 0.0, 0.0];
        let selected = mmr_select(&index, &query, 3, 0.5);

        // 应该返回所有 3 个结果（即使顺序可能不同）
        assert_eq!(selected.len(), 3);
        assert!(selected.contains(&0));
        assert!(selected.contains(&1));
        assert!(selected.contains(&2));
    }

    #[test]
    fn test_mmr_identical_vectors() {
        let mut index = EmbeddingIndex::with_dimension(3);

        // 两个完全相同的向量
        index.add(create_test_vector("v1", vec![1.0, 0.0, 0.0]));
        index.add(create_test_vector("v2", vec![1.0, 0.0, 0.0]));

        let query = vec![1.0, 0.0, 0.0];
        let selected = mmr_select(&index, &query, 2, 0.5);

        // 应该仍然返回两个结果
        assert_eq!(selected.len(), 2);
    }
}