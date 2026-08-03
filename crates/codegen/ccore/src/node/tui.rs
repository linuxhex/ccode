//! TUI Node - 终端渲染、用户输入
//!
//! TUI Node 的职责：
//! 1. 订阅 Agent 输出 → 渲染到终端（语法高亮 + diff 着色）
//! 2. 读取用户输入 → 发送到 Agent input topic（多行输入支持）
//! 3. 展示工具调用状态卡片（名称/状态/耗时/结果摘要）
//! 4. 展示 Agent 状态指示器（thinking/outputting/tool_calling）
//!
//! 渲染基于 ratatui + crossterm 后端

use async_trait::async_trait;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal,
};
use std::io;

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::node::{Node, NodeId, NodeType, NodeContext};
use crate::node::transport::NodeTransportHandle;

use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

/// Agent 状态显示
#[derive(Debug, Clone, PartialEq)]
pub enum AgentStatus {
    Idle,
    Thinking,
    Outputting,
    ToolCalling,
    Error,
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "●"),
            Self::Thinking => write!(f, "◔ thinking"),
            Self::Outputting => write!(f, "◉ outputting"),
            Self::ToolCalling => write!(f, "⚙ tool_calling"),
            Self::Error => write!(f, "✕ error"),
        }
    }
}

/// 工具执行卡片
#[derive(Debug, Clone)]
struct ToolCard {
    name: String,
    status: ToolStatus,
    result_summary: String,
}

#[derive(Debug, Clone, PartialEq)]
enum ToolStatus {
    Running,
    Done,
    Failed,
}

impl std::fmt::Display for ToolStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "⏳"),
            Self::Done => write!(f, "✅"),
            Self::Failed => write!(f, "❌"),
        }
    }
}

/// TUI 专用消息循环
///
/// 与标准 run_node 不同，TUI 需要同时监听消息总线和键盘输入。
/// 使用 tokio::select! 实现并发。
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
    tracing::info!("TUI Node {} 已启动，进入交互循环", node_id);

    let mut input_buffer = String::new();

    let mut heartbeat_timer = tokio::time::interval(std::time::Duration::from_secs(10));
    heartbeat_timer.tick().await;

    loop {
        tokio::select! {
            msg = transport.recv() => {
                match msg {
                    Ok(Some(msg)) => {
                        if msg.topic.as_str() == "sys/shutdown" {
                            break;
                        }
                        if msg.topic.as_str() == "sys/heartbeat" {
                            continue;
                        }
                        if msg.topic.as_str() == "sys/spawn" {
                            if node.primary_agent_id.is_none() {
                                if let Ok(payload) = FrameCodec::decode_payload::<serde_json::Value>(&msg) {
                                    if let Some(node_type) = payload["node_type"].as_str() {
                                        if node_type == "agent" || node_type == "thinker" {
                                            if let Some(agent_id) = payload["node_id"].as_str() {
                                                node.set_primary_agent(agent_id.to_string());
                                                tracing::info!("TUI 设置 primary agent：{}", agent_id);
                                            }
                                        }
                                    }
                                }
                            }
                            continue;
                        }
                        node.handle_message(msg, &handle).await?;
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::error!("TUI 传输层错误：{}", e);
                        break;
                    }
                }
            }
            _ = heartbeat_timer.tick() => {
                let heartbeat_msg = FrameCodec::new_message(
                    Topic::sys_heartbeat(),
                    node_id.as_str(),
                    &serde_json::json!({ "node_id": node_id.to_string() }),
                )?;
                if let Err(e) = handle.send_message(&heartbeat_msg).await {
                    tracing::warn!("TUI 心跳发送失败：{}", e);
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                if event::poll(std::time::Duration::from_millis(0))? {
                    if let Event::Key(key) = event::read()? {
                        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                        match key.code {
                            // Enter 发送消息；Ctrl+Enter 换行（多行输入）
                            KeyCode::Enter if ctrl => {
                                input_buffer.push('\n');
                            }
                            KeyCode::Enter => {
                                if !input_buffer.is_empty() {
                                    node.input_history.push(input_buffer.clone());
                                    node.history_index = None;
                                    if let Some(agent_id) = &node.primary_agent_id {
                                        let input_msg = FrameCodec::new_message(
                                            Topic::agent_input(agent_id),
                                            node_id.as_str(),
                                            &serde_json::json!({
                                                "role": "user",
                                                "content": input_buffer.clone(),
                                            }),
                                        )?;
                                        handle.send_message(&input_msg).await?;
                                    }
                                    input_buffer.clear();
                                    if node.terminal.is_some() {
                                        node.render()?;
                                    }
                                }
                            }
                            KeyCode::Up => {
                                if !node.input_history.is_empty() {
                                    let idx = match node.history_index {
                                        Some(i) if i > 0 => i - 1,
                                        _ => node.input_history.len() - 1,
                                    };
                                    input_buffer = node.input_history[idx].clone();
                                    node.history_index = Some(idx);
                                }
                            }
                            KeyCode::Down => {
                                if let Some(idx) = node.history_index {
                                    if idx + 1 < node.input_history.len() {
                                        input_buffer = node.input_history[idx + 1].clone();
                                        node.history_index = Some(idx + 1);
                                    } else {
                                        input_buffer.clear();
                                        node.history_index = None;
                                    }
                                }
                            }
                            KeyCode::PageUp => {
                                node.scroll_offset = node.scroll_offset.saturating_add(10);
                                if node.terminal.is_some() {
                                    node.render()?;
                                }
                            }
                            KeyCode::PageDown => {
                                node.scroll_offset = node.scroll_offset.saturating_sub(10);
                                if node.terminal.is_some() {
                                    node.render()?;
                                }
                            }
                            KeyCode::Char(c) => {
                                input_buffer.push(c);
                            }
                            KeyCode::Backspace => {
                                input_buffer.pop();
                            }
                            KeyCode::Tab => {
                                node.show_diff = !node.show_diff;
                                if node.terminal.is_some() {
                                    node.render()?;
                                }
                            }
                            KeyCode::Esc => {
                                tracing::info!("用户按 Esc 退出");
                                break;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    node.stop().await?;
    transport.shutdown().await;
    tracing::info!("TUI Node {} 已停止", node_id);
    Ok(())
}

/// 将 syntect 颜色转为 ratatui Color
fn syntect_to_ratatui_color(color: syntect::highlighting::Color) -> Color {
    Color::Rgb(color.r, color.g, color.b)
}

pub struct TUINode {
    id: NodeId,
    primary_agent_id: Option<String>,
    agent_status: AgentStatus,
    output_buffer: String,
    tool_status: String,
    terminal: Option<Terminal<CrosstermBackend<io::Stdout>>>,
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
    scroll_offset: usize,
    input_history: Vec<String>,
    history_index: Option<usize>,
    /// 工具执行卡片列表（最近 5 个工具）
    tool_cards: Vec<ToolCard>,
    /// 当前 diff 内容（来自 write/edit 工具的输出）
    diff_content: String,
    /// 是否显示 diff 区而非输出区
    show_diff: bool,
}

impl TUINode {
    pub fn new(id: NodeId) -> Self {
        Self {
            id,
            primary_agent_id: None,
            agent_status: AgentStatus::Idle,
            output_buffer: String::new(),
            tool_status: String::new(),
            terminal: None,
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            scroll_offset: 0,
            input_history: Vec::new(),
            history_index: None,
            tool_cards: Vec::new(),
            diff_content: String::new(),
            show_diff: false,
        }
    }

    pub fn set_primary_agent(&mut self, agent_id: String) {
        self.primary_agent_id = Some(agent_id);
    }

    fn init_terminal(&mut self) -> anyhow::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        self.terminal = Some(terminal);
        Ok(())
    }

    fn restore_terminal(&mut self) -> anyhow::Result<()> {
        if let Some(mut terminal) = self.terminal.take() {
            disable_raw_mode()?;
            execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        }
        Ok(())
    }

    /// 从工具输出中提取 diff 内容
    fn extract_diff(&mut self, tool_name: &str, output: &str) {
        // write/edit 工具的输出可能包含 diff
        if matches!(tool_name, "write" | "edit" | "write_file") {
            // 检测是否包含 unified diff 格式
            if output.contains("@@") && (output.contains("---") || output.contains("+++")) {
                self.diff_content = output.to_string();
                self.show_diff = true;
            } else {
                // 尝试从输出中提取 diff 块
                if let Some(diff_start) = output.find("```diff") {
                    if let Some(diff_end) = output[diff_start + 7..].find("```") {
                        self.diff_content = output[diff_start + 7..diff_start + 7 + diff_end].to_string();
                        self.show_diff = true;
                    }
                }
            }
        }
    }

    /// 渲染 TUI 界面
    fn render(&mut self) -> anyhow::Result<()> {
        let terminal = match self.terminal.as_mut() {
            Some(t) => t,
            None => return Ok(()),
        };

        let highlighted_lines = self.highlight_output();
        let diff_lines = self.render_diff();
        let has_tools = !self.tool_cards.is_empty();

        terminal.draw(|f| {
            let size = f.area();

            // 水平分割：主区域 + 工具卡片侧边栏
            let h_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(if has_tools {
                    vec![Constraint::Min(40), Constraint::Length(28)]
                } else {
                    vec![Constraint::Min(40), Constraint::Length(0)]
                })
                .split(size);

            // 垂直分割主区域
            let v_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),  // 状态栏
                    Constraint::Min(5),     // 输出区
                    Constraint::Length(2),  // 输入区
                ])
                .split(h_chunks[0]);

            // 状态栏
            let status_text = Line::from(vec![
                Span::styled(" ccode ", Style::default().fg(Color::Black).bg(Color::Cyan)),
                Span::raw(" "),
                Span::styled(
                    self.agent_status.to_string(),
                    Style::default()
                        .fg(match &self.agent_status {
                            AgentStatus::Thinking => Color::Yellow,
                            AgentStatus::Outputting => Color::Green,
                            AgentStatus::ToolCalling => Color::Blue,
                            AgentStatus::Error => Color::Red,
                            _ => Color::Gray,
                        })
                        .add_modifier(Modifier::BOLD),
                ),
                if !self.tool_status.is_empty() {
                    Span::styled(format!("  {}", self.tool_status), Style::default().fg(Color::DarkGray))
                } else {
                    Span::raw("")
                },
                if self.show_diff {
                    Span::styled("  [Tab] 切换diff视图", Style::default().fg(Color::Yellow))
                } else {
                    Span::raw("")
                },
            ]);
            let status_bar = Paragraph::new(status_text);
            f.render_widget(status_bar, v_chunks[0]);

            // 输出区 / Diff 区
            if self.show_diff {
                let visible = self.apply_scroll_offset(&diff_lines, v_chunks[1].height as usize);
                let diff_block = Paragraph::new(Text::from(visible))
                    .block(Block::default().borders(Borders::NONE).title(" Diff "))
                    .wrap(Wrap { trim: false });
                f.render_widget(diff_block, v_chunks[1]);
            } else {
                let visible = self.apply_scroll_offset(&highlighted_lines, v_chunks[1].height as usize);
                let output = Paragraph::new(Text::from(visible))
                    .block(Block::default().borders(Borders::NONE))
                    .wrap(Wrap { trim: false });
                f.render_widget(output, v_chunks[1]);
            }

            // 输入区
            let input_display = Line::from(vec![
                Span::styled(" > ", Style::default().fg(Color::Cyan)),
                Span::raw("Enter 发送 | Ctrl+Enter 换行 | Esc 退出 | ↑↓ 历史 | PgUp/PgDn 滚动"),
            ]);
            let input = Paragraph::new(input_display);
            f.render_widget(input, v_chunks[2]);

            // 工具卡片侧边栏
            if has_tools {
                self.render_tool_cards(f, h_chunks[1]);
            }
        })?;

        Ok(())
    }

    /// 渲染工具卡片侧边栏
    fn render_tool_cards(&self, f: &mut ratatui::Frame, area: Rect) {
        let card_count = self.tool_cards.len().min(5);
        let card_height = 3u16; // 每个卡片 3 行
        let total_height = (card_count as u16) * (card_height + 1);

        let cards: Vec<Line> = self.tool_cards
            .iter()
            .rev()
            .take(5)
            .flat_map(|card| {
                vec![
                    Line::from(vec![
                        Span::styled(
                            format!("{} {}", card.status, card.name),
                            Style::default()
                                .fg(match card.status {
                                    ToolStatus::Running => Color::Yellow,
                                    ToolStatus::Done => Color::Green,
                                    ToolStatus::Failed => Color::Red,
                                })
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Line::from(Span::styled(
                        if card.result_summary.len() > 25 {
                            format!("{}...", &card.result_summary[..25])
                        } else {
                            card.result_summary.clone()
                        },
                        Style::default().fg(Color::DarkGray),
                    )),
                    Line::from(Span::raw("")), // 分隔空行
                ]
            })
            .collect();

        let card_block = Block::default()
            .borders(Borders::LEFT)
            .title(" 工具 ")
            .border_style(Style::default().fg(Color::DarkGray));
        let card_paragraph = Paragraph::new(Text::from(cards))
            .block(card_block)
            .wrap(Wrap { trim: true });
        f.render_widget(card_paragraph, area);
    }

    /// 渲染 diff 内容（绿色/红色着色）
    fn render_diff(&self) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        for raw_line in self.diff_content.lines() {
            let span = if raw_line.starts_with("+++") || raw_line.starts_with("+") && !raw_line.starts_with("+++") {
                Span::styled(raw_line.to_string(), Style::default().fg(Color::Green))
            } else if raw_line.starts_with("---") || raw_line.starts_with("-") && !raw_line.starts_with("---") {
                Span::styled(raw_line.to_string(), Style::default().fg(Color::Red))
            } else if raw_line.starts_with("@@") {
                Span::styled(raw_line.to_string(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            } else {
                Span::raw(raw_line.to_string())
            };
            lines.push(Line::from(span));
        }
        lines
    }

    /// 语法高亮：解析输出缓冲区，对代码块进行着色
    fn highlight_output(&self) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut in_code_block = false;
        let mut code_lang: Option<String> = None;
        let mut code_lines: Vec<String> = Vec::new();

        let theme = &self.theme_set.themes["base16-ocean.dark"];

        for raw_line in self.output_buffer.lines() {
            let trimmed = raw_line.trim();

            if trimmed.starts_with("```") {
                if in_code_block {
                    let lang = code_lang.take().unwrap_or_default();
                    let syntax = self.syntax_set
                        .find_syntax_by_extension(&lang)
                        .or_else(|| self.syntax_set.find_syntax_by_token(&lang))
                        .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

                    for code_line in &code_lines {
                        let highlighted = syntect::util::highlight(
                            code_line,
                            &self.syntax_set,
                            syntax,
                            theme,
                        );
                        let spans: Vec<Span> = highlighted
                            .iter()
                            .map(|(style, text)| {
                                Span::styled(
                                    text.to_string(),
                                    Style::default()
                                        .fg(syntect_to_ratatui_color(style.foreground))
                                        .bg(syntect_to_ratatui_color(style.background))
                                        .add_modifier(if style.font_style.contains(syntect::highlighting::FontStyle::BOLD) {
                                            Modifier::BOLD
                                        } else {
                                            Modifier::empty()
                                        }),
                                )
                            })
                            .collect();
                        lines.push(Line::from(spans));
                    }
                    code_lines.clear();
                    in_code_block = false;
                } else {
                    in_code_block = true;
                    code_lang = Some(trimmed[3..].trim().to_string());
                    lines.push(Line::from(Span::styled(
                        raw_line.to_string(),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            } else if in_code_block {
                code_lines.push(raw_line.to_string());
            } else {
                lines.push(Line::from(Span::raw(raw_line.to_string())));
            }
        }

        // 未闭合的代码块
        if in_code_block && !code_lines.is_empty() {
            let lang = code_lang.unwrap_or_default();
            let syntax = self.syntax_set
                .find_syntax_by_extension(&lang)
                .or_else(|| self.syntax_set.find_syntax_by_token(&lang))
                .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

            for code_line in &code_lines {
                let highlighted = syntect::util::highlight(
                    code_line,
                    &self.syntax_set,
                    syntax,
                    theme,
                );
                let spans: Vec<Span> = highlighted
                    .iter()
                    .map(|(style, text)| {
                        Span::styled(
                            text.to_string(),
                            Style::default()
                                .fg(syntect_to_ratatui_color(style.foreground))
                                .add_modifier(if style.font_style.contains(syntect::highlighting::FontStyle::BOLD) {
                                    Modifier::BOLD
                                } else {
                                    Modifier::empty()
                                }),
                        )
                    })
                    .collect();
                lines.push(Line::from(spans));
            }
        }

        lines
    }

    fn apply_scroll_offset(&self, lines: &[Line<'static>], viewport_height: usize) -> Vec<Line<'static>> {
        let total_lines = lines.len();
        if total_lines <= viewport_height {
            return lines.to_vec();
        }
        let start = self.scroll_offset.min(total_lines.saturating_sub(viewport_height));
        lines[start..].to_vec()
    }
}

#[async_trait]
impl Node for TUINode {
    fn node_type(&self) -> NodeType {
        NodeType::TUI
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!("TUI Node 启动：{}", self.id);
        if std::env::var("CCODE_HEADLESS").is_err() {
            if let Err(e) = self.init_terminal() {
                tracing::warn!("终端初始化失败（headless 模式？）：{}", e);
            }
        }
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, _transport: &NodeTransportHandle) -> anyhow::Result<()> {
        match msg.topic.as_str() {
            t if t.ends_with("/output") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                if let Some(content) = payload["content"].as_str() {
                    self.agent_status = AgentStatus::Outputting;
                    self.output_buffer.push_str(content);

                    if self.terminal.is_none() {
                        print!("{}", content);
                        use std::io::Write;
                        if let Err(e) = std::io::stdout().flush() {
                            tracing::debug!("stdout flush 失败：{}", e);
                        }
                    } else {
                        self.render()?;
                    }
                }
            }
            t if t.ends_with("/tool_result") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let tool_name = payload["tool_name"].as_str().unwrap_or("unknown");
                let success = payload["success"].as_bool().unwrap_or(true);
                let output = payload["output"].as_str().unwrap_or("");

                // 更新工具卡片
                let status = if success { ToolStatus::Done } else { ToolStatus::Failed };
                let summary = output.lines().next().unwrap_or("").to_string();
                let card = ToolCard {
                    name: tool_name.to_string(),
                    status,
                    result_summary: summary,
                };
                self.tool_cards.push(card);
                if self.tool_cards.len() > 10 {
                    self.tool_cards.remove(0);
                }

                // 提取 diff 内容
                self.extract_diff(tool_name, output);

                if self.terminal.is_some() {
                    self.render()?;
                }
            }
            t if t.ends_with("/event") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                if let Some(state) = payload["state"].as_str() {
                    self.agent_status = match state {
                        "thinking" => AgentStatus::Thinking,
                        "outputting" => AgentStatus::Outputting,
                        "tool_calling" => AgentStatus::ToolCalling,
                        "error" => AgentStatus::Error,
                        _ => AgentStatus::Idle,
                    };
                }
                if let Some(tool_info) = payload["tool"].as_str() {
                    self.tool_status = tool_info.to_string();
                }
                if self.terminal.is_some() {
                    self.render()?;
                }
            }
            t if t.ends_with("/tool_call") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                if let Some(tool_name) = payload["tool_name"].as_str() {
                    self.agent_status = AgentStatus::ToolCalling;
                    self.tool_status = format!("{}", tool_name);
                    // 添加运行中的工具卡片
                    self.tool_cards.push(ToolCard {
                        name: tool_name.to_string(),
                        status: ToolStatus::Running,
                        result_summary: "执行中...".to_string(),
                    });
                    if self.terminal.is_some() {
                        self.render()?;
                    }
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
        tracing::info!("TUI Node 关闭：{}", self.id);
        Ok(())
    }
}