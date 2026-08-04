//! MCP 工具注册表 — 管理可通过 MCP 协议调用的内置工具
//!
//! 注册 ccode 的 12 个内置工具（read/write/edit/bash/glob/grep
//! + todo_write/web_search/web_fetch/git_status/list_directory/create_file），
//! 每个工具的调用通过消息总线转发给 ToolNode 执行。

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;
use tokio::process::Command as TokioCommand;

/// 带超时异步执行命令
///
/// 使用 `tokio::process::Command` 异步执行，不阻塞 tokio runtime；
/// 超过指定秒数则取消任务并返回超时错误。
/// 通过 `kill_on_drop` 确保超时或 future 被丢弃时子进程被终止，避免进程泄漏。
async fn run_command_with_timeout(
    mut cmd: TokioCommand,
    timeout_secs: u64,
) -> Result<std::process::Output, String> {
    cmd.kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(timeout_secs), cmd.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(format!("执行失败：{}", e)),
        Err(_) => Err("命令超时".into()),
    }
}

/// MCP 工具定义
pub struct McpToolDef {
    /// 工具名称
    pub name: String,
    /// 工具描述
    pub description: String,
    /// 输入参数的 JSON Schema
    pub input_schema: Value,
}

/// MCP 工具注册表
///
/// 管理所有可通过 MCP 协议调用的工具，
/// 提供注册、查询和调用接口。
pub struct McpToolRegistry {
    /// 工具定义映射（name → McpToolDef）
    tools: HashMap<String, McpToolDef>,
}

impl McpToolRegistry {
    /// 创建工具注册表并注册所有内置工具
    pub fn new() -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
        };
        registry.register_builtin_tools();
        registry
    }

    /// 注册所有内置工具
    fn register_builtin_tools(&mut self) {
        self.register_read_tool();
        self.register_write_tool();
        self.register_edit_tool();
        self.register_bash_tool();
        self.register_glob_tool();
        self.register_grep_tool();
        self.register_todo_write_tool();
        self.register_web_search_tool();
        self.register_web_fetch_tool();
        self.register_git_status_tool();
        self.register_list_directory_tool();
        self.register_create_file_tool();
    }

    /// 获取所有已注册工具的信息（名称、描述、输入 Schema）
    pub fn list_tools(&self) -> Vec<ToolInfo> {
        self.tools
            .values()
            .map(|def| ToolInfo {
                name: def.name.clone(),
                description: def.description.clone(),
                input_schema: def.input_schema.clone(),
            })
            .collect()
    }

    /// 执行内置工具（直接异步执行，不经过消息总线）
    ///
    /// 支持 read/write/edit/bash/glob/grep/todo_write/web_search/web_fetch/git_status/list_directory/create_file 共 12 个内置工具。
    /// 对于 bash 工具，仅允许白名单命令以确保安全。
    pub async fn execute_tool(&self, name: &str, arguments: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
        match name {
            "read" => {
                let path = arguments["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 path 参数"))?;
                match std::fs::read_to_string(path) {
                    Ok(content) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": content}],
                        "isError": false
                    })),
                    Err(e) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": format!("读取失败：{}", e)}],
                        "isError": true
                    }))
                }
            }
            "write" => {
                let path = arguments["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 path 参数"))?;
                let content = arguments["content"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 content 参数"))?;
                match std::fs::write(path, content) {
                    Ok(()) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": "写入成功"}],
                        "isError": false
                    })),
                    Err(e) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": format!("写入失败：{}", e)}],
                        "isError": true
                    }))
                }
            }
            "edit" => {
                let path = arguments["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 path 参数"))?;
                let old = arguments["old_string"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 old_string 参数"))?;
                let new = arguments["new_string"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 new_string 参数"))?;
                match std::fs::read_to_string(path) {
                    Ok(content) => {
                        let new_content = content.replacen(old, new, 1);
                        if new_content == content {
                            Ok(serde_json::json!({
                                "content": [{"type": "text", "text": "未找到匹配文本"}],
                                "isError": true
                            }))
                        } else {
                            match std::fs::write(path, &new_content) {
                                Ok(()) => Ok(serde_json::json!({
                                    "content": [{"type": "text", "text": "编辑成功"}],
                                    "isError": false
                                })),
                                Err(e) => Ok(serde_json::json!({
                                    "content": [{"type": "text", "text": format!("写入失败：{}", e)}],
                                    "isError": true
                                }))
                            }
                        }
                    }
                    Err(e) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": format!("读取失败：{}", e)}],
                        "isError": true
                    }))
                }
            }
            "bash" => {
                let command = arguments["command"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 command 参数"))?;
                // 安全：仅允许白名单命令（严格匹配首个 token，禁止路径形式，避免 /bin/ls、./ls 等绕过）
                let allowed = ["ls", "cat", "pwd", "echo", "which", "head", "tail", "wc", "grep", "find", "git", "cargo", "rustc"];
                let cmd_name = command.split_whitespace().next().unwrap_or("");
                if cmd_name.is_empty()
                    || cmd_name.contains('/')
                    || cmd_name.contains('\\')
                    || !allowed.contains(&cmd_name)
                {
                    return Ok(serde_json::json!({
                        "content": [{"type": "text", "text": format!("命令 {} 不在白名单中", cmd_name)}],
                        "isError": true
                    }));
                }
                // 安全：拒绝 shell 元字符，防止 `;`/`|`/`&`/`$()`/反引号/重定向等命令拼接或替换绕过白名单
                const FORBIDDEN_CHARS: &[char] = &[';', '|', '&', '`', '$', '(', ')', '<', '>', '\n', '\r'];
                if command.chars().any(|c| FORBIDDEN_CHARS.contains(&c)) {
                    tracing::warn!("bash 工具拒绝含 shell 元字符的命令：{}", command);
                    return Ok(serde_json::json!({
                        "content": [{"type": "text", "text": "命令包含禁用的 shell 元字符"}],
                        "isError": true
                    }));
                }
                // 异步执行，不阻塞 tokio runtime
                let mut cmd = TokioCommand::new("sh");
                cmd.arg("-c").arg(command).kill_on_drop(true);
                match cmd.output().await {
                    Ok(out) => {
                        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                        let text = if stderr.is_empty() { stdout } else { format!("{}\n{}", stdout, stderr) };
                        Ok(serde_json::json!({
                            "content": [{"type": "text", "text": text}],
                            "isError": !out.status.success()
                        }))
                    }
                    Err(e) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": format!("执行失败：{}", e)}],
                        "isError": true
                    }))
                }
            }
            "glob" => {
                let pattern = arguments["pattern"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 pattern 参数"))?;
                let path = arguments["path"].as_str().unwrap_or(".");
                let mut cmd = TokioCommand::new("find");
                cmd.arg(path).arg("-name").arg(pattern).kill_on_drop(true);
                match cmd.output().await {
                    Ok(out) => {
                        let text = String::from_utf8_lossy(&out.stdout).to_string();
                        Ok(serde_json::json!({
                            "content": [{"type": "text", "text": text}],
                            "isError": false
                        }))
                    }
                    Err(e) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": format!("查找失败：{}", e)}],
                        "isError": true
                    }))
                }
            }
            "grep" => {
                let pattern = arguments["pattern"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 pattern 参数"))?;
                let path = arguments["path"].as_str().unwrap_or(".");
                let mut cmd = TokioCommand::new("grep");
                cmd.arg("-r").arg(pattern).arg(path).kill_on_drop(true);
                match cmd.output().await {
                    Ok(out) => {
                        let text = String::from_utf8_lossy(&out.stdout).to_string();
                        Ok(serde_json::json!({
                            "content": [{"type": "text", "text": text}],
                            "isError": false
                        }))
                    }
                    Err(e) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": format!("搜索失败：{}", e)}],
                        "isError": true
                    }))
                }
            }
            "todo_write" => {
                let todos = &arguments["todos"];
                if todos.is_null() {
                    return Ok(serde_json::json!({
                        "content": [{"type": "text", "text": "缺少 todos 参数"}],
                        "isError": true
                    }));
                }
                let json = serde_json::to_string_pretty(todos)
                    .map_err(|e| anyhow::anyhow!("序列化失败：{}", e))?;
                match std::fs::write("/tmp/ccode_todos.json", &json) {
                    Ok(()) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": "Todo 列表已写入 /tmp/ccode_todos.json"}],
                        "isError": false
                    })),
                    Err(e) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": format!("写入失败：{}", e)}],
                        "isError": true
                    }))
                }
            }
            "web_search" => {
                let query = arguments["query"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 query 参数"))?;
                // 安全：参数化执行 curl，不经过 shell，杜绝命令注入；
                // query 做 URL 编码避免构造异常请求；`--` 终止 curl 选项解析，url 仅作位置参数
                let encoded = urlencoding::encode(query);
                let url = format!("https://api.duckduckgo.com/?q={}&format=json", encoded);
                let mut cmd = TokioCommand::new("curl");
                cmd.arg("-s").arg("--").arg(&url).kill_on_drop(true);
                match run_command_with_timeout(cmd, 5).await {
                    Ok(out) => {
                        // 截断到 2000 字节，等价于原 `| head -c 2000`（按字节截断，from_utf8_lossy 容错）
                        let mut bytes = out.stdout;
                        if bytes.len() > 2000 {
                            bytes.truncate(2000);
                        }
                        let text = String::from_utf8_lossy(&bytes).to_string();
                        Ok(serde_json::json!({
                            "content": [{"type": "text", "text": text}],
                            "isError": !out.status.success()
                        }))
                    }
                    Err(e) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": format!("搜索失败：{}", e)}],
                        "isError": true
                    }))
                }
            }
            "web_fetch" => {
                let url = arguments["url"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 url 参数"))?;
                // 安全：参数化执行 curl，不经过 shell，杜绝命令注入；
                // `--` 终止 curl 选项解析，避免 url 被当作 curl 参数（如 --output）注入
                let mut cmd = TokioCommand::new("curl");
                cmd.arg("-sL").arg("--").arg(url).kill_on_drop(true);
                match run_command_with_timeout(cmd, 5).await {
                    Ok(out) => {
                        // 截断到 10000 字节，等价于原 `| head -c 10000`
                        let mut bytes = out.stdout;
                        if bytes.len() > 10000 {
                            bytes.truncate(10000);
                        }
                        let text = String::from_utf8_lossy(&bytes).to_string();
                        Ok(serde_json::json!({
                            "content": [{"type": "text", "text": text}],
                            "isError": !out.status.success()
                        }))
                    }
                    Err(e) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": format!("获取失败：{}", e)}],
                        "isError": true
                    }))
                }
            }
            "git_status" => {
                let path = arguments["path"].as_str().unwrap_or(".");
                // 安全：参数化执行 git，不经过 shell，path 作为独立参数传递，杜绝命令注入
                let mut cmd = TokioCommand::new("git");
                cmd.arg("-C").arg(path).arg("status").arg("--short").kill_on_drop(true);
                match run_command_with_timeout(cmd, 5).await {
                    Ok(out) => {
                        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                        let text = if stderr.is_empty() { stdout } else { format!("{}\n{}", stdout, stderr) };
                        Ok(serde_json::json!({
                            "content": [{"type": "text", "text": text}],
                            "isError": !out.status.success()
                        }))
                    }
                    Err(e) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": format!("执行失败：{}", e)}],
                        "isError": true
                    }))
                }
            }
            "list_directory" => {
                let path = arguments["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 path 参数"))?;
                match std::fs::read_dir(path) {
                    Ok(entries) => {
                        let names: Vec<String> = entries
                            .filter_map(|e| e.ok())
                            .map(|e| e.file_name().to_string_lossy().into_owned())
                            .collect();
                        let text = names.join("\n");
                        Ok(serde_json::json!({
                            "content": [{"type": "text", "text": text}],
                            "isError": false
                        }))
                    }
                    Err(e) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": format!("读取目录失败：{}", e)}],
                        "isError": true
                    }))
                }
            }
            "create_file" => {
                let path = arguments["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 path 参数"))?;
                let content = arguments["content"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 content 参数"))?;
                if std::path::Path::new(path).exists() {
                    return Ok(serde_json::json!({
                        "content": [{"type": "text", "text": format!("文件已存在：{}", path)}],
                        "isError": true
                    }));
                }
                match std::fs::write(path, content) {
                    Ok(()) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": "文件创建成功"}],
                        "isError": false
                    })),
                    Err(e) => Ok(serde_json::json!({
                        "content": [{"type": "text", "text": format!("创建失败：{}", e)}],
                        "isError": true
                    }))
                }
            }
            _ => Err(anyhow::anyhow!("未知工具：{}", name))
        }
    }

    // ---- 内置工具注册 ----

    /// 注册 read 工具 — 读取文件内容
    fn register_read_tool(&mut self) {
        self.tools.insert(
            "read".into(),
            McpToolDef {
                name: "read".into(),
                description: "读取文件内容".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "文件路径",
                        },
                    },
                    "required": ["path"],
                }),
            },
        );
    }

    /// 注册 write 工具 — 写入文件
    fn register_write_tool(&mut self) {
        self.tools.insert(
            "write".into(),
            McpToolDef {
                name: "write".into(),
                description: "写入文件内容".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "文件路径",
                        },
                        "content": {
                            "type": "string",
                            "description": "写入内容",
                        },
                    },
                    "required": ["path", "content"],
                }),
            },
        );
    }

    /// 注册 edit 工具 — 编辑文件（精确字符串替换）
    fn register_edit_tool(&mut self) {
        self.tools.insert(
            "edit".into(),
            McpToolDef {
                name: "edit".into(),
                description: "编辑文件内容（精确字符串替换）".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "文件路径",
                        },
                        "old_string": {
                            "type": "string",
                            "description": "要替换的原始字符串",
                        },
                        "new_string": {
                            "type": "string",
                            "description": "替换后的新字符串",
                        },
                    },
                    "required": ["path", "old_string", "new_string"],
                }),
            },
        );
    }

    /// 注册 bash 工具 — 执行 Shell 命令
    fn register_bash_tool(&mut self) {
        self.tools.insert(
            "bash".into(),
            McpToolDef {
                name: "bash".into(),
                description: "执行 Shell 命令".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "要执行的 Shell 命令",
                        },
                    },
                    "required": ["command"],
                }),
            },
        );
    }

    /// 注册 glob 工具 — 文件模式匹配搜索
    fn register_glob_tool(&mut self) {
        self.tools.insert(
            "glob".into(),
            McpToolDef {
                name: "glob".into(),
                description: "文件模式匹配搜索".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Glob 匹配模式",
                        },
                        "path": {
                            "type": "string",
                            "description": "搜索根目录（可选）",
                        },
                    },
                    "required": ["pattern"],
                }),
            },
        );
    }

    /// 注册 grep 工具 — 内容正则搜索
    fn register_grep_tool(&mut self) {
        self.tools.insert(
            "grep".into(),
            McpToolDef {
                name: "grep".into(),
                description: "文件内容正则搜索".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "正则表达式模式",
                        },
                        "path": {
                            "type": "string",
                            "description": "搜索目录（可选）",
                        },
                        "include": {
                            "type": "string",
                            "description": "文件名过滤模式（可选）",
                        },
                    },
                    "required": ["pattern"],
                }),
            },
        );
    }

    /// 注册 todo_write 工具 — 写入/更新待办列表
    fn register_todo_write_tool(&mut self) {
        self.tools.insert(
            "todo_write".into(),
            McpToolDef {
                name: "todo_write".into(),
                description: "写入/更新待办列表，用于跟踪任务进度".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "description": "待办事项列表",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string", "description": "待办项唯一标识" },
                                    "content": { "type": "string", "description": "待办项内容" },
                                    "priority": { "type": "string", "description": "优先级（high/medium/low）" },
                                    "status": { "type": "string", "description": "状态（pending/in_progress/completed）" },
                                },
                                "required": ["id", "content", "priority", "status"],
                            },
                        },
                    },
                    "required": ["todos"],
                }),
            },
        );
    }

    /// 注册 web_search 工具 — 搜索网页
    fn register_web_search_tool(&mut self) {
        self.tools.insert(
            "web_search".into(),
            McpToolDef {
                name: "web_search".into(),
                description: "搜索网页，返回搜索结果".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "搜索查询关键词",
                        },
                    },
                    "required": ["query"],
                }),
            },
        );
    }

    /// 注册 web_fetch 工具 — 获取网页内容
    fn register_web_fetch_tool(&mut self) {
        self.tools.insert(
            "web_fetch".into(),
            McpToolDef {
                name: "web_fetch".into(),
                description: "获取指定 URL 的网页内容".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "要获取的网页 URL",
                        },
                    },
                    "required": ["url"],
                }),
            },
        );
    }

    /// 注册 git_status 工具 — 查看 Git 仓库状态
    fn register_git_status_tool(&mut self) {
        self.tools.insert(
            "git_status".into(),
            McpToolDef {
                name: "git_status".into(),
                description: "查看 Git 仓库状态".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "仓库路径（可选，默认为当前目录）",
                        },
                    },
                    "required": [],
                }),
            },
        );
    }

    /// 注册 list_directory 工具 — 列出目录内容
    fn register_list_directory_tool(&mut self) {
        self.tools.insert(
            "list_directory".into(),
            McpToolDef {
                name: "list_directory".into(),
                description: "列出目录内容".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "目录路径",
                        },
                    },
                    "required": ["path"],
                }),
            },
        );
    }

    /// 注册 create_file 工具 — 创建新文件
    fn register_create_file_tool(&mut self) {
        self.tools.insert(
            "create_file".into(),
            McpToolDef {
                name: "create_file".into(),
                description: "创建新文件（文件已存在则失败）".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "文件路径",
                        },
                        "content": {
                            "type": "string",
                            "description": "文件内容",
                        },
                    },
                    "required": ["path", "content"],
                }),
            },
        );
    }
}

/// 工具信息（用于 tools/list 响应）
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolInfo {
    /// 工具名称
    pub name: String,
    /// 工具描述
    pub description: String,
    /// 输入参数 JSON Schema
    pub input_schema: Value,
}
