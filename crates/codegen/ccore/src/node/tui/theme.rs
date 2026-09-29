//! TUI 主题 — 参考 opencode 配色

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone)]
pub struct Theme {
    pub primary: Color,
    pub secondary: Color,
    pub accent: Color,
    pub text: Color,
    pub text_muted: Color,
    pub text_emph: Color,
    pub bg: Color,
    pub border: Color,
    pub border_focus: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub md_heading: Color,
    pub md_link: Color,
    pub md_code: Color,
    pub md_strong: Color,
    pub md_emph: Color,
    pub md_blockquote: Color,
    pub md_list: Color,
    pub syntax_comment: Color,
    pub syntax_keyword: Color,
    pub syntax_string: Color,
    pub syntax_number: Color,
    pub syntax_function: Color,
    pub syntax_type: Color,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            primary: Color::Rgb(0xfa, 0xb2, 0x83),
            secondary: Color::Rgb(0x5c, 0x9c, 0xf5),
            accent: Color::Rgb(0x9d, 0x7c, 0xd8),
            text: Color::Rgb(0xe0, 0xe0, 0xe0),
            text_muted: Color::Rgb(0x6a, 0x6a, 0x6a),
            text_emph: Color::Rgb(0xe5, 0xc0, 0x7b),
            bg: Color::Rgb(0x21, 0x21, 0x21),
            border: Color::Rgb(0x4b, 0x4c, 0x5c),
            border_focus: Color::Rgb(0xfa, 0xb2, 0x83),
            success: Color::Rgb(0x7f, 0xd8, 0x8f),
            warning: Color::Rgb(0xf5, 0xa7, 0x42),
            error: Color::Rgb(0xe0, 0x6c, 0x75),
            md_heading: Color::Rgb(0x5c, 0x9c, 0xf5),
            md_link: Color::Rgb(0xfa, 0xb2, 0x83),
            md_code: Color::Rgb(0x7f, 0xd8, 0x8f),
            md_strong: Color::Rgb(0x9d, 0x7c, 0xd8),
            md_emph: Color::Rgb(0xe5, 0xc0, 0x7b),
            md_blockquote: Color::Rgb(0xe5, 0xc0, 0x7b),
            md_list: Color::Rgb(0xfa, 0xb2, 0x83),
            syntax_comment: Color::Rgb(0x5c, 0x63, 0x70),
            syntax_keyword: Color::Rgb(0xc6, 0x78, 0xdd),
            syntax_string: Color::Rgb(0x98, 0xc3, 0x79),
            syntax_number: Color::Rgb(0xd1, 0x9a, 0x66),
            syntax_function: Color::Rgb(0x61, 0xaf, 0xef),
            syntax_type: Color::Rgb(0xe5, 0xc0, 0x7b),
        }
    }

    pub fn user_prefix_style(&self) -> Style {
        Style::default().fg(self.primary).add_modifier(Modifier::BOLD)
    }

    pub fn user_label_style(&self) -> Style {
        Style::default()
            .fg(self.secondary)
            .add_modifier(Modifier::BOLD)
    }

    pub fn assistant_label_style(&self) -> Style {
        Style::default()
            .fg(self.primary)
            .add_modifier(Modifier::BOLD)
    }

    pub fn user_text_style(&self) -> Style {
        Style::default().fg(self.secondary)
    }

    pub fn assistant_text_style(&self) -> Style {
        Style::default().fg(self.text)
    }

    pub fn system_text_style(&self) -> Style {
        Style::default().fg(self.text_muted)
    }

    pub fn spinner_style(&self) -> Style {
        Style::default().fg(self.primary)
    }

    pub fn tool_name_style(&self) -> Style {
        Style::default().fg(self.warning).add_modifier(Modifier::BOLD)
    }

    pub fn tool_time_style(&self) -> Style {
        Style::default().fg(self.text_muted)
    }

    pub fn tool_done_style(&self) -> Style {
        Style::default().fg(self.success)
    }

    pub fn tool_fail_style(&self) -> Style {
        Style::default().fg(self.error)
    }

    pub fn border_style(&self) -> Style {
        Style::default().fg(self.border)
    }

    pub fn input_prefix_style(&self) -> Style {
        Style::default().fg(self.primary).add_modifier(Modifier::BOLD)
    }

    pub fn placeholder_style(&self) -> Style {
        Style::default().fg(self.text_muted)
    }

    pub fn status_style(&self) -> Style {
        Style::default().fg(self.text_muted)
    }

    pub fn status_active_style(&self) -> Style {
        Style::default().fg(self.primary)
    }

    pub fn model_style(&self) -> Style {
        Style::default().fg(self.text_muted)
    }

    pub fn token_style(&self) -> Style {
        Style::default().fg(self.text_muted)
    }

    pub fn cost_style(&self) -> Style {
        Style::default().fg(self.success)
    }

    pub fn help_style(&self) -> Style {
        Style::default().fg(self.text_muted)
    }

    pub fn primary_style_bold(&self) -> Style {
        Style::default().fg(self.primary).add_modifier(Modifier::BOLD)
    }

    pub fn text_muted_style(&self) -> Style {
        Style::default().fg(self.text_muted)
    }

    pub fn panel_bg(&self) -> Color {
        Color::Rgb(0x1e, 0x1e, 0x2e)
    }

    pub fn markdown_style(&self) -> ccode_markdown::MarkdownStyle {
        use anstyle::Style as AStyle;
        let rgb = |r: u8, g: u8, b: u8| -> AStyle {
            AStyle::new().fg_color(Some(anstyle::Color::Rgb(anstyle::RgbColor(r, g, b))))
        };
        let from_color = |c: Color| -> AStyle {
            match c {
                Color::Rgb(r, g, b) => rgb(r, g, b),
                _ => AStyle::new(),
            }
        };

        let heading = from_color(self.md_heading);
        let code = from_color(self.md_code);
        let strong = from_color(self.md_strong);
        let emph = from_color(self.md_emph);
        let muted = from_color(self.text_muted);
        let list = from_color(self.md_list);
        let text = from_color(self.text);

        ccode_markdown::MarkdownStyle {
            heading_inner: [
                heading.bold(),
                heading.bold(),
                heading.bold(),
                heading,
                heading,
                heading,
            ],
            heading_outer: [muted.hidden(); 6],
            strong_inner: strong.bold(),
            strong_outer: muted.hidden(),
            emphasis_inner: emph.italic(),
            emphasis_outer: muted.hidden(),
            strikethrough_inner: AStyle::new().strikethrough(),
            strikethrough_outer: muted.hidden(),
            inline_code_inner: code.bold(),
            inline_code_outer: muted.hidden(),
            blockquote_outer: muted.dimmed(),
            task_checked: AStyle::new(),
            task_unchecked: muted.dimmed(),
            list_item: list,
            rule: muted,
            link_outer: muted.hidden(),
            link_text: strong.bold(),
            link_url: muted.dimmed(),
            link_title: AStyle::new(),
            code_outer: muted.hidden(),
            code_language: muted.hidden(),
            code_untagged: AStyle::new(),
            code_background: AStyle::new(),
            table_outer: AStyle::new().bold(),
            text,
            math: emph.italic(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}
