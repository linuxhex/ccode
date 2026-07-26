//! # Hook 改写输入 + 注入上下文
//!
//! 借鉴 Claude Code 的 updatedInput + additionalContext 设计：
//! - PreToolUse hook 可以改写工具输入（updatedInput）
//!   例：Bash 命令 "rm -rf /tmp/build" → 改为 "rm -rf /tmp/build && echo done"
//! - PreToolUse hook 可以注入额外上下文（additionalContext）
//!   例：在文件编辑前注入编码规范提醒
//! - PostToolUse hook 可以补充上下文（additionalContext）
//!   例：在代码搜索后注入项目架构概述
//! - Stop hook 可以阻止 Agent 停止（block）并附加上下文

use serde::{Deserialize, Serialize};

/// Hook 改写结果
///
/// 包含四个维度的信息：
/// - updated_input：改写后的工具输入，None 表示不修改原始输入
/// - additional_context：注入的额外上下文，会被添加到 LLM 的下一轮对话中
/// - blocked：是否应该阻止此操作
/// - block_reason：阻止原因
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookRewriteResult {
    /// 改写后的工具输入（None 表示不修改）
    pub updated_input: Option<serde_json::Value>,
    /// 注入的额外上下文（会被添加到 LLM 的下一轮对话中）
    pub additional_context: Option<String>,
    /// 是否应该阻止此操作
    pub blocked: bool,
    /// 阻止原因
    pub block_reason: Option<String>,
}

impl Default for HookRewriteResult {
    fn default() -> Self {
        Self {
            updated_input: None,
            additional_context: None,
            blocked: false,
            block_reason: None,
        }
    }
}

/// Hook 输出解析
///
/// 解析 hook 命令的 JSON 输出，提取以下字段：
/// - updatedInput：改写后的工具输入
/// - additionalContext：注入的额外上下文
/// - blocked / blockReason：阻止标记和原因
/// - decision：allow/deny/ask 决策（deny 会设置 blocked，ask 会注入提醒）
pub fn parse_hook_output(raw_output: &str) -> HookRewriteResult {
    let mut result = HookRewriteResult::default();

    // 尝试解析为 JSON
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw_output) {
        // 提取 updatedInput：改写后的工具输入
        if let Some(updated) = parsed.get("updatedInput") {
            result.updated_input = Some(updated.clone());
        }

        // 提取 additionalContext：额外上下文
        if let Some(ctx) = parsed.get("additionalContext").and_then(|v| v.as_str()) {
            result.additional_context = Some(ctx.to_string());
        }

        // 提取 blocked：是否阻止
        if parsed
            .get("blocked")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            result.blocked = true;
            result.block_reason = parsed
                .get("blockReason")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }

        // 提取 decision：allow/deny/ask 决策
        if let Some(decision) = parsed.get("decision").and_then(|v| v.as_str()) {
            match decision {
                "deny" => {
                    result.blocked = true;
                    result.block_reason = Some(
                        parsed
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Hook denied")
                            .to_string(),
                    );
                }
                "ask" => {
                    // ask 模式下不阻止，但注入提醒上下文
                    result.additional_context = Some(format!(
                        "⚠️ Hook 提醒：{}",
                        parsed
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("需要确认")
                    ));
                }
                _ => {} // allow 或其他，不修改
            }
        }
    }

    result
}

/// 将 additionalContext 注入到 Agent 的上下文窗口
///
/// 借鉴 Claude Code 的方式：additionalContext 作为 user message 注入，
/// 使 Agent 在后续推理中能够感知 hook 提供的额外信息。
pub fn inject_additional_context(context: &mut Vec<serde_json::Value>, additional_context: &str) {
    if additional_context.is_empty() {
        return;
    }

    context.push(serde_json::json!({
        "role": "user",
        "content": format!("[Hook Context] {}", additional_context),
    }));
}
