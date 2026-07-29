//! L2 长期记忆 - 跨会话知识持久化（本地 JSON 文件）
//!
//! 存储项目架构、决策记录、用户偏好等跨会话知识
//! 使用 ~/.ccode/memory/ 目录下的 JSON 文件进行持久化
//!
//! 支持三种检索模式：
//! - 关键词搜索（trigram/子串匹配）
//! - 向量搜索（embedding cosine similarity）
//! - 混合搜索（关键词 + 向量 + 时间衰减 + 来源权重 + MMR 重排序）

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tokio::fs;

/// L2 知识条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEntry {
    pub id: String,
    pub category: KnowledgeCategory,
    pub content: String,
    pub embedding: Vec<f32>,
    pub metadata: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
}

/// 知识分类
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum KnowledgeCategory {
    /// 项目架构知识
    Architecture,
    /// 设计决策记录
    Decision,
    /// 用户偏好
    UserPreference,
    /// 代码模式
    CodePattern,
    /// 错误与修复记录
    ErrorFix,
}

/// 搜索结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// 条目 ID
    pub id: String,
    /// 条目内容
    pub content: String,
    /// 搜索分数
    pub score: f64,
    /// 搜索来源
    pub source: SearchSource,
    /// 创建时间（RFC3339）
    pub created_at: String,
}

/// 搜索来源
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SearchSource {
    Keyword,
    Vector,
    Hybrid,
}

/// L2 长期记忆
pub struct LongTermMemory {
    /// 本地存储目录（默认 ~/.ccode/memory/）
    storage_dir: PathBuf,
}

impl LongTermMemory {
    /// 创建长期记忆实例，使用默认存储目录 ~/.ccode/memory/
    pub fn new(_project_path: String, _user_path: String) -> Self {
        let storage_dir = Self::default_storage_dir();
        Self { storage_dir }
    }

    /// 使用自定义存储目录创建
    pub fn with_dir(storage_dir: PathBuf) -> Self {
        Self { storage_dir }
    }

    /// 获取默认存储目录 ~/.ccode/memory/
    fn default_storage_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".ccode").join("memory")
    }

    /// 确保存储目录存在
    async fn ensure_dir(&self) -> anyhow::Result<()> {
        fs::create_dir_all(&self.storage_dir).await?;
        Ok(())
    }

    /// 获取条目文件路径
    fn entry_path(&self, id: &str) -> PathBuf {
        self.storage_dir.join(format!("{}.json", id))
    }

    /// 存储知识条目，写入 JSON 文件
    pub async fn store(&self, entry: KnowledgeEntry) -> anyhow::Result<String> {
        self.ensure_dir().await?;
        let path = self.entry_path(&entry.id);
        let json = serde_json::to_string_pretty(&entry)?;
        fs::write(&path, json).await?;
        Ok(entry.id.clone())
    }

    /// 加载所有知识条目
    async fn load_all(&self) -> anyhow::Result<Vec<KnowledgeEntry>> {
        self.ensure_dir().await?;
        let mut entries = Vec::new();
        let mut dir = fs::read_dir(&self.storage_dir).await?;
        while let Some(item) = dir.next_entry().await? {
            let path = item.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                let content = fs::read_to_string(&path).await?;
                match serde_json::from_str::<KnowledgeEntry>(&content) {
                    Ok(knowledge) => entries.push(knowledge),
                    Err(e) => {
                        tracing::warn!("知识条目文件 {} 反序列化失败：{}", path.display(), e);
                    }
                }
            }
        }
        Ok(entries)
    }

    /// 语义检索（基于关键词匹配）
    ///
    /// 对查询文本和所有条目内容进行分词，计算关键词匹配相似度，返回 top_k 结果
    pub async fn search(
        &self,
        query: &str,
        _query_embedding: &[f32],
        top_k: usize,
    ) -> anyhow::Result<Vec<KnowledgeEntry>> {
        let entries = self.load_all().await?;
        let query_keywords = tokenize(query);

        if query_keywords.is_empty() {
            return Ok(Vec::new());
        }

        // 计算每条目的关键词匹配分数
        let mut scored: Vec<(f64, KnowledgeEntry)> = entries
            .into_iter()
            .map(|entry| {
                let entry_keywords = tokenize(&entry.content);
                let score = keyword_similarity(&query_keywords, &entry_keywords);
                (score, entry)
            })
            .collect();

        // 按分数降序排序，取 top_k
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        Ok(scored.into_iter().take(top_k).map(|(_, e)| e).collect())
    }

    /// 关键词搜索（子串/trigram 匹配）
    ///
    /// 使用 trigram 匹配和子串匹配双重策略
    pub async fn keyword_search(
        &self,
        query: &str,
        top_k: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let entries = self.load_all().await?;
        let query_lower = query.to_lowercase();
        let query_trigrams = compute_trigrams(&query_lower);
        let query_keywords = tokenize(query);

        let mut results: Vec<SearchResult> = entries
            .into_iter()
            .map(|entry| {
                let content_lower = entry.content.to_lowercase();

                // 子串匹配分数
                let substring_score = if content_lower.contains(&query_lower) {
                    1.0
                } else {
                    // 部分子串匹配
                    query_keywords
                        .iter()
                        .filter(|k| content_lower.contains(k.as_str()))
                        .count() as f64
                        / query_keywords.len().max(1) as f64
                };

                // Trigram 匹配分数
                let content_trigrams = compute_trigrams(&content_lower);
                let trigram_score = trigram_similarity(&query_trigrams, &content_trigrams);

                // 综合分数
                let score = 0.5 * substring_score + 0.5 * trigram_score;

                SearchResult {
                    id: entry.id.clone(),
                    content: entry.content.clone(),
                    score,
                    source: SearchSource::Keyword,
                    created_at: entry.created_at.clone(),
                }
            })
            .filter(|r| r.score > 0.0)
            .collect();

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(top_k);
        Ok(results)
    }

    /// 向量搜索（embedding cosine similarity）
    pub async fn vector_search(
        &self,
        query_embedding: &[f32],
        top_k: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let entries = self.load_all().await?;
        if query_embedding.is_empty() || top_k == 0 {
            return Ok(Vec::new());
        }

        let mut results: Vec<SearchResult> = entries
            .into_iter()
            .filter_map(|entry| {
                if entry.embedding.is_empty() {
                    return None;
                }
                let score = cosine_similarity_f64(query_embedding, &entry.embedding);
                if score <= 0.0 {
                    return None;
                }
                Some(SearchResult {
                    id: entry.id.clone(),
                    content: entry.content.clone(),
                    score: score as f64,
                    source: SearchSource::Vector,
                    created_at: entry.created_at.clone(),
                })
            })
            .collect();

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(top_k);
        Ok(results)
    }

    /// 混合搜索（借鉴 Claude Code hybrid_search）
    ///
    /// 融合三条路径：
    /// 1. 关键词搜索（trigram/子串匹配）
    /// 2. 向量搜索（embedding cosine similarity）
    /// 3. 时间衰减 + 来源权重
    /// 最终 MMR 重排序
    pub async fn hybrid_search(
        &self,
        query: &str,
        query_embedding: &[f32],
        top_k: usize,
        time_decay_hours: f64,
    ) -> anyhow::Result<Vec<SearchResult>> {
        // 1. 关键词搜索
        let keyword_results = self.keyword_search(query, top_k * 2).await?;

        // 2. 向量搜索
        let vector_results = self.vector_search(query_embedding, top_k * 2).await?;

        // 3. 合并 + 去重（取两条路径的最大分值）
        let mut merged = merge_search_results(keyword_results, vector_results);

        // 4. 时间衰减
        apply_time_decay(&mut merged, time_decay_hours);

        // 5. 来源权重
        apply_source_weights(&mut merged);

        // 6. 排序
        merged.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        merged.truncate(top_k);

        // 7. MMR 重排序
        let mmr_results = crate::memory::mmr::mmr_rerank(
            &merged
                .iter()
                .map(|r| (r.content.clone(), r.score))
                .collect::<Vec<_>>(),
            query,
            top_k,
            0.7,
        );

        // 映射回 SearchResult
        Ok(mmr_results
            .into_iter()
            .map(|(content, score)| {
                merged
                    .iter()
                    .find(|r| r.content == content)
                    .cloned()
                    .unwrap_or_else(|| SearchResult {
                        id: String::new(),
                        content,
                        score,
                        source: SearchSource::Hybrid,
                        created_at: String::new(),
                    })
            })
            .collect())
    }

    /// 删除知识条目
    pub async fn delete(&self, id: &str) -> anyhow::Result<()> {
        let path = self.entry_path(id);
        if path.exists() {
            fs::remove_file(&path).await?;
        }
        Ok(())
    }
}

/// 文本分词：按空白符和标点拆分为小写关键词，过滤单字符词
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
        .filter(|s| s.len() >= 2)
        .map(|s| s.to_string())
        .collect()
}

/// 计算关键词匹配相似度：查询词在条目词中的命中率
fn keyword_similarity(query: &[String], entry: &[String]) -> f64 {
    if entry.is_empty() {
        return 0.0;
    }
    let entry_set: HashSet<&String> = entry.iter().collect();
    let matches = query.iter().filter(|k| entry_set.contains(k)).count();
    matches as f64 / query.len() as f64
}

/// 计算 trigram 集合
fn compute_trigrams(text: &str) -> HashSet<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 3 {
        let s: String = chars.into_iter().collect();
        if !s.is_empty() {
            let mut set = HashSet::new();
            set.insert(s);
            return set;
        }
        return HashSet::new();
    }
    chars
        .windows(3)
        .map(|w| w.iter().collect::<String>())
        .collect()
}

/// 计算 trigram 相似度（Jaccard 系数）
fn trigram_similarity(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        return 0.0;
    }
    intersection / union
}

/// 计算两个向量的余弦相似度（f64 输出）
fn cosine_similarity_f64(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
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

/// 合并两条搜索路径的结果
///
/// 使用 content 作为键，取最大分值，两条路径都匹配时标记为 Hybrid
fn merge_search_results(
    keyword: Vec<SearchResult>,
    vector: Vec<SearchResult>,
) -> Vec<SearchResult> {
    let mut map: HashMap<String, SearchResult> = HashMap::new();
    for r in keyword {
        map.entry(r.content.clone())
            .and_modify(|existing| {
                existing.score = existing.score.max(r.score);
                existing.source = SearchSource::Hybrid;
            })
            .or_insert(r);
    }
    for r in vector {
        map.entry(r.content.clone())
            .and_modify(|existing| {
                existing.score = existing.score.max(r.score);
                existing.source = SearchSource::Hybrid;
            })
            .or_insert(r);
    }
    map.into_values().collect()
}

/// 时间衰减（指数衰减）
///
/// half_life_hours: 半衰期（小时），年龄每增加一个半衰期，分数减半
fn apply_time_decay(results: &mut [SearchResult], half_life_hours: f64) {
    let now = chrono::Utc::now();
    for r in results.iter_mut() {
        if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&r.created_at) {
            let age_hours =
                (now - created.with_timezone(&chrono::Utc)).num_seconds() as f64 / 3600.0;
            let decay = (-0.693 * age_hours / half_life_hours).exp(); // ln(2) ≈ 0.693
            r.score *= decay;
        }
    }
}

/// 来源权重（关键词0.3 + 向量0.7，Hybrid 取1.0）
fn apply_source_weights(results: &mut [SearchResult]) {
    for r in results.iter_mut() {
        let weight = match r.source {
            SearchSource::Keyword => 0.3,
            SearchSource::Vector => 0.7,
            SearchSource::Hybrid => 1.0,
        };
        r.score *= weight;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize() {
        let tokens = tokenize("Rust 是一门系统编程语言!");
        assert!(tokens.contains(&"rust".to_string()));
        assert!(tokens.contains(&"系统编程语言".to_string()));
    }

    #[test]
    fn test_keyword_similarity_full_match() {
        let query = tokenize("项目架构");
        let entry = tokenize("项目架构知识包括模块划分和依赖关系");
        let score = keyword_similarity(&query, &entry);
        assert!(score > 0.5, "应匹配大部分查询词，实际: {}", score);
    }

    #[test]
    fn test_keyword_similarity_no_match() {
        let query = tokenize("数据库连接池");
        let entry = tokenize("前端路由配置和样式主题");
        let score = keyword_similarity(&query, &entry);
        assert!(score < 0.3, "无匹配词应得低分，实际: {}", score);
    }

    #[test]
    fn test_compute_trigrams() {
        let trigrams = compute_trigrams("hello");
        assert!(trigrams.contains("hel"));
        assert!(trigrams.contains("ell"));
        assert!(trigrams.contains("llo"));
    }

    #[test]
    fn test_compute_trigrams_short() {
        let trigrams = compute_trigrams("ab");
        assert!(trigrams.contains("ab"));
    }

    #[test]
    fn test_compute_trigrams_empty() {
        let trigrams = compute_trigrams("");
        assert!(trigrams.is_empty());
    }

    #[test]
    fn test_trigram_similarity_identical() {
        let a = compute_trigrams("hello world");
        let b = compute_trigrams("hello world");
        let sim = trigram_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_trigram_similarity_no_overlap() {
        let a = compute_trigrams("abc");
        let b = compute_trigrams("xyz");
        let sim = trigram_similarity(&a, &b);
        assert!(sim < 0.01);
    }

    #[test]
    fn test_merge_search_results_dedup() {
        let keyword = vec![SearchResult {
            id: "1".into(),
            content: "shared content".into(),
            score: 0.5,
            source: SearchSource::Keyword,
            created_at: String::new(),
        }];
        let vector = vec![SearchResult {
            id: "2".into(),
            content: "shared content".into(),
            score: 0.8,
            source: SearchSource::Vector,
            created_at: String::new(),
        }];
        let merged = merge_search_results(keyword, vector);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].source, SearchSource::Hybrid);
        assert!((merged[0].score - 0.8).abs() < 1e-6); // 取最大分值
    }

    #[test]
    fn test_merge_search_results_distinct() {
        let keyword = vec![SearchResult {
            id: "1".into(),
            content: "keyword only".into(),
            score: 0.6,
            source: SearchSource::Keyword,
            created_at: String::new(),
        }];
        let vector = vec![SearchResult {
            id: "2".into(),
            content: "vector only".into(),
            score: 0.9,
            source: SearchSource::Vector,
            created_at: String::new(),
        }];
        let merged = merge_search_results(keyword, vector);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn test_apply_time_decay() {
        let now = chrono::Utc::now().to_rfc3339();
        let one_hour_ago = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();

        let mut results = vec![
            SearchResult {
                id: "1".into(),
                content: "fresh".into(),
                score: 1.0,
                source: SearchSource::Vector,
                created_at: now,
            },
            SearchResult {
                id: "2".into(),
                content: "old".into(),
                score: 1.0,
                source: SearchSource::Vector,
                created_at: one_hour_ago,
            },
        ];

        apply_time_decay(&mut results, 24.0); // 24 小时半衰期

        // 新条目分数接近 1.0，1小时前的条目分数略低
        assert!(results[0].score > results[1].score);
        assert!(results[0].score > 0.99);
        assert!(results[1].score > 0.9); // 1小时衰减很小
    }

    #[test]
    fn test_apply_time_decay_invalid_date() {
        let mut results = vec![SearchResult {
            id: "1".into(),
            content: "invalid date".into(),
            score: 1.0,
            source: SearchSource::Vector,
            created_at: "not-a-date".into(),
        }];

        apply_time_decay(&mut results, 24.0);
        // 无效日期不应修改分数
        assert!((results[0].score - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_apply_source_weights() {
        let mut results = vec![
            SearchResult {
                id: "1".into(),
                content: "keyword".into(),
                score: 1.0,
                source: SearchSource::Keyword,
                created_at: String::new(),
            },
            SearchResult {
                id: "2".into(),
                content: "vector".into(),
                score: 1.0,
                source: SearchSource::Vector,
                created_at: String::new(),
            },
            SearchResult {
                id: "3".into(),
                content: "hybrid".into(),
                score: 1.0,
                source: SearchSource::Hybrid,
                created_at: String::new(),
            },
        ];

        apply_source_weights(&mut results);

        assert!((results[0].score - 0.3).abs() < 1e-6);
        assert!((results[1].score - 0.7).abs() < 1e-6);
        assert!((results[2].score - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_f64_identical() {
        let a = vec![1.0_f32, 0.0, 0.0];
        let sim = cosine_similarity_f64(&a, &a);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_f64_orthogonal() {
        let a = vec![1.0_f32, 0.0, 0.0];
        let b = vec![0.0_f32, 1.0, 0.0];
        let sim = cosine_similarity_f64(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[tokio::test]
    async fn test_keyword_search() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ltm = LongTermMemory::with_dir(tmp.path().to_path_buf());

        ltm.store(KnowledgeEntry {
            id: "1".into(),
            category: KnowledgeCategory::Architecture,
            content: "项目使用 Rust 编写，采用模块化架构".into(),
            embedding: vec![],
            metadata: serde_json::json!({}),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        })
        .await
        .unwrap();

        ltm.store(KnowledgeEntry {
            id: "2".into(),
            category: KnowledgeCategory::CodePattern,
            content: "前端使用 React 和 TypeScript".into(),
            embedding: vec![],
            metadata: serde_json::json!({}),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        })
        .await
        .unwrap();

        let results = ltm.keyword_search("Rust 架构", 10).await.unwrap();
        assert!(!results.is_empty());
        assert!(results[0].content.contains("Rust"));
    }

    #[tokio::test]
    async fn test_vector_search() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ltm = LongTermMemory::with_dir(tmp.path().to_path_buf());

        ltm.store(KnowledgeEntry {
            id: "1".into(),
            category: KnowledgeCategory::Architecture,
            content: "项目架构信息".into(),
            embedding: vec![1.0, 0.0, 0.0],
            metadata: serde_json::json!({}),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        })
        .await
        .unwrap();

        ltm.store(KnowledgeEntry {
            id: "2".into(),
            category: KnowledgeCategory::CodePattern,
            content: "代码模式".into(),
            embedding: vec![0.0, 1.0, 0.0],
            metadata: serde_json::json!({}),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        })
        .await
        .unwrap();

        let query = vec![1.0, 0.0, 0.0];
        let results = ltm.vector_search(&query, 10).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "1");
    }

    #[tokio::test]
    async fn test_hybrid_search() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ltm = LongTermMemory::with_dir(tmp.path().to_path_buf());

        ltm.store(KnowledgeEntry {
            id: "1".into(),
            category: KnowledgeCategory::Architecture,
            content: "Rust 模块化架构设计".into(),
            embedding: vec![1.0, 0.0, 0.0],
            metadata: serde_json::json!({}),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        })
        .await
        .unwrap();

        let query_embedding = vec![1.0, 0.0, 0.0];
        let results = ltm
            .hybrid_search("Rust 架构", &query_embedding, 10, 168.0)
            .await
            .unwrap();
        assert!(!results.is_empty());
    }
}
