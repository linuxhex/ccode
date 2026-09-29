//! TUI Node — 终端渲染、用户输入
//!
//! 交互风格对齐 Claude Code：
//! - 左侧对话区（流式 Markdown + 代码高亮）
//! - 右侧工具面板（运行中工具 + 最近工具）
//! - 底部状态栏（状态 + token/费用 + 快捷键）
//! - 底部输入框（多行 + 历史记录）

pub mod theme;

use async_trait::async_trait;
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},

    Terminal,
};
use std::io;
use std::time::Instant;

use ccode_textarea::{TextArea, TextAreaState};

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::node::{Node, NodeId, NodeType, NodeContext};
use crate::node::transport::NodeTransportHandle;

use theme::Theme;

// ── 数据类型 ──

#[derive(Debug, Clone, PartialEq)]
pub enum AgentStatus {
    Idle,
    Thinking,
    Outputting,
    ToolCalling,
    Error,
}

impl AgentStatus {
    fn spinner(&self, frame: usize) -> &'static str {
        const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        match self {
            Self::Idle => "●",
            Self::Thinking | Self::Outputting | Self::ToolCalling => {
                FRAMES[frame % FRAMES.len()]
            }
            Self::Error => "✕",
        }
    }

    fn status_text(&self) -> &'static str {
        match self {
            Self::Idle => "",
            Self::Thinking => "Thinking...",
            Self::Outputting => "Generating...",
            Self::ToolCalling => "Running tool...",
            Self::Error => "Error",
        }
    }

    fn is_active(&self) -> bool {
        matches!(self, Self::Thinking | Self::Outputting | Self::ToolCalling)
    }
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
enum ChatRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone)]
struct ChatMessage {
    role: ChatRole,
    content: String,
}

#[derive(Debug, Clone)]
struct ToolCard {
    name: String,
    status: ToolStatus,
    result_summary: String,
    started_at: Option<Instant>,
    elapsed: Option<std::time::Duration>,
}

#[derive(Debug, Clone, PartialEq)]
enum ToolStatus {
    Running,
    Done,
    Failed,
}

// ── TUI Node 主循环 ──

pub async fn run_tui_node(
    mut node: TUINode,
    ctx: crate::node::NodeContext,
) -> anyhow::Result<()> {
    let node_id = node.node_id().clone();
    let subscriptions = node.subscriptions();

    let connect_info = crate::node::transport::NodeConnectInfo {
        router_addr: ctx.router_addr.clone(),
        pub_addr: ctx.pub_addr.clone(),
        node_id: node_id.clone(),
        node_type: node.node_type().as_str().to_string(),
        subscriptions,
        data_pub_addr: ctx.data_pub_addr.clone(),
        published_topics: Vec::new(),
        data_rep_addr: None,
        service_name: None,
    };

    let mut transport = crate::node::transport::NodeTransport::connect(&connect_info)
        .await?;

    let handle = transport.handle().clone();

    node.start(ctx).await?;
    let tui_started_at = Instant::now();
    const ESC_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_secs(1);

    let mut heartbeat_timer = tokio::time::interval(std::time::Duration::from_secs(10));
    heartbeat_timer.tick().await;

    let mut render_timer = tokio::time::interval(std::time::Duration::from_millis(16));
    render_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    render_timer.tick().await;

    if node.terminal.is_some() {
        node.needs_render = true;
        if let Err(e) = node.render() {
            tracing::warn!("TUI 初始渲染失败：{}", e);
        }
        node.needs_render = false;
    }

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let has_terminal = node.terminal.is_some();
    let mut event_task = if has_terminal {
        Some(tokio::spawn(async move {
            let mut event_stream = EventStream::new();
            while let Some(result) = event_stream.next().await {
                match result {
                    Ok(event) => {
                        if event_tx.send(event).is_err() { break; }
                    }
                    Err(e) => {
                        tracing::warn!("TUI EventStream 错误：{}", e);
                    }
                }
            }
        }))
    } else {
        None
    };

    loop {
        tokio::select! {
            biased;

            event = event_rx.recv() => {
                let Some(event) = event else { break; };
                match event {
                    Event::Resize(..) => {
                        node.needs_render = true;
                        continue;
                    }
                    Event::Key(key) => {
                        if key.kind != crossterm::event::KeyEventKind::Press {
                            continue;
                        }
                        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                        let is_send = (key.code == KeyCode::Enter && !ctrl && !shift)
                            || (key.code == KeyCode::Char('j') && ctrl);

                        if is_send {
                            let text = node.textarea.text().to_string();
                            if !text.is_empty() {
                                if let Some(agent_id) = node.primary_agent_id.clone() {
                                    node.input_history.push(text.clone());
                                    node.history_index = None;
                                    node.push_user_message(&text);
                                    node.auto_scroll = true;
                                    match FrameCodec::new_message(
                                        Topic::agent_input(&agent_id),
                                        node_id.as_str(),
                                        &serde_json::json!({
                                            "role": "user",
                                            "content": text,
                                        }),
                                    ) {
                                        Ok(input_msg) => {
                                            if let Err(e) = handle.send_message(&input_msg).await {
                                                tracing::warn!("TUI 发送输入失败：{}", e);
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!("TUI 编码输入失败：{}", e);
                                        }
                                    }
                                    node.textarea.set_text("");
                                } else {
                                    tracing::warn!("TUI 输入被忽略：primary_agent_id 尚未设置，Agent 可能尚未就绪");
                                    node.push_system_message("⏳ Agent 尚未就绪，请稍候再试...");
                                    node.auto_scroll = true;
                                }
                            }
                        } else {
                            match key.code {
                                KeyCode::Up => {
                                    if !node.input_history.is_empty() {
                                        let idx = match node.history_index {
                                            Some(i) if i > 0 => i - 1,
                                            _ => node.input_history.len() - 1,
                                        };
                                        node.textarea.set_text(&node.input_history[idx]);
                                        node.history_index = Some(idx);
                                    }
                                }
                                KeyCode::Down => {
                                    if let Some(idx) = node.history_index {
                                        if idx + 1 < node.input_history.len() {
                                            node.textarea.set_text(&node.input_history[idx + 1]);
                                            node.history_index = Some(idx + 1);
                                        } else {
                                            node.textarea.set_text("");
                                            node.history_index = None;
                                        }
                                    }
                                }
                                KeyCode::PageUp => {
                                    node.scroll_offset = node.scroll_offset.saturating_add(10);
                                    node.auto_scroll = false;
                                }
                                KeyCode::PageDown => {
                                    node.scroll_offset = node.scroll_offset.saturating_sub(10);
                                    if node.scroll_offset == 0 {
                                        node.auto_scroll = true;
                                    }
                                }
                                KeyCode::Esc => {
                                    if tui_started_at.elapsed() < ESC_GRACE_PERIOD {
                                        continue;
                                    }
                                    let _ = FrameCodec::new_message(
                                        Topic::sys_shutdown(),
                                        node_id.as_str(),
                                        &serde_json::json!({}),
                                    ).map(|msg| {
                                        let h = handle.clone();
                                        tokio::spawn(async move { h.send_message(&msg).await });
                                    });
                                    break;
                                }
                                _ => {
                                    node.textarea.input(key);
                                }
                            }
                        }
                        node.needs_render = true;
                    }
                    _ => {}
                }
            }
            msg = transport.recv() => {
                match msg {
                    Ok(Some(msg)) => {
                        if msg.topic.as_str() == "sys/shutdown" { break; }
                        if msg.topic.as_str() == "sys/heartbeat" { continue; }
                        if msg.topic.as_str() == "sys/spawn" {
                            if node.primary_agent_id.is_none() {
                                if let Ok(payload) = FrameCodec::decode_payload::<serde_json::Value>(&msg) {
                                    let spawn_type = payload["node_type"].as_str().unwrap_or("");
                                    let spawn_id = payload["node_id"].as_str().unwrap_or("");
                                    if (spawn_type == "agent" || spawn_type == "thinker") && !spawn_id.is_empty() {
                                        node.set_primary_agent(spawn_id.to_string());
                                        let text = node.textarea.text().to_string();
                                        if !text.is_empty() {
                                            node.push_user_message(&text);
                                            node.input_history.push(text.clone());
                                            node.history_index = None;
                                            node.auto_scroll = true;
                                            if let Ok(input_msg) = FrameCodec::new_message(
                                                Topic::agent_input(spawn_id),
                                                node_id.as_str(),
                                                &serde_json::json!({"role": "user", "content": text}),
                                            ) {
                                                let _ = handle.send_message(&input_msg).await;
                                            }
                                            node.textarea.set_text("");
                                        }
                                    }
                                }
                            }
                            continue;
                        }
                        if let Err(e) = node.handle_message(msg, &handle).await {
                            tracing::warn!("TUI handle_message 错误：{}", e);
                        }
                    }
                    Ok(None) => { break; }
                    Err(e) => {
                        tracing::error!("TUI 传输层错误：{}", e);
                        break;
                    }
                }
            }
            _ = render_timer.tick() => {
                if node.terminal.is_some() {
                    let spinner_active = node.agent_status.is_active();
                    if node.needs_render || spinner_active {
                        if let Err(e) = node.render() {
                            tracing::warn!("TUI 渲染失败：{}", e);
                        }
                        node.needs_render = false;
                    }
                }
            }
            _ = heartbeat_timer.tick() => {
                if let Ok(msg) = FrameCodec::new_message(
                    Topic::sys_heartbeat(),
                    node_id.as_str(),
                    &serde_json::json!({ "node_id": node_id.to_string() }),
                ) {
                    let _ = handle.send_message(&msg).await;
                }
            }
        }
    }

    if let Some(task) = event_task.take() {
        task.abort();
    }
    node.stop().await?;
    transport.shutdown().await;
    Ok(())
}

// ── TUINode 结构体 ──

pub struct TUINode {
    id: NodeId,
    primary_agent_id: Option<String>,
    agent_status: AgentStatus,
    messages: Vec<ChatMessage>,
    /// 缓存每条消息的 Markdown 渲染结果，避免每帧重新渲染
    md_cache: Vec<Option<Vec<Line<'static>>>>,
    /// 缓存时的终端宽度（宽度变化时清空缓存）
    md_cache_width: u16,
    textarea: TextArea,
    textarea_state: TextAreaState,
    terminal: Option<Terminal<CrosstermBackend<io::Stdout>>>,
    scroll_offset: usize,
    auto_scroll: bool,
    input_history: Vec<String>,
    history_index: Option<usize>,
    tool_cards: Vec<ToolCard>,
    frame_counter: usize,
    textarea_inner_area: Option<Rect>,
    total_input_tokens: usize,
    total_output_tokens: usize,
    estimated_cost: f64,
    model_name: String,
    theme: Theme,
    md_style: ccode_markdown::MarkdownStyle,
    needs_render: bool,
}

impl TUINode {
    pub fn new(id: NodeId) -> Self {
        let theme = Theme::dark();
        let md_style = theme.markdown_style();
        Self {
            id,
            primary_agent_id: None,
            agent_status: AgentStatus::Idle,
            messages: Vec::new(),
            md_cache: Vec::new(),
            md_cache_width: 0,
            textarea: TextArea::new(),
            textarea_state: TextAreaState::default(),
            terminal: None,
            scroll_offset: 0,
            auto_scroll: true,
            input_history: Vec::new(),
            history_index: None,
            tool_cards: Vec::new(),
            frame_counter: 0,
            textarea_inner_area: None,
            total_input_tokens: 0,
            total_output_tokens: 0,
            estimated_cost: 0.0,
            model_name: String::new(),
            theme,
            md_style,
            needs_render: false,
        }
    }

    pub fn set_primary_agent(&mut self, agent_id: String) {
        self.primary_agent_id = Some(agent_id);
    }

    pub fn set_runtime_info(&mut self, model: &str, _working_dir: &str, _context_window: usize) {
        self.model_name = model.to_string();
    }

    pub fn set_skills_count(&mut self, _count: usize) {}

    fn append_assistant_content(&mut self, chunk: &str) {
        if let Some(last) = self.messages.last_mut() {
            if last.role == ChatRole::Assistant {
                last.content.push_str(chunk);
                return;
            }
        }
        self.messages.push(ChatMessage {
            role: ChatRole::Assistant,
            content: chunk.to_string(),
        });
    }

    fn push_user_message(&mut self, text: &str) {
        self.messages.push(ChatMessage {
            role: ChatRole::User,
            content: text.to_string(),
        });
    }

    fn push_system_message(&mut self, text: &str) {
        self.messages.push(ChatMessage {
            role: ChatRole::System,
            content: text.to_string(),
        });
    }

    fn init_terminal(&mut self) -> anyhow::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        self.terminal = Some(Terminal::new(backend)?);
        Ok(())
    }

    fn restore_terminal(&mut self) -> anyhow::Result<()> {
        if let Some(mut terminal) = self.terminal.take() {
            disable_raw_mode()?;
            execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        }
        Ok(())
    }

    // ── 渲染 ──

    fn render(&mut self) -> anyhow::Result<()> {
        self.frame_counter = self.frame_counter.wrapping_add(1);
        let frame = self.frame_counter;

        let terminal = match self.terminal.as_mut() {
            Some(t) => t,
            None => return Ok(()),
        };

        let mut captured_inner: Option<Rect> = None;
        let captured_inner_ref = &mut captured_inner;

        let textarea = std::mem::replace(&mut self.textarea, TextArea::new());
        let mut textarea_state = std::mem::take(&mut self.textarea_state);

        let messages = &self.messages;
        let tool_cards = &self.tool_cards;
        let agent_status = &self.agent_status;
        let theme = &self.theme;
        let md_style = &self.md_style;
        let model_name = &self.model_name;
        let total_in = self.total_input_tokens;
        let total_out = self.total_output_tokens;
        let cost = self.estimated_cost;
        let auto_scroll = &mut self.auto_scroll;
        let scroll_offset = &mut self.scroll_offset;
        let md_cache = &mut self.md_cache;
        let md_cache_width = &mut self.md_cache_width;

        terminal.draw(|f| {
            let size = f.area();
            if size.width < 4 || size.height < 4 {
                return;
            }

            // 宽度变化时清空缓存
            if *md_cache_width != size.width {
                md_cache.clear();
                *md_cache_width = size.width;
            }
            // 确保缓存大小与消息数量一致
            while md_cache.len() < messages.len() {
                md_cache.push(None);
            }
            if md_cache.len() > messages.len() {
                md_cache.truncate(messages.len());
            }

            let input_text_lines = textarea.text().lines().count().max(1) as u16;
            let max_input_height = (size.height / 3).max(1);
            let input_height = input_text_lines.min(max_input_height);

            // 布局（Claude Code 风格）：
            // ┌─ 标题栏 ─────────────────────────────────────┐
            // ├─ 对话区 ────────────────────┼─ 工具侧栏 ────┤
            // │                            │               │
            // │                            │               │
            // ├─ 状态栏 ────────────────────────────────────┤
            // ├─ 输入区 ────────────────────────────────────┤
            let v_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),  // 标题栏
                    Constraint::Min(5),     // 主区（对话 + 工具侧栏）
                    Constraint::Length(1),  // 状态栏
                    Constraint::Length(input_height),
                ])
                .split(size);

            let title_area = v_chunks[0];
            let main_area = v_chunks[1];
            let status_area = v_chunks[2];
            let input_area = v_chunks[3];

            // 主区分左右：对话区 + 工具侧栏
            // 终端宽度不足 80 列时隐藏工具侧栏
            let show_sidebar = main_area.width >= 80;
            let (conv_area, sidebar_area) = if show_sidebar {
                let h_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Ratio(3, 4),  // 对话区占 3/4
                        Constraint::Ratio(1, 4),  // 工具侧栏占 1/4
                    ])
                    .split(main_area);
                (h_chunks[0], Some(h_chunks[1]))
            } else {
                (main_area, None)
            };

            // 标题栏
            render_title_bar(f, title_area, model_name, agent_status, frame, theme);

            // 对话区（使用缓存）
            render_conversation(
                f, conv_area, messages, md_cache, scroll_offset, auto_scroll,
                agent_status, frame, tool_cards, theme, md_style,
            );

            // 工具侧栏
            if let Some(sa) = sidebar_area {
                render_tool_sidebar(f, sa, tool_cards, frame, theme);
            }

            // 状态栏（底部）
            render_status_bar(
                f, status_area, agent_status, frame,
                model_name, total_in, total_out, cost, theme,
            );

            // 输入区
            render_input(f, input_area, &textarea, &mut textarea_state, captured_inner_ref, theme);
        })?;

        self.textarea = textarea;
        self.textarea_state = textarea_state;

        // 光标定位
        if let Some(inner) = captured_inner {
            self.textarea_inner_area = Some(inner);
            if let Some(term) = self.terminal.as_mut() {
                if inner.width > 2 {
                    let textarea_screen_area = Rect {
                        x: inner.x + 2,
                        y: inner.y,
                        width: inner.width - 2,
                        height: inner.height,
                    };
                    let (cx, cy) = self.textarea.cursor_pos_with_state(textarea_screen_area, self.textarea_state)
                        .unwrap_or((inner.x + 2, inner.y));
                    term.show_cursor()?;
                    term.set_cursor_position(ratatui::layout::Position { x: cx, y: cy })?;
                } else {
                    term.show_cursor()?;
                    term.set_cursor_position(ratatui::layout::Position { x: inner.x, y: inner.y })?;
                }
            }
        }

        Ok(())
    }
}

// ── 对话区渲染 ──

#[allow(clippy::too_many_arguments)]
fn render_conversation(
    f: &mut ratatui::Frame,
    area: Rect,
    messages: &[ChatMessage],
    md_cache: &mut Vec<Option<Vec<Line<'static>>>>,
    scroll_offset: &mut usize,
    auto_scroll: &mut bool,
    agent_status: &AgentStatus,
    frame: usize,
    tool_cards: &[ToolCard],
    theme: &Theme,
    md_style: &ccode_markdown::MarkdownStyle,
) {
    if area.width < 4 || area.height < 1 {
        return;
    }

    let mut all_lines: Vec<Line> = Vec::new();

    for (idx, msg) in messages.iter().enumerate() {
        match msg.role {
            ChatRole::User => {
                // 用户消息：上方间距 + "You" 标签 + 内容
                all_lines.push(Line::from(""));
                all_lines.push(Line::from(vec![
                    Span::styled(" You ", theme.user_label_style()),
                ]));
                for raw in msg.content.lines() {
                    let user_span = Span::styled(format!("  {raw}"), theme.user_text_style());
                    all_lines.push(Line::from(user_span));
                }
            }
            ChatRole::Assistant => {
                // 助手消息：上方间距 + 模型名标签 + Markdown 内容
                all_lines.push(Line::from(""));
                // 使用缓存的 Markdown 渲染结果
                // 最后一条助手消息在流式输出时始终重新渲染，其他消息使用缓存
                let is_last_assistant = idx == messages.len() - 1;
                let cached = if is_last_assistant {
                    None // 强制重新渲染最后一条
                } else {
                    md_cache.get(idx).and_then(|c| c.clone())
                };

                let md_lines = if let Some(cached_lines) = cached {
                    cached_lines
                } else {
                    let (lines, _) = ccode_markdown::render_markdown_ratatui(
                        &msg.content,
                        *md_style,
                        true,
                        None,
                    );
                    // 更新缓存（非最后一条才缓存，最后一条每帧都在变）
                    if !is_last_assistant && idx < md_cache.len() {
                        md_cache[idx] = Some(lines.clone());
                    }
                    lines
                };
                // 助手标签
                all_lines.push(Line::from(vec![
                    Span::styled(" ccode ", theme.assistant_label_style()),
                ]));
                // 内容缩进 2 格
                for line in md_lines {
                    let mut indented = Line::from("  ");
                    for span in line.spans {
                        indented.spans.push(span);
                    }
                    all_lines.push(indented);
                }
            }
            ChatRole::System => {
                all_lines.push(Line::from(""));
                all_lines.push(Line::from(Span::styled(
                    format!("· {} ·", &msg.content),
                    theme.system_text_style(),
                )));
            }
        }
    }

    // 工具卡片
    for card in tool_cards.iter().filter(|c| c.status == ToolStatus::Running).take(3) {
        let elapsed = card.started_at
            .map(|s| s.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        let summary = card.result_summary.chars().take(40).collect::<String>();
        all_lines.push(Line::from(vec![
            Span::styled("  ⏳ ", Style::default().fg(Color::Yellow)),
            Span::styled(&card.name, theme.tool_name_style()),
            Span::styled(format!("  {:.0}s", elapsed), theme.tool_time_style()),
            if summary.is_empty() {
                Span::raw("")
            } else {
                Span::styled(format!("  ·  {}", summary), theme.tool_time_style())
            },
        ]));
    }

    // Spinner
    if agent_status.is_active() {
        if let Some(last) = messages.last() {
            if last.role != ChatRole::Assistant {
                all_lines.push(Line::from(""));
            }
        }
        let spinner_char = agent_status.spinner(frame);
        let status_text = agent_status.status_text();
        all_lines.push(Line::from(vec![
            Span::styled(format!("  {} ", spinner_char), theme.spinner_style()),
            Span::styled(status_text, theme.status_active_style()),
        ]));
    }

    // 自动跟随滚动
    let viewport_h = area.height as usize;
    let total = all_lines.len();
    if *auto_scroll {
        *scroll_offset = 0;
    }
    let start = if total > viewport_h {
        (total - viewport_h).saturating_sub(*scroll_offset)
    } else {
        0
    };
    let visible = &all_lines[start..];

    let buf = f.buffer_mut();
    for (i, line) in visible.iter().enumerate() {
        let y = area.y + i as u16;
        if y >= area.y + area.height {
            break;
        }
        buf.set_line(area.x, y, line, area.width);
    }
}

// ── 标题栏 ──

fn render_title_bar(
    f: &mut ratatui::Frame,
    area: Rect,
    model_name: &str,
    agent_status: &AgentStatus,
    frame: usize,
    theme: &Theme,
) {
    if area.height < 1 || area.width < 10 {
        return;
    }
    let buf = f.buffer_mut();

    // 背景条
    let bg = " ".repeat(area.width as usize);
    buf.set_string(area.x, area.y, &bg, Style::default().bg(Color::Rgb(0x2a, 0x2a, 0x2a)));

    // 左侧：ccode logo + 状态
    let spinner = agent_status.spinner(frame);
    let status_text = agent_status.status_text();
    let left = format!(" ccode {spinner} {status_text}");
    buf.set_string(area.x, area.y, &left, theme.status_active_style());

    // 中间：模型名
    if !model_name.is_empty() && area.width > 40 {
        let model_display = format!(" {model_name} ");
        let mid_x = area.x + (area.width.saturating_sub(model_display.len() as u16)) / 2;
        buf.set_string(mid_x, area.y, &model_display, theme.model_style().add_modifier(Modifier::BOLD));
    }

    // 右侧：快捷键提示
    if area.width > 60 {
        let right = " ? help  Ctrl+C quit ";
        let right_x = area.x + area.width - right.len() as u16;
        buf.set_string(right_x, area.y, right, theme.help_style());
    }
}

// ── 工具侧栏 ──

fn render_tool_sidebar(
    f: &mut ratatui::Frame,
    area: Rect,
    tool_cards: &[ToolCard],
    frame: usize,
    theme: &Theme,
) {
    if area.height < 3 || area.width < 20 {
        return;
    }
    let buf = f.buffer_mut();

    // 标题
    let title = " Tools ";
    buf.set_string(area.x, area.y, title, theme.primary_style_bold());

    // 分隔线
    let sep = "─".repeat(area.width as usize);
    buf.set_string(area.x, area.y + 1, &sep, Style::default().fg(theme.border));

    // 运行中的工具
    let running: Vec<&ToolCard> = tool_cards.iter().filter(|c| c.status == ToolStatus::Running).collect();
    let mut y = area.y + 2;

    if running.is_empty() {
        if y < area.y + area.height {
            buf.set_string(area.x + 1, y, "  No active tools", theme.text_muted_style());
        }
    } else {
        for card in running.iter().take(10) {
            if y >= area.y + area.height.saturating_sub(1) {
                break;
            }
            let elapsed = card.started_at
                .map(|s| s.elapsed().as_secs_f32())
                .unwrap_or(0.0);
            let spinner_frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let spinner = spinner_frames[frame % spinner_frames.len()];
            let name = truncate_str(&card.name, (area.width as usize).saturating_sub(10));
            let line = format!(" {spinner} {name} {elapsed:.1}s");
            buf.set_string(area.x + 1, y, &line, theme.tool_name_style());
            y += 1;
        }
    }

    // 最近完成的工具
    let done: Vec<&ToolCard> = tool_cards.iter()
        .rev()
        .filter(|c| c.status != ToolStatus::Running)
        .take(5)
        .collect();

    if !done.is_empty() && y < area.y + area.height.saturating_sub(1) {
        // 分隔
        y += 1;
        if y < area.y + area.height {
            let recent = " Recent ";
            buf.set_string(area.x + 1, y, recent, theme.text_muted_style());
            y += 1;
        }
        for card in &done {
            if y >= area.y + area.height.saturating_sub(1) {
                break;
            }
            let (icon, style) = match card.status {
                ToolStatus::Done => ("✓", theme.tool_done_style()),
                ToolStatus::Failed => ("✕", theme.tool_fail_style()),
                ToolStatus::Running => ("", Style::default()),
            };
            let name = truncate_str(&card.name, (area.width as usize).saturating_sub(6));
            let line = format!("   {icon} {name}");
            buf.set_string(area.x + 1, y, &line, style);
            y += 1;
        }
    }
}

/// 按字符宽度截断字符串（视觉截断）
fn truncate_str(s: &str, max: usize) -> String {
    if max < 3 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        let mut result: String = chars.into_iter().take(max.saturating_sub(1)).collect();
        result.push('…');
        result
    }
}

// ── 状态栏 ──

#[allow(clippy::too_many_arguments)]
fn render_status_bar(
    f: &mut ratatui::Frame,
    area: Rect,
    agent_status: &AgentStatus,
    frame: usize,
    model_name: &str,
    total_in: usize,
    total_out: usize,
    cost: f64,
    theme: &Theme,
) {
    if area.height < 1 || area.width < 10 {
        return;
    }

    let buf = f.buffer_mut();

    // 分隔线
    let sep = "─".repeat(area.width as usize);
    buf.set_string(area.x, area.y, &sep, Style::default().fg(theme.border));

    // 左侧：状态
    let left = if agent_status.is_active() {
        let spinner = agent_status.spinner(frame);
        format!("{} {}", spinner, agent_status.status_text())
    } else {
        "● ready".to_string()
    };

    // 右侧：model + tokens + cost
    let right = if !model_name.is_empty() {
        format!(
            "{}  ↑{} ↓{}  ${:.4}",
            model_name, total_in, total_out, cost
        )
    } else {
        format!("↑{} ↓{}  ${:.4}", total_in, total_out, cost)
    };

    let left_style = if agent_status.is_active() {
        theme.status_active_style()
    } else {
        theme.status_style()
    };

    buf.set_string(area.x, area.y, &left, left_style);

    if right.len() < area.width as usize {
        let right_x = area.x + area.width - right.len() as u16;
        buf.set_string(right_x, area.y, &right, theme.model_style());
    }
}

// ── 输入区 ──

fn render_input(
    f: &mut ratatui::Frame,
    area: Rect,
    textarea: &TextArea,
    textarea_state: &mut TextAreaState,
    captured_inner: &mut Option<Rect>,
    theme: &Theme,
) {
    if area.height < 3 || area.width < 10 {
        return;
    }
    let buf = f.buffer_mut();

    // 边框（顶部 + 左右）
    let top_border = format!("┌{}┐", "─".repeat((area.width as usize).saturating_sub(2)));
    buf.set_string(area.x, area.y, &top_border, Style::default().fg(theme.border));

    for y in 1..area.height.saturating_sub(1) {
        buf.set_string(area.x, area.y + y, "│", Style::default().fg(theme.border));
        buf.set_string(area.x + area.width - 1, area.y + y, "│", Style::default().fg(theme.border));
    }

    // 底部边框 + 发送提示
    let bottom_left = "└";
    let bottom_right = "┘";
    buf.set_string(area.x, area.y + area.height - 1, bottom_left, Style::default().fg(theme.border));
    buf.set_string(area.x + area.width - 1, area.y + area.height - 1, bottom_right, Style::default().fg(theme.border));

    // 底部提示（Enter 发送 / Esc 取消）
    let hint = " Enter to send · Esc to clear ";
    let hint_x = area.x + area.width.saturating_sub(hint.len() as u16);
    // 用边框色覆盖底部线的中间部分
    let bottom_line_len = (area.width as usize).saturating_sub(2 + hint.len() + 2);
    let bottom_line = "─".repeat(bottom_line_len.max(0));
    buf.set_string(area.x + 1, area.y + area.height - 1, &bottom_line, Style::default().fg(theme.border));
    buf.set_string(hint_x.saturating_sub(1), area.y + area.height - 1, hint, theme.help_style());

    // 输入区域（边框内）
    let inner_x = area.x + 1;
    let inner_y = area.y + 1;
    let inner_w = area.width.saturating_sub(2);
    let inner_h = area.height.saturating_sub(2);

    let has_text = !textarea.text().is_empty();
    let prefix_style = if has_text {
        theme.input_prefix_style()
    } else {
        theme.placeholder_style()
    };

    let prefix_w = 2u16;
    buf.set_string(inner_x, inner_y, "› ", prefix_style);

    if inner_w > prefix_w {
        let textarea_area = Rect {
            x: inner_x + prefix_w,
            y: inner_y,
            width: inner_w - prefix_w,
            height: inner_h,
        };

        // 捕获光标位置的区域
        *captured_inner = Some(textarea_area);

        ratatui::widgets::StatefulWidgetRef::render_ref(&textarea, textarea_area, buf, textarea_state);

        if !has_text {
            buf.set_string(
                textarea_area.x,
                textarea_area.y,
                "Ask anything... (Shift+Enter for newline)",
                theme.placeholder_style(),
            );
        }
    }
}

fn estimate_cost(input_tokens: usize, output_tokens: usize) -> f64 {
    (input_tokens as f64) * 2.5 / 1_000_000.0 + (output_tokens as f64) * 10.0 / 1_000_000.0
}

// ── Node trait 实现 ──

#[async_trait]
impl Node for TUINode {
    fn node_type(&self) -> NodeType { NodeType::TUI }
    fn node_id(&self) -> &NodeId { &self.id }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        if std::env::var("CCODE_HEADLESS").is_err() {
            self.init_terminal()?;
        }
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, _transport: &NodeTransportHandle) -> anyhow::Result<()> {
        match msg.topic.as_str() {
            t if t.ends_with("/output") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                if let Some(content) = payload["content"].as_str() {
                    self.agent_status = AgentStatus::Outputting;
                    self.append_assistant_content(content);
                    if self.terminal.is_none() {
                        print!("{}", content);
                        use std::io::Write;
                        let _ = std::io::stdout().flush();
                    } else {
                        self.needs_render = true;
                    }
                }
            }
            t if t.ends_with("/tool_result") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let tool_name = payload["tool_name"].as_str().unwrap_or("unknown");
                let success = payload["success"].as_bool().unwrap_or(true);
                let output = payload["output"].as_str().unwrap_or("");

                if let Some(card) = self.tool_cards.iter_mut().rev()
                    .find(|c| c.name == tool_name && c.status == ToolStatus::Running)
                {
                    let elapsed = card.started_at.map(|s| s.elapsed());
                    card.status = if success { ToolStatus::Done } else { ToolStatus::Failed };
                    card.elapsed = elapsed;
                    card.result_summary = output.lines().next().unwrap_or("").to_string();
                } else {
                    self.tool_cards.push(ToolCard {
                        name: tool_name.to_string(),
                        status: if success { ToolStatus::Done } else { ToolStatus::Failed },
                        result_summary: output.lines().next().unwrap_or("").to_string(),
                        started_at: None,
                        elapsed: None,
                    });
                }

                if self.tool_cards.len() > 10 {
                    self.tool_cards.remove(0);
                }
                self.needs_render = true;
            }
            t if t.ends_with("/event") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                if let Some(state) = payload["state"].as_str() {
                    self.agent_status = match state {
                        "thinking" => AgentStatus::Thinking,
                        "outputting" => AgentStatus::Outputting,
                        "tool_calling" => AgentStatus::ToolCalling,
                        "error" => AgentStatus::Error,
                        "idle" => AgentStatus::Idle,
                        _ => AgentStatus::Idle,
                    };
                    // Headless 模式：Agent 进入 idle 状态后自动退出
                    if state == "idle" && self.terminal.is_none() {
                        tracing::info!("Headless 模式：Agent 已完成，发送关闭信号");
                        if let Ok(shutdown_msg) = FrameCodec::new_message(
                            Topic::sys_shutdown(),
                            self.id.as_str(),
                            &serde_json::json!({}),
                        ) {
                            let handle = _transport.clone();
                            tokio::spawn(async move {
                                let _ = handle.send_message(&shutdown_msg).await;
                            });
                        }
                    }
                }
                if let Some(input_tokens) = payload["input_tokens"].as_u64() {
                    self.total_input_tokens += input_tokens as usize;
                }
                if let Some(output_tokens) = payload["output_tokens"].as_u64() {
                    self.total_output_tokens += output_tokens as usize;
                }
                self.estimated_cost = estimate_cost(self.total_input_tokens, self.total_output_tokens);
                self.needs_render = true;
            }
            t if t.ends_with("/tool_call") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                if let Some(tool_name) = payload["tool_name"].as_str() {
                    self.agent_status = AgentStatus::ToolCalling;
                    self.tool_cards.push(ToolCard {
                        name: tool_name.to_string(),
                        status: ToolStatus::Running,
                        result_summary: "执行中...".to_string(),
                        started_at: Some(Instant::now()),
                        elapsed: None,
                    });
                    self.needs_render = true;
                }
            }
            "sys/shutdown" => {
                self.restore_terminal()?;
            }
            _ => {}
        }
        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        let mut subs = vec!["sys/shutdown".into(), "sys/spawn".into()];
        if let Some(agent_id) = &self.primary_agent_id {
            subs.push(format!("agent/{}/output", agent_id));
            subs.push(format!("agent/{}/event", agent_id));
            subs.push(format!("agent/{}/tool_call", agent_id));
            subs.push(format!("agent/{}/tool_result", agent_id));
        } else {
            subs.push("agent/*/output".into());
            subs.push("agent/*/event".into());
            subs.push("agent/*/tool_call".into());
            subs.push("agent/*/tool_result".into());
        }
        subs
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        self.restore_terminal()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_status_spinner() {
        for frame in 0..100 {
            let _ = AgentStatus::Thinking.spinner(frame);
            let _ = AgentStatus::Outputting.spinner(frame);
            let _ = AgentStatus::ToolCalling.spinner(frame);
        }
    }

    #[test]
    fn auto_scroll_default() {
        let node = TUINode::new(NodeId::new());
        assert!(node.auto_scroll);
    }

    #[test]
    fn input_height_empty() {
        let textarea = TextArea::new();
        let input_text_lines = textarea.text().lines().count().max(1) as u16;
        let terminal_height: u16 = 30;
        let max_input_height = (terminal_height / 3).max(1);
        let input_height = input_text_lines.min(max_input_height);
        assert_eq!(input_height, 1);
    }
}
