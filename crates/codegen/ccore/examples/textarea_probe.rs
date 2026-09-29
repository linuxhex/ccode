// 最小复现：用 ratatui 画一个带边框的 textarea，看是否显示
// 运行：cargo run --example textarea_probe -p ccore
use std::io;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use ccode_textarea::{TextArea, TextAreaState};

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut textarea = TextArea::new();
    let mut textarea_state = TextAreaState::default();

    loop {
        // 临时取出 textarea/state，避免 draw 闭包与光标定位的借用冲突
        let mut ta = std::mem::replace(&mut textarea, TextArea::new());
        let mut ts = std::mem::take(&mut textarea_state);
        let mut cursor_pos: Option<(u16, u16)> = None;

        terminal.draw(|f| {
            let size = f.area();

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),  // header
                    Constraint::Length(1),  // separator
                    Constraint::Min(3),     // textarea box
                    Constraint::Length(1),  // hint
                ])
                .split(size);

            // header
            let header = Paragraph::new(Line::from(vec![
                Span::styled(" ccode", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw("  (probe)"),
            ]));
            f.render_widget(header, chunks[0]);

            // separator
            let sep = Paragraph::new(Line::from(vec![
                Span::styled("─".repeat(size.width as usize), Style::default().fg(Color::DarkGray)),
            ]));
            f.render_widget(sep, chunks[1]);

            // bordered box
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .border_set(ratatui::symbols::border::ROUNDED);
            let inner = block.inner(chunks[2]);
            f.render_widget(block, chunks[2]);

            // ❯ prefix
            let prefix = Paragraph::new(Line::from(vec![
                Span::styled("❯ ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            ]));
            let prefix_area = Rect { x: inner.x, y: inner.y, width: 2.min(inner.width), height: inner.height };
            f.render_widget(prefix, prefix_area);

            // textarea
            if inner.width > 2 {
                let ta_area = Rect {
                    x: inner.x + 2,
                    y: inner.y,
                    width: inner.width - 2,
                    height: inner.height,
                };
                f.render_stateful_widget_ref(&ta, ta_area, &mut ts);
                cursor_pos = ta.cursor_pos_with_state(ta_area, ts);
            }
        })?;

        textarea = ta;
        textarea_state = ts;

        if let Some((cx, cy)) = cursor_pos {
            terminal.show_cursor()?;
            terminal.set_cursor_position(ratatui::layout::Position { x: cx, y: cy })?;
        }

        if event::poll(std::time::Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Esc || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)) {
                    break;
                }
                textarea.input(key);
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}
