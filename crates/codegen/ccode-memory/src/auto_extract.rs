//! 自动记忆提取
//!
//! 从对话消息中自动提取关键知识（决策、约束、纠正、偏好等），
//! 用于跨会话的知识持久化。超越 Claude Code 的简单关键词匹配，
//! 提供多策略提取管线：关键词匹配 → 正则模式匹配 → LLM 智能提取。
//!
//! ## 提取策略
//!
//! 1. **关键词匹配**（快速，低精度，置信度 ~0.6）
//! 2. **正则模式匹配**（中等精度，置信度 ~0.75）
//! 3. **LLM 提取**（慢，高精度，置信度由模型自评）
//!
//! LLM 提取由调用者通过 `build_llm_extraction_prompt` 构建 prompt，
//! 再通过 Sampler Node 完成实际调用，最后用 `parse_llm_response` 解析。

use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 核心类型
// ---------------------------------------------------------------------------

/// 知识类型
///
/// 将对话中的关键信息分类为不同语义类型，
/// 便于后续按类型检索和应用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum KnowledgeKind {
    /// 决策：用户或系统做出的选择
    Decision,
    /// 约束条件：必须遵守的限制
    Constraint,
    /// 已排除的方案：尝试后放弃的路径
    ExcludedApproach,
    /// 用户偏好：用户表达的倾向
    UserPreference,
    /// 错误纠正：对先前错误的修正
    Correction,
    /// 架构决策：影响系统结构的重要决定
    ArchitecturalDecision,
    /// 性能发现：性能相关的发现和优化
    PerformanceFinding,
    /// 安全约束：安全相关的限制
    SecurityConstraint,
    /// API 约定：API 设计的约定
    ApiConvention,
    /// 依赖关系：模块间的依赖信息
    Dependency,
}

/// 提取策略
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExtractionStrategy {
    /// 关键词匹配（快速，低精度）
    KeywordMatch,
    /// 正则模式匹配（中等精度）
    PatternMatch,
    /// LLM 提取（慢，高精度）
    LlmExtraction,
}

/// 提取的知识条目
///
/// 每条知识携带类型、内容、来源会话、提取时间、置信度、策略等，
/// 便于后续整合时去重、溯源和按置信度筛选。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedKnowledge {
    /// 知识类型
    pub kind: KnowledgeKind,
    /// 知识内容（原文片段或 LLM 摘要）
    pub content: String,
    /// 来源会话 ID
    pub source_session: String,
    /// 提取时间（ISO 8601）
    pub extracted_at: String,
    /// 置信度 0.0-1.0（关键词匹配低，LLM提取高）
    pub confidence: f64,
    /// 提取策略
    pub strategy: ExtractionStrategy,
    /// 上下文摘要（LLM 提取时提供）
    pub context_summary: Option<String>,
    /// 相关标签
    pub tags: Vec<String>,
}

/// 旧版知识条目（向后兼容）
///
/// 保留旧接口以兼容已有代码。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeItem {
    /// 知识类型
    pub kind: KnowledgeKind,
    /// 知识内容（原文片段）
    pub content: String,
    /// 来源会话 ID
    pub source_session: String,
    /// 提取时间（ISO 8601）
    pub extracted_at: String,
}

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// 提取器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionConfig {
    /// 启用的策略（按优先级排列）
    pub strategies: Vec<ExtractionStrategy>,
    /// 最低置信度阈值
    pub min_confidence: f64,
    /// 每条消息最大提取数
    pub max_extractions_per_message: usize,
    /// LLM 提取的 prompt 模板
    pub llm_prompt_template: String,
}

impl Default for ExtractionConfig {
    fn default() -> Self {
        Self {
            strategies: vec![
                ExtractionStrategy::KeywordMatch,
                ExtractionStrategy::PatternMatch,
                ExtractionStrategy::LlmExtraction,
            ],
            min_confidence: 0.5,
            max_extractions_per_message: 5,
            llm_prompt_template: include_str!("extraction_prompt.md").to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// LLM 提取内部类型
// ---------------------------------------------------------------------------

/// LLM 返回的单个提取项（用于反序列化）
#[derive(Debug, Deserialize)]
struct LlmExtractedItem {
    kind: KnowledgeKind,
    content: String,
    confidence: f64,
    tags: Vec<String>,
}

// ---------------------------------------------------------------------------
// 多策略知识提取器
// ---------------------------------------------------------------------------

/// 多策略知识提取器
///
/// 依次通过关键词匹配、正则模式匹配提取知识，
/// 并提供 LLM 提取的 prompt 构建和响应解析能力。
pub struct KnowledgeExtractor {
    config: ExtractionConfig,
    /// 关键词模式库（可扩展）
    keyword_patterns: HashMap<KnowledgeKind, Vec<String>>,
    /// 正则模式库
    regex_patterns: Vec<(KnowledgeKind, Regex)>,
}

impl KnowledgeExtractor {
    /// 创建新的提取器
    pub fn new(config: ExtractionConfig) -> Self {
        let keyword_patterns = Self::build_keyword_patterns();
        let regex_patterns = Self::build_regex_patterns();

        Self {
            config,
            keyword_patterns,
            regex_patterns,
        }
    }

    /// 使用默认配置创建提取器
    pub fn with_defaults() -> Self {
        Self::new(ExtractionConfig::default())
    }

    // -----------------------------------------------------------------------
    // 关键词模式
    // -----------------------------------------------------------------------

    fn build_keyword_patterns() -> HashMap<KnowledgeKind, Vec<String>> {
        let mut m = HashMap::new();

        m.insert(
            KnowledgeKind::Decision,
            vec![
                "决定".into(),
                "选择".into(),
                "确认".into(),
                "用这个方案".into(),
                "decided".into(),
                "chosen".into(),
                "we'll use".into(),
                "going with".into(),
            ],
        );

        m.insert(
            KnowledgeKind::Constraint,
            vec![
                "必须".into(),
                "不能".into(),
                "不要".into(),
                "禁止".into(),
                "must not".into(),
                "required".into(),
                "never".into(),
                "always".into(),
                "must be".into(),
            ],
        );

        m.insert(
            KnowledgeKind::Correction,
            vec![
                "不对".into(),
                "错了".into(),
                "应该是".into(),
                "不是这样".into(),
                "纠正".into(),
                "wrong".into(),
                "should be".into(),
                "actually".into(),
                "no, that's".into(),
            ],
        );

        m.insert(
            KnowledgeKind::UserPreference,
            vec![
                "更喜欢".into(),
                "希望".into(),
                "习惯".into(),
                "prefer".into(),
                "like".into(),
                "don't like".into(),
                "usually".into(),
            ],
        );

        m.insert(
            KnowledgeKind::ExcludedApproach,
            vec![
                "放弃".into(),
                "不行".into(),
                "失败".into(),
                "try another".into(),
                "doesn't work".into(),
                "gave up".into(),
            ],
        );

        m.insert(
            KnowledgeKind::ArchitecturalDecision,
            vec![
                "架构".into(),
                "模块".into(),
                "分层".into(),
                "architecture".into(),
                "module".into(),
                "layer".into(),
                "decoupled".into(),
            ],
        );

        m.insert(
            KnowledgeKind::PerformanceFinding,
            vec![
                "慢".into(),
                "性能".into(),
                "优化".into(),
                "slow".into(),
                "performance".into(),
                "bottleneck".into(),
                "latency".into(),
            ],
        );

        m.insert(
            KnowledgeKind::SecurityConstraint,
            vec![
                "安全".into(),
                "权限".into(),
                "沙箱".into(),
                "security".into(),
                "permission".into(),
                "auth".into(),
                "sandbox".into(),
            ],
        );

        m.insert(
            KnowledgeKind::ApiConvention,
            vec![
                "接口".into(),
                "格式".into(),
                "约定".into(),
                "API".into(),
                "convention".into(),
                "endpoint".into(),
                "RESTful".into(),
            ],
        );

        m.insert(
            KnowledgeKind::Dependency,
            vec![
                "依赖".into(),
                "引用".into(),
                "引入".into(),
                "depends on".into(),
                "requires".into(),
                "imports".into(),
            ],
        );

        m
    }

    // -----------------------------------------------------------------------
    // 正则模式
    // -----------------------------------------------------------------------

    fn build_regex_patterns() -> Vec<(KnowledgeKind, Regex)> {
        vec![
            // "必须X" / "X是必须的"
            (
                KnowledgeKind::Constraint,
                Regex::new(r"(?i)(must|需要|必须|should)\s+(use|be|have|使用|是|有)").unwrap(),
            ),
            // "不要X" / "禁止X"
            (
                KnowledgeKind::Constraint,
                Regex::new(r"(?i)(don't|never|禁止|不要|不能)\s+(use|do|使用|做)").unwrap(),
            ),
            // "选了X" / "决定X"
            (
                KnowledgeKind::Decision,
                Regex::new(r"(?i)(decided|chosen|选择|决定)\s+(to|on|使用|用)").unwrap(),
            ),
            // "X比Y好" / 偏好
            (
                KnowledgeKind::UserPreference,
                Regex::new(r"(?i)(prefer|比.*好|更好|prefer.*over)").unwrap(),
            ),
            // "应该是X" / "实际上X"
            (
                KnowledgeKind::Correction,
                Regex::new(r"(?i)(actually|should be|应该是|实际上|纠正)").unwrap(),
            ),
            // "放弃了X" / "X不行"
            (
                KnowledgeKind::ExcludedApproach,
                Regex::new(r"(?i)(doesn't work|not working|不行|放弃|failed)").unwrap(),
            ),
        ]
    }

    // -----------------------------------------------------------------------
    // 主提取管线
    // -----------------------------------------------------------------------

    /// 执行多策略提取
    ///
    /// 依次执行关键词匹配、正则模式匹配。LLM 提取需要外部调用，
    /// 通过 `build_llm_extraction_prompt` 和 `parse_llm_response` 完成。
    pub fn extract(
        &self,
        messages: &[&str],
        session_id: &str,
    ) -> Vec<ExtractedKnowledge> {
        let mut all_items = Vec::new();
        let now = chrono::Utc::now().to_rfc3339();

        for strategy in &self.config.strategies {
            match strategy {
                ExtractionStrategy::KeywordMatch => {
                    let items = self.extract_by_keywords(messages, session_id, &now);
                    all_items.extend(items);
                }
                ExtractionStrategy::PatternMatch => {
                    let items = self.extract_by_patterns(messages, session_id, &now);
                    all_items.extend(items);
                }
                ExtractionStrategy::LlmExtraction => {
                    // LLM 提取需要外部调用，这里只标记
                    // 实际调用由 ThinkerNode 通过 Sampler Node 完成
                    // 此处不执行 LLM 提取
                }
            }
        }

        // 按最低置信度过滤
        all_items.retain(|item| item.confidence >= self.config.min_confidence);

        // 去重：相同 kind + 相似 content 的只保留高置信度的
        all_items = self.deduplicate(all_items);

        // 限制每条消息的最大提取数
        all_items.truncate(self.config.max_extractions_per_message * messages.len().max(1));

        all_items
    }

    /// 关键词提取
    fn extract_by_keywords(
        &self,
        messages: &[&str],
        session_id: &str,
        now: &str,
    ) -> Vec<ExtractedKnowledge> {
        let mut items = Vec::new();

        for msg in messages {
            let msg_lower = msg.to_lowercase();
            for (kind, keywords) in &self.keyword_patterns {
                if keywords
                    .iter()
                    .any(|k| msg_lower.contains(&k.to_lowercase()))
                {
                    items.push(ExtractedKnowledge {
                        kind: kind.clone(),
                        content: msg.to_string(),
                        source_session: session_id.to_string(),
                        extracted_at: now.to_string(),
                        confidence: 0.6, // 关键词匹配置信度中等
                        strategy: ExtractionStrategy::KeywordMatch,
                        context_summary: None,
                        tags: vec![kind_tag(&kind)],
                    });
                }
            }
        }

        items
    }

    /// 正则模式提取
    fn extract_by_patterns(
        &self,
        messages: &[&str],
        session_id: &str,
        now: &str,
    ) -> Vec<ExtractedKnowledge> {
        let mut items = Vec::new();

        for msg in messages {
            for (kind, pattern) in &self.regex_patterns {
                if pattern.is_match(msg) {
                    items.push(ExtractedKnowledge {
                        kind: kind.clone(),
                        content: msg.to_string(),
                        source_session: session_id.to_string(),
                        extracted_at: now.to_string(),
                        confidence: 0.75, // 正则匹配置信度较高
                        strategy: ExtractionStrategy::PatternMatch,
                        context_summary: None,
                        tags: vec![kind_tag(&kind)],
                    });
                }
            }
        }

        items
    }

    /// 去重
    ///
    /// 相同 kind + content 前50字符相同时，只保留置信度更高的。
    fn deduplicate(&self, items: Vec<ExtractedKnowledge>) -> Vec<ExtractedKnowledge> {
        let mut seen: HashMap<(KnowledgeKind, String), usize> = HashMap::new();
        let mut result: Vec<ExtractedKnowledge> = Vec::new();

        for item in items {
            let key = (
                item.kind.clone(),
                item.content.chars().take(50).collect(),
            );
            if let Some(&existing_idx) = seen.get(&key) {
                // 保留置信度更高的
                if item.confidence > result[existing_idx].confidence {
                    result[existing_idx] = item;
                }
            } else {
                seen.insert(key, result.len());
                result.push(item);
            }
        }

        result
    }

    // -----------------------------------------------------------------------
    // LLM 提取（prompt 构建与响应解析）
    // -----------------------------------------------------------------------

    /// 构建 LLM 提取 prompt
    ///
    /// 将对话消息拼接为 LLM 提取 prompt，调用者通过 Sampler Node
    /// 将此 prompt 发送给 LLM。
    pub fn build_llm_extraction_prompt(&self, messages: &[&str]) -> String {
        let mut prompt = self.config.llm_prompt_template.clone();
        for (i, msg) in messages.iter().enumerate() {
            prompt.push_str(&format!("\n[{}] {}", i, msg));
        }
        prompt
    }

    /// 解析 LLM 提取响应
    ///
    /// LLM 返回 JSON 数组，解析为 ExtractedKnowledge 列表。
    /// 如果解析失败，返回空列表（不 panic）。
    pub fn parse_llm_response(
        &self,
        response: &str,
        session_id: &str,
    ) -> Vec<ExtractedKnowledge> {
        let json_str = extract_json_array(response);

        match serde_json::from_str::<Vec<LlmExtractedItem>>(&json_str) {
            Ok(items) => {
                let now = chrono::Utc::now().to_rfc3339();
                items
                    .into_iter()
                    .filter(|item| item.confidence >= self.config.min_confidence)
                    .map(|item| ExtractedKnowledge {
                        kind: item.kind,
                        content: item.content,
                        source_session: session_id.to_string(),
                        extracted_at: now.clone(),
                        confidence: item.confidence,
                        strategy: ExtractionStrategy::LlmExtraction,
                        context_summary: None,
                        tags: item.tags,
                    })
                    .collect()
            }
            Err(e) => {
                tracing::warn!("LLM extraction response parse error: {}", e);
                Vec::new()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 从 LLM 响应中提取 JSON 数组
fn extract_json_array(response: &str) -> String {
    // 尝试找到 ```json ... ``` 中的内容
    if let Some(start) = response.find("```json") {
        let after_start = start + 7;
        if let Some(end) = response[after_start..].find("```") {
            let json_end = after_start + end;
            if json_end > after_start {
                return response[after_start..json_end].trim().to_string();
            }
        }
    }

    // 尝试找到 [ ... ] 包裹的内容
    if let Some(start) = response.find('[') {
        if let Some(end) = response.rfind(']') {
            if end > start {
                return response[start..=end].to_string();
            }
        }
    }

    // 返回原始响应
    response.to_string()
}

/// 根据 KnowledgeKind 生成标签
fn kind_tag(kind: &KnowledgeKind) -> String {
    match kind {
        KnowledgeKind::Decision => "decision",
        KnowledgeKind::Constraint => "constraint",
        KnowledgeKind::ExcludedApproach => "excluded",
        KnowledgeKind::UserPreference => "preference",
        KnowledgeKind::Correction => "correction",
        KnowledgeKind::ArchitecturalDecision => "architecture",
        KnowledgeKind::PerformanceFinding => "performance",
        KnowledgeKind::SecurityConstraint => "security",
        KnowledgeKind::ApiConvention => "api",
        KnowledgeKind::Dependency => "dependency",
    }
    .to_string()
}

// ---------------------------------------------------------------------------
// 向后兼容的简单提取接口
// ---------------------------------------------------------------------------

/// 旧版简单提取函数（向后兼容）
///
/// 仅使用关键词匹配策略，返回旧版 KnowledgeItem 格式。
/// 新代码应使用 `KnowledgeExtractor`。
pub fn extract_knowledge_simple(
    messages: &[&str],
    session_id: &str,
) -> Vec<KnowledgeItem> {
    let extractor = KnowledgeExtractor::new(ExtractionConfig {
        strategies: vec![ExtractionStrategy::KeywordMatch],
        min_confidence: 0.0,
        max_extractions_per_message: 100,
        llm_prompt_template: String::new(),
    });

    extractor
        .extract(messages, session_id)
        .into_iter()
        .map(|ek| KnowledgeItem {
            kind: ek.kind,
            content: ek.content,
            source_session: ek.source_session,
            extracted_at: ek.extracted_at,
        })
        .collect()
}

/// 向后兼容：保留原函数名
pub fn extract_knowledge(messages: &[&str], session_id: &str) -> Vec<KnowledgeItem> {
    extract_knowledge_simple(messages, session_id)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // =======================================================================
    // 关键词提取测试
    // =======================================================================

    #[test]
    fn test_keyword_decision_chinese() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["我们决定使用 Rust 实现"], "s1");
        assert!(items
            .iter()
            .any(|i| i.kind == KnowledgeKind::Decision && i.strategy == ExtractionStrategy::KeywordMatch));
    }

    #[test]
    fn test_keyword_decision_english() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["We decided to use PostgreSQL"], "s1");
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::Decision));
    }

    #[test]
    fn test_keyword_constraint() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["必须使用 UTF-8 编码"], "s1");
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::Constraint));
    }

    #[test]
    fn test_keyword_correction() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["不对，应该是 42"], "s1");
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::Correction));
    }

    #[test]
    fn test_keyword_user_preference() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["我更喜欢用 TypeScript"], "s1");
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::UserPreference));
    }

    #[test]
    fn test_keyword_excluded_approach() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["这个方案不行，放弃"], "s1");
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::ExcludedApproach));
    }

    #[test]
    fn test_keyword_architectural_decision() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["架构采用微服务分层设计"], "s1");
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::ArchitecturalDecision));
    }

    #[test]
    fn test_keyword_performance_finding() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["性能瓶颈在数据库查询"], "s1");
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::PerformanceFinding));
    }

    #[test]
    fn test_keyword_security_constraint() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["安全权限必须严格控制"], "s1");
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::SecurityConstraint));
    }

    #[test]
    fn test_keyword_api_convention() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["接口约定使用 RESTful 风格"], "s1");
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::ApiConvention));
    }

    #[test]
    fn test_keyword_dependency() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["这个模块依赖 redis 库"], "s1");
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::Dependency));
    }

    #[test]
    fn test_keyword_no_match() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["今天天气不错"], "s1");
        assert!(items.is_empty());
    }

    // =======================================================================
    // 正则模式提取测试
    // =======================================================================

    #[test]
    fn test_regex_constraint_must() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["We must use Rust for this module"], "s1");
        assert!(items
            .iter()
            .any(|i| i.kind == KnowledgeKind::Constraint && i.strategy == ExtractionStrategy::PatternMatch));
    }

    #[test]
    fn test_regex_constraint_dont() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["Don't use global variables"], "s1");
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::Constraint));
    }

    #[test]
    fn test_regex_decision() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["We decided to use GraphQL"], "s1");
        // Both keyword and regex may match, dedup keeps higher confidence
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::Decision));
    }

    #[test]
    fn test_regex_preference() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["I prefer async/await over callbacks"], "s1");
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::UserPreference));
    }

    #[test]
    fn test_regex_correction() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["Actually the port should be 8080"], "s1");
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::Correction));
    }

    #[test]
    fn test_regex_excluded() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(&["That approach doesn't work here"], "s1");
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::ExcludedApproach));
    }

    // =======================================================================
    // LLM 提取测试
    // =======================================================================

    #[test]
    fn test_llm_prompt_building() {
        let ext = KnowledgeExtractor::with_defaults();
        let prompt = ext.build_llm_extraction_prompt(&["Hello", "World"]);
        assert!(prompt.contains("[0] Hello"));
        assert!(prompt.contains("[1] World"));
    }

    #[test]
    fn test_llm_response_parsing_json_block() {
        let ext = KnowledgeExtractor::with_defaults();
        let response = r#"
Here are the extracted items:

```json
[
  {"kind": "Constraint", "content": "Must use HTTPS", "confidence": 0.9, "tags": ["security", "network"]},
  {"kind": "Decision", "content": "Using Redis for caching", "confidence": 0.85, "tags": ["cache", "redis"]}
]
```
"#;
        let items = ext.parse_llm_response(response, "session-42");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, KnowledgeKind::Constraint);
        assert_eq!(items[0].content, "Must use HTTPS");
        assert!((items[0].confidence - 0.9).abs() < f64::EPSILON);
        assert_eq!(items[0].strategy, ExtractionStrategy::LlmExtraction);
        assert_eq!(items[0].source_session, "session-42");
        assert_eq!(items[1].kind, KnowledgeKind::Decision);
    }

    #[test]
    fn test_llm_response_parsing_raw_array() {
        let ext = KnowledgeExtractor::with_defaults();
        let response = r#"[{"kind": "Decision", "content": "Chose React", "confidence": 0.8, "tags": ["frontend"]}]"#;
        let items = ext.parse_llm_response(response, "s1");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, KnowledgeKind::Decision);
    }

    #[test]
    fn test_llm_response_parsing_invalid() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.parse_llm_response("this is not JSON", "s1");
        assert!(items.is_empty());
    }

    #[test]
    fn test_llm_response_confidence_filtering() {
        let ext = KnowledgeExtractor::new(ExtractionConfig {
            min_confidence: 0.8,
            ..ExtractionConfig::default()
        });
        let response = r#"[{"kind": "Decision", "content": "Low confidence item", "confidence": 0.3, "tags": []}]"#;
        let items = ext.parse_llm_response(response, "s1");
        assert!(items.is_empty());
    }

    // =======================================================================
    // 去重测试
    // =======================================================================

    #[test]
    fn test_deduplicate_keeps_higher_confidence() {
        let ext = KnowledgeExtractor::with_defaults();
        let now = chrono::Utc::now().to_rfc3339();

        let items = vec![
            ExtractedKnowledge {
                kind: KnowledgeKind::Decision,
                content: "We decided to use Rust for the entire backend service layer".into(),
                source_session: "s1".into(),
                extracted_at: now.clone(),
                confidence: 0.6,
                strategy: ExtractionStrategy::KeywordMatch,
                context_summary: None,
                tags: vec!["decision".into()],
            },
            ExtractedKnowledge {
                kind: KnowledgeKind::Decision,
                content: "We decided to use Rust for the entire backend service layer - confirmed".into(),
                source_session: "s1".into(),
                extracted_at: now.clone(),
                confidence: 0.75,
                strategy: ExtractionStrategy::PatternMatch,
                context_summary: None,
                tags: vec!["decision".into()],
            },
        ];

        let deduped = ext.deduplicate(items);
        // Both have same kind + same first 50 chars prefix, so dedup keeps higher
        assert_eq!(deduped.len(), 1);
        assert!((deduped[0].confidence - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn test_deduplicate_different_kinds_no_merge() {
        let ext = KnowledgeExtractor::with_defaults();
        let now = chrono::Utc::now().to_rfc3339();

        let items = vec![
            ExtractedKnowledge {
                kind: KnowledgeKind::Decision,
                content: "We decided to use Rust".into(),
                source_session: "s1".into(),
                extracted_at: now.clone(),
                confidence: 0.6,
                strategy: ExtractionStrategy::KeywordMatch,
                context_summary: None,
                tags: vec!["decision".into()],
            },
            ExtractedKnowledge {
                kind: KnowledgeKind::Constraint,
                content: "We decided to use Rust".into(),
                source_session: "s1".into(),
                extracted_at: now.clone(),
                confidence: 0.6,
                strategy: ExtractionStrategy::KeywordMatch,
                context_summary: None,
                tags: vec!["constraint".into()],
            },
        ];

        let deduped = ext.deduplicate(items);
        assert_eq!(deduped.len(), 2);
    }

    // =======================================================================
    // 置信度过滤测试
    // =======================================================================

    #[test]
    fn test_min_confidence_filtering() {
        let ext = KnowledgeExtractor::new(ExtractionConfig {
            strategies: vec![ExtractionStrategy::KeywordMatch],
            min_confidence: 0.7, // keyword is 0.6, so should be filtered
            max_extractions_per_message: 100,
            llm_prompt_template: String::new(),
        });
        let items = ext.extract(&["我们决定使用 Rust"], "s1");
        assert!(items.is_empty());
    }

    #[test]
    fn test_min_confidence_allows_pattern_match() {
        let ext = KnowledgeExtractor::new(ExtractionConfig {
            strategies: vec![ExtractionStrategy::PatternMatch],
            min_confidence: 0.7, // pattern is 0.75, should pass
            max_extractions_per_message: 100,
            llm_prompt_template: String::new(),
        });
        let items = ext.extract(&["We decided to use Rust"], "s1");
        assert!(!items.is_empty());
    }

    // =======================================================================
    // 多策略管线测试
    // =======================================================================

    #[test]
    fn test_multi_strategy_pipeline() {
        let ext = KnowledgeExtractor::with_defaults();
        let items = ext.extract(
            &[
                "我们决定使用 Rust",
                "We must use HTTPS for all endpoints",
                "普通消息",
            ],
            "s1",
        );
        // Should have extracted from both keyword and pattern strategies
        assert!(!items.is_empty());
        // Check kinds are present
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::Decision));
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::Constraint));
    }

    #[test]
    fn test_extraction_respects_max_per_message() {
        let ext = KnowledgeExtractor::new(ExtractionConfig {
            strategies: vec![ExtractionStrategy::KeywordMatch],
            min_confidence: 0.0,
            max_extractions_per_message: 1,
            llm_prompt_template: String::new(),
        });
        // One message with many keywords
        let items = ext.extract(
            &["决定选择确认必须不能禁止不对错了应该是架构性能安全权限接口依赖"],
            "s1",
        );
        // Should be truncated to max_extractions_per_message * messages.len() = 1 * 1 = 1
        assert!(items.len() <= 1);
    }

    // =======================================================================
    // 向后兼容测试
    // =======================================================================

    #[test]
    fn test_extract_knowledge_backward_compat() {
        let items = extract_knowledge(&["我们决定使用 Rust 实现"], "session-1");
        assert!(items.iter().any(|i| matches!(i.kind, KnowledgeKind::Decision)));
    }

    #[test]
    fn test_extract_knowledge_simple_backward_compat() {
        let items = extract_knowledge_simple(&["必须使用 UTF-8 编码"], "session-1");
        assert!(items.iter().any(|i| matches!(i.kind, KnowledgeKind::Constraint)));
    }

    // =======================================================================
    // extract_json_array 辅助函数测试
    // =======================================================================

    #[test]
    fn test_extract_json_array_from_code_block() {
        let response = "```json\n[{\"a\": 1}]\n```";
        assert_eq!(extract_json_array(response), "[{\"a\": 1}]");
    }

    #[test]
    fn test_extract_json_array_from_brackets() {
        let response = "Result: [{\"a\": 1}]";
        assert_eq!(extract_json_array(response), "[{\"a\": 1}]");
    }

    #[test]
    fn test_extract_json_array_fallback() {
        let response = "no json here";
        assert_eq!(extract_json_array(response), "no json here");
    }

    // =======================================================================
    // kind_tag 测试
    // =======================================================================

    #[test]
    fn test_kind_tag_values() {
        assert_eq!(kind_tag(&KnowledgeKind::Decision), "decision");
        assert_eq!(kind_tag(&KnowledgeKind::Constraint), "constraint");
        assert_eq!(kind_tag(&KnowledgeKind::ExcludedApproach), "excluded");
        assert_eq!(kind_tag(&KnowledgeKind::UserPreference), "preference");
        assert_eq!(kind_tag(&KnowledgeKind::Correction), "correction");
        assert_eq!(kind_tag(&KnowledgeKind::ArchitecturalDecision), "architecture");
        assert_eq!(kind_tag(&KnowledgeKind::PerformanceFinding), "performance");
        assert_eq!(kind_tag(&KnowledgeKind::SecurityConstraint), "security");
        assert_eq!(kind_tag(&KnowledgeKind::ApiConvention), "api");
        assert_eq!(kind_tag(&KnowledgeKind::Dependency), "dependency");
    }
}
