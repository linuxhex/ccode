//! 共享工具函数

use std::collections::HashSet;

/// 字符级 trigram Jaccard 相似度（0.0 ~ 1.0）
///
/// 将输入字符串拆为字符窗口（宽度 3），计算 trigram 集合的 Jaccard 系数。
/// 用于经验学习、情景记忆、元认知等模块的文本相似度比较。
pub fn trigram_similarity(a: &str, b: &str) -> f64 {
    let trigrams_a: HashSet<_> = a
        .chars()
        .collect::<Vec<_>>()
        .windows(3)
        .map(|w| w.iter().collect::<String>())
        .collect();
    let trigrams_b: HashSet<_> = b
        .chars()
        .collect::<Vec<_>>()
        .windows(3)
        .map(|w| w.iter().collect::<String>())
        .collect();

    if trigrams_a.is_empty() || trigrams_b.is_empty() {
        return 0.0;
    }

    let intersection = trigrams_a.intersection(&trigrams_b).count() as f64;
    let union = trigrams_a.union(&trigrams_b).count() as f64;
    intersection / union
}
