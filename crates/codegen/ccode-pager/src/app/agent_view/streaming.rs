//! 流式 Token 渲染器 — 逐 token 增量显示 LLM 响应
//!
//! 对标 Claude Code 的 Ink 流式渲染：每个 SSE chunk 到达时立即触发局部刷新，
//! 避免等全部响应完成后才显示。

use agent_client_protocol as acp;
use std::time::Instant;

/// 流式渲染事件
#[derive(Debug, Clone)]
pub enum StreamingEvent {
    /// 新 token 到达
    Token(String),
    /// 工具调用开始
    ToolCallStart { name: String, id: String },
    /// 工具调用结束
    ToolCallEnd { id: String, success: bool },
    /// 流式结束
    Finish,
}

/// Spinner 动画帧序列（Braille 旋转，与 Claude Code 一致）
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// 工具调用块（进行中/已完成）
#[derive(Debug, Clone)]
pub struct ToolCallBlock {
    pub name: String,
    pub id: String,
    pub start_time: Instant,
    pub end_time: Option<Instant>,
    pub success: Option<bool>,
}

impl ToolCallBlock {
    /// 获取当前 spinner 字符（进行中时旋转，完成时显示 ✓/✗）
    pub fn spinner_char(&self) -> &str {
        match (self.end_time, self.success) {
            (Some(_), Some(true)) => "✓",
            (Some(_), Some(false)) => "✗",
            (Some(_), None) => "?",
            (None, _) => {
                // 进行中：根据经过时间计算帧索引（100ms 一帧）
                let elapsed_ms = self.start_time.elapsed().as_millis() as u64;
                let frame_idx = ((elapsed_ms / 100) % SPINNER_FRAMES.len() as u64) as usize;
                SPINNER_FRAMES[frame_idx]
            }
        }
    }
}

impl ToolCallBlock {
    /// 工具调用是否仍在进行中（尚未收到结束事件）
    pub fn is_in_progress(&self) -> bool {
        self.end_time.is_none()
    }
}

/// 流式 Token 渲染器
///
/// 维护增量缓冲区，每个 token append 后标记 dirty，
/// ratatui event loop 检测 dirty 后触发局部刷新。
pub struct StreamingRenderer {
    /// 已接收的 token 缓冲区
    content: String,
    /// 工具调用块列表
    tool_blocks: Vec<ToolCallBlock>,
    /// 是否正在流式输出
    is_streaming: bool,
    /// 缓冲区是否有未刷新的内容
    dirty: bool,
    /// 上次刷新时间（用于节流）
    last_flush: Instant,
    /// 最小刷新间隔（毫秒）
    flush_interval_ms: u64,
    /// 已输出的 token 数（用于进度显示）
    token_count: usize,
}

impl StreamingRenderer {
    pub fn new() -> Self {
        Self {
            content: String::with_capacity(4096),
            tool_blocks: Vec::new(),
            is_streaming: false,
            dirty: false,
            last_flush: Instant::now(),
            flush_interval_ms: 16, // ~60fps
            token_count: 0,
        }
    }

    /// 开始流式输出
    pub fn start(&mut self) {
        self.is_streaming = true;
        self.content.clear();
        self.tool_blocks.clear();
        self.token_count = 0;
        self.dirty = true;
        tracing::debug!("流式渲染器启动");
    }

    /// 追加 token 到缓冲区
    pub fn append_token(&mut self, token: &str) {
        if !self.is_streaming {
            return;
        }
        self.content.push_str(token);
        self.token_count += 1;
        self.dirty = true;
    }

    /// 处理流式事件
    pub fn handle_event(&mut self, event: StreamingEvent) {
        match event {
            StreamingEvent::Token(t) => self.append_token(&t),
            StreamingEvent::ToolCallStart { name, id } => {
                tracing::debug!(tool_name = %name, tool_id = %id, "工具调用开始");
                self.tool_blocks.push(ToolCallBlock {
                    name,
                    id: id.clone(),
                    start_time: Instant::now(),
                    end_time: None,
                    success: None,
                });
                self.dirty = true;
            }
            StreamingEvent::ToolCallEnd { id, success } => {
                tracing::debug!(tool_id = %id, success, "工具调用结束");
                if let Some(block) = self.tool_blocks.iter_mut().find(|b| b.id == id) {
                    block.end_time = Some(Instant::now());
                    block.success = Some(success);
                }
                self.dirty = true;
            }
            StreamingEvent::Finish => {
                tracing::debug!(token_count = self.token_count, "流式输出结束");
                self.is_streaming = false;
                self.dirty = true;
            }
        }
    }

    /// 缓冲区是否有未刷新内容
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// 是否应该刷新（节流控制）
    pub fn should_flush(&self) -> bool {
        self.dirty && self.last_flush.elapsed().as_millis() as u64 >= self.flush_interval_ms
    }

    /// 标记已刷新
    pub fn mark_flushed(&mut self) {
        self.dirty = false;
        self.last_flush = Instant::now();
    }

    /// 流式结束，返回完整内容
    pub fn finish(&mut self) -> String {
        self.is_streaming = false;
        self.dirty = false;
        tracing::debug!(token_count = self.token_count, "流式渲染器结束，返回完整内容");
        std::mem::take(&mut self.content)
    }

    /// 获取当前内容（不消耗）
    pub fn content(&self) -> &str {
        &self.content
    }

    /// 获取工具调用块
    pub fn tool_blocks(&self) -> &[ToolCallBlock] {
        &self.tool_blocks
    }

    /// 是否正在流式输出
    pub fn is_streaming(&self) -> bool {
        self.is_streaming
    }

    /// 获取 token 数量
    pub fn token_count(&self) -> usize {
        self.token_count
    }

    /// 获取进行中的工具调用块
    pub fn in_progress_tool_blocks(&self) -> impl Iterator<Item = &ToolCallBlock> {
        self.tool_blocks.iter().filter(|b| b.is_in_progress())
    }

    /// 是否有正在进行的工具调用
    pub fn has_in_progress_tools(&self) -> bool {
        self.tool_blocks.iter().any(|b| b.is_in_progress())
    }
}

/// 将 ACP SessionUpdate 转换为 StreamingEvent。
///
/// 仅映射与流式渲染相关的变体（AgentMessageChunk、ToolCall、
/// ToolCallUpdate），其余变体返回 None。
pub fn streaming_event_from_acp_update(update: &acp::SessionUpdate) -> Option<StreamingEvent> {
    match update {
        acp::SessionUpdate::AgentMessageChunk(chunk) => {
            // 从 ContentBlock 中提取文本 token
            match &chunk.content {
                acp::ContentBlock::Text(text_content) => {
                    Some(StreamingEvent::Token(text_content.text.clone()))
                }
                _ => None,
            }
        }
        acp::SessionUpdate::AgentThoughtChunk(chunk) => {
            // 思考 token 也作为普通 token 送入渲染器
            match &chunk.content {
                acp::ContentBlock::Text(text_content) => {
                    Some(StreamingEvent::Token(text_content.text.clone()))
                }
                _ => None,
            }
        }
        acp::SessionUpdate::ToolCall(tc) => Some(StreamingEvent::ToolCallStart {
            name: tc.title.clone(),
            id: tc.tool_call_id.0.to_string(),
        }),
        acp::SessionUpdate::ToolCallUpdate(tcu) => {
            // 仅在工具调用完成（Completed/Failed）时发送 ToolCallEnd
            match tcu.fields.status {
                Some(acp::ToolCallStatus::Completed) => Some(StreamingEvent::ToolCallEnd {
                    id: tcu.tool_call_id.0.to_string(),
                    success: true,
                }),
                Some(acp::ToolCallStatus::Failed) => Some(StreamingEvent::ToolCallEnd {
                    id: tcu.tool_call_id.0.to_string(),
                    success: false,
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

impl Default for StreamingRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 新建渲染器初始状态正确() {
        let r = StreamingRenderer::new();
        assert!(!r.is_streaming());
        assert!(!r.is_dirty());
        assert_eq!(r.token_count(), 0);
        assert!(r.content().is_empty());
        assert!(r.tool_blocks().is_empty());
    }

    #[test]
    fn 启动后进入流式状态() {
        let mut r = StreamingRenderer::new();
        r.start();
        assert!(r.is_streaming());
        assert!(r.is_dirty());
    }

    #[test]
    fn 非流式状态追加token无效() {
        let mut r = StreamingRenderer::new();
        r.append_token("hello");
        assert_eq!(r.content(), "");
        assert_eq!(r.token_count(), 0);
    }

    #[test]
    fn 流式追加token更新缓冲区() {
        let mut r = StreamingRenderer::new();
        r.start();
        r.append_token("hello");
        r.append_token(" world");
        assert_eq!(r.content(), "hello world");
        assert_eq!(r.token_count(), 2);
    }

    #[test]
    fn 事件处理token追加() {
        let mut r = StreamingRenderer::new();
        r.start();
        r.handle_event(StreamingEvent::Token("hi".into()));
        assert_eq!(r.content(), "hi");
    }

    #[test]
    fn 工具调用开始和结束() {
        let mut r = StreamingRenderer::new();
        r.start();
        r.handle_event(StreamingEvent::ToolCallStart {
            name: "Edit".into(),
            id: "tc1".into(),
        });
        assert!(r.has_in_progress_tools());
        assert_eq!(r.tool_blocks().len(), 1);
        assert!(r.tool_blocks()[0].is_in_progress());

        r.handle_event(StreamingEvent::ToolCallEnd {
            id: "tc1".into(),
            success: true,
        });
        assert!(!r.has_in_progress_tools());
        assert!(r.tool_blocks()[0].success.unwrap());
    }

    #[test]
    fn finish返回内容并重置状态() {
        let mut r = StreamingRenderer::new();
        r.start();
        r.append_token("content");
        let content = r.finish();
        assert_eq!(content, "content");
        assert!(!r.is_streaming());
        assert!(!r.is_dirty());
        assert!(r.content().is_empty());
    }

    #[test]
    fn 节流控制正常工作() {
        let mut r = StreamingRenderer::new();
        r.start();
        // 刚启动时 should_flush 可能为 false（时间间隔不够）
        // 但 dirty 标志为 true
        assert!(r.is_dirty());

        // 标记已刷新
        r.mark_flushed();
        assert!(!r.is_dirty());
    }

    #[test]
    fn 重新启动清空旧内容() {
        let mut r = StreamingRenderer::new();
        r.start();
        r.append_token("old");
        r.start(); // 重新启动
        assert!(r.content().is_empty());
        assert_eq!(r.token_count(), 0);
    }

    #[test]
    fn 未知id的工具调用结束被忽略() {
        let mut r = StreamingRenderer::new();
        r.start();
        r.handle_event(StreamingEvent::ToolCallStart {
            name: "Edit".into(),
            id: "tc1".into(),
        });
        r.handle_event(StreamingEvent::ToolCallEnd {
            id: "unknown".into(),
            success: true,
        });
        // 原始工具调用仍在进行中
        assert!(r.has_in_progress_tools());
    }
}
