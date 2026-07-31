//! Tool Node - 接收工具调用请求，执行并返回结果
//!
//! Fusion: ToolNode 以生产 ccode-tools 能力为准。
//! Ask 模式：发布 agent/{id}/permission，等待 PermissionResponse 再执行。
//!
//! Tool Node 的完整工作流：
//! 1. 订阅 agent/*/tool_call topic
//! 2. 收到 ToolCallRequest → 权限检查 → 并发限流 → 执行工具 → 返回 ToolCallResult
//! 3. 结果通过消息总线发送到 agent/{src}/tool_result
//!
//! 并发控制：内置 Semaphore（默认 20 并发），防止工具执行过载。
//! 对应 ANS 的 tool_semaphore，但运行在 ToolNode 本地（独立进程无法共享 ANS）。

use async_trait::async_trait;
use std::collections::HashMap;
use tokio::sync::{Mutex, Semaphore, oneshot};

use crate::message::frame::FrameCodec;
use crate::message::payloads::{PermissionRequest, PermissionResponse};
use crate::message::Message;
use crate::message::Topic;
use crate::metrics::AgentMetrics;
use crate::node::{Node, NodeId, NodeType, NodeContext, PermissionMode};
use crate::node::transport::NodeTransportHandle;
use crate::tools::bridge::ToolBridge;
use crate::tools::ToolCallRequest;

/// 默认最大并发工具执行数
const DEFAULT_MAX_CONCURRENT_TOOLS: usize = 20;

/// Tool Node 实现
pub struct ToolNode {
    id: NodeId,
    /// 工具桥接器
    bridge: ToolBridge,
    /// 全局权限模式
    permission_mode: PermissionMode,
    /// 并发限流信号量（对应 ANS 的 tool_semaphore）
    concurrency_semaphore: Semaphore,
    /// 待处理的权限请求（tool_call_id → oneshot sender）
    pending_permissions: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    /// Hook 注册表（PreToolUse/PostToolUse 等事件分发）
    hook_registry: Option<ccode_hooks::discovery::HookRegistry>,
    /// 权限规则集（allow/deny/ask 规则引擎）
    permission_rules: Option<ccode_hooks::permission_rules::PermissionRuleSet>,
    /// 是否启用 Auto Mode（ML 分类器自动审批）
    auto_mode: bool,
}

impl ToolNode {
    pub fn new(id: NodeId) -> Self {
        Self {
            id,
            bridge: ToolBridge::new(),
            permission_mode: PermissionMode::Trust,
            concurrency_semaphore: Semaphore::new(DEFAULT_MAX_CONCURRENT_TOOLS),
            pending_permissions: Mutex::new(HashMap::new()),
            hook_registry: None,
            permission_rules: None,
            auto_mode: false,
        }
    }

    /// 设置 Hook 注册表（接入 PreToolUse/PostToolUse 事件分发）
    pub fn set_hook_registry(&mut self, registry: ccode_hooks::discovery::HookRegistry) {
        self.hook_registry = Some(registry);
    }

    /// 设置权限规则集（接入 allow/deny/ask 规则引擎）
    pub fn set_permission_rules(&mut self, rules: ccode_hooks::permission_rules::PermissionRuleSet) {
        self.permission_rules = Some(rules);
    }

    /// 启用 Auto Mode（ML 分类器自动审批）
    pub fn set_auto_mode(&mut self, enabled: bool) {
        self.auto_mode = enabled;
    }

    /// 设置权限模式
    pub fn set_permission_mode(&mut self, mode: PermissionMode) {
        self.permission_mode = mode;
    }

    /// 等待权限响应（通过总线）
    async fn wait_permission(&self, tool_call_id: &str) -> anyhow::Result<bool> {
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending_permissions.lock().await;
            pending.insert(tool_call_id.to_string(), tx);
        }
        // 等待权限响应，超时 60 秒
        match tokio::time::timeout(std::time::Duration::from_secs(60), rx).await {
            Ok(Ok(allowed)) => Ok(allowed),
            Ok(Err(_)) => {
                tracing::warn!("权限通道关闭：tool_call_id={}", tool_call_id);
                Ok(false)
            }
            Err(_) => {
                tracing::warn!("权限请求超时：tool_call_id={}", tool_call_id);
                // 清理超时的请求
                let mut pending = self.pending_permissions.lock().await;
                pending.remove(tool_call_id);
                Ok(false)
            }
        }
    }

    /// 处理权限响应（来自 AcpNode/TUINode）
    async fn handle_permission_response(&self, response: PermissionResponse) -> anyhow::Result<()> {
        let mut pending = self.pending_permissions.lock().await;
        if let Some(tx) = pending.remove(&response.tool_call_id) {
            let _ = tx.send(response.allowed);
            tracing::info!(
                "权限响应：tool_call_id={}, allowed={}",
                response.tool_call_id,
                response.allowed
            );
        } else {
            tracing::warn!(
                "权限响应无匹配请求：tool_call_id={}",
                response.tool_call_id
            );
        }
        Ok(())
    }

    /// 处理工具调用请求，通过消息总线返回结果
    ///
    /// 完整权限链路：PreToolUse Hook → 权限规则引擎 → 简单权限模式 → 执行 → PostToolUse Hook
    async fn handle_tool_call(
        &mut self,
        request: ToolCallRequest,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        // 并发限流：获取信号量许可
        let _permit = match self.concurrency_semaphore.try_acquire() {
            Ok(permit) => permit,
            Err(_) => {
                tracing::warn!(
                    available = self.concurrency_semaphore.available_permits(),
                    "工具并发已满，等待可用许可"
                );
                self.concurrency_semaphore.acquire().await
                    .map_err(|_| anyhow::anyhow!("信号量已关闭"))?
            }
        };

        // ── 阶段 1：PreToolUse Hook + 权限规则引擎 ──
        let mut effective_input = request.arguments.clone();
        if let Some(ref registry) = self.hook_registry {
            let envelope = ccode_hooks::event::HookEventEnvelope {
                hook_event_name: ccode_hooks::event::HookEventName::PreToolUse,
                session_id: request.agent_id.clone(),
                cwd: std::env::current_dir()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                workspace_root: std::env::current_dir()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                transcript_path: None,
                client_identifier: None,
                prompt_id: None,
                permission_mode: Some(format!("{:?}", self.permission_mode).to_lowercase()),
                payload: ccode_hooks::event::HookPayload::PreToolUse {
                    tool_name: request.tool_name.clone(),
                    tool_input: request.arguments.clone(),
                },
            };
            let ctx = ccode_hooks::runner::RunContext {
                session_id: &request.agent_id,
                cwd: &std::env::current_dir()
                    .unwrap_or_default()
                    .to_string_lossy(),
                env_overrides: None,
            };
            let pre_result = ccode_hooks::dispatcher::dispatch_pre_tool_use(
                registry,
                &envelope,
                &ctx,
                self.permission_rules.as_ref(),
                self.auto_mode,
            ).await;

            // Hook 改写：如果 hook 返回了 updatedInput，使用改写后的参数
            if let Some(ref updated) = pre_result.rewrite.updated_input {
                effective_input = updated.clone();
                tracing::info!(
                    "PreToolUse Hook 改写工具参数：tool={}",
                    request.tool_name
                );
            }

            // Hook 决策：deny 则不执行
            match pre_result.decision {
                ccode_hooks::result::HookDecision::Deny { reason } => {
                    tracing::info!(
                        "PreToolUse Hook 拒绝工具执行：tool={}, reason={}",
                        request.tool_name, reason
                    );
                    let result_msg = FrameCodec::new_message(
                        Topic::agent_tool_result(&request.agent_id),
                        self.id.as_str(),
                        &serde_json::json!({
                            "tool_call_id": request.tool_call_id,
                            "output": format!("工具 {} 被 Hook 拒绝：{}", request.tool_name, reason),
                            "success": false,
                            "duration_ms": 0,
                        }),
                    )?;
                    if let Err(_) = transport.publish_data(&result_msg).await {
                        transport.send_message(&result_msg).await?;
                    }
                    return Ok(());
                }
                ccode_hooks::result::HookDecision::Allow => {}
                ccode_hooks::result::HookDecision::Ask => {
                    // Hook 建议询问，降级到简单权限模式
                }
            }

            // 权限链结果（如果规则引擎给出了决策）
            if let Some(ref chain) = pre_result.chain_result {
                match &chain.decision {
                    ccode_hooks::permission_rules::PermissionDecision::Deny { reason } => {
                        tracing::info!(
                            "权限规则引擎拒绝：tool={}, reason={}",
                            request.tool_name, reason
                        );
                        let result_msg = FrameCodec::new_message(
                            Topic::agent_tool_result(&request.agent_id),
                            self.id.as_str(),
                            &serde_json::json!({
                                "tool_call_id": request.tool_call_id,
                                "output": format!("工具 {} 被权限规则拒绝：{}", request.tool_name, reason),
                                "success": false,
                                "duration_ms": 0,
                                "alternatives": chain.alternatives,
                            }),
                        )?;
                        if let Err(_) = transport.publish_data(&result_msg).await {
                            transport.send_message(&result_msg).await?;
                        }
                        return Ok(());
                    }
                    ccode_hooks::permission_rules::PermissionDecision::Allow => {
                        tracing::debug!("权限规则引擎允许：tool={}", request.tool_name);
                    }
                    ccode_hooks::permission_rules::PermissionDecision::Ask { .. } => {
                        // 降级到下面的简单权限模式
                    }
                }
            }
        }

        // ── 阶段 2：简单权限模式（Ask 模式弹确认框） ──
        if self.bridge.needs_confirmation(&request.tool_name, self.permission_mode) {
            match self.permission_mode {
                PermissionMode::Ask => {
                    let perm_req = PermissionRequest {
                        agent_id: request.agent_id.clone(),
                        tool_call_id: request.tool_call_id.clone(),
                        tool_name: request.tool_name.clone(),
                        arguments: request.arguments.clone(),
                        reason: None,
                    };
                    let perm_msg = FrameCodec::new_message(
                        Topic::agent_permission(&request.agent_id),
                        self.id.as_str(),
                        &perm_req,
                    )?;
                    if let Err(e) = transport.publish_data(&perm_msg).await {
                        tracing::warn!("data-plane publish failed: {}, falling back", e);
                        transport.send_message(&perm_msg).await?;
                    }

                    let allowed = self.wait_permission(&request.tool_call_id).await?;
                    if !allowed {
                        AgentMetrics::global().record_error("tool_permission_denied");
                        let result_msg = FrameCodec::new_message(
                            Topic::agent_tool_result(&request.agent_id),
                            self.id.as_str(),
                            &serde_json::json!({
                                "tool_call_id": request.tool_call_id,
                                "output": format!("工具 {} 被用户拒绝", request.tool_name),
                                "success": false,
                                "duration_ms": 0,
                            }),
                        )?;
                        if let Err(_) = transport.publish_data(&result_msg).await {
                            transport.send_message(&result_msg).await?;
                        }
                        return Ok(());
                    }
                }
                PermissionMode::Trust => {
                    tracing::info!(
                        "工具 {} 需要确认（Trust 模式），自动批准",
                        request.tool_name
                    );
                }
                PermissionMode::Yolo => {}
            }
        }

        // ── 阶段 3：执行工具 ──
        // 使用可能被 Hook 改写过的参数
        let effective_request = ToolCallRequest {
            arguments: effective_input,
            ..request
        };
        let result = self.bridge.execute(&effective_request).await;

        AgentMetrics::global()
            .record_tool_execution_time(&effective_request.tool_name, result.duration_ms as f64);
        if !result.success {
            AgentMetrics::global().record_error("tool_execution_failed");
        }

        // ── 阶段 4：PostToolUse Hook ──
        if let Some(ref registry) = self.hook_registry {
            let envelope = ccode_hooks::event::HookEventEnvelope {
                hook_event_name: if result.success {
                    ccode_hooks::event::HookEventName::PostToolUse
                } else {
                    ccode_hooks::event::HookEventName::PostToolUseFailure
                },
                session_id: effective_request.agent_id.clone(),
                cwd: std::env::current_dir()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                workspace_root: std::env::current_dir()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                transcript_path: None,
                client_identifier: None,
                prompt_id: None,
                permission_mode: Some(format!("{:?}", self.permission_mode).to_lowercase()),
                payload: ccode_hooks::event::HookPayload::PostToolUse {
                    tool_name: effective_request.tool_name.clone(),
                    tool_input: effective_request.arguments.clone(),
                    tool_output: result.output.clone(),
                    success: result.success,
                    duration_ms: result.duration_ms,
                },
            };
            let ctx = ccode_hooks::runner::RunContext {
                session_id: &effective_request.agent_id,
                cwd: &std::env::current_dir()
                    .unwrap_or_default()
                    .to_string_lossy(),
                env_overrides: None,
            };
            let post_event = if result.success {
                ccode_hooks::event::HookEventName::PostToolUse
            } else {
                ccode_hooks::event::HookEventName::PostToolUseFailure
            };
            let _post_results = ccode_hooks::dispatcher::dispatch_non_blocking(
                registry, post_event, &envelope, &ctx,
            ).await;
        }

        // 将结果封装为消息，优先通过数据面 PUB 发送
        let tool_result_msg = FrameCodec::new_message(
            Topic::agent_tool_result(&effective_request.agent_id),
            self.id.as_str(),
            &result,
        )?;
        if let Err(_) = transport.publish_data(&tool_result_msg).await {
            transport.send_message(&tool_result_msg).await?;
        }

        tracing::debug!(
            "工具执行完成：tool_call_id={}, success={}, duration={}ms",
            result.tool_call_id, result.success, result.duration_ms
        );
        Ok(())
    }
}

#[async_trait]
impl Node for ToolNode {
    fn node_type(&self) -> NodeType {
        NodeType::Tool
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!(
            "Tool Node 启动：{} (工具数={}, 执行器数={}, 权限={:?})",
            self.id,
            self.bridge.tool_count(),
            self.bridge.executor_count(),
            self.permission_mode,
        );
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        match msg.topic.as_str() {
            t if t.ends_with("/tool_call") => {
                let request: ToolCallRequest = FrameCodec::decode_payload(&msg)?;
                tracing::debug!(
                    "Tool Node 收到工具调用：{} (agent={})",
                    request.tool_name, request.agent_id
                );
                self.handle_tool_call(request, transport).await?;
            }
            t if t.ends_with("/permission") => {
                // 处理权限响应（来自 AcpNode/TUINode）
                let response: PermissionResponse = FrameCodec::decode_payload(&msg)?;
                self.handle_permission_response(response).await?;
            }
            "sys/spawn" => {
                // 收到 Node 上线广播，广播工具列表给 Agent
                let tool_defs: Vec<crate::sampler::provider::ToolDefinition> = self.bridge
                    .tool_definitions()
                    .iter()
                    .map(|td| crate::sampler::provider::ToolDefinition {
                        name: td.name.clone(),
                        description: td.description.clone(),
                        parameters: td.parameters.clone(),
                    })
                    .collect();
                if !tool_defs.is_empty() {
                    let register_msg = FrameCodec::new_message(
                        Topic::tool_register(),
                        self.id.as_str(),
                        &tool_defs,
                    )?;
                    transport.send_message(&register_msg).await?;
                    tracing::info!("Tool Node 广播工具注册：{} 个工具", tool_defs.len());
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec![
            "agent/*/tool_call".into(),
            "agent/*/permission".into(),
            "sys/spawn".into(),
            "sys/shutdown".into(),
        ]
    }

    /// Tool 发布的 topic（数据面 PUB）
    fn published_topics(&self) -> Vec<String> {
        vec!["agent/*/tool_result".into(), "tool/register".into()]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        tracing::info!("Tool Node 关闭：{}", self.id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn permission_oneshot_allowed() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut pending = std::collections::HashMap::new();
        pending.insert("tc-1".to_string(), tx);

        // 模拟 handle_permission_response：发送 allowed=true
        if let Some(sender) = pending.remove("tc-1") {
            let _ = sender.send(true);
        }

        let result = rx.await.unwrap();
        assert!(result, "permission should be allowed");
    }

    #[tokio::test]
    async fn permission_oneshot_denied() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut pending = std::collections::HashMap::new();
        pending.insert("tc-2".to_string(), tx);

        if let Some(sender) = pending.remove("tc-2") {
            let _ = sender.send(false);
        }

        let result = rx.await.unwrap();
        assert!(!result, "permission should be denied");
    }

    #[tokio::test]
    async fn permission_oneshot_timeout() {
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
        let mut pending = std::collections::HashMap::new();
        pending.insert("tc-3".to_string(), tx);

        // 不发送响应，验证超时
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            rx,
        )
        .await;

        assert!(result.is_err(), "should timeout when no response");
        // pending 中条目仍在（超时后应清理）
        assert!(pending.contains_key("tc-3"));
    }
}
