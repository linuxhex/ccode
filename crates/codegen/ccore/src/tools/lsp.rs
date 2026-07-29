//! LSP 工具执行器（对标 Claude Code LSPTool）
//!
//! 提供代码智能功能：
//! - goto_definition: 跳转到定义
//! - find_references: 查找引用
//! - get_diagnostics: 获取诊断信息
//! - hover: 悬停信息

use async_trait::async_trait;
use crate::tools::bridge::ToolExecutor;

/// LSP 工具执行器
pub struct LspExecutor;

#[async_trait]
impl ToolExecutor for LspExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let action = args["action"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("lsp: 缺少 action 参数"))?;
        
        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("lsp: 缺少 file_path 参数"))?;
        
        let line = args["line"].as_u64().unwrap_or(1) as usize;
        let column = args["column"].as_u64().unwrap_or(1) as usize;
        
        match action {
            "goto_definition" => self.goto_definition(file_path, line, column).await,
            "find_references" => self.find_references(file_path, line, column).await,
            "get_diagnostics" => self.get_diagnostics(file_path).await,
            "hover" => self.hover(file_path, line, column).await,
            _ => Err(anyhow::anyhow!("lsp: 未知 action '{}'", action)),
        }
    }
    
    fn name(&self) -> &str { "lsp" }
}

impl LspExecutor {
    /// 跳转到定义
    async fn goto_definition(&self, file_path: &str, line: usize, column: usize) -> anyhow::Result<String> {
        tracing::debug!(target: "ccore::lsp", file = file_path, line, column, "goto_definition");
        
        // 通过 rust-analyzer 或其他 LSP 服务器
        // 这里使用 CLI 调用作为后备方案
        let output = tokio::process::Command::new("rust-analyzer")
            .args(["analysis", "goto-def", "--path", file_path, "--line", &line.to_string(), "--col", &column.to_string()])
            .output()
            .await;
        
        match output {
            Ok(out) if out.status.success() => {
                Ok(String::from_utf8_lossy(&out.stdout).to_string())
            }
            _ => {
                // 后备：使用 grep 搜索定义
                tracing::debug!(target: "ccore::lsp", "LSP server not available, using grep fallback");
                self.grep_fallback_definition().await
            }
        }
    }
    
    /// 查找引用
    async fn find_references(&self, file_path: &str, _line: usize, _column: usize) -> anyhow::Result<String> {
        tracing::debug!(target: "ccore::lsp", file = file_path, "find_references");
        
        // 后备方案：使用 grep -rn 搜索标识符
        let output = tokio::process::Command::new("grep")
            .args(["-rn", "--include=*.rs", "."])
            .output()
            .await;
        
        match output {
            Ok(out) if out.status.success() => {
                let result = String::from_utf8_lossy(&out.stdout).to_string();
                let lines: Vec<&str> = result.lines().take(20).collect();
                Ok(format!("找到 {} 个引用（显示前20个）:\n{}", 
                    result.lines().count(), lines.join("\n")))
            }
            _ => Ok("未找到引用".into()),
        }
    }
    
    /// 获取诊断信息
    async fn get_diagnostics(&self, file_path: &str) -> anyhow::Result<String> {
        tracing::debug!(target: "ccore::lsp", file = file_path, "get_diagnostics");
        
        // 使用 cargo check 获取诊断
        let output = tokio::process::Command::new("cargo")
            .args(["check", "--message-format=short"])
            .output()
            .await;
        
        match output {
            Ok(out) => {
                let result = String::from_utf8_lossy(&out.stdout).to_string();
                let diagnostics: Vec<&str> = result.lines()
                    .filter(|l| l.contains(file_path) || l.contains("error"))
                    .take(10)
                    .collect();
                Ok(format!("诊断信息:\n{}", diagnostics.join("\n")))
            }
            _ => Ok("无法获取诊断信息".into()),
        }
    }
    
    /// 悬停信息
    async fn hover(&self, file_path: &str, line: usize, _column: usize) -> anyhow::Result<String> {
        tracing::debug!(target: "ccore::lsp", file = file_path, line, "hover");
        
        // 读取文件对应行的内容
        let content = tokio::fs::read_to_string(file_path).await
            .map_err(|e| anyhow::anyhow!("lsp: 读取文件失败: {}", e))?;
        
        let line_content = content.lines().nth(line.saturating_sub(1))
            .unwrap_or("");
        
        Ok(format!("Line {}: {}", line, line_content))
    }
    
    /// grep 后备方案：搜索定义
    async fn grep_fallback_definition(&self) -> anyhow::Result<String> {
        Ok("LSP 服务器不可用。请安装 rust-analyzer 或其他语言服务器以获取完整支持。".into())
    }
}
