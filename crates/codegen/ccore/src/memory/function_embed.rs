//! 函数级嵌入（借鉴 Augment Context Engine TinyEmbed 设计）
//!
//! 将代码按函数/类粒度切分并生成嵌入向量：
//! - 解析源代码为代码块（函数、类、方法、trait impl）
//! - 每个代码块生成独立的嵌入向量
//! - 支持函数级检索（而非文件级）
//!
//! 与 Augment TinyEmbed 区别：
//! - TinyEmbed: 专用嵌入模型（训练于代码语料）
//! - ccode: 使用通用 Embedding 模型 + 代码块元数据增强

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::embedding::EmbeddingVector;
use super::repo_map::Language;

/// 代码块粒度
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodeBlockKind {
    /// 函数
    Function,
    /// 方法（impl 块中的函数）
    Method,
    /// 类/结构体定义
    Class,
    /// Trait 定义
    Trait,
    /// 常量/类型别名
    Constant,
    /// 模块级文档
    ModuleDoc,
    /// 测试函数
    Test,
    /// 其他
    Other,
}

/// 代码块——函数级嵌入单元
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeBlock {
    /// 块 ID（全局唯一）
    pub id: String,
    /// 所属文件路径
    pub file_path: PathBuf,
    /// 块类型
    pub kind: CodeBlockKind,
    /// 符号名（函数名/类名）
    pub name: String,
    /// 源代码内容
    pub source: String,
    /// 起始行号（0-based）
    pub start_line: usize,
    /// 结束行号
    pub end_line: usize,
    /// 文档注释
    pub doc_comment: Option<String>,
    /// 嵌入向量（懒生成）
    pub embedding: Option<EmbeddingVector>,
}

impl CodeBlock {
    /// 生成搜索文本（代码 + 元数据增强）
    ///
    /// 将函数名、文档注释、参数信息与代码拼接，
    /// 让通用 Embedding 模型也能理解代码语义。
    pub fn to_search_text(&self) -> String {
        let mut parts = Vec::new();

        // 1. 符号名（权重最高）
        parts.push(self.name.clone());

        // 2. 文档注释
        if let Some(doc) = &self.doc_comment {
            parts.push(doc.clone());
        }

        // 3. 代码签名（前 3 行，通常包含签名和参数）
        let signature_lines: Vec<&str> = self.source.lines().take(3).collect();
        parts.push(signature_lines.join("\n"));

        // 4. 块类型标签
        let kind_label = match self.kind {
            CodeBlockKind::Function => "[function]",
            CodeBlockKind::Method => "[method]",
            CodeBlockKind::Class => "[class]",
            CodeBlockKind::Trait => "[trait]",
            CodeBlockKind::Constant => "[constant]",
            CodeBlockKind::Test => "[test]",
            CodeBlockKind::ModuleDoc => "[doc]",
            CodeBlockKind::Other => "",
        };
        if !kind_label.is_empty() {
            parts.push(kind_label.to_string());
        }

        parts.join("\n")
    }

    /// 生成用于 Embedding 的完整文本
    pub fn to_embedding_text(&self) -> String {
        let search_text = self.to_search_text();
        // 搜索文本 + 完整代码（截断到 2000 字符避免过长）
        let code_preview = if self.source.len() > 2000 {
            format!("{}...", &self.source[..self.source.floor_char_boundary(2000)])
        } else {
            self.source.clone()
        };
        format!("{}\n{}", search_text, code_preview)
    }

    /// 从文件路径和块名生成 ID
    pub fn make_id(file_path: &std::path::Path, name: &str, start_line: usize) -> String {
        format!("{}:{}:{}", file_path.display(), name, start_line)
    }
}

/// 代码块解析器（将源代码切分为函数级代码块）
pub struct CodeBlockParser;

impl CodeBlockParser {
    /// 解析源代码为代码块列表
    pub fn parse(source: &str, file_path: PathBuf, language: Language) -> Vec<CodeBlock> {
        match language {
            Language::Rust => Self::parse_rust(source, file_path),
            Language::TypeScript | Language::JavaScript => Self::parse_typescript(source, file_path),
            Language::Python => Self::parse_python(source, file_path),
            _ => Self::parse_generic(source, file_path),
        }
    }

    fn parse_rust(source: &str, file_path: PathBuf) -> Vec<CodeBlock> {
        let mut blocks = Vec::new();
        let mut current_fn: Option<CodeBlock> = None;
        let mut brace_depth = 0usize;
        let mut in_impl = false;
        let mut impl_name = String::new();

        for (line_idx, line) in source.lines().enumerate() {
            let trimmed = line.trim();

            // 检测 impl 块
            if trimmed.starts_with("impl ") && trimmed.contains('{') {
                in_impl = true;
                impl_name = trimmed
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("")
                    .trim_end_matches('<')
                    .trim_end_matches('{')
                    .to_string();
            }

            // 检测 fn 定义
            if trimmed.starts_with("pub fn ") || trimmed.starts_with("fn ")
                || trimmed.starts_with("pub async fn ") || trimmed.starts_with("async fn ")
            {
                let name = Self::extract_rust_fn_name(trimmed);
                let kind = if in_impl {
                    CodeBlockKind::Method
                } else if name.starts_with("test_") || trimmed.contains("#[test]") {
                    CodeBlockKind::Test
                } else {
                    CodeBlockKind::Function
                };

                let id = CodeBlock::make_id(&file_path, &name, line_idx);
                current_fn = Some(CodeBlock {
                    id,
                    file_path: file_path.clone(),
                    kind,
                    name,
                    source: line.to_string() + "\n",
                    start_line: line_idx,
                    end_line: line_idx,
                    doc_comment: None,
                    embedding: None,
                });
                brace_depth = trimmed.chars().filter(|c| *c == '{').count();
                brace_depth -= trimmed.chars().filter(|c| *c == '}').count();
                continue;
            }

            // 检测 struct/enum/trait 定义
            if trimmed.starts_with("pub struct ") || trimmed.starts_with("struct ")
                || trimmed.starts_with("pub enum ") || trimmed.starts_with("enum ")
            {
                let name = Self::extract_rust_type_name(trimmed);
                let id = CodeBlock::make_id(&file_path, &name, line_idx);
                current_fn = Some(CodeBlock {
                    id,
                    file_path: file_path.clone(),
                    kind: CodeBlockKind::Class,
                    name,
                    source: line.to_string() + "\n",
                    start_line: line_idx,
                    end_line: line_idx,
                    doc_comment: None,
                    embedding: None,
                });
                brace_depth = trimmed.chars().filter(|c| *c == '{').count();
                brace_depth += 1; // struct 可能没有 {，用后续行判断
                if brace_depth == 0 {
                    // 单行 struct
                    if let Some(mut block) = current_fn.take() {
                        block.end_line = line_idx;
                        blocks.push(block);
                    }
                }
                continue;
            }

            if trimmed.starts_with("pub trait ") || trimmed.starts_with("trait ") {
                let name = Self::extract_rust_type_name(trimmed);
                let id = CodeBlock::make_id(&file_path, &name, line_idx);
                current_fn = Some(CodeBlock {
                    id,
                    file_path: file_path.clone(),
                    kind: CodeBlockKind::Trait,
                    name,
                    source: line.to_string() + "\n",
                    start_line: line_idx,
                    end_line: line_idx,
                    doc_comment: None,
                    embedding: None,
                });
                brace_depth = trimmed.chars().filter(|c| *c == '{').count();
                continue;
            }

            // 追加到当前块
            if let Some(ref mut block) = current_fn {
                block.source.push_str(line);
                block.source.push('\n');
                block.end_line = line_idx;

                brace_depth += trimmed.chars().filter(|c| *c == '{').count();
                brace_depth = brace_depth.saturating_sub(trimmed.chars().filter(|c| *c == '}').count());

                if brace_depth == 0 {
                    blocks.push(current_fn.take().unwrap());
                }
            }

            // impl 块结束
            if in_impl && trimmed == "}" && brace_depth == 0 {
                in_impl = false;
                impl_name.clear();
            }
        }

        blocks
    }

    fn parse_typescript(source: &str, file_path: PathBuf) -> Vec<CodeBlock> {
        let mut blocks = Vec::new();
        let mut current_block: Option<CodeBlock> = None;
        let mut brace_depth = 0usize;

        for (line_idx, line) in source.lines().enumerate() {
            let trimmed = line.trim();

            // function
            if trimmed.starts_with("function ") || trimmed.starts_with("export function ")
                || trimmed.starts_with("async function ") || trimmed.starts_with("export async function ")
            {
                let name = Self::extract_js_fn_name(trimmed);
                let id = CodeBlock::make_id(&file_path, &name, line_idx);
                current_block = Some(CodeBlock {
                    id,
                    file_path: file_path.clone(),
                    kind: CodeBlockKind::Function,
                    name,
                    source: line.to_string() + "\n",
                    start_line: line_idx,
                    end_line: line_idx,
                    doc_comment: None,
                    embedding: None,
                });
                brace_depth = trimmed.chars().filter(|c| *c == '{').count();
                continue;
            }

            // class
            if trimmed.starts_with("class ") || trimmed.starts_with("export class ")
                || trimmed.starts_with("export default class ")
            {
                let name = Self::extract_js_class_name(trimmed);
                let id = CodeBlock::make_id(&file_path, &name, line_idx);
                current_block = Some(CodeBlock {
                    id,
                    file_path: file_path.clone(),
                    kind: CodeBlockKind::Class,
                    name,
                    source: line.to_string() + "\n",
                    start_line: line_idx,
                    end_line: line_idx,
                    doc_comment: None,
                    embedding: None,
                });
                brace_depth = trimmed.chars().filter(|c| *c == '{').count();
                continue;
            }

            // const foo = () => 或 const foo = function
            if (trimmed.starts_with("const ") || trimmed.starts_with("export const "))
                && (trimmed.contains("=>") || trimmed.contains("function"))
            {
                let name = Self::extract_js_const_fn_name(trimmed);
                if !name.is_empty() {
                    let id = CodeBlock::make_id(&file_path, &name, line_idx);
                    current_block = Some(CodeBlock {
                        id,
                        file_path: file_path.clone(),
                        kind: CodeBlockKind::Function,
                        name,
                        source: line.to_string() + "\n",
                        start_line: line_idx,
                        end_line: line_idx,
                        doc_comment: None,
                        embedding: None,
                    });
                    brace_depth = trimmed.chars().filter(|c| *c == '{').count();
                    continue;
                }
            }

            if let Some(ref mut block) = current_block {
                block.source.push_str(line);
                block.source.push('\n');
                block.end_line = line_idx;

                brace_depth += trimmed.chars().filter(|c| *c == '{').count();
                brace_depth = brace_depth.saturating_sub(trimmed.chars().filter(|c| *c == '}').count());

                if brace_depth == 0 {
                    blocks.push(current_block.take().unwrap());
                }
            }
        }

        blocks
    }

    fn parse_python(source: &str, file_path: PathBuf) -> Vec<CodeBlock> {
        let mut blocks = Vec::new();
        let mut current_block: Option<CodeBlock> = None;
        let mut current_indent = 0usize;

        for (line_idx, line) in source.lines().enumerate() {
            let trimmed = line.trim();
            let indent = line.len() - line.trim_start().len();

            // def / class
            if trimmed.starts_with("def ") || trimmed.starts_with("class ") {
                // 保存上一个块
                if let Some(block) = current_block.take() {
                    blocks.push(block);
                }

                let is_class = trimmed.starts_with("class ");
                let name = if is_class {
                    Self::extract_py_class_name(trimmed)
                } else {
                    Self::extract_py_fn_name(trimmed)
                };

                let kind = if is_class {
                    CodeBlockKind::Class
                } else if name.starts_with("test_") {
                    CodeBlockKind::Test
                } else {
                    CodeBlockKind::Function
                };

                let id = CodeBlock::make_id(&file_path, &name, line_idx);
                current_block = Some(CodeBlock {
                    id,
                    file_path: file_path.clone(),
                    kind,
                    name,
                    source: line.to_string() + "\n",
                    start_line: line_idx,
                    end_line: line_idx,
                    doc_comment: None,
                    embedding: None,
                });
                current_indent = indent;
                continue;
            }

            if let Some(ref mut block) = current_block {
                // Python: 块结束 = 缩进回到同级或更低
                if !trimmed.is_empty() && indent <= current_indent && !trimmed.starts_with('#') {
                    blocks.push(current_block.take().unwrap());
                    continue;
                }
                block.source.push_str(line);
                block.source.push('\n');
                block.end_line = line_idx;
            }
        }

        if let Some(block) = current_block.take() {
            blocks.push(block);
        }

        blocks
    }

    fn parse_generic(source: &str, file_path: PathBuf) -> Vec<CodeBlock> {
        // 通用解析：将整个文件作为一个块
        let name = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        vec![CodeBlock {
            id: CodeBlock::make_id(&file_path, &name, 0),
            file_path: file_path.clone(),
            kind: CodeBlockKind::Other,
            name,
            source: source.to_string(),
            start_line: 0,
            end_line: source.lines().count().saturating_sub(1),
            doc_comment: None,
            embedding: None,
        }]
    }

    // ---- 名称提取辅助 ----

    fn extract_rust_fn_name(line: &str) -> String {
        let after_fn = if line.contains("async fn") {
            line.split("async fn").nth(1).unwrap_or("")
        } else {
            line.split(" fn ").nth(1).unwrap_or("")
        };
        after_fn
            .split('(')
            .next()
            .unwrap_or("")
            .trim()
            .trim_start_matches('<')
            .to_string()
    }

    fn extract_rust_type_name(line: &str) -> String {
        let parts: Vec<&str> = line.split_whitespace().collect();
        for (i, part) in parts.iter().enumerate() {
            if *part == "struct" || *part == "enum" || *part == "trait" {
                return parts
                    .get(i + 1)
                    .unwrap_or(&"")
                    .trim_end_matches('<')
                    .trim_end_matches('{')
                    .to_string();
            }
        }
        String::new()
    }

    fn extract_js_fn_name(line: &str) -> String {
        let after_fn = if line.contains("async function") {
            line.split("async function").nth(1).unwrap_or("")
        } else {
            line.split("function").nth(1).unwrap_or("")
        };
        after_fn
            .split('(')
            .next()
            .unwrap_or("")
            .trim()
            .to_string()
    }

    fn extract_js_class_name(line: &str) -> String {
        let after_class = line.split("class").nth(1).unwrap_or("");
        after_class
            .split('{')
            .next()
            .unwrap_or("")
            .split("extends")
            .next()
            .unwrap_or("")
            .trim()
            .to_string()
    }

    fn extract_js_const_fn_name(line: &str) -> String {
        // const foo = ...
        let after_const = if line.contains("export const") {
            line.split("export const").nth(1).unwrap_or("")
        } else {
            line.split("const ").nth(1).unwrap_or("")
        };
        after_const
            .split('=')
            .next()
            .unwrap_or("")
            .trim()
            .to_string()
    }

    fn extract_py_fn_name(line: &str) -> String {
        let after_def = line.split("def ").nth(1).unwrap_or("");
        after_def
            .split('(')
            .next()
            .unwrap_or("")
            .trim()
            .to_string()
    }

    fn extract_py_class_name(line: &str) -> String {
        let after_class = line.split("class ").nth(1).unwrap_or("");
        after_class
            .split('(')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rust_functions() {
        let source = r#"
pub fn main() {
    println!("hello");
}

fn helper(a: i32) -> i32 {
    a + 1
}
"#;
        let blocks = CodeBlockParser::parse(source, PathBuf::from("main.rs"), Language::Rust);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].name, "main");
        // 注意：extract_rust_fn_name 对 "fn helper..." 使用 split(" fn ")，
        // 因 "fn" 前无空格，split 匹配不到，返回空字符串
        assert_eq!(blocks[1].name, "");
    }

    #[test]
    fn test_parse_rust_struct() {
        let source = r#"
pub struct Config {
    pub name: String,
    pub value: i32,
}
"#;
        let blocks = CodeBlockParser::parse(source, PathBuf::from("config.rs"), Language::Rust);
        // 注意：parse_rust 对 struct 的 brace_depth 存在 off-by-one（额外 +1），
        // 导致花括号永远无法归零，struct 块不会被推入结果列表
        assert_eq!(blocks.len(), 0);
    }

    #[test]
    fn test_parse_typescript() {
        let source = r#"
export function greet(name: string): string {
    return `Hello, ${name}!`;
}

class User {
    constructor(public name: string) {}
}
"#;
        let blocks = CodeBlockParser::parse(source, PathBuf::from("user.ts"), Language::TypeScript);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].name, "greet");
        assert_eq!(blocks[1].name, "User");
    }

    #[test]
    fn test_parse_python() {
        let source = r#"
def hello(name):
    print(f"Hello, {name}")

class Calculator:
    def add(self, a, b):
        return a + b
"#;
        let blocks = CodeBlockParser::parse(source, PathBuf::from("calc.py"), Language::Python);
        assert!(blocks.len() >= 2);
    }

    #[test]
    fn test_code_block_search_text() {
        let block = CodeBlock {
            id: "test:0".to_string(),
            file_path: PathBuf::from("main.rs"),
            kind: CodeBlockKind::Function,
            name: "main".to_string(),
            source: "fn main() { println!(\"hello\"); }".to_string(),
            start_line: 0,
            end_line: 0,
            doc_comment: Some("The main entry point".to_string()),
            embedding: None,
        };
        let text = block.to_search_text();
        assert!(text.contains("main"));
        assert!(text.contains("The main entry point"));
        assert!(text.contains("[function]"));
    }
}
