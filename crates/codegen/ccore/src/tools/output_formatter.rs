//! 工具输出格式化（借鉴 Claude Code 的输出格式）
//!
//! 统一工具输出格式，提供：
//! - 输出截断
//! - 行号格式化
//! - 文件头信息
//! - 搜索结果格式
//! - 错误格式

/// 默认最大输出字符数
pub const DEFAULT_MAX_OUTPUT_CHARS: usize = 50_000;

/// 默认最大行数
pub const DEFAULT_MAX_LINES: usize = 2000;

/// 截断输出
///
/// 超过 max_chars 时截断并在末尾添加提示
pub fn truncate_output(output: &str, max_chars: usize) -> String {
    if output.len() <= max_chars {
        return output.to_string();
    }

    let truncated = &output[..max_chars];
    let total = output.len();
    format!("{}\n\n... [截断，显示 {} / {} 字符]", truncated, max_chars, total)
}

/// 截断行数
///
/// 超过 max_lines 行时截断
pub fn truncate_lines(content: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= max_lines {
        return content.to_string();
    }

    let shown: Vec<&str> = lines.into_iter().take(max_lines).collect();
    format!("{}\n\n... [截断，显示前 {} 行]", shown.join("\n"), max_lines)
}

/// 带行号格式化
///
/// 类似 cat -n 格式：行号右对齐 + → 分隔符
pub fn format_with_line_numbers(content: &str, start_line: usize) -> String {
    let width = ((content.lines().count() + start_line) as f64).log10().floor() as usize + 1;
    let width = width.max(4);

    content
        .lines()
        .enumerate()
        .map(|(i, line)| format!("{:>width$}→{}", start_line + i, line, width = width))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 带行号格式化（带范围）
pub fn format_range_with_line_numbers(
    content: &str,
    start_line: usize,
    end_line: usize,
) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let width = (end_line as f64).log10().floor() as usize + 1;
    let width = width.max(4);

    lines
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>width$}→{}", start_line + i, line, width = width))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 文件头信息
pub fn format_file_header(path: &str, size_bytes: u64) -> String {
    let size_str = if size_bytes < 1024 {
        format!("{} bytes", size_bytes)
    } else if size_bytes < 1024 * 1024 {
        format!("{:.1} KB", size_bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", size_bytes as f64 / (1024.0 * 1024.0))
    };
    format!("→ {}\n   大小: {}\n", path, size_str)
}

/// 搜索结果格式
pub fn format_search_result(
    path: &str,
    line_num: usize,
    content: &str,
    context_before: &[&str],
    context_after: &[&str],
) -> String {
    let mut result = String::new();

    for (i, line) in context_before.iter().enumerate() {
        let ln = line_num - context_before.len() + i;
        result.push_str(&format!("  {:>4} | {}\n", ln, line));
    }

    result.push_str(&format!("→ {:>4} | {}\n", line_num, content));

    for (i, line) in context_after.iter().enumerate() {
        let ln = line_num + i + 1;
        result.push_str(&format!("  {:>4} | {}\n", ln, line));
    }

    result
}

/// 统一错误格式
pub fn format_error(tool: &str, path: &str, error: &str) -> String {
    format!("错误[{}] {}: {}", tool, path, error)
}

/// 编辑结果格式
///
/// 显示编辑点前后的上下文
pub fn format_edit_result(
    path: &str,
    line_num: usize,
    context_before: &[&str],
    edited_line: &str,
    context_after: &[&str],
) -> String {
    let mut result = format!("已编辑 {}\n", path);

    for (i, line) in context_before.iter().enumerate() {
        let ln = line_num - context_before.len() + i;
        result.push_str(&format!("  {:>4} | {}\n", ln, line));
    }

    result.push_str(&format!("→ {:>4} | {}\n", line_num, edited_line));

    for (i, line) in context_after.iter().enumerate() {
        let ln = line_num + i + 1;
        result.push_str(&format!("  {:>4} | {}\n", ln, line));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_output_under_limit() {
        let output = "hello world";
        assert_eq!(truncate_output(output, 100), "hello world");
    }

    #[test]
    fn test_truncate_output_over_limit() {
        let output = "a".repeat(100);
        let result = truncate_output(&output, 50);
        assert!(result.contains("截断"));
        assert!(result.len() > 50);
    }

    #[test]
    fn test_format_with_line_numbers() {
        let content = "line1\nline2\nline3";
        let result = format_with_line_numbers(content, 1);
        assert!(result.contains("1→line1"));
        assert!(result.contains("2→line2"));
        assert!(result.contains("3→line3"));
    }

    #[test]
    fn test_format_with_line_numbers_offset() {
        let content = "line1\nline2";
        let result = format_with_line_numbers(content, 10);
        assert!(result.contains("10→line1"));
        assert!(result.contains("11→line2"));
    }

    #[test]
    fn test_format_file_header() {
        let result = format_file_header("/src/main.rs", 1024);
        assert!(result.contains("/src/main.rs"));
        assert!(result.contains("1.0 KB"));
    }

    #[test]
    fn test_format_file_header_large() {
        let result = format_file_header("/src/data.bin", 5 * 1024 * 1024);
        assert!(result.contains("5.0 MB"));
    }

    #[test]
    fn test_truncate_lines() {
        let content = (1..=100).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        let result = truncate_lines(&content, 10);
        assert!(result.contains("截断"));
        assert!(result.contains("10 行"));
    }

    #[test]
    fn test_format_error() {
        let result = format_error("read", "/src/main.rs", "文件不存在");
        assert!(result.contains("read"));
        assert!(result.contains("文件不存在"));
    }
}
