
use ratatui::style::{Color, Style};
use ratatui::text::Span;

const SWEEP_PERIOD: f32 = 60.0;
const BAND_WIDTH: f32 = 0.25;

fn shimmer_char_opacity(char_idx: usize, total_chars: usize, tick: u64) -> f32 {
    if total_chars <= 1 {
        return 0.0;
    }
    let pos = char_idx as f32 / (total_chars - 1) as f32;
    let phase = (tick as f32 % SWEEP_PERIOD) / SWEEP_PERIOD;
    let band_center = phase * (1.0 + 2.0 * BAND_WIDTH) - BAND_WIDTH;
    let dist = (pos - band_center).abs();
    if dist < BAND_WIDTH {
        0.5 * (1.0 + (std::f32::consts::PI * dist / BAND_WIDTH).cos())
    } else {
        0.0
    }
}

fn color_to_rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(idx) => {
            let palette = ratatui::style::Color::Indexed(idx);
            let _ = palette;
            (198, 198, 198)
        }
        _ => (198, 198, 198),
    }
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let (ar, ag, ab) = color_to_rgb(a);
    let (br, bg, bb) = color_to_rgb(b);
    let t = t.clamp(0.0, 1.0);
    let r = (ar as f32 + (br as f32 - ar as f32) * t).round() as u8;
    let g = (ag as f32 + (bg as f32 - ag as f32) * t).round() as u8;
    let b_ch = (ab as f32 + (bb as f32 - ab as f32) * t).round() as u8;
    Color::Rgb(r, g, b_ch)
}

pub fn shimmer_spans(text: &str, base: Color, highlight: Color, tick: u64) -> Vec<Span<'static>> {
    let total = text.chars().count();
    if total == 0 {
        return vec![];
    }
    text.chars()
        .enumerate()
        .map(|(i, ch)| {
            let opacity = shimmer_char_opacity(i, total, tick);
            let color = lerp_color(base, highlight, opacity);
            Span::styled(String::from(ch), Style::default().fg(color))
        })
        .collect()
}

pub fn shimmer_label(
    text: &str,
    tick: u64,
    base_style: Style,
    theme: &crate::theme::Theme,
) -> Vec<Span<'static>> {
    let base = base_style.fg.unwrap_or(theme.gray);
    shimmer_spans(text, base, theme.text_primary, tick)
}
