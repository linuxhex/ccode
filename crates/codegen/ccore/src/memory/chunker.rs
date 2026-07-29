//! Markdown 语义分块器
//!
//! 将 Markdown 内容分割为语义块，保留标题上下文。
//! 支持：
//! - 按标题层级分割（## 优先）
//! - 单块最大长度限制
//! - 标题上下文注入（子块继承父标题路径）
//! - 重叠窗口（块间保留 overlap 行）
//! - 行号追踪

use serde::{Deserialize, Serialize};

/// 分块配置
#[derive(Debug, Clone)]
pub struct ChunkConfig {
    /// 单块最大字符数（默认 2000）
    pub max_chunk_chars: usize,
    /// 块间重叠行数（默认 2）
    pub overlap_lines: usize,
    /// 最小分割层级（默认 2 = ## 级别）
    pub min_heading_level: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            max_chunk_chars: 2000,
            overlap_lines: 2,
            min_heading_level: 2,
        }
    }
}

/// 分块结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    /// 块内容
    pub content: String,
    /// 标题路径（如 "Architecture > Database > Schema"）
    pub heading_path: String,
    /// 起始行号（0-based）
    pub start_line: usize,
    /// 结束行号
    pub end_line: usize,
}

/// 文档中的一个段落（由标题分隔）
struct Section<'a> {
    /// 段落中的行
    lines: Vec<&'a str>,
    /// 在原文中的起始行号
    start_line: usize,
    /// 标题栈上下文
    header_context: String,
}

/// 检测标题层级。返回 `Some(level)` 如果是有效的 Markdown 标题行。
///
/// 有效标题格式：1-6 个 `#` 后跟空格或行尾。
pub fn header_level(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let level = trimmed.chars().take_while(|&c| c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &trimmed[level..];
    if rest.is_empty() || rest.starts_with(' ') {
        Some(level)
    } else {
        None
    }
}

/// 将 Markdown 文本分块
///
/// 策略：
/// 1. 按标题行分割（>= min_heading_level 的标题）
/// 2. 如果段落超长，在空行（段落边界）处分割
/// 3. 如果单段仍然超长，在行边界处分割
/// 4. 子块继承父标题路径作为上下文
/// 5. 块间保留 overlap_lines 行重叠
pub fn chunk_markdown(text: &str, config: &ChunkConfig) -> Vec<Chunk> {
    if text.is_empty() {
        return vec![];
    }

    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return vec![];
    }

    // 如果整个内容在限制内，直接返回单块
    if text.len() <= config.max_chunk_chars {
        return vec![Chunk {
            content: text.to_string(),
            heading_path: String::new(),
            start_line: 0,
            end_line: lines.len(),
        }];
    }

    // 按标题分割
    let sections = split_by_headers(&lines, config.min_heading_level);
    let mut chunks = Vec::new();

    for section in &sections {
        let section_text = section.lines.join("\n");

        if section_text.len() <= config.max_chunk_chars {
            chunks.push(Chunk {
                content: add_header_context(&section.header_context, &section_text),
                heading_path: section.header_context.clone(),
                start_line: section.start_line,
                end_line: section.start_line + section.lines.len(),
            });
        } else {
            // 段落超长 - 按段落边界分割
            let sub_chunks = split_section_by_paragraphs(
                section,
                config.max_chunk_chars,
                config.overlap_lines,
            );
            chunks.extend(sub_chunks);
        }
    }

    chunks
}

/// 将纯文本分块（非 Markdown）
///
/// 简单的按行分割，每块不超过 max_chars 字符，块间保留 overlap_lines 行重叠。
pub fn chunk_text(text: &str, max_chars: usize, overlap_lines: usize) -> Vec<Chunk> {
    if text.is_empty() {
        return vec![];
    }

    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return vec![];
    }

    if text.len() <= max_chars {
        return vec![Chunk {
            content: text.to_string(),
            heading_path: String::new(),
            start_line: 0,
            end_line: lines.len(),
        }];
    }

    let mut chunks = Vec::new();
    let mut current_lines: Vec<String> = Vec::new();
    let mut current_start = 0;

    for (i, line) in lines.iter().enumerate() {
        current_lines.push(line.to_string());
        let joined = current_lines.join("\n");

        if joined.len() > max_chars && current_lines.len() > 1 {
            // 移除最后一行，flush 当前块
            current_lines.pop();
            let content = current_lines.join("\n");
            chunks.push(Chunk {
                content,
                heading_path: String::new(),
                start_line: current_start,
                end_line: i,
            });

            // 下一块从 overlap 行开始
            let overlap_start = if overlap_lines > 0 && i >= overlap_lines {
                i.saturating_sub(overlap_lines)
            } else {
                i
            };
            current_start = overlap_start;
            current_lines = lines[overlap_start..=i]
                .iter()
                .map(|s| s.to_string())
                .collect();
        }
    }

    // Flush 剩余
    if !current_lines.is_empty() {
        let content = current_lines.join("\n");
        // 避免重复：如果内容与上一块完全重叠，跳过
        if chunks.last().map_or(true, |last| {
            last.start_line != current_start || last.content != content
        }) {
            chunks.push(Chunk {
                content,
                heading_path: String::new(),
                start_line: current_start,
                end_line: lines.len(),
            });
        }
    }

    chunks
}

/// 按标题行分割文档为段落
fn split_by_headers<'a>(lines: &[&'a str], min_level: usize) -> Vec<Section<'a>> {
    let mut sections: Vec<Section<'a>> = Vec::new();
    let mut current_lines: Vec<&'a str> = Vec::new();
    let mut current_start = 0;
    let mut header_stack: Vec<(usize, String)> = Vec::new(); // (level, text)

    for (i, &line) in lines.iter().enumerate() {
        if let Some(level) = header_level(line) {
            if level >= min_level {
                // Flush 前一段落
                if !current_lines.is_empty() {
                    sections.push(Section {
                        lines: std::mem::take(&mut current_lines),
                        start_line: current_start,
                        header_context: format_header_context(&header_stack),
                    });
                }
                current_start = i;

                // 更新标题栈：弹出同级别或更深的标题
                while header_stack.last().is_some_and(|(l, _)| *l >= level) {
                    header_stack.pop();
                }
                header_stack.push((level, line.to_string()));
            }
        }
        current_lines.push(line);
    }

    // Flush 最后一段落
    if !current_lines.is_empty() {
        sections.push(Section {
            lines: current_lines,
            start_line: current_start,
            header_context: format_header_context(&header_stack),
        });
    }

    sections
}

/// 按段落边界分割超长段落
fn split_section_by_paragraphs(
    section: &Section<'_>,
    max_chars: usize,
    overlap_lines: usize,
) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut current_text = String::new();
    let mut current_start = section.start_line;
    let mut line_count_in_current = 0;

    for (i, &line) in section.lines.iter().enumerate() {
        let is_blank = line.trim().is_empty();

        // 段落边界：空行 + 当前累积文本不为空 + 加入当前行后超长
        if is_blank && !current_text.is_empty() {
            let would_be_len = current_text.len() + 1 + line.len();
            if would_be_len > max_chars && line_count_in_current > 0 {
                // Flush 当前块
                let flushed = current_text.trim().to_string();
                chunks.push(Chunk {
                    content: add_header_context(&section.header_context, &flushed),
                    heading_path: section.header_context.clone(),
                    start_line: current_start,
                    end_line: section.start_line + i,
                });

                // overlap：从前面取 overlap_lines 行
                current_text = build_overlap_text(&section.lines, i, overlap_lines);
                current_start = section.start_line + i.saturating_sub(overlap_lines);
                line_count_in_current = overlap_lines;
                continue;
            }
        }

        if !current_text.is_empty() {
            current_text.push('\n');
        }
        current_text.push_str(line);
        line_count_in_current += 1;

        // 单行推超了 max_chars，在行边界切割
        if current_text.len() > max_chars && line_count_in_current > 1 {
            // 在最后一个换行处切割
            if let Some(split_pos) = current_text.rfind('\n') {
                let (keep, remainder) = current_text.split_at(split_pos);
                chunks.push(Chunk {
                    content: add_header_context(&section.header_context, keep.trim()),
                    heading_path: section.header_context.clone(),
                    start_line: current_start,
                    end_line: section.start_line + i,
                });
                current_text = remainder.trim_start_matches('\n').to_string();
                current_start = section.start_line + i;
                line_count_in_current = 1;
            }
        }
    }

    // Flush 剩余
    let trimmed = current_text.trim();
    if !trimmed.is_empty() {
        chunks.push(Chunk {
            content: add_header_context(&section.header_context, trimmed),
            heading_path: section.header_context.clone(),
            start_line: current_start,
            end_line: section.start_line + section.lines.len(),
        });
    }

    chunks
}

/// 构建 overlap 文本（从前一块末尾取 overlap_lines 行）
fn build_overlap_text(lines: &[&str], current_idx: usize, overlap_lines: usize) -> String {
    if overlap_lines == 0 || current_idx == 0 {
        return String::new();
    }
    let start = current_idx.saturating_sub(overlap_lines);
    lines[start..current_idx].join("\n")
}

/// 将标题栈格式化为上下文字符串（如 "## Architecture > ### Design"）
fn format_header_context(stack: &[(usize, String)]) -> String {
    if stack.len() <= 1 {
        return String::new();
    }
    // 跳过最后一个（当前段落自己的标题），只保留父级
    stack[..stack.len() - 1]
        .iter()
        .map(|(_, text)| text.trim().to_string())
        .collect::<Vec<_>>()
        .join(" > ")
}

/// 将标题上下文注入到块文本前
fn add_header_context(context: &str, text: &str) -> String {
    if context.is_empty() {
        text.to_string()
    } else {
        format!("[Context: {context}]\n\n{text}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_level_detection() {
        assert_eq!(header_level("# Title"), Some(1));
        assert_eq!(header_level("## Section"), Some(2));
        assert_eq!(header_level("### Subsection"), Some(3));
        assert_eq!(header_level("#### Deep"), Some(4));
        assert_eq!(header_level("#hashtag"), None); // 无空格
        assert_eq!(header_level("not a header"), None);
        assert_eq!(header_level(""), None);
        assert_eq!(header_level("##"), Some(2)); // 无文本标题
    }

    #[test]
    fn test_chunk_empty_content() {
        let config = ChunkConfig::default();
        let chunks = chunk_markdown("", &config);
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_chunk_small_content_single_chunk() {
        let config = ChunkConfig::default();
        let content = "# Title\n\nSome text here.";
        let chunks = chunk_markdown(content, &config);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, content);
        assert_eq!(chunks[0].start_line, 0);
    }

    #[test]
    fn test_chunk_splits_on_headers() {
        let config = ChunkConfig {
            max_chunk_chars: 80,
            overlap_lines: 0,
            min_heading_level: 2,
        };
        let content = "## Section 1\n\nContent for section 1 goes here with enough text to matter.\n\n\
                        ## Section 2\n\nContent for section 2 is also significant enough to be a chunk.";
        let chunks = chunk_markdown(content, &config);
        assert!(
            chunks.len() >= 2,
            "should split into at least 2 chunks, got {}",
            chunks.len()
        );
        assert!(chunks[0].content.contains("Section 1"));
        assert!(chunks.last().unwrap().content.contains("Section 2"));
    }

    #[test]
    fn test_chunk_header_context_for_subsections() {
        let config = ChunkConfig {
            max_chunk_chars: 60,
            overlap_lines: 0,
            min_heading_level: 2,
        };
        let content = "## Parent\n\nIntro.\n\n### Child\n\nChild content that is long enough to be its own chunk definitely.";
        let chunks = chunk_markdown(content, &config);
        // 子段落应有父标题上下文
        let child_chunk = chunks.iter().find(|c| c.content.contains("Child content"));
        assert!(child_chunk.is_some(), "should have a child chunk");
        assert!(
            child_chunk.unwrap().content.contains("[Context: ## Parent]"),
            "child chunk should have parent header context, got: {}",
            child_chunk.unwrap().content
        );
    }

    #[test]
    fn test_chunk_large_section_splits_on_paragraphs() {
        let para1 = "A".repeat(100);
        let para2 = "B".repeat(100);
        let content = format!("## Big Section\n\n{para1}\n\n{para2}");
        let config = ChunkConfig {
            max_chunk_chars: 150,
            overlap_lines: 0,
            min_heading_level: 2,
        };
        let chunks = chunk_markdown(&content, &config);
        assert!(
            chunks.len() >= 2,
            "should split large section, got {} chunks",
            chunks.len()
        );
    }

    #[test]
    fn test_chunk_no_headings() {
        let config = ChunkConfig {
            max_chunk_chars: 50,
            overlap_lines: 0,
            min_heading_level: 2,
        };
        let content = "Line one is here.\nLine two is here.\nLine three is here.\nLine four is here.";
        let chunks = chunk_markdown(content, &config);
        // 没有标题时，整个内容作为单块（若不超长）或按段落分割
        assert!(!chunks.is_empty());
    }

    #[test]
    fn test_chunk_very_long_line() {
        let config = ChunkConfig {
            max_chunk_chars: 50,
            overlap_lines: 0,
            min_heading_level: 2,
        };
        let long_line = "A".repeat(200);
        let content = format!("## Header\n\n{long_line}");
        let chunks = chunk_markdown(&content, &config);
        // 即使单行超长，也应返回至少一个块
        assert!(!chunks.is_empty());
    }

    #[test]
    fn test_chunk_text_simple() {
        let chunks = chunk_text("Line 1\nLine 2\nLine 3\nLine 4", 20, 0);
        assert!(!chunks.is_empty());
        // 所有内容应被覆盖
        let total_lines: usize = chunks.iter().map(|c| c.content.lines().count()).sum();
        assert!(total_lines >= 4, "should cover all 4 lines");
    }

    #[test]
    fn test_chunk_text_empty() {
        let chunks = chunk_text("", 100, 0);
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_chunk_preserves_code_blocks() {
        let config = ChunkConfig::default();
        let content = "## Code\n\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\nSome text.";
        let chunks = chunk_markdown(content, &config);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("```rust"));
        assert!(chunks[0].content.contains("fn main()"));
    }

    #[test]
    fn test_chunk_heading_path_inheritance() {
        let config = ChunkConfig {
            max_chunk_chars: 60,
            overlap_lines: 0,
            min_heading_level: 2,
        };
        let content = "## Architecture\n\nSome intro.\n\n### Database\n\nDB content here is important.\n\n#### Schema\n\nSchema details.";
        let chunks = chunk_markdown(content, &config);
        // Schema 块应继承 Architecture > Database 路径
        let schema_chunk = chunks.iter().find(|c| c.content.contains("Schema details"));
        if let Some(chunk) = schema_chunk {
            assert!(
                chunk.heading_path.contains("Architecture"),
                "Schema chunk should have Architecture in heading path"
            );
        }
    }

    #[test]
    fn test_chunk_overlap_lines() {
        let config = ChunkConfig {
            max_chunk_chars: 80,
            overlap_lines: 2,
            min_heading_level: 2,
        };
        let para1 = "X".repeat(60);
        let para2 = "Y".repeat(60);
        let content = format!("## Section\n\n{para1}\n\n{para2}");
        let chunks = chunk_markdown(&content, &config);
        assert!(
            chunks.len() >= 2,
            "should split into multiple chunks, got {}",
            chunks.len()
        );
    }
}
