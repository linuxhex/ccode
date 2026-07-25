//! L2 长期记忆 - 跨会话知识持久化（本地 JSON 文件）
//!
//! 存储项目架构、决策记录、用户偏好等跨会话知识
//! 使用 ~/.ccode/memory/ 目录下的 JSON 文件进行持久化，基于关键词匹配实现检索

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
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
                if let Ok(knowledge) = serde_json::from_str::<KnowledgeEntry>(&content) {
                    entries.push(knowledge);
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
}
