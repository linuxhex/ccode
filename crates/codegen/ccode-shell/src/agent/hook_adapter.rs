//! Hook 桥接适配器 — 将 ccode-hooks dispatcher 适配到 ccore 的 HookDispatcher trait
//!
//! ccore 定义了 HookDispatcher trait（hook_bridge.rs），
//! ccode-hooks 实现了具体的 dispatcher（dispatcher.rs），
//! 本文件提供适配器将两者桥接。
//!
//! 核心职责：
//! 1. 将 ccore 的 ToolCallContext 转换为 ccode-hooks 的 HookEventEnvelope
//! 2. 调用 ccode-hooks 的 dispatcher 函数
//! 3. 将 ccode-hooks 的 HookDecision 转换回 ccore 的 HookDecision
//!
//! fail-open 原则：任何 Hook 执行错误不阻塞工具调用，返回 Allow。

use std::sync::Arc;

use async_trait::async_trait;

use ccore::tools::hook_bridge::{HookDecision, HookDispatcher, ToolCallContext};
use ccode_hooks::discovery::HookRegistry;
use ccode_hooks::event::{HookEventEnvelope, HookEventName, HookPayload};
use ccode_hooks::permission_rules::PermissionRuleSet;
use ccode_hooks::runner::RunContext;

/// HookDispatcher 适配器 — 桥接 ccode-hooks 到 ccore
///
/// 持有 ccode-hooks 的 HookRegistry、PermissionRuleSet 和会话信息，
/// 将 ccore 的 ToolCallContext 转换为 ccode-hooks 的 HookEventEnvelope，
/// 调用 dispatcher，将结果映射回 ccore 的 HookDecision。
pub struct HookDispatcherAdapter {
    /// Hook 注册表（从配置文件加载的 PreToolUse/PostToolUse hook 列表）
    registry: Arc<HookRegistry>,
    /// 权限规则集（allow/deny/ask 规则引擎，可选）
    permission_rules: Option<PermissionRuleSet>,
    /// 是否启用 Auto Mode（ML 分类器自动审批）
    auto_mode: bool,
    /// 会话 ID（用于 RunContext 和 envelope）
    session_id: String,
    /// 工作目录（用于 RunContext 和 envelope）
    workspace_root: String,
}

impl HookDispatcherAdapter {
    /// 创建适配器
    ///
    /// # 参数
    /// - `registry`: 从配置文件加载的 HookRegistry
    /// - `permission_rules`: 权限规则集（可选，传 None 则跳过规则引擎阶段）
    /// - `auto_mode`: 是否启用 Auto Mode
    /// - `session_id`: 当前会话 ID
    /// - `workspace_root`: 工作目录路径
    pub fn new(
        registry: HookRegistry,
        permission_rules: Option<PermissionRuleSet>,
        auto_mode: bool,
        session_id: String,
        workspace_root: String,
    ) -> Self {
        Self {
            registry: Arc::new(registry),
            permission_rules,
            auto_mode,
            session_id,
            workspace_root,
        }
    }

    /// 从 ToolCallContext 构建 PreToolUse 的 HookEventEnvelope
    fn build_pre_tool_use_envelope(&self, ctx: &ToolCallContext) -> HookEventEnvelope {
        let (tool_input, truncated) = ccode_hooks::event::truncate_payload(ctx.input.clone());
        HookEventEnvelope {
            hook_event_name: HookEventName::PreToolUse,
            session_id: self.session_id.clone(),
            cwd: ctx.working_dir.clone(),
            workspace_root: self.workspace_root.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            transcript_path: None,
            client_identifier: None,
            prompt_id: None,
            permission_mode: None,
            payload: HookPayload::PreToolUse {
                tool_name: ctx.tool_name.clone(),
                tool_use_id: String::new(),
                tool_input,
                tool_input_truncated: truncated,
                subagent_type: None,
            },
        }
    }

    /// 从 ToolCallContext 构建 PostToolUse 的 HookEventEnvelope
    fn build_post_tool_use_envelope(
        &self,
        ctx: &ToolCallContext,
        result: &str,
        success: bool,
    ) -> HookEventEnvelope {
        let (tool_input, input_truncated) = ccode_hooks::event::truncate_payload(ctx.input.clone());
        let tool_result_value = serde_json::Value::String(result.to_string());
        let (tool_result, result_truncated) =
            ccode_hooks::event::truncate_payload(tool_result_value);
        HookEventEnvelope {
            hook_event_name: HookEventName::PostToolUse,
            session_id: self.session_id.clone(),
            cwd: ctx.working_dir.clone(),
            workspace_root: self.workspace_root.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            transcript_path: None,
            client_identifier: None,
            prompt_id: None,
            permission_mode: None,
            payload: HookPayload::PostToolUse {
                tool_name: ctx.tool_name.clone(),
                tool_use_id: String::new(),
                tool_input,
                tool_result,
                tool_input_truncated: input_truncated,
                tool_result_truncated: result_truncated,
                duration_ms: None,
                is_backgrounded: false,
                subagent_type: None,
            },
        }
    }

    /// 构建 RunContext
    fn run_context(&self) -> RunContext<'_> {
        RunContext {
            session_id: &self.session_id,
            workspace_root: &self.workspace_root,
        }
    }
}

#[async_trait]
impl HookDispatcher for HookDispatcherAdapter {
    /// 工具执行前 Hook
    ///
    /// 调用 ccode-hooks 的 dispatch_pre_tool_use，
    /// 将 ccode-hooks 的 HookDecision 映射为 ccore 的 HookDecision。
    /// fail-open：Hook 执行失败时返回 Allow，不阻塞工具调用。
    async fn pre_tool_use(&self, ctx: &ToolCallContext) -> HookDecision {
        // 快速路径：无注册的 PreToolUse hook 且无规则，直接放行
        if !self.registry.has_enabled_hooks_for_canonical(HookEventName::PreToolUse)
            && self.permission_rules.is_none()
        {
            return HookDecision::Allow;
        }

        let envelope = self.build_pre_tool_use_envelope(ctx);
        let run_ctx = self.run_context();

        let pre_result = ccode_hooks::dispatcher::dispatch_pre_tool_use(
            &self.registry,
            &envelope,
            &run_ctx,
            self.permission_rules.as_ref(),
            self.auto_mode,
        )
        .await;

        // 将 ccode-hooks 的 HookDecision 映射为 ccore 的 HookDecision
        match pre_result.decision {
            ccode_hooks::result::HookDecision::Allow => {
                // 如果 hook 改写了输入参数，返回 Rewrite
                if let Some(updated_input) = pre_result.rewrite.updated_input {
                    HookDecision::Rewrite {
                        updated_input,
                        additional_context: pre_result.rewrite.additional_context,
                    }
                } else {
                    HookDecision::Allow
                }
            }
            ccode_hooks::result::HookDecision::Deny { reason, .. } => {
                HookDecision::Deny(reason)
            }
        }
    }

    /// 工具执行后 Hook
    ///
    /// 调用 ccode-hooks 的 dispatch_non_blocking，
    /// 仅做日志记录和通知，不阻塞结果。
    /// fail-open：Hook 执行失败时仅记录 warn，不影响结果。
    async fn post_tool_use(&self, ctx: &ToolCallContext, result: &str, success: bool) {
        // 快速路径：无注册的 PostToolUse hook，直接返回
        if !self.registry.has_enabled_hooks_for_canonical(HookEventName::PostToolUse) {
            return;
        }

        let envelope = self.build_post_tool_use_envelope(ctx, result, success);
        let run_ctx = self.run_context();

        // PostToolUse 是 observe-only 事件，使用 dispatch_non_blocking
        let hook_results = ccode_hooks::dispatcher::dispatch_non_blocking(
            &self.registry,
            HookEventName::PostToolUse,
            &envelope,
            &run_ctx,
        )
        .await;

        // 记录 hook 执行结果（仅日志，不影响工具结果）
        for hr in &hook_results {
            match hr {
                ccode_hooks::result::HookRunResult::Success { hook_name, .. } => {
                    tracing::debug!(hook_name = %hook_name, "post_tool_use hook 执行成功");
                }
                ccode_hooks::result::HookRunResult::Failed { hook_name, error, .. } => {
                    tracing::warn!(
                        hook_name = %hook_name,
                        error = %error,
                        "post_tool_use hook 执行失败（fail-open，不影响工具结果）"
                    );
                }
                ccode_hooks::result::HookRunResult::Blocked { hook_name, detail, .. } => {
                    tracing::info!(
                        hook_name = %hook_name,
                        detail = %detail,
                        "post_tool_use hook 返回 block（observe-only，已记录）"
                    );
                }
                ccode_hooks::result::HookRunResult::Skipped { hook_name } => {
                    tracing::debug!(hook_name = %hook_name, "post_tool_use hook 已跳过");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccode_hooks::discovery::HookRegistry;

    /// 空注册表的适配器应直接返回 Allow
    #[tokio::test]
    async fn empty_registry_pre_tool_use_allows() {
        let adapter = HookDispatcherAdapter::new(
            HookRegistry::default(),
            None,
            false,
            "test-session".to_string(),
            "/tmp".to_string(),
        );
        let ctx = ToolCallContext {
            tool_name: "read_file".to_string(),
            input: serde_json::json!({"path": "/tmp/test.rs"}),
            agent_id: "agent-1".to_string(),
            working_dir: "/tmp".to_string(),
        };
        let decision = adapter.pre_tool_use(&ctx).await;
        assert_eq!(decision, HookDecision::Allow);
    }

    /// 空注册表的适配器 post_tool_use 应不报错
    #[tokio::test]
    async fn empty_registry_post_tool_use_noop() {
        let adapter = HookDispatcherAdapter::new(
            HookRegistry::default(),
            None,
            false,
            "test-session".to_string(),
            "/tmp".to_string(),
        );
        let ctx = ToolCallContext {
            tool_name: "read_file".to_string(),
            input: serde_json::json!({"path": "/tmp/test.rs"}),
            agent_id: "agent-1".to_string(),
            working_dir: "/tmp".to_string(),
        };
        // 不应 panic
        adapter.post_tool_use(&ctx, "ok", true).await;
    }

    /// 构建 PreToolUse envelope 的字段映射正确
    #[test]
    fn pre_tool_use_envelope_fields() {
        let adapter = HookDispatcherAdapter::new(
            HookRegistry::default(),
            None,
            false,
            "sess-123".to_string(),
            "/workspace".to_string(),
        );
        let ctx = ToolCallContext {
            tool_name: "bash".to_string(),
            input: serde_json::json!({"command": "ls"}),
            agent_id: "agent-1".to_string(),
            working_dir: "/workspace".to_string(),
        };
        let envelope = adapter.build_pre_tool_use_envelope(&ctx);
        assert_eq!(envelope.session_id, "sess-123");
        assert_eq!(envelope.workspace_root, "/workspace");
        assert_eq!(envelope.cwd, "/workspace");
        assert!(matches!(envelope.payload, HookPayload::PreToolUse { ref tool_name, .. } if tool_name == "bash"));
    }

    /// 构建 PostToolUse envelope 的字段映射正确
    #[test]
    fn post_tool_use_envelope_fields() {
        let adapter = HookDispatcherAdapter::new(
            HookRegistry::default(),
            None,
            false,
            "sess-456".to_string(),
            "/project".to_string(),
        );
        let ctx = ToolCallContext {
            tool_name: "write".to_string(),
            input: serde_json::json!({"path": "/tmp/out.rs"}),
            agent_id: "agent-2".to_string(),
            working_dir: "/project".to_string(),
        };
        let envelope = adapter.build_post_tool_use_envelope(&ctx, "written", true);
        assert_eq!(envelope.session_id, "sess-456");
        assert!(matches!(envelope.payload, HookPayload::PostToolUse { ref tool_name, .. } if tool_name == "write"));
    }
}
