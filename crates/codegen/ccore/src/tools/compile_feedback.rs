//! 编译反馈模块 - 解析 cargo check 输出，格式化为可注入 Agent 上下文的错误信息
//!
//! Agent 在改完代码后通过本模块获取结构化编译反馈：
//! - `run_cargo_check` 调用 cargo check --message-format=json，逐行解析 JSON 提取编译器消息
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
}
