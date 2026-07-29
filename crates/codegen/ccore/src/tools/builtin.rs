//! 内置工具执行器 - 核心工具的实际执行逻辑
//!
//! 实现了 ccode 运行所需的最小工具集：
//! - bash: 执行 shell 命令（含安全预检查、命令缓存）
//! - read: 读取文件（含分页、过滤、安全校验、读取追踪）
//! - write: 写入文件（含原子写入、验证、先读后写约束）
//! - edit: 编辑文件（支持 replace/insert/write 模式，原子写入、diff 展示）
//! - grep: 搜索文件内容（含类型过滤、上下文、回退）
//! - glob: 搜索文件名（含排除、排序）
//! - list_dir: 列出目录内容（含递归、隐藏文件、类型标记）
//! - web_search: 网页搜索（DuckDuckGo API）
//! - web_fetch: 网页抓取（HTML→文本转换）

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use super::bridge::ToolExecutor;
use super::output_formatter;
use super::path_validator;
use super::read_tracker;

// ─── Bash 命令缓存（借鉴 Claude Code 命令缓存）──────────────────────────────

/// Bash 命令缓存
static BASH_CACHE: std::sync::OnceLock<Mutex<HashMap<String, CachedBashResult>>> = std::sync::OnceLock::new();

/// 缓存的命令结果
#[derive(Clone)]
struct CachedBashResult {
    stdout: String,
    exit_code: i32,
    timestamp: std::time::Instant,
}

/// 判断命令是否为只读命令（可缓存）
fn is_readonly_command(command: &str) -> bool {
    let readonly_prefixes = [
        "ls", "cat", "head", "tail", "grep", "find", "wc",
        "echo", "pwd", "which", "type", "git status", "git diff", "git log",
        "git branch", "cargo check", "cargo test", "cargo build", "npm list",
        "pip list", "python --version", "node --version", "rustc --version",
    ];
    let cmd_with_args = command.trim();
    readonly_prefixes.iter().any(|prefix| cmd_with_args.starts_with(prefix))
}

// ─── BashExecutor ────────────────────────────────────────────────────────────

pub struct BashExecutor;

/// 危险命令关键字（拒绝执行）
const DANGEROUS_COMMANDS: &[&str] = &[
    "rm -rf /",
    "rm -rf ~",
    "rm -rf /*",
    "mkfs",
    "dd if=",
    "> /dev/sd",
    "chmod 777 /",
    "chmod -R 777 /",
    "shutdown",
    "reboot",
    "init 0",
    "init 6",
    ":(){:|:&};:",  // fork bomb
    "mv / ",
];

impl BashExecutor {
    fn get_cache() -> &'static Mutex<HashMap<String, CachedBashResult>> {
        BASH_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// 检查缓存
    fn check_cache(command: &str, working_dir: Option<&str>) -> Option<String> {
        let cache = Self::get_cache();
        if let Ok(cache) = cache.lock() {
            let key = format!("{}:{}", working_dir.unwrap_or(""), command);
            if let Some(cached) = cache.get(&key) {
                // 缓存 5 分钟有效
                if cached.timestamp.elapsed().as_secs() < 300 {
                    let mut result = cached.stdout.clone();
                    if cached.exit_code != 0 {
                        result.push_str(&format!("\n[exit code: {}] (cached)", cached.exit_code));
                    }
                    result.push_str("\n(cached result)");
                    return Some(result);
                }
            }
        }
        None
    }

    /// 存入缓存
    fn store_cache(command: &str, working_dir: Option<&str>, stdout: &str, exit_code: i32) {
        let cache = Self::get_cache();
        if let Ok(mut cache) = cache.lock() {
            let key = format!("{}:{}", working_dir.unwrap_or(""), command);
            cache.insert(key, CachedBashResult {
                stdout: stdout.to_string(),
                exit_code,
                timestamp: std::time::Instant::now(),
            });
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for BashExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let command = args["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("bash: 缺少 command 参数"))?;

        // 安全预检查：检测危险命令
        let command_lower = command.to_lowercase();
        for dangerous in DANGEROUS_COMMANDS {
            if command_lower.contains(&dangerous.to_lowercase()) {
                return Ok(format!(
                    "错误：命令被安全策略拒绝\n  命令包含危险模式: '{}'\n  如确需执行，请手动操作",
                    dangerous
                ));
            }
        }

        let timeout_secs = args["timeout"].as_u64().unwrap_or(120);
        let working_dir = args["working_dir"].as_str();

        // 检查缓存（只读命令才缓存）
        if is_readonly_command(command) {
            if let Some(cached) = Self::check_cache(command, working_dir) {
                return Ok(cached);
            }
        }

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
                if !output.status.success() {
                    result.push_str(&format!("\n[exit code: {}]", output.status.code().unwrap_or(-1)));
                }

                // 缓存只读命令结果
                if is_readonly_command(command) && output.status.success() {
                    Self::store_cache(command, working_dir, &stdout, output.status.code().unwrap_or(-1));
                }

                // 输出截断
                Ok(output_formatter::truncate_output(&result, 50_000))
            }
            Ok(Err(e)) => Err(anyhow::anyhow!("bash 执行失败：{}", e)),
            Err(_) => Err(anyhow::anyhow!(
                "bash 执行超时（{}秒），建议增加 timeout 参数或使用后台执行",
                timeout_secs
            )),
        }
    }

    fn name(&self) -> &str { "bash" }
}

// ─── ReadExecutor ────────────────────────────────────────────────────────────

pub struct ReadExecutor;

#[async_trait::async_trait]
impl ToolExecutor for ReadExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("read: 缺少 path 参数"))?;

        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
        let limit = args["limit"].as_u64().unwrap_or(2000) as usize;
        let show_line_numbers = args["show_line_numbers"].as_bool().unwrap_or(true);

        // 路径安全校验
        let workspace = std::env::current_dir().unwrap_or_default();
        let validation = path_validator::validate_path(path, &workspace);

        if !validation.in_workspace {
            return Err(anyhow::anyhow!(
                "read: 路径 {} 在工作目录外，操作被拒绝", path
            ));
        }

        if validation.is_binary {
            return Err(anyhow::anyhow!(
                "read: {} 是二进制文件，无法以文本方式读取\n  文件大小: 请使用 bash 工具查看",
                path
            ));
        }

        if validation.is_sensitive {
            return Err(anyhow::anyhow!(
                "read: {} 可能包含敏感信息，读取被拒绝\n  如确需读取，请使用 bash: cat {}",
                path, path
            ));
        }

        // 读取文件
        let content = tokio::fs::read_to_string(&validation.canonical).await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => anyhow::anyhow!("read: 文件 {} 不存在", path),
                std::io::ErrorKind::PermissionDenied => anyhow::anyhow!("read: 无权限读取 {}", path),
                std::io::ErrorKind::IsADirectory => anyhow::anyhow!("read: {} 是目录，请使用 list_dir", path),
                _ => anyhow::anyhow!("read: 读取 {} 失败：{}", path, e),
            })?;

        // 记录文件已被读取（先读后写约束）
        read_tracker::mark_file_read(path);

        // 文件大小检查
        let total_lines = content.lines().count();
        let metadata = tokio::fs::metadata(&validation.canonical).await;
        let file_size = metadata.map(|m| m.len()).unwrap_or(0);

        if file_size > 1_048_576 {  // > 1MB
            if offset == 0 && limit == 2000 {
                return Ok(format!(
                    "read: 文件较大 ({:.1} MB, {} 行)\n  建议使用 offset 和 limit 参数分页读取\n  示例: read(path=\"{}\", offset=1, limit=100)",
                    file_size as f64 / (1024.0 * 1024.0),
                    total_lines,
                    path
                ));
            }
        }

        // 分页读取
        let lines: Vec<&str> = content.lines().collect();
        let start = offset.max(1).min(lines.len() + 1) - 1;  // 1-based → 0-based
        let end = (start + limit).min(lines.len());
        let selected_lines = &lines[start..end];
        let content_slice = selected_lines.join("\n");

        // 格式化输出
        let mut result = String::new();

        // 文件头
        result.push_str(&output_formatter::format_file_header(path, file_size));

        // 分页信息
        if offset > 0 || end < lines.len() {
            result.push_str(&format!("   行 {}-{} / {}\n", start + 1, end, total_lines));
        }
        result.push('\n');

        // 内容（带行号）
        if show_line_numbers {
            result.push_str(&output_formatter::format_with_line_numbers(&content_slice, start + 1));
        } else {
            result.push_str(&content_slice);
        }

        Ok(result)
    }

    fn name(&self) -> &str { "read" }
}

// ─── WriteExecutor ───────────────────────────────────────────────────────────

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

        // 先读后写约束：检查文件是否已被读取
        // 如果文件已存在，必须先读取
        let workspace = std::env::current_dir().unwrap_or_default();
        let validation = path_validator::validate_path(path, &workspace);

        if !validation.in_workspace {
            return Err(anyhow::anyhow!(
                "write: 路径 {} 在工作目录外，操作被拒绝", path
            ));
        }

        // 如果文件已存在，检查是否已读取
        if validation.canonical.exists() {
            read_tracker::require_file_read(path)?;
        }

        // 二进制内容检测
        if content.contains('\0') {
            return Err(anyhow::anyhow!(
                "write: 内容包含 NUL 字节，可能是二进制数据\n  二进制文件请使用 bash: base64/xxd 等工具"
            ));
        }

        // 确保父目录存在
        if let Some(parent) = validation.canonical.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // 原子写入：先写临时文件，再 rename
        let tmp_path = validation.canonical.with_extension("tmp");
        tokio::fs::write(&tmp_path, content).await
            .map_err(|e| anyhow::anyhow!("write: 写入临时文件失败：{}", e))?;

        tokio::fs::rename(&tmp_path, &validation.canonical).await
            .map_err(|e| {
                // rename 失败时清理临时文件
                let _ = std::fs::remove_file(&tmp_path);
                anyhow::anyhow!("write: 重命名到目标路径失败：{}", e)
            })?;

        // 写入后验证
        let verified = tokio::fs::read_to_string(&validation.canonical).await
            .map_err(|e| anyhow::anyhow!("write: 验证写入失败：{}", e))?;

        if verified.len() != content.len() {
            return Err(anyhow::anyhow!(
                "write: 验证失败，写入 {} 字节但验证到 {} 字节",
                content.len(), verified.len()
            ));
        }

        Ok(format!("已写入 {} ({} bytes, 已验证)", path, content.len()))
    }

    fn name(&self) -> &str { "write" }
}

// ─── EditExecutor ────────────────────────────────────────────────────────────

pub struct EditExecutor;

struct EditContext {
    before: Vec<String>,
    edited: String,
    after: Vec<String>,
}

#[async_trait::async_trait]
impl ToolExecutor for EditExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("edit: 缺少 path 参数"))?;

        let mode = args["mode"].as_str().unwrap_or("replace");

        // 路径安全校验
        let workspace = std::env::current_dir().unwrap_or_default();
        let validation = path_validator::validate_path(path, &workspace);

        if !validation.in_workspace {
            return Err(anyhow::anyhow!(
                "edit: 路径 {} 在工作目录外，操作被拒绝", path
            ));
        }

        // 先读后写约束：编辑前必须先读取文件
        read_tracker::require_file_read(path)?;

        match mode {
            "replace" => self.execute_replace(args, &validation.canonical).await,
            "insert" => self.execute_insert(args, &validation.canonical).await,
            "write" => self.execute_write(args, &validation.canonical).await,
            _ => Err(anyhow::anyhow!("edit: 未知模式 '{}'，支持: replace/insert/write", mode)),
        }
    }

    fn name(&self) -> &str { "edit" }
}

impl EditExecutor {
    /// 替换模式（原有逻辑升级，含 diff 展示）
    async fn execute_replace(&self, args: &serde_json::Value, canonical: &Path) -> anyhow::Result<String> {
        let old_text = args["old_text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("edit: 缺少 old_text 参数"))?;
        let new_text = args["new_text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("edit: 缺少 new_text 参数"))?;

        let content = tokio::fs::read_to_string(canonical).await
            .map_err(|e| anyhow::anyhow!("edit: 读取 {} 失败：{}", canonical.display(), e))?;

        let replace_all = args["replace_all"].as_bool().unwrap_or(false);

        let (new_content, _match_count) = if replace_all {
            let count = content.matches(old_text).count();
            if count == 0 {
                return Err(anyhow::anyhow!("edit: 未找到搜索文本"));
            }
            (content.replace(old_text, new_text), count)
        } else {
            let count = content.matches(old_text).count();
            if count == 0 {
                return Err(anyhow::anyhow!("edit: 未找到搜索文本"));
            }
            if count > 1 {
                return Err(anyhow::anyhow!(
                    "edit: 找到 {} 处匹配，请提供更具体的上下文或设置 replace_all=true",
                    count
                ));
            }
            (content.replacen(old_text, new_text, 1), 1)
        };

        // 生成 diff（借鉴 Claude Code 编辑后的 diff 展示）
        let diff = output_formatter::generate_unified_diff(
            &content, &new_content, &canonical.display().to_string(), 3,
        );

        // 原子写入
        self.atomic_write(canonical, &new_content).await?;

        // 找到编辑点并展示上下文
        let edit_line = self.find_edit_line(&content, old_text);
        let context = self.get_edit_context(&new_content, edit_line, 3);

        let edit_result = output_formatter::format_edit_result(
            &canonical.display().to_string(),
            edit_line,
            &context.before.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            &context.edited,
            &context.after.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        );

        Ok(format!("{}\n\nDiff:\n{}", edit_result, diff))
    }

    /// 插入模式
    async fn execute_insert(&self, args: &serde_json::Value, canonical: &Path) -> anyhow::Result<String> {
        let after_line = args["after_line"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("edit(insert): 缺少 after_line 参数"))? as usize;
        let content = args["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("edit(insert): 缺少 content 参数"))?;

        let file_content = tokio::fs::read_to_string(canonical).await
            .map_err(|e| anyhow::anyhow!("edit: 读取 {} 失败：{}", canonical.display(), e))?;

        let lines: Vec<&str> = file_content.lines().collect();
        if after_line > lines.len() {
            return Err(anyhow::anyhow!(
                "edit: after_line {} 超出文件行数 {}",
                after_line, lines.len()
            ));
        }

        let mut new_lines = lines[..after_line].to_vec();
        for line in content.lines() {
            new_lines.push(line);
        }
        new_lines.extend_from_slice(&lines[after_line..]);

        let new_content = new_lines.join("\n");

        // 生成 diff
        let diff = output_formatter::generate_unified_diff(
            &file_content, &new_content, &canonical.display().to_string(), 3,
        );

        self.atomic_write(canonical, &new_content).await?;

        Ok(format!("已插入到 {} 后（{} 行）\n\nDiff:\n{}", after_line, content.lines().count(), diff))
    }

    /// 完全写入模式
    async fn execute_write(&self, args: &serde_json::Value, canonical: &Path) -> anyhow::Result<String> {
        let content = args["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("edit(write): 缺少 content 参数"))?;

        self.atomic_write(canonical, content).await?;

        Ok(format!("已覆盖写入 {} ({} bytes)", canonical.display(), content.len()))
    }

    /// 原子写入
    async fn atomic_write(&self, path: &Path, content: &str) -> anyhow::Result<()> {
        let tmp_path = path.with_extension("tmp");
        tokio::fs::write(&tmp_path, content).await
            .map_err(|e| anyhow::anyhow!("写入临时文件失败：{}", e))?;
        tokio::fs::rename(&tmp_path, path).await
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                anyhow::anyhow!("重命名失败：{}", e)
            })?;
        Ok(())
    }

    /// 找到编辑点行号
    fn find_edit_line(&self, content: &str, old_text: &str) -> usize {
        for (i, line) in content.lines().enumerate() {
            if line.contains(old_text) || old_text.lines().any(|l| line.contains(l)) {
                return i + 1;
            }
        }
        1
    }

    /// 获取编辑点上下文
    fn get_edit_context(&self, content: &str, edit_line: usize, context_size: usize) -> EditContext {
        let lines: Vec<&str> = content.lines().collect();
        let edit_idx = edit_line.saturating_sub(1);

        let before_start = edit_idx.saturating_sub(context_size);
        let before: Vec<String> = lines[before_start..edit_idx].iter().map(|s| s.to_string()).collect();

        let edited = lines.get(edit_idx).unwrap_or(&"").to_string();

        let after_end = (edit_idx + context_size + 1).min(lines.len());
        let after: Vec<String> = lines[edit_idx + 1..after_end].iter().map(|s| s.to_string()).collect();

        EditContext { before, edited, after }
    }
}

// ─── GrepExecutor ────────────────────────────────────────────────────────────

pub struct GrepExecutor;

#[async_trait::async_trait]
impl ToolExecutor for GrepExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("grep: 缺少 pattern 参数"))?;
        let path = args["path"].as_str().unwrap_or(".");
        let case_insensitive = args["case_insensitive"].as_bool().unwrap_or(false);
        let context_lines = args["context"].as_u64().unwrap_or(0);
        let file_type = args["file_type"].as_str();
        let include = args["include"].as_str();
        let exclude = args["exclude"].as_str();

        let mut cmd = tokio::process::Command::new("rg");
        cmd.arg("--line-number")
            .arg("--max-count").arg("200");

        if case_insensitive {
            cmd.arg("-i");
        }

        if context_lines > 0 {
            cmd.arg("-C").arg(context_lines.to_string());
        }

        // 类型过滤
        if let Some(ft) = file_type {
            cmd.arg("--type").arg(ft);
        }

        // 包含/排除模式
        if let Some(inc) = include {
            cmd.arg("--glob").arg(inc);
        }
        if let Some(exc) = exclude {
            cmd.arg("--glob").arg(format!("!{}", exc));
        }

        cmd.arg(pattern).arg(path);

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let output = cmd.output().await;

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if stdout.is_empty() {
                    Ok("未找到匹配结果".into())
                } else {
                    // 统计匹配
                    let match_count = stdout.lines().count();
                    let file_count = stdout.lines()
                        .filter_map(|l| l.split(':').next())
                        .collect::<std::collections::HashSet<_>>()
                        .len();

                    let mut result = format!("在 {} 个文件中找到 {} 处匹配\n\n", file_count, match_count);
                    result.push_str(&output_formatter::truncate_lines(&stdout, 200));
                    Ok(result)
                }
            }
            Err(e) => {
                // rg 不存在，回退到简单文本搜索
                if e.kind() == std::io::ErrorKind::NotFound {
                    self.fallback_grep(pattern, path, case_insensitive).await
                } else {
                    Err(anyhow::anyhow!("grep: 执行失败：{}", e))
                }
            }
        }
    }

    fn name(&self) -> &str { "grep" }
}

impl GrepExecutor {
    /// 简单文本搜索回退（当 ripgrep 不可用时）
    async fn fallback_grep(&self, pattern: &str, path: &str, case_insensitive: bool) -> anyhow::Result<String> {
        let mut cmd = tokio::process::Command::new("grep");
        cmd.arg("-rn")
            .arg("--max-count=200");

        if case_insensitive {
            cmd.arg("-i");
        }

        cmd.arg(pattern).arg(path);
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let output = cmd.output().await
            .map_err(|e| anyhow::anyhow!("grep: 回退搜索也失败：{}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.is_empty() {
            Ok("未找到匹配结果".into())
        } else {
            Ok(output_formatter::truncate_lines(&stdout, 200))
        }
    }
}

// ─── GlobExecutor ────────────────────────────────────────────────────────────

pub struct GlobExecutor;

#[async_trait::async_trait]
impl ToolExecutor for GlobExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("glob: 缺少 pattern 参数"))?;
        let path = args["path"].as_str().unwrap_or(".");
        let exclude = args["exclude"].as_str();

        let mut cmd = tokio::process::Command::new("find");
        cmd.arg(path)
            .arg("-name")
            .arg(pattern)
            .arg("-type")
            .arg("f");

        // 排除模式
        if let Some(exc) = exclude {
            cmd.arg("-not")
                .arg("-path")
                .arg(exc);
        }

        // 排除常见忽略目录
        cmd.arg("-not")
            .arg("-path")
            .arg("*/.git/*")
            .arg("-not")
            .arg("-path")
            .arg("*/node_modules/*")
            .arg("-not")
            .arg("-path")
            .arg("*/target/*");

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let output = cmd.output().await
            .map_err(|e| anyhow::anyhow!("glob: 执行失败：{}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.is_empty() {
            Ok("未找到匹配文件".into())
        } else {
            let mut lines: Vec<&str> = stdout.lines().take(200).collect();
            lines.sort();
            let count = lines.len();
            let mut result = format!("找到 {} 个文件\n\n", count);
            result.push_str(&lines.join("\n"));
            Ok(result)
        }
    }

    fn name(&self) -> &str { "glob" }
}

// ─── ListDirExecutor ─────────────────────────────────────────────────────────

pub struct ListDirExecutor;

#[async_trait::async_trait]
impl ToolExecutor for ListDirExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let path = args["path"].as_str().unwrap_or(".");
        let recursive = args["recursive"].as_bool().unwrap_or(false);
        let show_hidden = args["show_hidden"].as_bool().unwrap_or(false);
        let max_depth = args["max_depth"].as_u64().unwrap_or(if recursive { 3 } else { 1 }) as usize;

        let mut entries = Vec::new();
        self.list_dir_recursive(path, 0, max_depth, show_hidden, &mut entries).await?;

        // 排序：目录在前，文件在后
        entries.sort_by(|a, b| {
            let a_is_dir = a.ends_with('/');
            let b_is_dir = b.ends_with('/');
            match (a_is_dir, b_is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.cmp(b),
            }
        });

        if entries.is_empty() {
            Ok("(空目录)".into())
        } else {
            Ok(entries.join("\n"))
        }
    }

    fn name(&self) -> &str { "list_dir" }
}

impl ListDirExecutor {
    async fn list_dir_recursive(
        &self,
        path: &str,
        depth: usize,
        max_depth: usize,
        show_hidden: bool,
        entries: &mut Vec<String>,
    ) -> anyhow::Result<()> {
        if depth >= max_depth {
            return Ok(());
        }

        let mut dir_entries = tokio::fs::read_dir(path).await
            .map_err(|e| anyhow::anyhow!("list_dir: 读取 {} 失败：{}", path, e))?;

        let mut items = Vec::new();
        while let Some(entry) = dir_entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();

            // 隐藏文件过滤
            if !show_hidden && name.starts_with('.') {
                continue;
            }

            let meta = entry.metadata().await?;
            let type_indicator = if meta.is_dir() {
                "/"
            } else {
                ""
            };

            let size = if meta.is_file() {
                format!(" ({} bytes)", meta.len())
            } else {
                String::new()
            };

            let indent = "  ".repeat(depth);
            items.push(format!("{}{}{}{}", indent, name, type_indicator, size));

            // 递归处理子目录
            if meta.is_dir() && depth + 1 < max_depth && !name.starts_with('.') {
                let sub_path = format!("{}/{}", path, name);
                Box::pin(self.list_dir_recursive(&sub_path, depth + 1, max_depth, show_hidden, entries)).await?;
            }
        }

        items.sort();
        entries.extend(items);
        Ok(())
    }
}

// ─── WebSearchExecutor ───────────────────────────────────────────────────────

/// WebSearch 工具执行器（借鉴 Claude Code WebSearchTool）
pub struct WebSearchExecutor;

#[async_trait::async_trait]
impl ToolExecutor for WebSearchExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("web_search: 缺少 query 参数"))?;

        let client = reqwest::Client::new();

        // 使用 DuckDuckGo Instant Answer API
        let url = format!(
            "https://api.duckduckgo.com/?q={}&format=json&no_html=1",
            urlencoding::encode(query)
        );

        let response = client.get(&url)
            .timeout(std::time::Duration::from_secs(30))
            .header("User-Agent", "Mozilla/5.0 (compatible; ccore/1.0)")
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("web_search: 请求失败：{}", e))?;

        let body: serde_json::Value = response.json().await
            .map_err(|e| anyhow::anyhow!("web_search: 解析响应失败：{}", e))?;

        // 提取搜索结果
        let mut results = String::new();

        if let Some(abstract_text) = body["AbstractText"].as_str() {
            if !abstract_text.is_empty() {
                results.push_str(&format!("摘要: {}\n", abstract_text));
            }
        }
        if let Some(abstract_url) = body["AbstractURL"].as_str() {
            if !abstract_url.is_empty() {
                results.push_str(&format!("来源: {}\n", abstract_url));
            }
        }

        // 相关主题
        if let Some(topics) = body["RelatedTopics"].as_array() {
            let valid_topics: Vec<&serde_json::Value> = topics
                .iter()
                .filter(|t| t["Text"].as_str().map(|s| !s.is_empty()).unwrap_or(false))
                .take(5)
                .collect();

            if !valid_topics.is_empty() {
                results.push_str("\n相关主题:\n");
                for (i, topic) in valid_topics.iter().enumerate() {
                    if let Some(text) = topic["Text"].as_str() {
                        results.push_str(&format!("{}. {}\n", i + 1, text));
                    }
                    if let Some(url) = topic["FirstURL"].as_str() {
                        results.push_str(&format!("   链接: {}\n", url));
                    }
                }
            }
        }

        // 结果链接
        if let Some(results_arr) = body["Results"].as_array() {
            if !results_arr.is_empty() {
                results.push_str("\n搜索结果:\n");
                for (i, r) in results_arr.iter().take(5).enumerate() {
                    if let Some(text) = r["Text"].as_str() {
                        results.push_str(&format!("{}. {}\n", i + 1, text));
                    }
                    if let Some(url) = r["FirstURL"].as_str() {
                        results.push_str(&format!("   链接: {}\n", url));
                    }
                }
            }
        }

        if results.is_empty() {
            results = format!("未找到 '{}' 的搜索结果", query);
        }

        Ok(results)
    }

    fn name(&self) -> &str { "web_search" }
}

// ─── WebFetchExecutor ────────────────────────────────────────────────────────

/// WebFetch 工具执行器（借鉴 Claude Code WebFetchTool）
pub struct WebFetchExecutor;

#[async_trait::async_trait]
impl ToolExecutor for WebFetchExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let url = args["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("web_fetch: 缺少 url 参数"))?;

        // URL 基本校验
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(anyhow::anyhow!("web_fetch: URL 必须以 http:// 或 https:// 开头"));
        }

        let client = reqwest::Client::new();
        let response = client.get(url)
            .timeout(std::time::Duration::from_secs(30))
            .header("User-Agent", "Mozilla/5.0 (compatible; ccore/1.0)")
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("web_fetch: 请求失败：{}", e))?;

        let content_type = response.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let text = response.text().await
            .map_err(|e| anyhow::anyhow!("web_fetch: 读取响应失败：{}", e))?;

        if content_type.contains("text/html") {
            let converted = html_to_text(&text);
            Ok(output_formatter::truncate_output(&converted, 50_000))
        } else {
            Ok(output_formatter::truncate_output(&text, 50_000))
        }
    }

    fn name(&self) -> &str { "web_fetch" }
}

/// 简单的 HTML → 文本转换（去除标签）
fn html_to_text(html: &str) -> String {
    let re_script = regex::Regex::new(r"(?s)<script[^>]*>.*?</script>").unwrap_or_else(|_| regex::Regex::new("").unwrap());
    let re_style = regex::Regex::new(r"(?s)<style[^>]*>.*?</style>").unwrap_or_else(|_| regex::Regex::new("").unwrap());
    let re_tag = regex::Regex::new(r"<[^>]+>").unwrap_or_else(|_| regex::Regex::new("").unwrap());
    let re_entity = regex::Regex::new(r"&nbsp;|&amp;|&lt;|&gt;|&quot;").unwrap_or_else(|_| regex::Regex::new("").unwrap());

    let text = re_script.replace_all(html, "").to_string();
    let text = re_style.replace_all(&text, "").to_string();
    let text = re_tag.replace_all(&text, "").to_string();
    let text = re_entity.replace_all(&text, " ").to_string();

    // 清理多余空白
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ─── 注册所有内置执行器 ──────────────────────────────────────────────────────

/// 注册所有内置工具执行器到 ToolBridge
pub fn register_builtin_executors(bridge: &mut super::bridge::ToolBridge) {
    bridge.register_executor(Box::new(BashExecutor));
    bridge.register_executor(Box::new(ReadExecutor));
    bridge.register_executor(Box::new(WriteExecutor));
    bridge.register_executor(Box::new(EditExecutor));
    bridge.register_executor(Box::new(GrepExecutor));
    bridge.register_executor(Box::new(GlobExecutor));
    bridge.register_executor(Box::new(ListDirExecutor));
    bridge.register_executor(Box::new(WebSearchExecutor));
    bridge.register_executor(Box::new(WebFetchExecutor));

    // 注册 post_hook：Write/Edit 后对 .rs 文件运行 rustfmt --check 做增量验证
    bridge.register_post_hook(Box::new(super::rustfmt_hook::RustfmtHook));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_readonly_command() {
        assert!(is_readonly_command("ls -la"));
        assert!(is_readonly_command("cat file.txt"));
        assert!(is_readonly_command("git status"));
        assert!(is_readonly_command("git diff HEAD~1"));
        assert!(is_readonly_command("cargo check"));
        assert!(is_readonly_command("pwd"));
        assert!(is_readonly_command("echo hello"));
        assert!(is_readonly_command("which python3"));
        assert!(!is_readonly_command("rm file.txt"));
        assert!(!is_readonly_command("npm install"));
        assert!(!is_readonly_command("cargo run"));
    }

    #[test]
    fn test_bash_cache_store_and_retrieve() {
        // 清空缓存
        let cache = BashExecutor::get_cache();
        if let Ok(mut c) = cache.lock() {
            c.clear();
        }

        BashExecutor::store_cache("ls -la", None, "file1\nfile2\n", 0);
        let cached = BashExecutor::check_cache("ls -la", None);
        assert!(cached.is_some());
        let result = cached.unwrap();
        assert!(result.contains("file1"));
        assert!(result.contains("cached result"));
    }

    #[test]
    fn test_bash_cache_not_found() {
        let cache = BashExecutor::get_cache();
        if let Ok(mut c) = cache.lock() {
            c.clear();
        }

        let cached = BashExecutor::check_cache("nonexistent_cmd", None);
        assert!(cached.is_none());
    }

    #[test]
    fn test_bash_cache_with_working_dir() {
        let cache = BashExecutor::get_cache();
        if let Ok(mut c) = cache.lock() {
            c.clear();
        }

        BashExecutor::store_cache("ls", Some("/home"), "files\n", 0);
        let cached = BashExecutor::check_cache("ls", Some("/home"));
        assert!(cached.is_some());

        // 不同工作目录应返回 None
        let cached = BashExecutor::check_cache("ls", Some("/tmp"));
        assert!(cached.is_none());
    }

    #[test]
    fn test_html_to_text() {
        let html = r#"<html><head><style>body{}</style></head><body><script>var x=1;</script><h1>Title</h1><p>Hello &amp; World</p></body></html>"#;
        let text = html_to_text(html);
        assert!(text.contains("Title"));
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
        assert!(!text.contains("<script>"));
        assert!(!text.contains("<style>"));
        assert!(!text.contains("<h1>"));
        assert!(!text.contains("<p>"));
    }

    #[test]
    fn test_html_to_text_entities() {
        let html = "<p>&nbsp;Hello &amp; &lt;World&gt; &quot;test&quot;</p>";
        let text = html_to_text(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
        assert!(text.contains("test"));
        assert!(!text.contains("&amp;"));
        assert!(!text.contains("&lt;"));
    }

    #[test]
    fn test_read_tracker_integration() {
        // 使用全局追踪器
        read_tracker::global_read_tracker().clear();

        // 未读取的文件应返回错误
        let result = read_tracker::require_file_read("/test/unread.rs");
        assert!(result.is_err());

        // 读取后应成功
        read_tracker::mark_file_read("/test/read.rs");
        let result = read_tracker::require_file_read("/test/read.rs");
        assert!(result.is_ok());

        // 清理
        read_tracker::global_read_tracker().clear();
    }
}
