//! 微压缩层（MicroCompact）
//!
//! 对话上下文中的轻量级压缩：对超龄的工具结果内容进行清除或摘要替换，
//! 保留工具调用的意图和结论，减少上下文占用而不丢失关键语义。
//!
//! 参考 Claude Code 的微压缩策略：在 LLM 上下文窗口内对过时工具输出
//! 做最小化替换，而非完全丢弃。

use std::time::Duration;

// 白名单与 `is_compactable` 已抽到 [`crate::compactable`]，供 budget/snip/
// micro_compact 共享同一份分类，避免三处各自维护导致漂移。
pub use crate::compactable::{COMPACTABLE_TOOLS, is_compactable};

/// 旧工具结果的清除标记
///
/// 当工具结果超过最大保留时间时，内容替换为此标记，
/// 表示该位置曾有工具输出但已被微压缩清除。
pub const CLEARED_MESSAGE: &str = "[Old tool result content cleared]";

/// 微压缩配置
#[derive(Debug, Clone)]
pub struct MicroCompactConfig {
    /// 工具结果的最大保留时间，超过此时间的工具输出将被清除
    pub max_age: Duration,
    /// 保留的语义摘要最大长度（字符数），截取内容前 N 字符作为摘要
    pub summary_max_chars: usize,
}

impl Default for MicroCompactConfig {
    fn default() -> Self {
        Self {
            max_age: Duration::from_secs(300), // 5 分钟
            summary_max_chars: 200,
        }
    }
}

/// 微压缩结果统计
#[derive(Debug, Clone)]
pub struct MicroCompactResult {
    /// 被清除的工具结果数量
    pub cleared_count: usize,
    /// 保留摘要的工具结果数量
    pub summarized_count: usize,
}

/// 对消息列表执行微压缩
///
/// 遍历消息，对每个可压缩的工具结果检查年龄：
/// - 超过 max_age → 替换输出为 CLEARED_MESSAGE，保留前 summary_max_chars 字符作为摘要
/// - 未超龄 → 保持原样
///
/// 返回微压缩后的消息列表和统计结果。
pub fn micro_compact_messages<T: MicroCompactable>(
    messages: &[T],
    config: &MicroCompactConfig,
    now: std::time::Instant,
) -> Vec<T> {
    messages
        .iter()
        .map(|msg| {
            // 仅处理可压缩的工具结果消息
            if !msg.is_tool_result() {
                return msg.clone();
            }

            let tool_name = match msg.tool_name() {
                Some(name) => name,
                None => return msg.clone(),
            };

            if !is_compactable(tool_name) {
                return msg.clone();
            }

            // 计算消息年龄
            let age = now.duration_since(msg.created_at());
            if age <= config.max_age {
                return msg.clone();
            }

            // 超龄：生成摘要替换内容
            let content = msg.content();
            if content.len() <= config.summary_max_chars {
                // 内容本身很短，直接用清除标记
                msg.with_content(CLEARED_MESSAGE.to_string())
            } else {
                // 保留前 N 字符作为摘要，附加清除标记
                let summary = format!(
                    "{}\n{}",
                    &content[..config.summary_max_chars],
                    CLEARED_MESSAGE
                );
                msg.with_content(summary)
            }
        })
        .collect()
}

/// 消息的微压缩 trait，由宿主实现
///
/// 宿主（如 ccode-shell）需要为其消息类型实现此 trait，
/// 以便微压缩层能够访问消息的时间、类型、内容和工具信息。
pub trait MicroCompactable: Clone {
    /// 消息创建时间
    fn created_at(&self) -> std::time::Instant;
    /// 是否为工具结果
    fn is_tool_result(&self) -> bool;
    /// 工具名称（仅工具结果有）
    fn tool_name(&self) -> Option<&str>;
    /// 获取内容
    fn content(&self) -> &str;
    /// 设置内容（用于替换为清除标记或摘要）
    fn with_content(&self, new_content: String) -> Self;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_compactable() {
        assert!(is_compactable("FileRead"));
        assert!(is_compactable("fileread")); // 大小写不敏感
        assert!(is_compactable("Bash"));
        assert!(!is_compactable("AgentTool"));
        assert!(!is_compactable("UnknownTool"));
    }

    #[test]
    fn test_default_config() {
        let config = MicroCompactConfig::default();
        assert_eq!(config.max_age, Duration::from_secs(300));
        assert_eq!(config.summary_max_chars, 200);
    }
}
