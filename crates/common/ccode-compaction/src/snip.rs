//! Snip — 超长工具输出截断。
//!
//! 在微压缩之前先做硬截断：对单次工具调用结果按行数/字符数/匹配数上限
//! 保留首尾，中间用 `[... K lines/chars/matches omitted ...]` 标注。
//!
//! 对标 Claude Code 5 层压缩管道的第 2 层（Snip）。与 [`crate::budget`]
//! 的区别：budget 按 token 预算截断（粗粒度、跨调用），snip 按结构化
//! 单位截断（行/字符/匹配，细粒度、单调用），二者叠加使用。

use crate::compactable::is_compactable;

/// 每工具的截断阈值。
///
/// 值参考 Claude Code：FileRead 2000 行、Bash 10000 字符、Grep 500 匹配、
/// Glob 1000 条目。`0` 表示该维度不限制。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnipConfig {
    /// FileRead / ReadFile 最大行数。
    pub file_read_max_lines: usize,
    /// Bash 最大字符数。
    pub bash_max_chars: usize,
    /// Grep 最大匹配数（以 `---` 分隔的块计）。
    pub grep_max_matches: usize,
    /// Glob 最大条目数（非空行计）。
    pub glob_max_entries: usize,
    /// ListDir 最大条目数。
    pub list_dir_max_entries: usize,
    /// WebFetch / WebSearch 最大字符数。
    pub web_max_chars: usize,
}

impl Default for SnipConfig {
    fn default() -> Self {
        Self {
            file_read_max_lines: 2000,
            bash_max_chars: 10_000,
            grep_max_matches: 500,
            glob_max_entries: 1000,
            list_dir_max_entries: 1000,
            web_max_chars: 10_000,
        }
    }
}

/// 截断结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnipResult {
    /// 截断后（或原样）的输出。
    pub output: String,
    /// 原输出的估算 token 数。
    pub original_tokens: usize,
    /// 节省的 token 数（未截断为 0）。
    pub saved_tokens: usize,
    /// 是否发生了截断。
    pub truncated: bool,
}

impl SnipResult {
    fn unchanged(output: &str) -> Self {
        Self {
            output: output.to_string(),
            original_tokens: estimate_tokens(output),
            saved_tokens: 0,
            truncated: false,
        }
    }
}

/// 对工具输出执行 snip 截断。
///
/// 非可压缩工具（见 [`is_compactable`]）原样返回。按工具名分派到对应的
/// 截断策略（行/字符/匹配/条目）。
pub fn snip(tool: &str, output: &str, config: &SnipConfig) -> SnipResult {
    if !is_compactable(tool) || output.is_empty() {
        return SnipResult::unchanged(output);
    }
    let lower = tool.to_ascii_lowercase();
    if lower.contains("bash") {
        snip_chars(output, config.bash_max_chars, "chars")
    } else if lower.contains("grep") {
        snip_blocks(output, config.grep_max_matches, "matches")
    } else if lower.contains("glob") || lower.contains("list") {
        snip_lines(output, config.glob_max_entries.max(config.list_dir_max_entries), "entries")
    } else if lower.contains("web") {
        snip_chars(output, config.web_max_chars, "chars")
    } else {
        // FileRead / ReadFile / FileEdit / FileWrite 等按行截断。
        snip_lines(output, config.file_read_max_lines, "lines")
    }
}

/// 按行截断：保留首 `max/2` 行 + 尾 `max/2` 行，中间标注省略行数。
fn snip_lines(output: &str, max: usize, unit: &str) -> SnipResult {
    if max == 0 {
        return SnipResult::unchanged(output);
    }
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= max {
        return SnipResult::unchanged(output);
    }
    let keep = max / 2;
    let head = &lines[..keep.min(lines.len())];
    let tail_start = lines.len().saturating_sub(keep);
    let tail = &lines[tail_start.max(keep)..];
    let omitted = lines.len() - head.len() - tail.len();
    let note = format!("[... {omitted} {unit} omitted ...]");
    let mut out = String::with_capacity(output.len());
    out.push_str(&head.join("\n"));
    out.push('\n');
    out.push_str(&note);
    out.push('\n');
    out.push_str(&tail.join("\n"));
    truncated_result(output, out)
}

/// 按字符截断：保留首尾各 `max/2` 字符，中间标注省略字符数。
fn snip_chars(output: &str, max: usize, unit: &str) -> SnipResult {
    if max == 0 || output.len() <= max {
        return SnipResult::unchanged(output);
    }
    let keep = max / 2;
    let head = &output[..keep.min(output.len())];
    let tail_start = output.len().saturating_sub(keep);
    let tail = &output[tail_start.max(keep)..];
    let omitted = output.len() - head.len() - tail.len();
    let note = format!("[... {omitted} {unit} omitted ...]");
    let out = format!("{head}\n{note}\n{tail}");
    truncated_result(output, out)
}

/// 按块截断（Grep 的 `---` 分隔匹配块）：保留首尾各 `max/2` 块。
fn snip_blocks(output: &str, max: usize, unit: &str) -> SnipResult {
    if max == 0 {
        return SnipResult::unchanged(output);
    }
    // Grep 输出常以空行或 `--` 分隔；按连续非空段计数。
    let blocks: Vec<&str> = output.split("\n\n").collect();
    if blocks.len() <= max {
        return SnipResult::unchanged(output);
    }
    let keep = max / 2;
    let head = &blocks[..keep.min(blocks.len())];
    let tail_start = blocks.len().saturating_sub(keep);
    let tail = &blocks[tail_start.max(keep)..];
    let omitted = blocks.len() - head.len() - tail.len();
    let note = format!("[... {omitted} {unit} omitted ...]");
    let out = format!("{}\n{}\n{}", head.join("\n\n"), note, tail.join("\n\n"));
    truncated_result(output, out)
}

fn truncated_result(original: &str, output: String) -> SnipResult {
    let original_tokens = estimate_tokens(original);
    let saved = original_tokens.saturating_sub(estimate_tokens(&output));
    SnipResult {
        output,
        original_tokens,
        saved_tokens: saved,
        truncated: true,
    }
}

/// 粗略 token 估算：`chars / 4`（与 [`crate::budget::estimate_tokens`] 一致）。
pub fn estimate_tokens(s: &str) -> usize {
    s.chars().count() / 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_compactable_is_unchanged() {
        let cfg = SnipConfig::default();
        let out = "x\n".repeat(10_000);
        let r = snip("AgentTool", &out, &cfg);
        assert!(!r.truncated);
    }

    #[test]
    fn file_read_snips_by_lines() {
        let cfg = SnipConfig::default();
        let out = (1..=5000).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let r = snip("FileRead", &out, &cfg);
        assert!(r.truncated);
        assert!(r.output.contains("lines omitted"));
        assert!(r.output.contains("line 1"));
        assert!(r.output.contains("line 5000"));
    }

    #[test]
    fn bash_snips_by_chars() {
        let cfg = SnipConfig::default();
        let out = "a".repeat(30_000);
        let r = snip("Bash", &out, &cfg);
        assert!(r.truncated);
        assert!(r.output.contains("chars omitted"));
    }

    #[test]
    fn grep_snips_by_blocks() {
        let cfg = SnipConfig::default();
        // 1000 个块，每块 "match"
        let out = (1..=1000).map(|_| "match").collect::<Vec<_>>().join("\n\n");
        let r = snip("Grep", &out, &cfg);
        assert!(r.truncated);
        assert!(r.output.contains("matches omitted"));
    }

    #[test]
    fn short_output_is_unchanged() {
        let cfg = SnipConfig::default();
        let r = snip("FileRead", "short", &cfg);
        assert!(!r.truncated);
    }
}
