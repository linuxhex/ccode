//! Agent Node - 完整的 Agent 循环实现
//!
//! Agent 核心循环：
//! 1. 接收 input（来自 TUI 或父 Agent）
//! 2. 构建 L0 工作记忆 → 发送 sampler/request
//! 3. 收到 LLM 流式响应 → 解析 tool_call 或 text
//! 4. 如果有 tool_call → 发送到 Tool Node → 收到 tool_result → 回到步骤 2
//! 5. 如果是纯文本 → 发送到 TUI/父 Agent → 等待下一轮 input
//! 6. 每轮结束执行滑动窗口更新
//! 7. Doom Loop 检测：重复工具调用超过阈值则终止

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::node::{Node, NodeId, NodeType, NodeContext};
use crate::node::transport::NodeTransportHandle;
use crate::agent::{AgentConfig, AgentState};
use crate::agent::doom_loop::DoomLoopDetector;
use crate::agent::orchestrator::Orchestrator;
use crate::agent::subagent::SubAgentCrashed;
use crate::memory::working::WorkingMemory;
use crate::memory::short_term::ShortTermMemory;
use crate::memory::window::SlidingWindow;
use crate::sampler::provider::{
    SampleRequest, ChatMessage, StreamChunk, StreamChannel, ToolDefinition as SamplerToolDefinition,
};

/// Agent 工具调用状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingToolCall {
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// Agent Node 实现
pub struct AgentNode {
    /// Node 唯一 ID
    id: NodeId,
    /// Agent 配置
    config: AgentConfig,
    /// 当前状态
    state: AgentState,
    /// L0 工作记忆
    working_memory: WorkingMemory,
    /// L1 短期记忆
    short_term_memory: ShortTermMemory,
    /// 子 Agent 编排器
    orchestrator: Orchestrator,
    /// Doom Loop 检测器
    doom_loop_detector: DoomLoopDetector,
    /// 滑动窗口更新器
    sliding_window: SlidingWindow,
    /// 等待中的工具调用（tool_call_id → PendingToolCall）
    /// 在收到 stream done 时，这些被发送给 ToolNode 执行
    pending_tool_calls: HashMap<String, PendingToolCall>,
    /// 正在执行中的工具调用 ID 集合（已发送给 ToolNode，等待结果）
    pending_tool_results: std::collections::HashSet<String>,
    /// 当前 LLM 采样请求 ID
    current_sample_request_id: Option<String>,
    /// 已使用的 token 数
    tokens_used: u32,
    /// 已执行轮次
    turns_executed: u32,
}

impl AgentNode {
    pub fn new(id: NodeId, config: AgentConfig) -> Self {
        let max_tokens = 128_000;
        Self {
            id,
            state: AgentState::Idle,
            working_memory: WorkingMemory::new(max_tokens),
            short_term_memory: ShortTermMemory::new(),
            orchestrator: Orchestrator::new(10),
            doom_loop_detector: DoomLoopDetector::new(10, 3),
            sliding_window: SlidingWindow::new(max_tokens),
            config,
            pending_tool_calls: HashMap::new(),
            pending_tool_results: std::collections::HashSet::new(),
            current_sample_request_id: None,
            tokens_used: 0,
            turns_executed: 0,
        }
    }

    /// 处理用户输入或父 Agent 指令
    fn handle_input(&mut self, content: &str, role: &str) {
        // 将输入存入 L1 短期记忆（永不丢弃）
        let _entry_id = self.short_term_memory.store(
            role.to_string(),
            content.to_string(),
            Self::estimate_tokens(content),
            false,
        );

        // 将输入推入 L0 工作记忆
        self.working_memory.push_hot(
            role.to_string(),
            content.to_string(),
            Self::estimate_tokens(content),
        );

        self.state = AgentState::Thinking;
        tracing::debug!("Agent {} 收到输入，轮次 {}", self.id, self.turns_executed);
    }

    /// 构建采样请求，发送到 Sampler Node
    fn build_sample_request(&mut self) -> SampleRequest {
        let messages: Vec<ChatMessage> = self.working_memory
            .to_chat_messages()
            .into_iter()
            .map(|(role, content)| ChatMessage { role, content })
            .collect();

        let request_id = uuid::Uuid::new_v4().to_string();
        self.current_sample_request_id = Some(request_id.clone());

        SampleRequest {
            request_id,
            agent_id: self.id.to_string(),
            model: self.config.model.clone(),
            messages,
            tools: self.config.tools.clone(),
            stream: true,
            reasoning_effort: None,
            max_tokens: None,
            temperature: None,
        }
    }

    /// 处理 LLM 流式返回的 chunk
    async fn handle_stream_chunk(
        &mut self,
        chunk: &StreamChunk,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        match chunk.channel {
            StreamChannel::Text => {
                self.state = AgentState::Outputting;
                // 文本内容流式转发到 TUI
                let output_msg = FrameCodec::new_message(
                    Topic::agent_output(self.id.as_str()),
                    self.id.as_str(),
                    &serde_json::json!({
                        "channel": "text",
                        "content": chunk.content,
                    }),
                )?;
                transport.send_message(&output_msg).await?;
            }
            StreamChannel::Reasoning => {
                // 推理链内容，可选展示给用户
            }
            StreamChannel::ToolCall => {
                self.state = AgentState::ToolCalling;
                // 优先使用结构化的 tool_call 字段，回退到从 content 解析
                let tool_call = if let Some(tc) = &chunk.tool_call {
                    PendingToolCall {
                        tool_call_id: tc.tool_call_id.clone(),
                        tool_name: tc.tool_name.clone(),
                        arguments: serde_json::from_str(&tc.arguments)
                            .unwrap_or(serde_json::Value::Null),
                    }
                } else {
                    serde_json::from_str(&chunk.content)
                        .unwrap_or_else(|_| PendingToolCall {
                            tool_call_id: uuid::Uuid::new_v4().to_string(),
                            tool_name: "unknown".into(),
                            arguments: serde_json::Value::Null,
                        })
                };
                self.pending_tool_calls.insert(tool_call.tool_call_id.clone(), tool_call);
            }
        }
        Ok(())
    }

    /// 处理工具调用结果，返回 true 表示所有工具调用已完成，应重新采样
    fn handle_tool_result(&mut self, tool_call_id: &str, output: &str, _success: bool) -> bool {
        // 从 pending_tool_results 中移除
        self.pending_tool_results.remove(tool_call_id);

        // 将工具结果存入记忆
        let token_count = Self::estimate_tokens(output);
        self.short_term_memory.store(
            "tool".to_string(),
            output.to_string(),
            token_count,
            true,
        );
        self.working_memory.push_hot("tool".to_string(), output.to_string(), token_count);

        // 如果没有更多 pending 工具结果，标记为需要重新采样
        if self.pending_tool_results.is_empty() {
            self.state = AgentState::Thinking;
            true
        } else {
            // 还有工具在执行中
            false
        }
    }

    /// 每轮结束时执行滑动窗口更新
    fn update_context_window(&mut self) {
        self.turns_executed += 1;
    }

    /// 检查是否达到最大轮次
    fn check_max_turns(&self) -> bool {
        if let Some(max) = self.config.max_turns {
            self.turns_executed >= max
        } else {
            false
        }
    }

    /// 检查 Doom Loop
    fn check_doom_loop(&self) -> bool {
        self.doom_loop_detector.detect().detected
    }

    /// 估算文本的 token 数（粗略：1 token ≈ 4 字符）
    fn estimate_tokens(text: &str) -> u32 {
        (text.len() as f32 / 4.0).ceil() as u32
    }
}

#[async_trait]
impl Node for AgentNode {
    fn node_type(&self) -> NodeType {
        NodeType::Agent
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!(
            "Agent Node 启动：{} (type={:?}, model={})",
            self.id, self.config.agent_type, self.config.model
        );
        self.state = AgentState::Idle;
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        let topic = msg.topic.as_str();

        match topic {
            // 收到用户输入或父 Agent 指令
            t if t.ends_with("/input") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let content = payload["content"].as_str().unwrap_or("");
                let role = payload["role"].as_str().unwrap_or("user");
                self.handle_input(content, role);

                // 构建采样请求并发送到 Sampler
                let request = self.build_sample_request();
                let sample_msg = FrameCodec::new_message(
                    Topic::sampler_request(),
                    self.id.as_str(),
                    &request,
                )?;
                transport.send_message(&sample_msg).await?;
            }

            // 收到 LLM 流式返回
            t if t.starts_with("sampler/") && t.ends_with("/stream") => {
                // 先尝试解码为 JSON 检查消息类型
                let raw_value: serde_json::Value = FrameCodec::decode_payload(&msg)?;

                // 检查是否为 done 消息
                if raw_value.get("type").and_then(|v| v.as_str()) == Some("done") {
                    // 只处理自己发起的采样请求
                    if let Some(req_id) = raw_value.get("request_id").and_then(|v| v.as_str()) {
                        if Some(req_id) != self.current_sample_request_id.as_ref() {
                            return Ok(());
                        }
                    }

                    // 采样完成，检查是否有待执行的工具调用
                    if !self.pending_tool_calls.is_empty() {
                        self.state = AgentState::ToolCalling;
                        // 将所有 pending tool calls 先收集出来，避免 drain 跨 await 借用冲突
                        let tool_calls: Vec<(String, PendingToolCall)> =
                            self.pending_tool_calls.drain().collect();
                        for (tool_call_id, pending) in tool_calls {
                            // 记录到 pending_tool_results，等待 Tool Node 返回结果
                            self.pending_tool_results.insert(tool_call_id.clone());

                            let tool_call_msg = FrameCodec::new_message(
                                Topic::agent_tool_call(self.id.as_str()),
                                self.id.as_str(),
                                &serde_json::json!({
                                    "tool_call_id": tool_call_id,
                                    "tool_name": pending.tool_name,
                                    "arguments": pending.arguments,
                                    "agent_id": self.id.to_string(),
                                }),
                            )?;
                            transport.send_message(&tool_call_msg).await?;
                            tracing::debug!(
                                "Agent {} 发送工具调用：{} ({})",
                                self.id, pending.tool_name, tool_call_id
                            );
                        }
                    } else {
                        // 没有工具调用，采样完成，重置状态
                        self.current_sample_request_id = None;
                        self.state = AgentState::Idle;
                        self.update_context_window();
                        tracing::debug!("Agent {} 采样完成", self.id);
                    }
                    return Ok(());
                }

                // 检查是否为错误消息
                if raw_value.get("error").is_some() {
                    tracing::error!(
                        "Agent {} 收到采样错误：{:?}",
                        self.id,
                        raw_value.get("error")
                    );
                    self.current_sample_request_id = None;
                    self.state = AgentState::Error;
                    return Ok(());
                }

                // 正常 StreamChunk
                let chunk: StreamChunk = FrameCodec::decode_payload(&msg)?;
                if Some(&chunk.request_id) != self.current_sample_request_id.as_ref() {
                    return Ok(());
                }
                self.handle_stream_chunk(&chunk, transport).await?;
            }

            // 收到工具执行结果
            t if t.ends_with("/tool_result") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let tool_call_id = payload["tool_call_id"].as_str().unwrap_or("");
                let output = payload["output"].as_str().unwrap_or("");
                let success = payload["success"].as_bool().unwrap_or(true);
                let all_done = self.handle_tool_result(tool_call_id, output, success);

                // 所有工具调用完成，重新发起采样请求
                if all_done {
                    self.current_sample_request_id = None;
                    let request = self.build_sample_request();
                    let sample_msg = FrameCodec::new_message(
                        Topic::sampler_request(),
                        self.id.as_str(),
                        &request,
                    )?;
                    transport.send_message(&sample_msg).await?;
                }
            }

            // 收到子 Agent 事件（崩溃/完成）
            t if t.ends_with("/event") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                if let Some(error) = payload.get("error") {
                    let crashed = SubAgentCrashed {
                        node_id: payload["node_id"].as_str().unwrap_or("").into(),
                        error: error.as_str().unwrap_or("unknown").into(),
                    };
                    tracing::warn!("子 Agent 崩溃：{} - {}", crashed.node_id, crashed.error);
                }
            }

            // 收到工具注册消息（Tool Node 广播可用工具）
            "tool/register" => {
                let tools: Vec<SamplerToolDefinition> = FrameCodec::decode_payload(&msg)?;
                self.config.tools = tools;
                tracing::info!("Agent {} 收到工具注册，共 {} 个工具", self.id, self.config.tools.len());
            }

            // 收到系统 shutdown 信号
            "sys/shutdown" => {
                self.state = AgentState::Done;
            }

            _ => {
                tracing::warn!("Agent 收到未知 topic：{}", topic);
            }
        }

        // 检查 Doom Loop
        if self.check_doom_loop() {
            tracing::warn!("Doom Loop 检测触发，终止 Agent 循环");
            self.state = AgentState::Error;
        }

        // 检查最大轮次
        if self.check_max_turns() {
            tracing::info!("达到最大轮次 {}，Agent 结束", self.turns_executed);
            self.state = AgentState::Done;
        }

        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        let mut subs = vec![
            format!("agent/{}/input", self.id),
            format!("agent/{}/tool_result", self.id),
            format!("agent/{}/event", self.id),
            "sampler/*/stream".into(),
            "tool/register".into(),
            "sys/shutdown".into(),
        ];

        // 如果有活跃子 Agent，也订阅它们的输出和事件
        for sub in self.orchestrator.active_subagents() {
            subs.push(format!("agent/{}/output", sub.node_id));
            subs.push(format!("agent/{}/event", sub.node_id));
        }

        subs
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        self.state = AgentState::Done;
        tracing::info!("Agent Node 关闭：{}", self.id);
        Ok(())
    }
}
