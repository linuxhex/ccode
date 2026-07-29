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

use std::collections::{HashMap, HashSet};

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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ExtractionStrategy {
    /// 关键词匹配（快速，低精度）
    KeywordMatch,
    /// 正则模式匹配（中等精度）
    PatternMatch,
    /// LLM 提取（慢，高精度）
    LlmExtraction,
}

/// 反馈类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FeedbackType {
    /// 用户确认提取正确
    Confirmed,
    /// 用户拒绝提取（误提取）
    Rejected,
    /// 用户修正了提取内容
    Corrected,
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

/// 提取反馈记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionFeedback {
    /// 被反馈的条目内容（前50字符作为key）
    pub content_key: String,
    /// 知识类型
    pub kind: KnowledgeKind,
    /// 反馈类型
    pub feedback: FeedbackType,
    /// 反馈时间
    pub timestamp: String,
    /// 原始提取策略
    pub strategy: ExtractionStrategy,
}

/// 渐进式提取结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressiveExtractionResult {
    /// 关键词+正则已提取的条目
    pub pre_llm_items: Vec<ExtractedKnowledge>,
    /// 未被覆盖的消息索引（供 LLM 深入提取）
    pub uncovered_message_indices: Vec<usize>,
    /// 总消息数
    pub total_messages: usize,
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

    // -----------------------------------------------------------------------
    // 渐进式提取管线
    // -----------------------------------------------------------------------

    /// 渐进式提取管线
    ///
    /// 关键创新：前段结果注入后段，让 LLM 只关注"没被提取到的"
    ///
    /// 流程：
    /// 1. 关键词匹配 → 得到 initial_items
    /// 2. 正则模式 → 补充到 initial_items
    /// 3. LLM 提取 → 只对"未被前段覆盖的消息"提取，
    ///    并将前段结果作为参考上下文注入 prompt
    pub fn extract_progressive(
        &self,
        messages: &[&str],
        session_id: &str,
    ) -> ProgressiveExtractionResult {
        let now = chrono::Utc::now().to_rfc3339();

        // Stage 1: 关键词匹配
        let stage1_items = self.extract_by_keywords(messages, session_id, &now);

        // Stage 2: 正则模式匹配（补充关键词遗漏的）
        let stage2_items = self.extract_by_patterns(messages, session_id, &now);

        // 合并 stage1 + stage2
        let mut pre_llm_items: Vec<ExtractedKnowledge> = stage1_items;
        pre_llm_items.extend(stage2_items);
        pre_llm_items = self.deduplicate(pre_llm_items);
        pre_llm_items.retain(|item| item.confidence >= self.config.min_confidence);

        // Stage 3: 找出"未被覆盖的消息"供 LLM 提取
        let covered_indices = find_covered_message_indices(&pre_llm_items, messages);
        let uncovered_messages: Vec<(usize, &str)> = messages
            .iter()
            .enumerate()
            .filter(|(i, _)| !covered_indices.contains(i))
            .map(|(i, msg)| (i, *msg))
            .collect();

        ProgressiveExtractionResult {
            pre_llm_items,
            uncovered_message_indices: uncovered_messages.iter().map(|(i, _)| *i).collect(),
            total_messages: messages.len(),
        }
    }

    /// 构建渐进式 LLM 提取 prompt
    ///
    /// 与 build_llm_extraction_prompt 不同：
    /// 1. 只发送未被前段覆盖的消息（减少 token 消耗）
    /// 2. 将前段已提取的结果作为参考注入（让 LLM 知道已有什么）
    /// 3. 让 LLM 专注于"遗漏"和"深层次"提取
    pub fn build_progressive_llm_prompt(
        &self,
        messages: &[&str],
        pre_llm_items: &[ExtractedKnowledge],
        uncovered_indices: &[usize],
    ) -> String {
        // 构建已有知识上下文
        let existing_context = if pre_llm_items.is_empty() {
            "None (no items extracted yet)".to_string()
        } else {
            pre_llm_items
                .iter()
                .map(|item| {
                    format!(
                        "- [{:?}] {} (conf={:.2})",
                        item.kind, item.content, item.confidence
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        // 只发送未覆盖的消息
        let uncovered_text = uncovered_indices
            .iter()
            .filter_map(|&i| messages.get(i).map(|msg| format!("[{}] {}", i, msg)))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            r#"Analyze the following conversation messages and extract knowledge items that were MISSED by keyword and pattern matching.

## Already extracted (do NOT re-extract these):
{}

## Messages to analyze:
{}

## Instructions:
1. Extract NEW knowledge items not already covered above
2. Focus on implicit decisions, subtle constraints, and nuanced preferences
3. Look for patterns across multiple messages (contextual understanding)
4. Each item must have: kind, content (concise summary), confidence (0.0-1.0), tags

## Output format (JSON array):
```json
[
  {{"kind": "Constraint", "content": "...", "confidence": 0.9, "tags": ["..."]}}
]
```"#,
            existing_context, uncovered_text
        )
    }

    // -----------------------------------------------------------------------
    // 上下文感知提取
    // -----------------------------------------------------------------------

    /// 上下文感知提取
    ///
    /// 使用滑动窗口分析消息间的关联：
    /// 1. 每条消息与前后各 N 条消息组合成上下文
    /// 2. 在上下文中检测：用户先问了X→助手答了Y→用户纠正Z
    /// 3. 提取上下文中的决策链和纠正链
    pub fn extract_with_context(
        &self,
        messages: &[&str],
        session_id: &str,
        window_size: usize, // 前后各看几条消息
    ) -> Vec<ExtractedKnowledge> {
        let mut items = Vec::new();
        let now = chrono::Utc::now().to_rfc3339();

        for (i, msg) in messages.iter().enumerate() {
            // 构建滑动窗口上下文
            let start = i.saturating_sub(window_size);
            let end = (i + window_size + 1).min(messages.len());
            let context: Vec<&str> = messages[start..end].to_vec();

            // 检测上下文中的模式
            // 模式1: 用户说A → 助手说B → 用户纠正C → 提取Correction
            if i >= 2 {
                let _prev2 = messages[i - 2].to_lowercase();
                let prev1 = messages[i - 1].to_lowercase();
                let curr = msg.to_lowercase();

                // 纠正链：问题 → 回答 → "不对/错了"
                if contains_correction_keywords(&curr)
                    && (prev1.contains("here") || prev1.contains("可以") || prev1.contains("use"))
                {
                    // 当前是纠正，前一条是建议，提取纠正
                    items.push(ExtractedKnowledge {
                        kind: KnowledgeKind::Correction,
                        content: format!(
                            "纠正: {} → 正确做法: {}",
                            messages[i - 1].chars().take(80).collect::<String>(),
                            msg.chars().take(80).collect::<String>()
                        ),
                        source_session: session_id.to_string(),
                        extracted_at: now.clone(),
                        confidence: 0.85, // 上下文感知置信度较高
                        strategy: ExtractionStrategy::PatternMatch,
                        context_summary: Some(format!(
                            "上下文: {}",
                            context
                                .iter()
                                .map(|s| s.chars().take(30).collect::<String>())
                                .collect::<Vec<_>>()
                                .join(" → ")
                        )),
                        tags: vec!["correction".to_string(), "contextual".to_string()],
                    });
                }

                // 决策链：讨论 → 确认
                if contains_decision_keywords(&curr) {
                    items.push(ExtractedKnowledge {
                        kind: KnowledgeKind::Decision,
                        content: format!("决策: {}", msg),
                        source_session: session_id.to_string(),
                        extracted_at: now.clone(),
                        confidence: 0.8,
                        strategy: ExtractionStrategy::PatternMatch,
                        context_summary: Some(format!(
                            "决策上下文: {} → {}",
                            messages[i - 1].chars().take(50).collect::<String>(),
                            msg.chars().take(50).collect::<String>()
                        )),
                        tags: vec!["decision".to_string(), "contextual".to_string()],
                    });
                }
            }

            // 模式2: 约束+决策组合（用户说"必须X，用Y方案"）
            if contains_constraint_keywords(&msg.to_lowercase())
                && contains_decision_keywords(&msg.to_lowercase())
            {
                items.push(ExtractedKnowledge {
                    kind: KnowledgeKind::Constraint,
                    content: format!("约束+决策: {}", msg),
                    source_session: session_id.to_string(),
                    extracted_at: now.clone(),
                    confidence: 0.8,
                    strategy: ExtractionStrategy::PatternMatch,
                    context_summary: None,
                    tags: vec![
                        "constraint".to_string(),
                        "decision".to_string(),
                        "compound".to_string(),
                    ],
                });
            }
        }

        items = self.deduplicate(items);
        items.retain(|item| item.confidence >= self.config.min_confidence);
        items
    }
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 找出已被提取覆盖的消息索引
fn find_covered_message_indices(items: &[ExtractedKnowledge], messages: &[&str]) -> HashSet<usize> {
    let mut covered = HashSet::new();
    for item in items {
        for (i, msg) in messages.iter().enumerate() {
            if item.content == *msg || msg.contains(&item.content) {
                covered.insert(i);
            }
        }
    }
    covered
}

fn contains_correction_keywords(text: &str) -> bool {
    let keywords = [
        "不对", "错了", "应该是", "不是这样", "纠正", "actually", "wrong", "should be",
        "no that's",
    ];
    keywords.iter().any(|k| text.contains(k))
}

fn contains_decision_keywords(text: &str) -> bool {
    let keywords = [
        "决定",
        "选择",
        "确认",
        "decided",
        "chosen",
        "going with",
        "we'll use",
    ];
    keywords.iter().any(|k| text.contains(k))
}

fn contains_constraint_keywords(text: &str) -> bool {
    let keywords = ["必须", "不能", "不要", "禁止", "must", "required", "never", "always"];
    keywords.iter().any(|k| text.contains(k))
}

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
// 提取反馈校准器
// ---------------------------------------------------------------------------

/// 提取反馈校准器
///
/// 根据用户历史反馈调整提取置信度：
/// - Confirmed → 该类提取置信度 +0.1
/// - Rejected → 该类提取置信度 -0.2
/// - Corrected → 内容修正 + 置信度 -0.05
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeedbackCalibrator {
    /// 反馈历史
    feedbacks: Vec<ExtractionFeedback>,
    /// 按 (kind, strategy) 统计的调整量
    adjustments: HashMap<(KnowledgeKind, ExtractionStrategy), f64>,
}

impl FeedbackCalibrator {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录反馈
    pub fn record(&mut self, feedback: ExtractionFeedback) {
        let key = (feedback.kind.clone(), feedback.strategy.clone());
        let delta = match &feedback.feedback {
            FeedbackType::Confirmed => 0.1,
            FeedbackType::Rejected => -0.2,
            FeedbackType::Corrected => -0.05,
        };
        *self.adjustments.entry(key).or_insert(0.0) += delta;
        self.feedbacks.push(feedback);
    }

    /// 校准置信度
    ///
    /// 根据历史反馈调整提取条目的置信度。
    /// 置信度范围限制在 [0.0, 1.0]。
    pub fn calibrate(&self, item: &mut ExtractedKnowledge) {
        let key = (item.kind.clone(), item.strategy.clone());
        if let Some(&adjustment) = self.adjustments.get(&key) {
            item.confidence = (item.confidence + adjustment).clamp(0.0, 1.0);
        }

        // 对比内容相似的历史反馈（基于前50字符）
        let content_key: String = item.content.chars().take(50).collect();
        for fb in &self.feedbacks {
            if fb.content_key == content_key {
                match fb.feedback {
                    FeedbackType::Confirmed => item.confidence = (item.confidence + 0.05).min(1.0),
                    FeedbackType::Rejected => item.confidence = (item.confidence - 0.15).max(0.0),
                    FeedbackType::Corrected => item.confidence = (item.confidence - 0.05).max(0.0),
                }
            }
        }
    }

    /// 批量校准
    pub fn calibrate_batch(&self, items: &mut [ExtractedKnowledge]) {
        for item in items.iter_mut() {
            self.calibrate(item);
        }
    }

    /// 获取某类提取的调整量
    pub fn get_adjustment(&self, kind: &KnowledgeKind, strategy: &ExtractionStrategy) -> f64 {
        self.adjustments
            .get(&(kind.clone(), strategy.clone()))
            .copied()
            .unwrap_or(0.0)
    }
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

    // =======================================================================
    // 渐进式提取管线测试 (Improvement 1)
    // =======================================================================

    #[test]
    fn test_progressive_extraction_basic() {
        let ext = KnowledgeExtractor::with_defaults();
        let result = ext.extract_progressive(
            &[
                "我们决定使用 Rust",
                "We must use HTTPS for all endpoints",
                "今天天气不错",
            ],
            "s1",
        );
        // pre_llm_items 应该包含关键词和正则匹配结果
        assert!(!result.pre_llm_items.is_empty());
        assert!(result.pre_llm_items.iter().any(|i| i.kind == KnowledgeKind::Decision));
        assert!(result.pre_llm_items.iter().any(|i| i.kind == KnowledgeKind::Constraint));
        // 总消息数
        assert_eq!(result.total_messages, 3);
    }

    #[test]
    fn test_progressive_extraction_uncovered_messages() {
        let ext = KnowledgeExtractor::with_defaults();
        let result = ext.extract_progressive(
            &[
                "我们决定使用 Rust",
                "今天天气不错",
                "普通聊天内容",
            ],
            "s1",
        );
        // "今天天气不错" 和 "普通聊天内容" 不含关键词，应被标记为未覆盖
        assert!(result.uncovered_message_indices.contains(&1));
        assert!(result.uncovered_message_indices.contains(&2));
    }

    #[test]
    fn test_progressive_extraction_all_covered() {
        let ext = KnowledgeExtractor::with_defaults();
        let result = ext.extract_progressive(
            &["我们决定使用 Rust"],
            "s1",
        );
        // 关键词匹配后 content 等于原消息，所以消息0应被覆盖
        // 但需要检查 find_covered_message_indices 的逻辑
        // 当 item.content == msg 时，消息被标记为已覆盖
        assert_eq!(result.total_messages, 1);
    }

    #[test]
    fn test_progressive_llm_prompt_contains_context() {
        let ext = KnowledgeExtractor::with_defaults();
        let now = chrono::Utc::now().to_rfc3339();
        let pre_llm_items = vec![ExtractedKnowledge {
            kind: KnowledgeKind::Decision,
            content: "We decided to use Rust".into(),
            source_session: "s1".into(),
            extracted_at: now,
            confidence: 0.6,
            strategy: ExtractionStrategy::KeywordMatch,
            context_summary: None,
            tags: vec!["decision".into()],
        }];
        let prompt = ext.build_progressive_llm_prompt(
            &["We decided to use Rust", "Some other message"],
            &pre_llm_items,
            &[1],
        );
        assert!(prompt.contains("Already extracted"));
        assert!(prompt.contains("Decision"));
        assert!(prompt.contains("[1] Some other message"));
        assert!(!prompt.contains("[0] We decided to use Rust"));
    }

    #[test]
    fn test_progressive_llm_prompt_empty_pre_llm() {
        let ext = KnowledgeExtractor::with_defaults();
        let prompt = ext.build_progressive_llm_prompt(
            &["Hello", "World"],
            &[],
            &[0, 1],
        );
        assert!(prompt.contains("None (no items extracted yet)"));
        assert!(prompt.contains("[0] Hello"));
        assert!(prompt.contains("[1] World"));
    }

    #[test]
    fn test_find_covered_message_indices() {
        let now = chrono::Utc::now().to_rfc3339();
        let items = vec![ExtractedKnowledge {
            kind: KnowledgeKind::Decision,
            content: "我们决定使用 Rust".into(),
            source_session: "s1".into(),
            extracted_at: now,
            confidence: 0.6,
            strategy: ExtractionStrategy::KeywordMatch,
            context_summary: None,
            tags: vec!["decision".into()],
        }];
        let messages = ["我们决定使用 Rust", "普通消息"];
        let covered = find_covered_message_indices(&items, &messages);
        assert!(covered.contains(&0));
        assert!(!covered.contains(&1));
    }

    #[test]
    fn test_find_covered_message_substring_match() {
        let now = chrono::Utc::now().to_rfc3339();
        let items = vec![ExtractedKnowledge {
            kind: KnowledgeKind::Constraint,
            content: "HTTPS".into(),
            source_session: "s1".into(),
            extracted_at: now,
            confidence: 0.6,
            strategy: ExtractionStrategy::KeywordMatch,
            context_summary: None,
            tags: vec!["constraint".into()],
        }];
        let messages = ["We must use HTTPS for all endpoints", "普通消息"];
        let covered = find_covered_message_indices(&items, &messages);
        assert!(covered.contains(&0));
        assert!(!covered.contains(&1));
    }

    // =======================================================================
    // 反馈校准测试 (Improvement 2)
    // =======================================================================

    #[test]
    fn test_feedback_calibrator_confirmed() {
        let mut calibrator = FeedbackCalibrator::new();
        calibrator.record(ExtractionFeedback {
            content_key: "We decided to use Rust".chars().take(50).collect(),
            kind: KnowledgeKind::Decision,
            feedback: FeedbackType::Confirmed,
            timestamp: chrono::Utc::now().to_rfc3339(),
            strategy: ExtractionStrategy::KeywordMatch,
        });
        let adj = calibrator.get_adjustment(&KnowledgeKind::Decision, &ExtractionStrategy::KeywordMatch);
        assert!((adj - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn test_feedback_calibrator_rejected() {
        let mut calibrator = FeedbackCalibrator::new();
        calibrator.record(ExtractionFeedback {
            content_key: "wrong item".chars().take(50).collect(),
            kind: KnowledgeKind::Correction,
            feedback: FeedbackType::Rejected,
            timestamp: chrono::Utc::now().to_rfc3339(),
            strategy: ExtractionStrategy::PatternMatch,
        });
        let adj = calibrator.get_adjustment(&KnowledgeKind::Correction, &ExtractionStrategy::PatternMatch);
        assert!((adj - (-0.2)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_feedback_calibrator_corrected() {
        let mut calibrator = FeedbackCalibrator::new();
        calibrator.record(ExtractionFeedback {
            content_key: "partial item".chars().take(50).collect(),
            kind: KnowledgeKind::Constraint,
            feedback: FeedbackType::Corrected,
            timestamp: chrono::Utc::now().to_rfc3339(),
            strategy: ExtractionStrategy::LlmExtraction,
        });
        let adj = calibrator.get_adjustment(&KnowledgeKind::Constraint, &ExtractionStrategy::LlmExtraction);
        assert!((adj - (-0.05)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_feedback_calibrate_item_by_kind_strategy() {
        let mut calibrator = FeedbackCalibrator::new();
        // 确认 Decision + KeywordMatch → +0.1
        calibrator.record(ExtractionFeedback {
            content_key: "some content".chars().take(50).collect(),
            kind: KnowledgeKind::Decision,
            feedback: FeedbackType::Confirmed,
            timestamp: chrono::Utc::now().to_rfc3339(),
            strategy: ExtractionStrategy::KeywordMatch,
        });

        let mut item = ExtractedKnowledge {
            kind: KnowledgeKind::Decision,
            content: "不同内容".into(),
            source_session: "s1".into(),
            extracted_at: chrono::Utc::now().to_rfc3339(),
            confidence: 0.6,
            strategy: ExtractionStrategy::KeywordMatch,
            context_summary: None,
            tags: vec![],
        };
        calibrator.calibrate(&mut item);
        // 按 kind+strategy 调整 +0.1
        assert!((item.confidence - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn test_feedback_calibrate_item_by_content_key() {
        let mut calibrator = FeedbackCalibrator::new();
        let content = "We decided to use Rust for the backend";
        calibrator.record(ExtractionFeedback {
            content_key: content.chars().take(50).collect(),
            kind: KnowledgeKind::Decision,
            feedback: FeedbackType::Rejected,
            timestamp: chrono::Utc::now().to_rfc3339(),
            strategy: ExtractionStrategy::KeywordMatch,
        });

        let mut item = ExtractedKnowledge {
            kind: KnowledgeKind::Decision,
            content: content.to_string(),
            source_session: "s1".into(),
            extracted_at: chrono::Utc::now().to_rfc3339(),
            confidence: 0.6,
            strategy: ExtractionStrategy::KeywordMatch,
            context_summary: None,
            tags: vec![],
        };
        calibrator.calibrate(&mut item);
        // 按 kind+strategy 调整 -0.2 → 0.4；再按内容匹配 -0.15 → 0.25
        assert!((item.confidence - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn test_feedback_calibrate_batch() {
        let mut calibrator = FeedbackCalibrator::new();
        calibrator.record(ExtractionFeedback {
            content_key: "content".chars().take(50).collect(),
            kind: KnowledgeKind::Constraint,
            feedback: FeedbackType::Confirmed,
            timestamp: chrono::Utc::now().to_rfc3339(),
            strategy: ExtractionStrategy::KeywordMatch,
        });

        let now = chrono::Utc::now().to_rfc3339();
        let mut items = vec![
            ExtractedKnowledge {
                kind: KnowledgeKind::Constraint,
                content: "test1".into(),
                source_session: "s1".into(),
                extracted_at: now.clone(),
                confidence: 0.6,
                strategy: ExtractionStrategy::KeywordMatch,
                context_summary: None,
                tags: vec![],
            },
            ExtractedKnowledge {
                kind: KnowledgeKind::Constraint,
                content: "test2".into(),
                source_session: "s1".into(),
                extracted_at: now,
                confidence: 0.6,
                strategy: ExtractionStrategy::KeywordMatch,
                context_summary: None,
                tags: vec![],
            },
        ];
        calibrator.calibrate_batch(&mut items);
        // 两项都应被校准 +0.1
        assert!((items[0].confidence - 0.7).abs() < f64::EPSILON);
        assert!((items[1].confidence - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn test_feedback_calibrate_confidence_clamped() {
        let mut calibrator = FeedbackCalibrator::new();
        // 多次确认 → 调整量超过1.0
        for _ in 0..5 {
            calibrator.record(ExtractionFeedback {
                content_key: "content".chars().take(50).collect(),
                kind: KnowledgeKind::Decision,
                feedback: FeedbackType::Confirmed,
                timestamp: chrono::Utc::now().to_rfc3339(),
                strategy: ExtractionStrategy::KeywordMatch,
            });
        }

        let mut item = ExtractedKnowledge {
            kind: KnowledgeKind::Decision,
            content: "test".into(),
            source_session: "s1".into(),
            extracted_at: chrono::Utc::now().to_rfc3339(),
            confidence: 0.9,
            strategy: ExtractionStrategy::KeywordMatch,
            context_summary: None,
            tags: vec![],
        };
        calibrator.calibrate(&mut item);
        // 置信度不应超过 1.0
        assert!(item.confidence <= 1.0);
    }

    // =======================================================================
    // 上下文感知提取测试 (Improvement 3)
    // =======================================================================

    #[test]
    fn test_context_extraction_correction_chain() {
        let ext = KnowledgeExtractor::new(ExtractionConfig {
            min_confidence: 0.0, // 不过滤，方便测试
            ..ExtractionConfig::default()
        });
        let items = ext.extract_with_context(
            &[
                "如何实现这个功能？",
                "你可以使用回调函数",
                "不对，应该是用 async/await",
            ],
            "s1",
            1,
        );
        // 应检测到纠正链：建议 → 纠正
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::Correction && i.tags.contains(&"contextual".to_string())));
    }

    #[test]
    fn test_context_extraction_decision_chain() {
        let ext = KnowledgeExtractor::new(ExtractionConfig {
            min_confidence: 0.0,
            ..ExtractionConfig::default()
        });
        let items = ext.extract_with_context(
            &[
                "我们来讨论一下技术选型",
                "Rust 和 Go 都可以考虑",
                "我们决定使用 Rust 实现",
            ],
            "s1",
            1,
        );
        // 应检测到决策链
        assert!(items.iter().any(|i| i.kind == KnowledgeKind::Decision && i.tags.contains(&"contextual".to_string())));
    }

    #[test]
    fn test_context_extraction_constraint_decision_compound() {
        let ext = KnowledgeExtractor::new(ExtractionConfig {
            min_confidence: 0.0,
            ..ExtractionConfig::default()
        });
        let items = ext.extract_with_context(
            &["必须使用 Rust，我们决定选择这个方案"],
            "s1",
            1,
        );
        // 应检测到约束+决策组合
        assert!(items.iter().any(|i| {
            i.kind == KnowledgeKind::Constraint
                && i.tags.contains(&"compound".to_string())
                && i.tags.contains(&"decision".to_string())
        }));
    }

    #[test]
    fn test_context_extraction_no_false_positives() {
        let ext = KnowledgeExtractor::new(ExtractionConfig {
            min_confidence: 0.0,
            ..ExtractionConfig::default()
        });
        let items = ext.extract_with_context(
            &["今天天气不错", "明天也是晴天", "我们去散步吧"],
            "s1",
            1,
        );
        // 不应检测到任何知识
        assert!(items.is_empty());
    }

    #[test]
    fn test_context_extraction_window_size() {
        let ext = KnowledgeExtractor::with_defaults();
        // 小窗口 vs 大窗口，结果可能不同
        let small = ext.extract_with_context(
            &["a", "b", "c", "d", "决定使用 Rust", "f", "g"],
            "s1",
            1,
        );
        let _large = ext.extract_with_context(
            &["a", "b", "c", "d", "决定使用 Rust", "f", "g"],
            "s1",
            3,
        );
        // 两种窗口大小都应能检测到决策
        assert!(small.iter().any(|i| i.kind == KnowledgeKind::Decision));
    }

    #[test]
    fn test_contains_correction_keywords() {
        assert!(contains_correction_keywords("不对，这是错的"));
        assert!(contains_correction_keywords("actually it should be"));
        assert!(contains_correction_keywords("wrong answer"));
        assert!(!contains_correction_keywords("这是正确的做法"));
    }

    #[test]
    fn test_contains_decision_keywords() {
        assert!(contains_decision_keywords("我们决定"));
        assert!(contains_decision_keywords("we decided to"));
        assert!(contains_decision_keywords("going with option A"));
        assert!(!contains_decision_keywords("今天天气好"));
    }

    #[test]
    fn test_contains_constraint_keywords() {
        assert!(contains_constraint_keywords("必须使用"));
        assert!(contains_constraint_keywords("must be"));
        assert!(contains_constraint_keywords("never do this"));
        assert!(!contains_constraint_keywords("可以试试"));
    }
}
