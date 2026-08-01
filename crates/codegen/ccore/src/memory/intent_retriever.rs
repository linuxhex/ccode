//! 意图检索（借鉴 Augment Context Engine Retriever-XL 设计）
//!
//! 在向量检索前加一步意图理解：
//! 1. 用户查询 → LLM 展开为多维度检索意图
//! 2. 每个意图独立检索
//! 3. 合并去重 + MMR 多样性排序
//!
//! 与 Augment Retriever-XL 区别：
//! - Retriever-XL: 专用检索模型（训练于代码意图-代码对）
//! - ccode: LLM 意图展开 + 通用向量检索 + MMR

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::embedding::EmbeddingIndex;
use super::function_embed::CodeBlock;
use super::repo_map::RepoMap;
use std::path::PathBuf;

/// 检索意图
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalIntent {
    /// 意图描述
    pub description: String,
    /// 搜索关键词
    pub keywords: Vec<String>,
    /// 期望的代码块类型
    pub expected_kinds: Vec<String>,
    /// 权重（0.0-1.0）
    pub weight: f64,
}

/// 检索结果
#[derive(Debug, Clone)]
pub struct RetrievalResult {
    /// 代码块 ID
    pub block_id: String,
    /// 文件路径
    pub file_path: PathBuf,
    /// 符号名
    pub name: String,
    /// 相关度分数
    pub relevance_score: f64,
    /// 来源意图
    pub source_intent: String,
    /// 代码预览
    pub preview: String,
}

/// 意图检索器
pub struct IntentRetriever {
    /// 嵌入索引
    embedding_index: EmbeddingIndex,
    /// 代码块表（block_id → CodeBlock）
    code_blocks: HashMap<String, CodeBlock>,
    /// Repo Map（用于依赖链扩展）
    repo_map: Option<RepoMap>,
}

impl IntentRetriever {
    /// 创建新的意图检索器
    pub fn new(embedding_index: EmbeddingIndex) -> Self {
        Self {
            embedding_index,
            code_blocks: HashMap::new(),
            repo_map: None,
        }
    }

    /// 设置 Repo Map
    pub fn set_repo_map(&mut self, repo_map: RepoMap) {
        self.repo_map = Some(repo_map);
    }

    /// 注册代码块
    pub fn register_block(&mut self, block: CodeBlock) {
        self.code_blocks.insert(block.id.clone(), block);
    }

    /// 批量注册代码块
    pub fn register_blocks(&mut self, blocks: Vec<CodeBlock>) {
        for block in blocks {
            self.register_block(block);
        }
    }

    /// 基于意图列表检索
    ///
    /// 对每个意图独立检索 top-k，合并后用 MMR 去重排序
    pub fn search_by_intents(
        &self,
        intents: &[RetrievalIntent],
        query_embedding: &[f32],
        top_k: usize,
    ) -> Vec<RetrievalResult> {
        let mut all_results: Vec<RetrievalResult> = Vec::new();

        for intent in intents {
            let k = (top_k as f64 * intent.weight).max(3.0) as usize;
            let raw = self.embedding_index.search(query_embedding, k);

            for (idx, score) in raw {
                let vector = &self.embedding_index.vectors()[idx];
                if let Some(block) = self.code_blocks.get(&vector.entry_id) {
                    all_results.push(RetrievalResult {
                        block_id: block.id.clone(),
                        file_path: block.file_path.clone(),
                        name: block.name.clone(),
                        relevance_score: (score as f64) * intent.weight,
                        source_intent: intent.description.clone(),
                        preview: block.source.chars().take(200).collect(),
                    });
                }
            }
        }

        // 去重（同一代码块可能被多个意图命中）
        let mut seen = std::collections::HashSet::new();
        all_results.retain(|r| seen.insert(r.block_id.clone()));

        // 按相关度排序
        all_results.sort_by(|a, b| b.relevance_score.partial_cmp(&a.relevance_score).unwrap_or(std::cmp::Ordering::Equal));

        all_results.into_iter().take(top_k).collect()
    }

    /// 意图展开：将用户查询展开为多个检索意图
    ///
    /// 规则化意图展开（无需 LLM 调用）：
    /// 1. 原始查询作为主意图
    /// 2. 提取关键词作为精确匹配意图
    /// 3. 识别代码实体模式（函数/类/模块）作为类型意图
    /// 4. 依赖链扩展（如果命中文件，扩展其依赖）
    pub fn expand_intents(query: &str) -> Vec<RetrievalIntent> {
        let mut intents = Vec::new();

        // 1. 主意图：原始查询
        intents.push(RetrievalIntent {
            description: format!("主查询: {}", query),
            keywords: query.split_whitespace().map(|s| s.to_lowercase()).collect(),
            expected_kinds: vec![],
            weight: 1.0,
        });

        // 2. 关键词意图：提取 CamelCase 和 snake_case 组件
        let components = Self::extract_name_components(query);
        if !components.is_empty() {
            intents.push(RetrievalIntent {
                description: "名称组件匹配".to_string(),
                keywords: components.clone(),
                expected_kinds: vec![],
                weight: 0.7,
            });

            // 3. 组合搜索（如 "User Model" → "UserModel", "user_model"）
            let combined_camel: String = components.iter().map(|c| {
                let mut chars = c.chars();
                chars.next().map(|f| f.to_uppercase().to_string()).unwrap_or_default()
                    + &chars.as_str().to_lowercase()
            }).collect();
            let combined_snake = components.join("_").to_lowercase();

            intents.push(RetrievalIntent {
                description: "组合名称匹配".to_string(),
                keywords: vec![combined_camel, combined_snake],
                expected_kinds: vec![],
                weight: 0.6,
            });
        }

        // 4. 类型意图：识别代码实体关键词
        let type_intents = Self::detect_type_intents(query);
        intents.extend(type_intents);

        intents
    }

    /// 基于依赖链扩展检索结果
    ///
    /// 对命中的代码块，额外检索其依赖的文件中的代码块
    pub fn expand_with_dependencies(
        &self,
        results: &[RetrievalResult],
        max_additional: usize,
    ) -> Vec<RetrievalResult> {
        let repo_map = match &self.repo_map {
            Some(m) => m,
            None => return Vec::new(),
        };

        let mut additional = Vec::new();
        let mut seen_files = std::collections::HashSet::new();

        for result in results {
            seen_files.insert(result.file_path.clone());
        }

        for result in results {
            // 查找该文件的所有依赖
            let deps = repo_map.all_dependencies(&result.file_path);
            for dep_path in deps {
                if seen_files.contains(&dep_path) {
                    continue;
                }
                seen_files.insert(dep_path.clone());

                // 将依赖文件中的代码块加入结果
                for block in self.code_blocks.values() {
                    if block.file_path == dep_path && additional.len() < max_additional {
                        additional.push(RetrievalResult {
                            block_id: block.id.clone(),
                            file_path: block.file_path.clone(),
                            name: block.name.clone(),
                            relevance_score: result.relevance_score * 0.5,
                            source_intent: format!("依赖链扩展: {}", result.name),
                            preview: block.source.chars().take(200).collect(),
                        });
                    }
                }
            }
        }

        additional
    }

    /// 获取所有代码块数量
    pub fn block_count(&self) -> usize {
        self.code_blocks.len()
    }

    // ---- 内部方法 ----

    fn extract_name_components(query: &str) -> Vec<String> {
        let mut components = Vec::new();
        for word in query.split_whitespace() {
            // CamelCase 拆分
            let mut current = String::new();
            for ch in word.chars() {
                if ch.is_uppercase() && !current.is_empty() {
                    components.push(current.to_lowercase());
                    current = ch.to_string();
                } else {
                    current.push(ch);
                }
            }
            if !current.is_empty() {
                components.push(current.to_lowercase());
            }

            // snake_case 拆分
            if word.contains('_') {
                for part in word.split('_') {
                    let lower = part.to_lowercase();
                    if !components.contains(&lower) && !lower.is_empty() {
                        components.push(lower);
                    }
                }
            }
        }
        components.dedup();
        components
    }

    fn detect_type_intents(query: &str) -> Vec<RetrievalIntent> {
        let mut intents = Vec::new();
        let lower = query.to_lowercase();

        if lower.contains("函数") || lower.contains("function") || lower.contains("fn ") {
            intents.push(RetrievalIntent {
                description: "函数搜索".to_string(),
                keywords: vec![],
                expected_kinds: vec!["function".to_string(), "method".to_string()],
                weight: 0.5,
            });
        }

        if lower.contains("类") || lower.contains("class") || lower.contains("struct") {
            intents.push(RetrievalIntent {
                description: "类/结构体搜索".to_string(),
                keywords: vec![],
                expected_kinds: vec!["class".to_string()],
                weight: 0.5,
            });
        }

        if lower.contains("测试") || lower.contains("test") {
            intents.push(RetrievalIntent {
                description: "测试搜索".to_string(),
                keywords: vec![],
                expected_kinds: vec!["test".to_string()],
                weight: 0.4,
            });
        }

        if lower.contains("接口") || lower.contains("trait") || lower.contains("interface") {
            intents.push(RetrievalIntent {
                description: "接口/Trait 搜索".to_string(),
                keywords: vec![],
                expected_kinds: vec!["trait".to_string()],
                weight: 0.4,
            });
        }

        intents
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_intents_basic() {
        let intents = IntentRetriever::expand_intents("User login function");
        assert!(!intents.is_empty());
        assert!(intents[0].description.contains("主查询"));
    }

    #[test]
    fn test_expand_intents_camel_case() {
        let intents = IntentRetriever::expand_intents("UserModel");
        let combined = intents.iter().find(|i| i.description.contains("组合"));
        assert!(combined.is_some());
    }

    #[test]
    fn test_expand_intents_type_detection() {
        let intents = IntentRetriever::expand_intents("查找登录函数");
        let fn_intent = intents.iter().find(|i| i.description.contains("函数搜索"));
        assert!(fn_intent.is_some());
    }

    #[test]
    fn test_extract_name_components() {
        let components = IntentRetriever::extract_name_components("UserModel login_handler");
        assert!(components.contains(&"user".to_string()));
        assert!(components.contains(&"model".to_string()));
        assert!(components.contains(&"login".to_string()));
        assert!(components.contains(&"handler".to_string()));
    }
}
