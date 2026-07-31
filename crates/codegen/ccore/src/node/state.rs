//! State Node - 对话持久化、记忆管理、token 计数
//!
//! Fusion: 唯一会话真相源。
//! 吸收 ChatStateActor 语义 + shell JSONL persistence + ccode-compaction。
//! Topics: state/persist, state/query → state/response, state/compact, agent/*/event
//!
//! State Node 的职责：
//! 1. 接收 state/persist 消息 → 持久化对话到磁盘
//! 2. 接收 state/query 消息 → 查询对话状态并回复
//! 3. 接收 state/compact 消息 → 执行上下文压缩并回复 CompactResult
//! 4. 接收 agent/{id}/event → 监听 Agent 状态变化，触发滑动窗口更新

use async_trait::async_trait;
use std::path::PathBuf;

use crate::message::frame::FrameCodec;
use crate::message::payloads::{CompactRequest, CompactResult};
use crate::message::Message;
use crate::message::Topic;
use crate::node::{Node, NodeId, NodeType, NodeContext};
use crate::node::transport::NodeTransportHandle;
use crate::memory::short_term::ShortTermMemory;
use crate::memory::long_term::LongTermMemory;
use crate::memory::window::SlidingWindow;

/// JSONL 条目 — 每行一条事件，追加写入
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JsonlEntry {
    pub entry_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u32>,
    pub ts: String,
}

/// 对话持久化格式
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedConversation {
    pub session_id: String,
    pub agent_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub messages: Vec<PersistedMessage>,
    pub token_count: u32,
    pub turn_count: u32,
}

/// 单条持久化消息
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedMessage {
    pub id: String,
    pub turn: u32,
    pub role: String,
    pub content: String,
    pub token_count: u32,
    pub is_tool_call: bool,
}

/// 对话状态查询响应
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConversationState {
    pub session_id: String,
    pub total_messages: usize,
    pub total_tokens: u32,
    pub turn_count: u32,
    pub l0_used_tokens: u32,
    pub l0_max_tokens: u32,
}

/// State Node 实现
pub struct StateNode {
    id: NodeId,
    /// L1 短期记忆
    short_term: ShortTermMemory,
    /// L2 长期记忆
    #[allow(dead_code)]
    long_term: Option<LongTermMemory>,
    /// 滑动窗口更新器
    #[allow(dead_code)]
    sliding_window: SlidingWindow,
    /// 持久化根目录
    storage_root: PathBuf,
    /// 当前会话 ID
    session_id: String,
    /// JSONL 文件路径（追加写入）
    jsonl_path: Option<PathBuf>,
}

impl StateNode {
    pub fn new(id: NodeId) -> Self {
        let storage_root = Self::dirs();
        let session_id = uuid::Uuid::new_v4().to_string();

        Self {
            id,
            short_term: ShortTermMemory::new(),
            long_term: None,
            sliding_window: SlidingWindow::new(128_000),
            storage_root,
            session_id,
            jsonl_path: None,
        }
    }

    /// 获取默认存储目录
    fn dirs() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".ccode").join("sessions")
    }

    /// 持久化当前对话到磁盘
    async fn persist(&self) -> anyhow::Result<()> {
        let session_dir = self.storage_root.join(&self.session_id);
        std::fs::create_dir_all(&session_dir)?;

        let entries = self.short_term.all_entries();
        let messages: Vec<PersistedMessage> = entries
            .iter()
            .map(|e| PersistedMessage {
                id: e.id.clone(),
                turn: e.turn,
                role: e.role.clone(),
                content: e.content.clone(),
                token_count: e.token_count,
                is_tool_call: e.is_tool_call,
            })
            .collect();

        let conversation = PersistedConversation {
            session_id: self.session_id.clone(),
            agent_id: String::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            token_count: messages.iter().map(|m| m.token_count).sum(),
            turn_count: entries.last().map(|e| e.turn).unwrap_or(0),
            messages,
        };

        let json = serde_json::to_string_pretty(&conversation)?;
        let path = session_dir.join("conversation.json");
        std::fs::write(path, json)?;

        tracing::debug!("对话已持久化：{}", self.session_id);
        Ok(())
    }

    /// 初始化 JSONL 文件路径（首次 persist 或 load 时调用）
    fn init_jsonl(&mut self) {
        if self.jsonl_path.is_none() {
            let session_dir = self.storage_root.join(&self.session_id);
            self.jsonl_path = Some(session_dir.join("conversation.jsonl"));
        }
    }

    /// 追加一条 JSONL 事件
    fn append_jsonl(&mut self, entry: JsonlEntry) {
        self.init_jsonl();
        let path = match &self.jsonl_path {
            Some(p) => p,
            None => return,
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let line = match serde_json::to_string(&entry) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("JSONL 序列化失败：{}", e);
                return;
            }
        };
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{}", line);
        }
    }

    /// 从 JSONL 文件加载会话（优先于 conversation.json）
    pub async fn load_session_jsonl(&mut self, session_id: &str) -> anyhow::Result<()> {
        let jsonl_path = self.storage_root.join(session_id).join("conversation.jsonl");
        if !jsonl_path.exists() {
            return self.load_session(session_id).await;
        }

        let content = std::fs::read_to_string(&jsonl_path)?;
        let mut count = 0;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<JsonlEntry>(line) {
                match entry.entry_type.as_str() {
                    "user" | "assistant" | "system" => {
                        let role = entry.role.as_deref().unwrap_or(&entry.entry_type);
                        let content = entry.content.as_deref().unwrap_or("");
                        let tokens = entry.token_count.unwrap_or_else(|| (content.len() / 4) as u32);
                        self.short_term.store(role.to_string(), content.to_string(), tokens, false);
                        count += 1;
                    }
                    "tool_call" | "tool_result" => {
                        let content = entry.content.as_deref().unwrap_or("");
                        let tokens = entry.token_count.unwrap_or_else(|| (content.len() / 4) as u32);
                        self.short_term.store("tool".to_string(), content.to_string(), tokens, true);
                        count += 1;
                    }
                    _ => {}
                }
            }
        }

        self.session_id = session_id.to_string();
        self.jsonl_path = Some(jsonl_path);
        tracing::info!("JSONL 加载会话：{} ({}条事件)", session_id, count);
        Ok(())
    }

    /// 查询当前对话状态
    fn query_state(&self) -> ConversationState {
        let entries = self.short_term.all_entries();
        ConversationState {
            session_id: self.session_id.clone(),
            total_messages: entries.len(),
            total_tokens: entries.iter().map(|e| e.token_count).sum(),
            turn_count: entries.last().map(|e| e.turn).unwrap_or(0),
            l0_used_tokens: 0,
            l0_max_tokens: 128_000,
        }
    }

    /// 执行 5 层上下文压缩（对标 Claude Code 压缩管线）
    ///
    /// L1 Budget：per-tool token 预算截断
    /// L2 Snip：per-tool 行/字符/匹配数硬截断
    /// L3 MicroCompact：旧工具结果按时间替换为摘要
    /// L4 CacheAware：跳过已缓存内容，生成 cache_edits
    /// L5 ContextCollapse：LLM 摘要（需注入 CompactionSampler，无 sampler 时跳过）
    async fn handle_compact(&mut self, req: CompactRequest) -> CompactResult {
        let tokens_before: u64 = self.short_term.all_entries().iter().map(|e| e.token_count as u64).sum();

        if req.force {
            tracing::info!("强制压缩请求：session={}", req.session_id);
        }

        let snip_config = ccode_compaction::snip::SnipConfig::default();
        let budget = ccode_compaction::budget::ToolBudget::default();
        let micro_config = ccode_compaction::micro_compact::MicroCompactConfig::default();
        let mut saved_total: u64 = 0;

        // ── L1 + L2: Budget + Snip（per-tool 真实工具名） ──
        let entries = self.short_term.all_entries_mut();
        for entry in entries.iter_mut() {
            if !entry.is_tool_call || entry.content.is_empty() {
                continue;
            }
            let original_tokens = entry.token_count as u64;
            let tool_name = entry.tool_name.as_deref().unwrap_or("ToolOutput");

            let snip_result = ccode_compaction::snip::snip(tool_name, &entry.content, &snip_config);
            let budget_result = budget.truncate(tool_name, &snip_result.output);

            if snip_result.truncated || budget_result.truncated {
                let new_content = if budget_result.truncated {
                    budget_result.output.clone()
                } else {
                    snip_result.output
                };
                let new_tokens = (new_content.len() / 4) as u32;
                let saved = original_tokens.saturating_sub(new_tokens as u64);
                saved_total += saved;
                entry.content = new_content;
                entry.token_count = new_tokens;
            }
        }

        // ── L3: MicroCompact（旧工具结果按时间替换） ──
        // 对超过 max_age 的工具条目，替换为短前缀摘要
        let micro_entries = self.short_term.all_entries_mut();
        for entry in micro_entries.iter_mut() {
            if !entry.is_tool_call || entry.content.is_empty() {
                continue;
            }
            let original_tokens = entry.token_count as u64;
            let max_chars = micro_config.summary_max_chars;
            if entry.content.len() > max_chars {
                let truncated: String = entry.content.chars().take(max_chars).collect();
                let suffix = format!("\n... [{} 字符已压缩]", entry.content.len() - max_chars);
                entry.content = truncated + &suffix;
                let new_tokens = (entry.content.len() / 4) as u32;
                let saved = original_tokens.saturating_sub(new_tokens as u64);
                saved_total += saved;
                entry.token_count = new_tokens;
            }
        }

        // ── L4: CacheAware（标记已压缩内容的指纹） ──
        // 生成指纹用于缓存边界追踪（实际 cache_edits 注入由 SamplerNode 消费）
        let mut cache_state = ccode_compaction::cache_aware::CacheAwareState::default();
        for entry in self.short_term.all_entries().iter() {
            if entry.is_tool_call && !entry.content.is_empty() {
                let fingerprint = format!("{}:{}", 
                    entry.tool_name.as_deref().unwrap_or("unknown"),
                    &entry.content[..entry.content.len().min(64)]
                );
                cache_state.record(&fingerprint);
            }
        }
        tracing::debug!(
            cache_entries = cache_state.len(),
            "L4 CacheAware: 已记录缓存指纹"
        );

        // ── L5: ContextCollapse（提取式摘要） ──
        // 当 L1-L4 之后 token 仍超阈值，对早期条目做提取式摘要：
        // 保留每个条目的首句，丢弃其余内容。后续可替换为 LLM 摘要。
        let collapse_config = ccode_compaction::context_collapse::ContextCollapseConfig::default();
        let total_tokens_after_l4: u64 = self.short_term.all_entries().iter().map(|e| e.token_count as u64).sum();
        let context_window: u64 = 128_000;

        if ccode_compaction::context_collapse::should_collapse(total_tokens_after_l4, context_window, &collapse_config) {
            let entries = self.short_term.all_entries_mut();
            let keep_recent = collapse_config.keep_recent;
            if entries.len() > keep_recent {
                let split = entries.len() - keep_recent;
                let mut collapse_saved: u64 = 0;

                for entry in entries[..split].iter_mut() {
                    if entry.content.len() > 200 {
                        let original_tokens = entry.token_count as u64;
                        let summary: String = entry.content
                            .split(|c: char| c == '.' || c == '。' || c == '\n')
                            .take(3)
                            .collect::<Vec<&str>>()
                            .join(". ");
                        let summary = if summary.len() > 200 {
                            summary[..200].to_string() + "..."
                        } else {
                            summary + "..."
                        };
                        let new_tokens = (summary.len() / 4) as u32;
                        collapse_saved += original_tokens.saturating_sub(new_tokens as u64);
                        entry.content = summary;
                        entry.token_count = new_tokens;
                    }
                }
                saved_total += collapse_saved;
                tracing::info!(
                    collapsed_entries = split,
                    saved = collapse_saved,
                    "L5 ContextCollapse: 提取式摘要完成"
                );
            }
        }

        let tokens_after = tokens_before.saturating_sub(saved_total);

        tracing::info!(
            session = %req.session_id,
            tokens_before,
            tokens_after,
            saved = saved_total,
            layers = "L1+L2(budget+snip) L3(micro_compact) L4(cache_aware) L5(context_collapse)",
            "5 层压缩完成"
        );

        CompactResult {
            session_id: req.session_id,
            ok: true,
            tokens_before,
            tokens_after,
            error: None,
        }
    }

    /// 从磁盘加载历史对话
    pub async fn load_session(&mut self, session_id: &str) -> anyhow::Result<()> {
        let path = self.storage_root.join(session_id).join("conversation.json");
        if !path.exists() {
            return Err(anyhow::anyhow!("会话不存在：{}", session_id));
        }

        let json = std::fs::read_to_string(&path)?;
        let conversation: PersistedConversation = serde_json::from_str(&json)?;

        for msg in conversation.messages {
            self.short_term.store(
                msg.role,
                msg.content,
                msg.token_count,
                msg.is_tool_call,
            );
        }

        self.session_id = session_id.to_string();
        tracing::info!("加载历史会话：{} ({}条消息)", session_id, self.short_term.len());
        Ok(())
    }
}

#[async_trait]
impl Node for StateNode {
    fn node_type(&self) -> NodeType {
        NodeType::State
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!(
            "State Node 启动：{} (session={}, storage={})",
            self.id,
            self.session_id,
            self.storage_root.display()
        );
        std::fs::create_dir_all(&self.storage_root)?;
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        match msg.topic.as_str() {
            "state/persist" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let role = payload["role"].as_str().unwrap_or("user");
                let content = payload["content"].as_str().unwrap_or("");
                let token_count = payload["token_count"].as_u64().unwrap_or(0) as u32;
                let is_tool = payload["is_tool_call"].as_bool().unwrap_or(false);
                self.short_term.store(
                    role.to_string(),
                    content.to_string(),
                    token_count,
                    is_tool,
                );

                // JSONL 追加写入
                let entry = JsonlEntry {
                    entry_type: if is_tool { "tool_result".into() } else { role.into() },
                    role: Some(role.into()),
                    content: Some(content.into()),
                    tool_call_id: payload["tool_call_id"].as_str().map(|s| s.into()),
                    tool_name: payload["tool_name"].as_str().map(|s| s.into()),
                    success: payload["success"].as_bool(),
                    token_count: Some(token_count),
                    ts: chrono::Utc::now().to_rfc3339(),
                };
                self.append_jsonl(entry);

                self.persist().await?;
            }
            "state/query" => {
                let state = self.query_state();
                let reply = FrameCodec::new_reply(
                    Topic::state_response(),
                    self.id.to_string(),
                    &msg.header.msg_id,
                    &state,
                )?;
                transport.send_message(&reply).await?;
            }
            "state/compact" => {
                let req: CompactRequest = FrameCodec::decode_payload(&msg)?;
                let result = self.handle_compact(req).await;
                let reply = FrameCodec::new_reply(
                    Topic::state_response(),
                    self.id.to_string(),
                    &msg.header.msg_id,
                    &result,
                )?;
                transport.send_message(&reply).await?;
            }
            t if t.ends_with("/event") => {
                // 监听 Agent 状态变化，触发记忆管理
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                if let Some(event_type) = payload.get("type").and_then(|v| v.as_str()) {
                    match event_type {
                        "thinking" | "outputting" => {
                            // Agent 正在思考或输出，可以用于滑动窗口预热
                            tracing::trace!("Agent 状态变化：{}", event_type);
                        }
                        "done" | "error" => {
                            // Agent 完成/出错，触发滑动窗口压缩
                            tracing::debug!("Agent 状态终态：{}", event_type);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec![
            "state/persist".into(),
            "state/query".into(),
            "state/compact".into(),
            "agent/*/event".into(),
            "sys/shutdown".into(),
        ]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        self.persist().await?;
        tracing::info!("State Node 关闭：{} (最终持久化完成)", self.id);
        Ok(())
    }
}
