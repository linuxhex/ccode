//! 编译反馈模块 - 多语言编译检查，格式化为可注入 Agent 上下文的错误信息
//!
//! Agent 在改完代码后通过本模块获取结构化编译反馈：
//! - `run_compile_check` 根据项目类型和编辑文件自动选择检查器
//! - `run_cargo_check` 调用 cargo check --message-format=json（Rust）
//! - `run_typecheck_check` 调用 npx tsc --noEmit（TypeScript/JavaScript）
//! - `run_go_check` 调用 go build ./...（Go）
//! - `run_python_syntax_check` 调用 python -m py_compile（Python）
//! - `run_rustfmt_check` 单文件 rustfmt --check 检查格式
//! - `format_for_injection` 将报告折叠为 system prompt 可直接拼接的文本

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use tokio::process::Command;

/// 编译错误/警告条目
#[derive(Debug, Clone, Deserialize)]
pub struct CompileError {
    /// 源文件路径
    pub file: String,
    /// 行号（1-based）
    pub line: usize,
    /// 列号（1-based）
    pub column: usize,
    /// 错误/警告文本
    pub message: String,
    /// 级别："error" 或 "warning"
    pub level: String,
}

/// 编译报告
#[derive(Debug, Clone)]
pub struct CompileReport {
    /// 错误列表
    pub errors: Vec<CompileError>,
    /// 警告列表
    pub warnings: Vec<CompileError>,
    /// 是否编译通过（无 error 即视为通过）
    pub success: bool,
}

/// cargo --message-format=json 单行消息的中间解析结构
#[derive(Debug, Deserialize)]
struct CargoMessage {
    reason: String,
    message: Option<CompilerMessage>,
}

/// 编译器消息体
#[derive(Debug, Deserialize)]
struct CompilerMessage {
    level: String,
    message: String,
    spans: Vec<Span>,
}

/// 源码定位区间，取起始位置作为错误位置
#[derive(Debug, Deserialize)]
struct Span {
    file_name: String,
    line_start: usize,
    column_start: usize,
}

/// 运行 cargo check 并解析输出
///
/// 使用 JSON 消息格式逐行解析 compiler-message，分离 error 与 warning。
/// 命令自身非零退出且未从 JSON 提取到 error 时，将 stderr 作为兜底 error 收录，
/// 以覆盖 cargo 自身失败（如缺失 Cargo.toml）这类非编译错误场景。
pub async fn run_cargo_check(workdir: &Path) -> Result<CompileReport> {
    let output = Command::new("cargo")
        .args(["check", "--message-format=json"])
        .current_dir(workdir)
        .env("TERM", "dumb") // 抑制颜色码，避免污染 JSON 解析
        .output()
        .await
        .context("执行 cargo check 失败")?;

    let mut errors: Vec<CompileError> = Vec::new();
    let mut warnings: Vec<CompileError> = Vec::new();

    // stdout 每行一个 JSON 对象，逐行解析提取 compiler-message
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed: CargoMessage = match serde_json::from_str(line) {
            Ok(msg) => msg,
            Err(_) => continue, // 跳过非 JSON 行（如 cargo 进度提示）
        };
        if parsed.reason != "compiler-message" {
            continue;
        }
        let Some(msg) = parsed.message else {
            continue;
        };
        // 取首个 span 作为定位，缺 span 时退化为空文件名 + 0 行列
        let (file, line_no, column) = msg
            .spans
            .first()
            .map(|s| (s.file_name.clone(), s.line_start, s.column_start))
            .unwrap_or_default();
        let entry = CompileError {
            file,
            line: line_no,
            column,
            message: msg.message,
            level: msg.level.clone(),
        };
        if msg.level == "error" {
            errors.push(entry);
        } else if msg.level == "warning" {
            warnings.push(entry);
        }
    }

    // 命令非零退出但 JSON 未提取到 error：将 stderr 作为兜底错误
    if !output.status.success() && errors.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr_trimmed = stderr.trim();
        if !stderr_trimmed.is_empty() {
            errors.push(CompileError {
                file: String::new(),
                line: 0,
                column: 0,
                message: stderr_trimmed.to_string(),
                level: "error".to_string(),
            });
        }
    }

    let success = errors.is_empty();
    Ok(CompileReport {
        errors,
        warnings,
        success,
    })
}

/// 项目类型，根据根目录标记文件自动检测
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectType {
    Rust,
    TypeScript,
    Go,
    Python,
    Unknown,
}

/// 根据工作目录标记文件检测项目类型
///
/// 优先级：Cargo.toml > go.mod > package.json > pyproject.toml > setup.py > requirements.txt
pub fn detect_project_type(workdir: &Path) -> ProjectType {
    if workdir.join("Cargo.toml").exists() {
        ProjectType::Rust
    } else if workdir.join("go.mod").exists() {
        ProjectType::Go
    } else if workdir.join("package.json").exists() {
        ProjectType::TypeScript
    } else if workdir.join("pyproject.toml").exists()
        || workdir.join("setup.py").exists()
        || workdir.join("requirements.txt").exists()
    {
        ProjectType::Python
    } else {
        ProjectType::Unknown
    }
}

/// 根据文件扩展名判断语言类型
fn extension_to_language(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some("typescript"),
        "go" => Some("go"),
        "py" | "pyi" => Some("python"),
        "java" | "kt" | "kts" => Some("java"),
        _ => None,
    }
}

/// 从编辑文件列表中提取涉及的语言集合
fn detect_languages_from_files(edited_files: &[String]) -> Vec<&'static str> {
    let mut langs: Vec<&'static str> = Vec::new();
    for file in edited_files {
        let path = Path::new(file);
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if let Some(lang) = extension_to_language(ext) {
                if !langs.contains(&lang) {
                    langs.push(lang);
                }
            }
        }
    }
    langs
}

/// 统一编译检查入口：根据项目类型和编辑文件自动选择检查器
///
/// 优先按编辑文件的语言决定检查器；若编辑文件无法判断，则按项目类型回退。
/// 目前支持 Rust（cargo check）、TypeScript（tsc --noEmit）、Go（go build）、
/// Python（py_compile 语法检查）。
pub async fn run_compile_check(
    workdir: &Path,
    edited_files: &[String],
) -> Result<CompileReport> {
    let file_langs = detect_languages_from_files(edited_files);
    let project_type = detect_project_type(workdir);

    // 按编辑文件语言优先选择检查器
    if file_langs.contains(&"rust") || project_type == ProjectType::Rust {
        return run_cargo_check(workdir).await;
    }
    if file_langs.contains(&"typescript") || project_type == ProjectType::TypeScript {
        return run_typecheck_check(workdir).await;
    }
    if file_langs.contains(&"go") || project_type == ProjectType::Go {
        return run_go_check(workdir).await;
    }
    if file_langs.contains(&"python") || project_type == ProjectType::Python {
        return run_python_syntax_check(workdir, edited_files).await;
    }

    // 未知项目类型：不执行检查
    Ok(CompileReport {
        errors: vec![],
        warnings: vec![],
        success: true,
    })
}

/// TypeScript/JavaScript 类型检查：npx tsc --noEmit
///
/// 解析 tsc 输出格式 "file(line,col): error TSxxxx: message"，
/// 分离 error 与 warning，兜底将 stderr 作为错误。
pub async fn run_typecheck_check(workdir: &Path) -> Result<CompileReport> {
    let output = Command::new("npx")
        .args(["tsc", "--noEmit", "--pretty", "false"])
        .current_dir(workdir)
        .env("TERM", "dumb")
        .output()
        .await
        .context("执行 npx tsc --noEmit 失败")?;

    let mut errors: Vec<CompileError> = Vec::new();
    let mut warnings: Vec<CompileError> = Vec::new();

    // tsc 输出格式：src/foo.ts(10,5): error TS2304: Cannot find name 'x'.
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(entry) = parse_tsc_line(line) {
            if entry.level == "error" {
                errors.push(entry);
            } else {
                warnings.push(entry);
            }
        }
    }

    if !output.status.success() && errors.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr_trimmed = stderr.trim();
        if !stderr_trimmed.is_empty() {
            errors.push(CompileError {
                file: String::new(),
                line: 0,
                column: 0,
                message: stderr_trimmed.to_string(),
                level: "error".to_string(),
            });
        }
    }

    let success = errors.is_empty();
    Ok(CompileReport {
        errors,
        warnings,
        success,
    })
}

/// 解析 tsc 单行输出：src/foo.ts(10,5): error TS2304: message
fn parse_tsc_line(line: &str) -> Option<CompileError> {
    let paren_pos = line.find('(')?;
    let close_paren = line.find(')')?;
    let file = &line[..paren_pos];
    let loc = &line[paren_pos + 1..close_paren];
    let rest = line[close_paren + 1..].trim_start_matches(|c: char| c == ':' || c.is_whitespace());

    let mut parts = loc.split(',');
    let line_no: usize = parts.next()?.parse().ok()?;
    let col: usize = parts.next()?.parse().ok()?;

    let (level, message) = if let Some(stripped) = rest.strip_prefix("error ") {
        ("error", stripped)
    } else if let Some(stripped) = rest.strip_prefix("warning ") {
        ("warning", stripped)
    } else {
        ("error", rest)
    };

    Some(CompileError {
        file: file.to_string(),
        line: line_no,
        column: col,
        message: message.to_string(),
        level: level.to_string(),
    })
}

/// Go 编译检查：go build ./...
///
/// 解析 go 编译器输出格式 "file:line:col: message"，
/// 区分 error 与 warning（go build 仅输出 error，go vet 输出 warning）。
pub async fn run_go_check(workdir: &Path) -> Result<CompileReport> {
    let output = Command::new("go")
        .args(["build", "./..."])
        .current_dir(workdir)
        .env("TERM", "dumb")
        .output()
        .await
        .context("执行 go build 失败")?;

    let mut errors: Vec<CompileError> = Vec::new();

    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // go 编译错误格式：./file.go:10:5: undefined: x
        if let Some(entry) = parse_go_error(line) {
            errors.push(entry);
        }
    }

    if !output.status.success() && errors.is_empty() {
        let stderr_trimmed = stderr.trim();
        if !stderr_trimmed.is_empty() {
            errors.push(CompileError {
                file: String::new(),
                line: 0,
                column: 0,
                message: stderr_trimmed.to_string(),
                level: "error".to_string(),
            });
        }
    }

    let success = errors.is_empty();
    Ok(CompileReport {
        errors,
        warnings: vec![],
        success,
    })
}

/// 解析 go 编译错误行：./file.go:10:5: message
fn parse_go_error(line: &str) -> Option<CompileError> {
    let parts: Vec<&str> = line.splitn(4, ':').collect();
    if parts.len() < 4 {
        return None;
    }
    let file = parts[0].trim().to_string();
    let line_no: usize = parts[1].trim().parse().ok()?;
    let col: usize = parts[2].trim().parse().ok()?;
    let message = parts[3].trim().to_string();

    if file.is_empty() || line_no == 0 {
        return None;
    }

    Some(CompileError {
        file,
        line: line_no,
        column: col,
        message,
        level: "error".to_string(),
    })
}

/// Python 语法检查：对编辑的 .py 文件逐个运行 python -m py_compile
///
/// py_compile 语法错误输出格式：
///   File "file.py", line 10, in <module>
///     <source line>
/// SyntaxError: <message>
pub async fn run_python_syntax_check(
    workdir: &Path,
    edited_files: &[String],
) -> Result<CompileReport> {
    let mut errors: Vec<CompileError> = Vec::new();

    let py_files: Vec<&str> = edited_files
        .iter()
        .filter(|f| f.ends_with(".py") || f.ends_with(".pyi"))
        .map(|f| f.as_str())
        .collect();

    if py_files.is_empty() {
        // 无 Python 文件编辑，尝试对整个项目做 import 检查
        return run_python_import_check(workdir).await;
    }

    for file in &py_files {
        let filepath = if Path::new(file).is_absolute() {
            file.to_string()
        } else {
            workdir.join(file).to_string_lossy().to_string()
        };
        let output = Command::new("python3")
            .args(["-m", "py_compile", &filepath])
            .current_dir(workdir)
            .output()
            .await;

        let output = match output {
            Ok(o) => o,
            Err(_) => continue, // python3 未安装则跳过
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if let Some(entry) = parse_python_syntax_error(&stderr, file) {
                errors.push(entry);
            }
        }
    }

    let success = errors.is_empty();
    Ok(CompileReport {
        errors,
        warnings: vec![],
        success,
    })
}

/// 解析 Python SyntaxError 输出
fn parse_python_syntax_error(stderr: &str, fallback_file: &str) -> Option<CompileError> {
    // File "xxx.py", line 10
    for line in stderr.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("File \"") {
            if let Some(quote_end) = rest.find('"') {
                let file = &rest[..quote_end];
                let after_file = &rest[quote_end + 1..];
                if let Some(line_start) = after_file.find("line ") {
                    let line_str = &after_file[line_start + 5..];
                    let line_no: usize = line_str
                        .split(|c: char| !c.is_ascii_digit())
                        .next()?
                        .parse()
                        .ok()?;
                    // 取最后一行作为错误消息（通常是 SyntaxError: xxx）
                    let message = stderr
                        .lines()
                        .rev()
                        .find(|l| l.trim().starts_with("SyntaxError:"))
                        .map(|l| l.trim().to_string())
                        .unwrap_or_else(|| "SyntaxError".to_string());
                    return Some(CompileError {
                        file: if file.is_empty() {
                            fallback_file.to_string()
                        } else {
                            file.to_string()
                        },
                        line: line_no,
                        column: 0,
                        message,
                        level: "error".to_string(),
                    });
                }
            }
        }
    }
    None
}

/// Python 项目级 import 检查：尝试 import 主模块
///
/// 仅在有 __init__.py 或 setup.py 时尝试，失败不影响主流程。
async fn run_python_import_check(workdir: &Path) -> Result<CompileReport> {
    let src_dir = workdir.join("src");
    let has_init = src_dir.join("__init__.py").exists();
    let has_setup = workdir.join("setup.py").exists();

    if !has_init && !has_setup {
        return Ok(CompileReport {
            errors: vec![],
            warnings: vec![],
            success: true,
        });
    }

    let project_name = workdir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .replace('-', "_");

    let output = Command::new("python3")
        .args(["-c", &format!("import {}", project_name)])
        .current_dir(workdir)
        .output()
        .await
        .context("执行 python import 检查失败")?;

    let mut errors = Vec::new();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr_trimmed = stderr.trim();
        if !stderr_trimmed.is_empty() {
            errors.push(CompileError {
                file: String::new(),
                line: 0,
                column: 0,
                message: stderr_trimmed.to_string(),
                level: "error".to_string(),
            });
        }
    }

    let success = errors.is_empty();
    Ok(CompileReport {
        errors,
        warnings: vec![],
        success,
    })
}

/// 运行 rustfmt --check 单文件检查
///
/// 返回 None 表示格式合规；返回 Some(String) 给出问题描述（优先 stderr，回退 stdout，再回退固定提示）。
pub async fn run_rustfmt_check(filepath: &Path) -> Result<Option<String>> {
    let output = Command::new("rustfmt")
        .args(["--check", filepath.to_str().unwrap_or_default()])
        .output()
        .await
        .context("执行 rustfmt --check 失败")?;

    if output.status.success() {
        return Ok(None);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr_trimmed = stderr.trim();
    if !stderr_trimmed.is_empty() {
        return Ok(Some(stderr_trimmed.to_string()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout_trimmed = stdout.trim();
    if !stdout_trimmed.is_empty() {
        return Ok(Some(stdout_trimmed.to_string()));
    }

    Ok(Some("格式不符合 rustfmt 规范".to_string()))
}

/// 将编译报告格式化为可注入 system prompt 的文本
///
/// - 编译通过且无警告：返回空字符串
/// - 仅有警告：输出 "编译警告：" 段
/// - 含错误：输出 "编译发现以下错误：" 段；若同时存在警告则追加警告段
pub fn format_for_injection(report: &CompileReport) -> String {
    let has_errors = !report.errors.is_empty();
    let has_warnings = !report.warnings.is_empty();

    if !has_errors && !has_warnings {
        return String::new();
    }

    let mut sections: Vec<String> = Vec::new();

    if has_errors {
        let mut buf = String::from("编译发现以下错误：");
        for (idx, err) in report.errors.iter().enumerate() {
            buf.push_str(&format!(
                "\n{}. {}:{}:{} {}",
                idx + 1,
                err.file,
                err.line,
                err.column,
                err.message
            ));
        }
        sections.push(buf);
    }

    if has_warnings {
        let mut buf = String::from("编译警告：");
        for (idx, warn) in report.warnings.iter().enumerate() {
            buf.push_str(&format!(
                "\n{}. {}:{}:{} {}",
                idx + 1,
                warn.file,
                warn.line,
                warn.column,
                warn.message
            ));
        }
        sections.push(buf);
    }

    sections.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_empty_when_success_no_warnings() {
        let report = CompileReport {
            errors: vec![],
            warnings: vec![],
            success: true,
        };
        assert_eq!(format_for_injection(&report), "");
    }

    #[test]
    fn format_errors_section() {
        let report = CompileReport {
            errors: vec![
                CompileError {
                    file: "src/main.rs".into(),
                    line: 10,
                    column: 5,
                    message: "expected `;`".into(),
                    level: "error".into(),
                },
                CompileError {
                    file: "src/lib.rs".into(),
                    line: 3,
                    column: 1,
                    message: "cannot find value `x`".into(),
                    level: "error".into(),
                },
            ],
            warnings: vec![],
            success: false,
        };
        let out = format_for_injection(&report);
        assert!(out.starts_with("编译发现以下错误："));
        assert!(out.contains("1. src/main.rs:10:5 expected `;`"));
        assert!(out.contains("2. src/lib.rs:3:1 cannot find value `x`"));
    }

    #[test]
    fn format_warnings_only_section() {
        let report = CompileReport {
            errors: vec![],
            warnings: vec![CompileError {
                file: "src/main.rs".into(),
                line: 1,
                column: 1,
                message: "unused import".into(),
                level: "warning".into(),
            }],
            success: true,
        };
        let out = format_for_injection(&report);
        assert!(out.starts_with("编译警告："));
        assert!(out.contains("1. src/main.rs:1:1 unused import"));
        assert!(!out.contains("编译发现以下错误"));
    }

    #[test]
    fn format_errors_and_warnings_both() {
        let report = CompileReport {
            errors: vec![CompileError {
                file: "a.rs".into(),
                line: 1,
                column: 1,
                message: "err".into(),
                level: "error".into(),
            }],
            warnings: vec![CompileError {
                file: "b.rs".into(),
                line: 2,
                column: 2,
                message: "warn".into(),
                level: "warning".into(),
            }],
            success: false,
        };
        let out = format_for_injection(&report);
        assert!(out.contains("编译发现以下错误："));
        assert!(out.contains("编译警告："));
    }

    #[test]
    fn extension_to_language_rust() {
        assert_eq!(extension_to_language("rs"), Some("rust"));
    }

    #[test]
    fn extension_to_language_typescript() {
        assert_eq!(extension_to_language("ts"), Some("typescript"));
        assert_eq!(extension_to_language("tsx"), Some("typescript"));
        assert_eq!(extension_to_language("js"), Some("typescript"));
        assert_eq!(extension_to_language("jsx"), Some("typescript"));
    }

    #[test]
    fn extension_to_language_go() {
        assert_eq!(extension_to_language("go"), Some("go"));
    }

    #[test]
    fn extension_to_language_python() {
        assert_eq!(extension_to_language("py"), Some("python"));
        assert_eq!(extension_to_language("pyi"), Some("python"));
    }

    #[test]
    fn extension_to_language_unknown() {
        assert_eq!(extension_to_language("md"), None);
        assert_eq!(extension_to_language("txt"), None);
    }

    #[test]
    fn detect_languages_from_files_mixed() {
        let files = vec![
            "src/main.rs".to_string(),
            "src/app.ts".to_string(),
            "README.md".to_string(),
        ];
        let langs = detect_languages_from_files(&files);
        assert!(langs.contains(&"rust"));
        assert!(langs.contains(&"typescript"));
        assert_eq!(langs.len(), 2);
    }

    #[test]
    fn detect_project_type_rust() {
        let dir = std::env::temp_dir().join("ccore_test_project_rust");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("Cargo.toml"), "[package]");
        assert_eq!(detect_project_type(&dir), ProjectType::Rust);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_project_type_typescript() {
        let dir = std::env::temp_dir().join("ccore_test_project_ts");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("package.json"), "{}");
        assert_eq!(detect_project_type(&dir), ProjectType::TypeScript);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_project_type_unknown() {
        let dir = std::env::temp_dir().join("ccore_test_project_unknown");
        let _ = std::fs::create_dir_all(&dir);
        assert_eq!(detect_project_type(&dir), ProjectType::Unknown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_tsc_line_error() {
        let line = "src/foo.ts(10,5): error TS2304: Cannot find name 'x'.";
        let entry = parse_tsc_line(line).unwrap();
        assert_eq!(entry.file, "src/foo.ts");
        assert_eq!(entry.line, 10);
        assert_eq!(entry.column, 5);
        assert_eq!(entry.level, "error");
        assert!(entry.message.contains("Cannot find name 'x'"));
    }

    #[test]
    fn parse_tsc_line_warning() {
        let line = "src/bar.ts(1,1): warning TS6133: 'x' is declared but never used.";
        let entry = parse_tsc_line(line).unwrap();
        assert_eq!(entry.level, "warning");
    }

    #[test]
    fn parse_go_error_line() {
        let line = "./main.go:10:5: undefined: x";
        let entry = parse_go_error(line).unwrap();
        assert_eq!(entry.file, "./main.go");
        assert_eq!(entry.line, 10);
        assert_eq!(entry.column, 5);
        assert_eq!(entry.message, "undefined: x");
    }

    #[test]
    fn parse_go_error_invalid() {
        assert!(parse_go_error("not an error").is_none());
    }

    #[test]
    fn parse_python_syntax_error_basic() {
        let stderr = "  File \"test.py\", line 10\n    bad syntax\nSyntaxError: invalid syntax";
        let entry = parse_python_syntax_error(stderr, "test.py").unwrap();
        assert_eq!(entry.file, "test.py");
        assert_eq!(entry.line, 10);
        assert!(entry.message.contains("SyntaxError"));
    }
}
