//! 自动记忆提取
//!
//! 从对话消息中自动提取关键知识（决策、约束、纠正、偏好等），
//! 用于跨会话的知识持久化。参考 Claude Code 的 autoDream 模式：
//! 在对话结束时扫描关键信息，将其提炼为可检索的知识条目。

use serde::{Deserialize, Serialize};

/// 提取的知识条目
///
/// 每条知识携带类型、内容、来源会话和提取时间，
/// 便于后续整合时去重和溯源。
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

/// 知识类型
///
/// 将对话中的关键信息分类为不同语义类型，
/// 便于后续按类型检索和应用。
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

/// 从对话中提取关键知识
///
/// 扫描对话中的决策、约束、纠正、偏好等关键信息，
/// 返回提取到的知识条目列表。
///
/// 当前采用关键词匹配策略，后续可升级为 LLM 提取。
pub fn extract_knowledge(
    messages: &[&str],
    session_id: &str,
) -> Vec<KnowledgeItem> {
    let mut items = Vec::new();
    let now = chrono::Utc::now().to_rfc3339();

    for msg in messages {
        // 检测决策关键词
        if contains_decision(msg) {
            items.push(KnowledgeItem {
                kind: KnowledgeKind::Decision,
                content: msg.to_string(),
                source_session: session_id.to_string(),
                extracted_at: now.clone(),
            });
        }
        // 检测约束关键词
        if contains_constraint(msg) {
            items.push(KnowledgeItem {
                kind: KnowledgeKind::Constraint,
                content: msg.to_string(),
                source_session: session_id.to_string(),
                extracted_at: now.clone(),
            });
        }
        // 检测纠正关键词
        if contains_correction(msg) {
            items.push(KnowledgeItem {
                kind: KnowledgeKind::Correction,
                content: msg.to_string(),
                source_session: session_id.to_string(),
                extracted_at: now.clone(),
            });
        }
    }

    items
}

/// 检测消息中是否包含决策关键词
fn contains_decision(msg: &str) -> bool {
    let keywords = ["决定", "选择", "确认", "用这个方案", "decided", "chosen"];
    keywords.iter().any(|k| msg.contains(k))
}

/// 检测消息中是否包含约束关键词
fn contains_constraint(msg: &str) -> bool {
    let keywords = ["必须", "不能", "不要", "禁止", "must not", "required"];
    keywords.iter().any(|k| msg.contains(k))
}

/// 检测消息中是否包含纠正关键词
fn contains_correction(msg: &str) -> bool {
    let keywords = ["不对", "错了", "应该是", "不是这样", "纠正", "wrong", "should be"];
    keywords.iter().any(|k| msg.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_decision() {
        let items = extract_knowledge(&["我们决定使用 Rust 实现"], "session-1");
        assert!(items.iter().any(|i| matches!(i.kind, KnowledgeKind::Decision)));
    }

    #[test]
    fn test_extract_constraint() {
        let items = extract_knowledge(&["必须使用 UTF-8 编码"], "session-1");
        assert!(items.iter().any(|i| matches!(i.kind, KnowledgeKind::Constraint)));
    }

    #[test]
    fn test_extract_correction() {
        let items = extract_knowledge(&["不对，应该是 42"], "session-1");
        assert!(items.iter().any(|i| matches!(i.kind, KnowledgeKind::Correction)));
    }

    #[test]
    fn test_extract_empty() {
        let items = extract_knowledge(&["这是一条普通消息"], "session-1");
        assert!(items.is_empty());
    }
}
