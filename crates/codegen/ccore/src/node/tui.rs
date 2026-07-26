//! TUI Node - 终端渲染、用户输入
//!
//! TUI Node 的职责：
//! 1. 订阅 Agent 输出 → 渲染到终端
//! 2. 读取用户输入 → 发送到 Agent input topic
//! 3. 展示工具调用状态（等待/执行中/完成）
//! 4. 展示 Agent 状态指示器（thinking/outputting/tool_calling）
//!
//! 渲染基于 ratatui + crossterm 后端

use async_trait::async_trait;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
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

    // 构建连接信息
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

    // 连接消息总线
    let mut transport = crate::node::transport::NodeTransport::connect(&connect_info)
    .await?;

    let handle = transport.handle().clone();

    // 启动 TUI
    node.start(ctx).await?;
    tracing::info!("TUI Node {} 已启动，进入交互循环", node_id);

    // 输入缓冲区
    let mut input_buffer = String::new();

    // 心跳定时器：每 10 秒发送一次心跳
    let mut heartbeat_timer = tokio::time::interval(std::time::Duration::from_secs(10));
    heartbeat_timer.tick().await; // 消耗首次立即触发

    // 交互循环：同时等待消息和键盘输入
    loop {
        tokio::select! {
            // 消息总线消息
            msg = transport.recv() => {
                match msg {
                    Ok(Some(msg)) => {
                        if msg.topic.as_str() == "sys/shutdown" {
                            break;
                        }
                        if msg.topic.as_str() == "sys/heartbeat" {
                            continue;
                        }
                        // 收到 sys/spawn 时记录 primary agent ID
                        if msg.topic.as_str() == "sys/spawn" {
                            if node.primary_agent_id.is_none() {
                                if let Ok(payload) = FrameCodec::decode_payload::<serde_json::Value>(&msg) {
                                    if let Some(node_type) = payload["node_type"].as_str() {
                                        if node_type == "agent" {
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
            // 定期发送心跳
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
            // 键盘输入
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                // 非阻塞检查键盘输入
                if event::poll(std::time::Duration::from_millis(0))? {
                    if let Event::Key(key) = event::read()? {
                        match key.code {
                            KeyCode::Enter => {
                                if !input_buffer.is_empty() {
                                    // 发送输入到 Agent
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
                                    // 重新渲染输入区
                                    if node.terminal.is_some() {
                                        node.render()?;
                                    }
                                }
                            }
                            KeyCode::Char(c) => {
                                input_buffer.push(c);
                            }
                            KeyCode::Backspace => {
                                input_buffer.pop();
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
pub struct TUINode {
    id: NodeId,
    primary_agent_id: Option<String>,
    /// 当前 Agent 状态
    agent_status: AgentStatus,
    /// Agent 输出缓冲区
    output_buffer: String,
    /// 工具调用状态显示
    tool_status: String,
    /// 终端（lazy init）
    terminal: Option<Terminal<CrosstermBackend<io::Stdout>>>,
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
        }
    }

    /// 设置主 Agent ID（收到 sys/spawn 后更新）
    pub fn set_primary_agent(&mut self, agent_id: String) {
        self.primary_agent_id = Some(agent_id);
    }

    /// 初始化终端
    fn init_terminal(&mut self) -> anyhow::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        self.terminal = Some(terminal);
        Ok(())
    }

    /// 恢复终端
    fn restore_terminal(&mut self) -> anyhow::Result<()> {
        if let Some(mut terminal) = self.terminal.take() {
            disable_raw_mode()?;
            execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        }
        Ok(())
    }

    /// 渲染 TUI 界面
    fn render(&mut self) -> anyhow::Result<()> {
        let terminal = match self.terminal.as_mut() {
            Some(t) => t,
            None => return Ok(()),
        };

        terminal.draw(|f| {
            let size = f.area();

            // 布局：上方状态栏 + 中间输出区 + 下方输入区
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),  // 状态栏
                    Constraint::Min(5),     // 输出区
                    Constraint::Length(2),  // 输入区
                ])
                .split(size);

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
            ]);
            let status_bar = Paragraph::new(status_text);
            f.render_widget(status_bar, chunks[0]);

            // 输出区
            let output_text = Text::from(self.output_buffer.clone());
            let output = Paragraph::new(output_text)
                .block(Block::default().borders(Borders::NONE))
                .wrap(Wrap { trim: false });
            f.render_widget(output, chunks[1]);

            // 输入区
            let input_text = Line::from(vec![
                Span::styled(" > ", Style::default().fg(Color::Cyan)),
                Span::raw("按 Enter 发送消息，Esc 退出"),
            ]);
            let input = Paragraph::new(input_text);
            f.render_widget(input, chunks[2]);
        })?;

        Ok(())
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
        // 初始化终端（在非 headless 模式下）
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
                // 渲染 agent 输出到终端
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                if let Some(content) = payload["content"].as_str() {
                    self.agent_status = AgentStatus::Outputting;
                    self.output_buffer.push_str(content);

                    // 非终端模式直接 stdout 输出
                    if self.terminal.is_none() {
                        print!("{}", content);
                        use std::io::Write;
                        let _ = std::io::stdout().flush();
                    } else {
                        self.render()?;
                    }
                }
            }
            t if t.ends_with("/event") => {
                // 更新状态显示
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
                    self.tool_status = format!("执行工具：{}", tool_name);
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
        } else {
            subs.push("agent/*/output".into());
            subs.push("agent/*/event".into());
            subs.push("agent/*/tool_call".into());
        }
        subs
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        self.restore_terminal()?;
        tracing::info!("TUI Node 关闭：{}", self.id);
        Ok(())
    }
}
