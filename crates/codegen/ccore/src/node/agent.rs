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
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::metrics::AgentMetrics;
use crate::node::{Node, NodeId, NodeType, NodeContext};
use crate::node::transport::NodeTransportHandle;
use crate::agent::{AgentConfig, AgentState};
use crate::agent::doom_loop::{DoomLoopDetector, DoomLoopResult, EscapeAction};
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
    /// 下一轮需禁用的工具名（Doom Loop 逃脱：仅禁用一轮，构建下一次采样请求后清空）
    disabled_tool_next_round: Option<String>,
    /// 滑动窗口更新器
    #[allow(dead_code)]
    sliding_window: SlidingWindow,
    /// 等待中的工具调用（tool_call_id → PendingToolCall）
    /// 在收到 stream done 时，这些被发送给 ToolNode 执行
    pending_tool_calls: HashMap<String, PendingToolCall>,
    /// 正在执行中的工具调用（tool_call_id → tool_name，等待结果返回）
    /// 使用 HashMap 以便在收到 tool_result 时记录工具名维度的执行耗时
    pending_tool_results: HashMap<String, String>,
    /// 当前 LLM 采样请求 ID
    current_sample_request_id: Option<String>,
    /// 当前推理周期的开始时间，用于计算 inference latency
    inference_start: Option<std::time::Instant>,
    /// 已使用的 token 数
    #[allow(dead_code)]
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
            disabled_tool_next_round: None,
            sliding_window: SlidingWindow::new(max_tokens),
            config,
            pending_tool_calls: HashMap::new(),
            pending_tool_results: HashMap::new(),
            current_sample_request_id: None,
            inference_start: None,
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
        let msg_role = crate::memory::working::MessageRole::try_from(role)
            .unwrap_or(crate::memory::working::MessageRole::User);
        self.working_memory.push_hot(
            msg_role,
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

        // Doom Loop 逃脱：若上一轮检测到循环，本轮过滤掉被禁用的工具（仅禁用一轮）
        let mut tools = self.config.tools.clone();
        if let Some(tool_name) = self.disabled_tool_next_round.take() {
            tools.retain(|t| t.name != tool_name);
            tracing::warn!("Doom Loop 逃脱：本轮采样已过滤工具 {}", tool_name);
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        self.current_sample_request_id = Some(request_id.clone());

        SampleRequest {
            request_id,
            agent_id: self.id.to_string(),
            model: self.config.model.clone(),
            messages,
            tools,
            stream: true,
            // 根据 Doom Loop 降级等级调整推理强度（None=原级/High, 0.5=Medium, 0.2=Low）
            reasoning_effort: self.doom_loop_detector.current_reasoning_effort(),
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

                // 记录工具调用到 Doom Loop 检测器
                let args_hash = Self::hash_tool_args(&tool_call.arguments);
                self.doom_loop_detector.record(tool_call.tool_name.clone(), args_hash);

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
        self.working_memory.push_user(output.to_string(), token_count);

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

    /// 检查 Doom Loop，返回检测结果（含逃脱动作）
    fn check_doom_loop(&mut self) -> DoomLoopResult {
        self.doom_loop_detector.detect()
    }

    /// 估算文本的 token 数（粗略：1 token ≈ 4 字符）
    fn estimate_tokens(text: &str) -> u32 {
        (text.len() as f32 / 4.0).ceil() as u32
    }

    /// 计算工具参数的哈希值，用于 Doom Loop 检测
    ///
    /// 使用规范化的 JSON 字符串进行哈希，确保不同 key 顺序的等价 JSON 对象
    /// 产生相同的哈希值，避免循环检测漏报。
    fn hash_tool_args(args: &serde_json::Value) -> u64 {
        let mut hasher = DefaultHasher::new();
        // 规范化：使用 serde_json 的紧凑格式序列化，自动排序 object key
        let canonical = serde_json::to_string(args).unwrap_or_else(|_| args.to_string());
        canonical.hash(&mut hasher);
        hasher.finish()
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
        AgentMetrics::global().record_agent_started();
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

                // 记录新一轮循环开始 + 推理起始时间（用于计算 inference latency）
                AgentMetrics::global().record_loop_count(1);
                self.inference_start = Some(std::time::Instant::now());

                // 构建采样请求并通过数据面 PUB 发送到 Sampler
                let request = self.build_sample_request();
                let sample_msg = FrameCodec::new_message(
                    Topic::sampler_request(),
                    self.id.as_str(),
                    &request,
                )?;
                // 优先使用数据面 PUB 直连（如果可用），否则走控制面
                if let Err(e) = transport.publish_data(&sample_msg).await {
                    tracing::debug!("数据面 PUB 发送失败，回退到控制面：{}", e);
                    transport.send_message(&sample_msg).await?;
                }
            }

            // 收到 LLM 流式返回
            t if t.starts_with("sampler/") && t.ends_with("/stream") => {
                // 先尝试解码为 JSON 检查消息类型
                let raw_value: serde_json::Value = FrameCodec::decode_payload(&msg)?;

                // 检查是否为 done 消息
                if raw_value.get("type").and_then(|v| v.as_str()) == Some("done") {
                    // 只处理自己发起的采样请求
                    if let Some(req_id) = raw_value.get("request_id").and_then(|v| v.as_str()) {
                        if Some(req_id) != self.current_sample_request_id.as_deref() {
                            return Ok(());
                        }
                    }

                    // 采样完成，记录推理延迟（从请求发起到 done 响应的耗时）
                if let Some(start) = self.inference_start.take() {
                    AgentMetrics::global()
                        .record_inference_latency(start.elapsed().as_millis() as f64);
                }

                // 采样完成，检查是否有待执行的工具调用
                if !self.pending_tool_calls.is_empty() {
                    self.state = AgentState::ToolCalling;
                    // 将所有 pending tool calls 先收集出来，避免 drain 跨 await 借用冲突
                    let tool_calls: Vec<(String, PendingToolCall)> =
                        self.pending_tool_calls.drain().collect();
                    for (tool_call_id, pending) in tool_calls {
                        // 记录到 pending_tool_results（含工具名，便于结果返回时记录执行耗时）
                        self.pending_tool_results
                            .insert(tool_call_id.clone(), pending.tool_name.clone());

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
                            // 优先使用数据面 PUB 直连（如果可用），否则走控制面
                            if let Err(e) = transport.publish_data(&tool_call_msg).await {
                                tracing::debug!("数据面 PUB 发送 tool_call 失败，回退到控制面：{}", e);
                                transport.send_message(&tool_call_msg).await?;
                            }
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
                    self.inference_start = None;
                    AgentMetrics::global().record_error("sampler_error");
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
                let duration_ms = payload["duration_ms"].as_u64().unwrap_or(0);

                // 在 handle_tool_result 移除条目前先取出工具名，用于记录执行耗时
                // metrics 埋点失败不影响主流程
                if let Some(tool_name) = self.pending_tool_results.get(tool_call_id).cloned() {
                    AgentMetrics::global()
                        .record_tool_execution_time(&tool_name, duration_ms as f64);
                    if !success {
                        AgentMetrics::global().record_error("tool_execution_failed");
                    }
                }

                let all_done = self.handle_tool_result(tool_call_id, output, success);

                // 所有工具调用完成，重新发起采样请求
                if all_done {
                    self.current_sample_request_id = None;
                    // 新一轮推理循环：记录循环次数 + 重置推理起始时间
                    AgentMetrics::global().record_loop_count(1);
                    self.inference_start = Some(std::time::Instant::now());
                    let request = self.build_sample_request();
                    let sample_msg = FrameCodec::new_message(
                        Topic::sampler_request(),
                        self.id.as_str(),
                        &request,
                    )?;
                    // 优先使用数据面 PUB 直连
                    if let Err(e) = transport.publish_data(&sample_msg).await {
                        tracing::debug!("数据面 PUB 发送采样请求失败，回退到控制面：{}", e);
                        transport.send_message(&sample_msg).await?;
                    }
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
                    AgentMetrics::global().record_error("subagent_crashed");
                    // 从编排器中移除（NodeId 需要类型转换）
                    let node_id: NodeId = crashed.node_id.clone().into();
                    self.orchestrator.remove_subagent(&node_id);
                } else if payload.get("type").and_then(|v| v.as_str()) == Some("completed") {
                    // 子 Agent 正常完成
                    let subagent_id = payload["node_id"].as_str().unwrap_or("");
                    let output = payload["output"].as_str().unwrap_or("");
                    tracing::info!("子 Agent 完成：{}, 输出长度={}", subagent_id, output.len());
                    // 将子 Agent 结果存入工作记忆
                    let token_count = Self::estimate_tokens(output);
                    self.working_memory.push_user(output.to_string(), token_count);
                    // 从编排器中移除（NodeId 需要类型转换）
                    let node_id: NodeId = subagent_id.to_string().into();
                    self.orchestrator.remove_subagent(&node_id);
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

        // 检查 Doom Loop 并应用逃脱策略（注入提示 + 临时禁用重复工具 + 降级模型）
        let doom_result = self.check_doom_loop();
        if doom_result.detected {
            tracing::warn!(
                "Doom Loop 检测触发：工具 {:?} 重复 {} 次，应用逃脱策略（{} 个动作）",
                doom_result.repeated_tool, doom_result.repeat_count, doom_result.escape_actions.len()
            );
            AgentMetrics::global().record_error("doom_loop");

            // 逐个应用逃脱动作，帮助 Agent 跳出循环
            for action in &doom_result.escape_actions {
                match action {
                    EscapeAction::InjectHint(hint) => {
                        // 将换策略提示注入工作记忆的 system 消息，下一轮采样会带入上下文
                        self.working_memory.push_system(hint.clone(), Self::estimate_tokens(hint));
                        tracing::warn!("Doom Loop 逃脱：已注入换策略提示到工作记忆");
                    }
                    EscapeAction::DisableTool(tool) => {
                        // 标记下一轮禁用该工具（仅禁用一轮，非永久；构建下次请求时清空）
                        self.disabled_tool_next_round = Some(tool.clone());
                        tracing::warn!("Doom Loop 逃脱：下一轮禁用工具 {}", tool);
                    }
                    EscapeAction::DegradeModel => {
                        // 降级模型：reasoning_effort 随降级等级从 High→Medium→Low 递减
                        // 实际值在 build_sample_request 中通过 current_reasoning_effort() 生效
                        let level = self.doom_loop_detector.model_degrade_level();
                        tracing::warn!(
                            "Doom Loop 逃脱：降级模型（reasoning_effort level={}）",
                            level
                        );
                    }
                }
            }

            // 通知用户当前正在尝试跳出循环（不再直接终止，而是降级续跑）
            let notice_msg = FrameCodec::new_message(
                Topic::agent_output(self.id.as_str()),
                self.id.as_str(),
                &serde_json::json!({
                    "channel": "text",
                    "content": format!(
                        "检测到工具循环（{} 重复 {} 次），已注入换策略提示、临时禁用该工具并降级模型，正在尝试跳出循环。",
                        doom_result.repeated_tool.as_deref().unwrap_or("unknown"),
                        doom_result.repeat_count
                    ),
                }),
            )?;
            transport.send_message(&notice_msg).await?;
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

    /// Agent 发布的 topic（数据面 PUB）
    fn published_topics(&self) -> Vec<String> {
        vec![
            format!("agent/{}/output", self.id),
            format!("agent/{}/tool_call", self.id),
            format!("agent/{}/event", self.id),
        ]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        self.state = AgentState::Done;
        tracing::info!("Agent Node 关闭：{}", self.id);
        AgentMetrics::global().record_agent_stopped();
        Ok(())
    }
}
