//! MCP 工具注册表 — 管理可通过 MCP 协议调用的内置工具
//!
//! 注册 ccode 的 6 个内置工具（read/write/edit/bash/glob/grep），
//! 每个工具的调用通过消息总线转发给 ToolNode 执行。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::message::{FrameCodec, Message, Topic};

/// 工具处理函数类型 — 异步函数指针，接收参数返回结果
type ToolHandler = Box<
    dyn Fn(serde_json::Value) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, String>> + Send>>
        + Send
        + Sync,
>;

/// MCP 工具定义
pub struct McpToolDef {
    /// 工具名称
    pub name: String,
    /// 工具描述
    pub description: String,
    /// 输入参数的 JSON Schema
    pub input_schema: Value,
    /// 工具处理函数
    handler: ToolHandler,
}

/// MCP 工具注册表
///
/// 管理所有可通过 MCP 协议调用的工具，
/// 提供注册、查询和调用接口。
pub struct McpToolRegistry {
    /// 工具定义映射（name → McpToolDef）
    tools: HashMap<String, McpToolDef>,
    /// 消息总线发送端（用于向 ToolNode 发送工具调用请求）
    bus_tx: tokio::sync::mpsc::Sender<Message>,
}

impl McpToolRegistry {
    /// 创建工具注册表并注册所有内置工具
    pub fn new(bus_tx: tokio::sync::mpsc::Sender<Message>) -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
            bus_tx,
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

    /// 调用指定工具
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, String> {
        let def = self
            .tools
            .get(name)
            .ok_or_else(|| format!("未知工具：{}", name))?;
        (def.handler)(arguments).await
    }

    /// 通过消息总线向 ToolNode 发送工具调用请求
    #[allow(dead_code)]
    async fn send_tool_request(&self, tool_name: &str, arguments: Value) -> Result<Value, String> {
        let payload = serde_json::json!({
            "tool": tool_name,
            "arguments": arguments,
        });

        let msg = FrameCodec::new_message(
            Topic::new("tool/call"),
            "mcp-server",
            &payload,
        )
        .map_err(|e| format!("创建工具调用消息失败：{}", e))?;

        self.bus_tx
            .send(msg)
            .await
            .map_err(|e| format!("发送工具调用请求失败：{}", e))?;

        // 返回确认结果（实际执行结果由 ToolNode 异步返回）
        Ok(serde_json::json!({
            "status": "dispatched",
            "tool": tool_name,
        }))
    }

    /// 执行内置工具（同步直接执行，不经过消息总线）
    ///
    /// 支持 read/write/edit/bash/glob/grep 六个内置工具。
    /// 对于 bash 工具，仅允许白名单命令以确保安全。
    pub fn execute_tool(&self, name: &str, arguments: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
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
                // 安全：仅允许白名单命令
                let allowed = ["ls", "cat", "pwd", "echo", "which", "head", "tail", "wc", "grep", "find", "git", "cargo", "rustc"];
                let cmd_name = command.split_whitespace().next().unwrap_or("");
                if !allowed.contains(&cmd_name) {
                    return Ok(serde_json::json!({
                        "content": [{"type": "text", "text": format!("命令 {} 不在白名单中", cmd_name)}],
                        "isError": true
                    }));
                }
                let output = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(command)
                    .output();
                match output {
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
                let output = std::process::Command::new("find")
                    .arg(path)
                    .arg("-name")
                    .arg(pattern)
                    .output();
                match output {
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
                let output = std::process::Command::new("grep")
                    .arg("-r")
                    .arg(pattern)
                    .arg(path)
                    .output();
                match output {
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
            _ => Err(anyhow::anyhow!("未知工具：{}", name))
        }
    }

    // ---- 内置工具注册 ----

    /// 注册 read 工具 — 读取文件内容
    fn register_read_tool(&mut self) {
        let bus_tx = self.bus_tx.clone();
        let handler = Box::new(move |args: Value| {
            let bus_tx = bus_tx.clone();
            Box::pin(async move {
                let path = args["path"].as_str().unwrap_or("");
                if path.is_empty() {
                    return Err("read 工具缺少 path 参数".into());
                }
                let payload = serde_json::json!({
                    "tool": "read",
                    "arguments": args,
                });
                let msg = FrameCodec::new_message(
                    Topic::new("tool/call"),
                    "mcp-server",
                    &payload,
                )
                .map_err(|e| format!("创建消息失败：{}", e))?;
                bus_tx.send(msg).await
                    .map_err(|e| format!("发送失败：{}", e))?;
                Ok(serde_json::json!({"status": "dispatched", "tool": "read"}))
            })
                as Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
        });

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
                handler,
            },
        );
    }

    /// 注册 write 工具 — 写入文件
    fn register_write_tool(&mut self) {
        let bus_tx = self.bus_tx.clone();
        let handler = Box::new(move |args: Value| {
            let bus_tx = bus_tx.clone();
            Box::pin(async move {
                let path = args["path"].as_str().unwrap_or("");
                if path.is_empty() {
                    return Err("write 工具缺少 path 参数".into());
                }
                let payload = serde_json::json!({
                    "tool": "write",
                    "arguments": args,
                });
                let msg = FrameCodec::new_message(
                    Topic::new("tool/call"),
                    "mcp-server",
                    &payload,
                )
                .map_err(|e| format!("创建消息失败：{}", e))?;
                bus_tx.send(msg).await
                    .map_err(|e| format!("发送失败：{}", e))?;
                Ok(serde_json::json!({"status": "dispatched", "tool": "write"}))
            })
                as Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
        });

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
                handler,
            },
        );
    }

    /// 注册 edit 工具 — 编辑文件（精确字符串替换）
    fn register_edit_tool(&mut self) {
        let bus_tx = self.bus_tx.clone();
        let handler = Box::new(move |args: Value| {
            let bus_tx = bus_tx.clone();
            Box::pin(async move {
                let path = args["path"].as_str().unwrap_or("");
                if path.is_empty() {
                    return Err("edit 工具缺少 path 参数".into());
                }
                let payload = serde_json::json!({
                    "tool": "edit",
                    "arguments": args,
                });
                let msg = FrameCodec::new_message(
                    Topic::new("tool/call"),
                    "mcp-server",
                    &payload,
                )
                .map_err(|e| format!("创建消息失败：{}", e))?;
                bus_tx.send(msg).await
                    .map_err(|e| format!("发送失败：{}", e))?;
                Ok(serde_json::json!({"status": "dispatched", "tool": "edit"}))
            })
                as Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
        });

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
                handler,
            },
        );
    }

    /// 注册 bash 工具 — 执行 Shell 命令
    fn register_bash_tool(&mut self) {
        let bus_tx = self.bus_tx.clone();
        let handler = Box::new(move |args: Value| {
            let bus_tx = bus_tx.clone();
            Box::pin(async move {
                let command = args["command"].as_str().unwrap_or("");
                if command.is_empty() {
                    return Err("bash 工具缺少 command 参数".into());
                }
                let payload = serde_json::json!({
                    "tool": "bash",
                    "arguments": args,
                });
                let msg = FrameCodec::new_message(
                    Topic::new("tool/call"),
                    "mcp-server",
                    &payload,
                )
                .map_err(|e| format!("创建消息失败：{}", e))?;
                bus_tx.send(msg).await
                    .map_err(|e| format!("发送失败：{}", e))?;
                Ok(serde_json::json!({"status": "dispatched", "tool": "bash"}))
            })
                as Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
        });

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
                handler,
            },
        );
    }

    /// 注册 glob 工具 — 文件模式匹配搜索
    fn register_glob_tool(&mut self) {
        let bus_tx = self.bus_tx.clone();
        let handler = Box::new(move |args: Value| {
            let bus_tx = bus_tx.clone();
            Box::pin(async move {
                let pattern = args["pattern"].as_str().unwrap_or("");
                if pattern.is_empty() {
                    return Err("glob 工具缺少 pattern 参数".into());
                }
                let payload = serde_json::json!({
                    "tool": "glob",
                    "arguments": args,
                });
                let msg = FrameCodec::new_message(
                    Topic::new("tool/call"),
                    "mcp-server",
                    &payload,
                )
                .map_err(|e| format!("创建消息失败：{}", e))?;
                bus_tx.send(msg).await
                    .map_err(|e| format!("发送失败：{}", e))?;
                Ok(serde_json::json!({"status": "dispatched", "tool": "glob"}))
            })
                as Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
        });

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
                handler,
            },
        );
    }

    /// 注册 grep 工具 — 内容正则搜索
    fn register_grep_tool(&mut self) {
        let bus_tx = self.bus_tx.clone();
        let handler = Box::new(move |args: Value| {
            let bus_tx = bus_tx.clone();
            Box::pin(async move {
                let pattern = args["pattern"].as_str().unwrap_or("");
                if pattern.is_empty() {
                    return Err("grep 工具缺少 pattern 参数".into());
                }
                let payload = serde_json::json!({
                    "tool": "grep",
                    "arguments": args,
                });
                let msg = FrameCodec::new_message(
                    Topic::new("tool/call"),
                    "mcp-server",
                    &payload,
                )
                .map_err(|e| format!("创建消息失败：{}", e))?;
                bus_tx.send(msg).await
                    .map_err(|e| format!("发送失败：{}", e))?;
                Ok(serde_json::json!({"status": "dispatched", "tool": "grep"}))
            })
                as Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
        });

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
                handler,
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
