//! Micro-compact gate — 工具结果入历史前的 snip + budget 截断。
//!
//! 在工具执行完成后、push 到 chat_state 之前，对可压缩工具的输出做两层截断：
//! 1. Snip：按行/字符/匹配数硬截断（细粒度、单调用）
//! 2. Budget：按 token 预算截断（粗粒度、跨调用）
//!
//! 二者叠加使用，避免单次超大输出撑爆上下文。

use ccode_compaction::{budget::ToolBudget, snip::{snip, SnipConfig}};
use ccode_sampling_types::ConversationItem;

/// 对工具结果执行 snip + budget 截断。
///
/// 非可压缩工具原样返回。可压缩工具先做 snip 硬截断，再做 budget 预算截断。
pub fn snip_and_budget_tool_result(
    tool_chat: ConversationItem,
    tool_name: &str,
) -> ConversationItem {
    // 提取工具结果内容
    let content = match &tool_chat {
        ConversationItem::ToolResult(t) => t.content.as_ref(),
        _ => return tool_chat,
    };

    // 第一层：Snip 硬截断
    let snip_config = SnipConfig::default();
    let snip_result = snip(tool_name, content, &snip_config);

    // 第二层：Budget 预算截断
    let budget = ToolBudget::default();
    let budget_result = budget.truncate(tool_name, &snip_result.output);

    // 如果都没截断，原样返回
    if !snip_result.truncated && !budget_result.truncated {
        return tool_chat;
    }

    // 构建截断后的工具结果（budget 结果优先，因为它在 snip 之后）
    let final_content = budget_result.output.clone();

    match tool_chat {
        ConversationItem::ToolResult(mut t) => {
            t.content = final_content.into();
            ConversationItem::ToolResult(t)
        }
        other => other,
    }
}
