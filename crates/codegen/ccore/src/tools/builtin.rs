//! 内置工具执行器 - 核心工具的实际执行逻辑
//!
//! 实现了 ccode 运行所需的最小工具集：
//! - bash: 执行 shell 命令
//! - read: 读取文件
//! - write: 写入文件
//! - edit: 编辑文件（搜索替换）
//! - grep: 搜索文件内容
//! - glob: 搜索文件名
//! - list_dir: 列出目录内容

use super::bridge::ToolExecutor;

/// Bash 工具执行器
pub struct BashExecutor;

#[async_trait::async_trait]
impl ToolExecutor for BashExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let command = args["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("bash: 缺少 command 参数"))?;

        let timeout_secs = args["timeout"]
            .as_u64()
            .unwrap_or(120);

        let working_dir = args["working_dir"].as_str();

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(command);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            cmd.output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let mut result = String::new();
                if !stdout.is_empty() {
                    result.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    result.push_str("[stderr]\n");
                    result.push_str(&stderr);
                }
                if output.status.success() {
                    Ok(result)
                } else {
                    Ok(format!(
                        "{}\n[exit code: {}]",
                        result,
                        output.status.code().unwrap_or(-1)
                    ))
                }
            }
            Ok(Err(e)) => Err(anyhow::anyhow!("bash 执行失败：{}", e)),
            Err(_) => Err(anyhow::anyhow!("bash 执行超时（{}秒）", timeout_secs)),
        }
    }

    fn name(&self) -> &str {
        "bash"
    }
}

/// Read 工具执行器
pub struct ReadExecutor;

#[async_trait::async_trait]
impl ToolExecutor for ReadExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("read: 缺少 path 参数"))?;

        let content = tokio::fs::read_to_string(path).await
            .map_err(|e| anyhow::anyhow!("read: 读取 {} 失败：{}", path, e))?;

        let mut result = String::new();
        for (i, line) in content.lines().enumerate() {
            result.push_str(&format!("{:>6}→{}\n", i + 1, line));
        }
        Ok(result)
    }

    fn name(&self) -> &str {
        "read"
    }
}

/// Write 工具执行器
pub struct WriteExecutor;

#[async_trait::async_trait]
impl ToolExecutor for WriteExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("write: 缺少 path 参数"))?;
        let content = args["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("write: 缺少 content 参数"))?;

        // 确保父目录存在
        if let Some(parent) = std::path::Path::new(path).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        tokio::fs::write(path, content).await
            .map_err(|e| anyhow::anyhow!("write: 写入 {} 失败：{}", path, e))?;

        Ok(format!("已写入 {} ({} bytes)", path, content.len()))
    }

    fn name(&self) -> &str {
        "write"
    }
}

/// Edit 工具执行器（搜索替换模式）
pub struct EditExecutor;

#[async_trait::async_trait]
impl ToolExecutor for EditExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("edit: 缺少 path 参数"))?;
        let old_text = args["old_text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("edit: 缺少 old_text 参数"))?;
        let new_text = args["new_text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("edit: 缺少 new_text 参数"))?;

        let content = tokio::fs::read_to_string(path).await
            .map_err(|e| anyhow::anyhow!("edit: 读取 {} 失败：{}", path, e))?;

        let replace_all = args["replace_all"].as_bool().unwrap_or(false);

        let new_content = if replace_all {
            if !content.contains(old_text) {
                return Err(anyhow::anyhow!(
                    "edit: 在 {} 中未找到搜索文本", path
                ));
            }
            content.replace(old_text, new_text)
        } else {
            let count = content.matches(old_text).count();
            if count == 0 {
                return Err(anyhow::anyhow!(
                    "edit: 在 {} 中未找到搜索文本", path
                ));
            }
            if count > 1 {
                return Err(anyhow::anyhow!(
                    "edit: 在 {} 中找到 {} 处匹配，请提供更具体的上下文或设置 replace_all=true",
                    path, count
                ));
            }
            content.replacen(old_text, new_text, 1)
        };

        tokio::fs::write(path, &new_content).await
            .map_err(|e| anyhow::anyhow!("edit: 写入 {} 失败：{}", path, e))?;

        Ok(format!("已编辑 {} (替换了 1 处)", path))
    }

    fn name(&self) -> &str {
        "edit"
    }
}

/// Grep 工具执行器
pub struct GrepExecutor;

#[async_trait::async_trait]
impl ToolExecutor for GrepExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("grep: 缺少 pattern 参数"))?;
        let path = args["path"].as_str().unwrap_or(".");
        let case_insensitive = args["case_insensitive"].as_bool().unwrap_or(false);

        let mut cmd = tokio::process::Command::new("rg");
        cmd.arg("--line-number")
            .arg("--max-count")
            .arg("200");

        if case_insensitive {
            cmd.arg("-i");
        }

        cmd.arg(pattern).arg(path);

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let output = cmd.output().await
            .map_err(|e| anyhow::anyhow!("grep: 执行失败：{}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.is_empty() {
            Ok("未找到匹配结果".into())
        } else {
            // 限制输出长度
            let lines: Vec<&str> = stdout.lines().take(200).collect();
            Ok(lines.join("\n"))
        }
    }

    fn name(&self) -> &str {
        "grep"
    }
}

/// Glob 工具执行器
pub struct GlobExecutor;

#[async_trait::async_trait]
impl ToolExecutor for GlobExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("glob: 缺少 pattern 参数"))?;
        let path = args["path"].as_str().unwrap_or(".");

        let mut cmd = tokio::process::Command::new("find");
        cmd.arg(path)
            .arg("-name")
            .arg(pattern)
            .arg("-type")
            .arg("f");

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let output = cmd.output().await
            .map_err(|e| anyhow::anyhow!("glob: 执行失败：{}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.is_empty() {
            Ok("未找到匹配文件".into())
        } else {
            let lines: Vec<&str> = stdout.lines().take(200).collect();
            Ok(lines.join("\n"))
        }
    }

    fn name(&self) -> &str {
        "glob"
    }
}

/// ListDir 工具执行器
pub struct ListDirExecutor;

#[async_trait::async_trait]
impl ToolExecutor for ListDirExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let path = args["path"].as_str().unwrap_or(".");

        let mut entries = tokio::fs::read_dir(path).await
            .map_err(|e| anyhow::anyhow!("list_dir: 读取 {} 失败：{}", path, e))?;

        let mut result = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            let meta = entry.metadata().await?;
            let type_indicator = if meta.is_dir() { "/" } else { "" };
            let size = if meta.is_file() {
                format!(" ({} bytes)", meta.len())
            } else {
                String::new()
            };
            result.push(format!("{}{}{}", name, type_indicator, size));
        }

        result.sort();
        if result.is_empty() {
            Ok("(空目录)".into())
        } else {
            Ok(result.join("\n"))
        }
    }

    fn name(&self) -> &str {
        "list_dir"
    }
}

/// 注册所有内置工具执行器到 ToolBridge
pub fn register_builtin_executors(bridge: &mut super::bridge::ToolBridge) {
    bridge.register_executor(Box::new(BashExecutor));
    bridge.register_executor(Box::new(ReadExecutor));
    bridge.register_executor(Box::new(WriteExecutor));
    bridge.register_executor(Box::new(EditExecutor));
    bridge.register_executor(Box::new(GrepExecutor));
    bridge.register_executor(Box::new(GlobExecutor));
    bridge.register_executor(Box::new(ListDirExecutor));

    // 注册 post_hook：Write/Edit 后对 .rs 文件运行 rustfmt --check 做增量验证
    bridge.register_post_hook(Box::new(super::rustfmt_hook::RustfmtHook));
}
