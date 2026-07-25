//! 工具桥接层 - 将消息总线上的工具调用请求转换为实际执行
//!
//! 桥接层的设计思路：
//! 1. 定义工具注册表：每个工具的名称、描述、参数 schema、分类、权限要求
//! 2. 工具分发：根据 tool_name 查找对应的执行器
//! 3. 权限检查：根据 PermissionMode 决定是否需要用户确认
//! 4. 执行 + 结果封装：执行工具 → 封装为 ToolCallResult
//!
//! 注意：ccode 最终会直接依赖 xai-grok-tools crate，调用其 Tool trait 实现。
//! 工具桥接层：定义 22 个工具元数据 + 执行器接口 + 动态注册 + 超时控制

use std::collections::HashMap;
use std::time::Instant;

use crate::tools::{
    ToolCallRequest, ToolCallResult, ToolDefinition, ToolEntry, ToolCategory,
};
use crate::node::PermissionMode;

/// 工具执行器 trait - 每个工具需要实现此 trait
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    /// 执行工具
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String>;

    /// 工具名称
    fn name(&self) -> &str;
}

/// 工具桥接器 - 管理工具注册、权限检查和分发
pub struct ToolBridge {
    /// 已注册的工具条目
    entries: HashMap<String, ToolEntry>,
    /// 工具执行器（实际执行逻辑，后续集成 grok 工具时填充）
    executors: HashMap<String, Box<dyn ToolExecutor>>,
    /// 工具执行超时（秒）
    execution_timeout_secs: u64,
}

impl ToolBridge {
    pub fn new() -> Self {
        let mut bridge = Self {
            entries: HashMap::new(),
            executors: HashMap::new(),
            execution_timeout_secs: 120,
        };
        bridge.register_defaults();
        // 注册内置工具执行器（bash/read/write/edit/grep/glob/list_dir）
        super::builtin::register_builtin_executors(&mut bridge);
        bridge
    }

    /// 注册默认工具集（从 grok 迁移的 20 个工具）
    fn register_defaults(&mut self) {
        // ---- 文件系统 ----
        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "bash".into(),
                description: "执行 shell 命令。支持交互式命令、管道、重定向。工作目录为项目根目录。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "要执行的 shell 命令" },
                        "timeout": { "type": "number", "description": "超时秒数，默认 120" }
                    },
                    "required": ["command"]
                }),
            },
            category: ToolCategory::Shell,
            requires_confirmation: true,
            read_only: false,
        });

        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "read_file".into(),
                description: "读取文件内容。支持指定行范围。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "文件路径" },
                        "offset": { "type": "number", "description": "起始行号（从 1 开始）" },
                        "limit": { "type": "number", "description": "最大读取行数" }
                    },
                    "required": ["path"]
                }),
            },
            category: ToolCategory::FileSystem,
            requires_confirmation: false,
            read_only: true,
        });

        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "write_file".into(),
                description: "写入文件。如果文件不存在则创建，存在则覆盖。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "文件路径" },
                        "content": { "type": "string", "description": "文件内容" }
                    },
                    "required": ["path", "content"]
                }),
            },
            category: ToolCategory::FileSystem,
            requires_confirmation: true,
            read_only: false,
        });

        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "search_replace".into(),
                description: "搜索文件中的文本并替换。支持多行搜索。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "文件路径" },
                        "search": { "type": "string", "description": "要搜索的文本" },
                        "replace": { "type": "string", "description": "替换文本" }
                    },
                    "required": ["path", "search", "replace"]
                }),
            },
            category: ToolCategory::FileSystem,
            requires_confirmation: true,
            read_only: false,
        });

        // ---- 搜索 ----
        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "grep".into(),
                description: "搜索文件内容（基于 ripgrep）。支持正则表达式。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "搜索模式（支持正则）" },
                        "path": { "type": "string", "description": "搜索目录" },
                        "glob": { "type": "string", "description": "文件名过滤（如 *.rs）" }
                    },
                    "required": ["pattern"]
                }),
            },
            category: ToolCategory::Search,
            requires_confirmation: false,
            read_only: true,
        });

        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "list_dir".into(),
                description: "列出目录内容。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "目录路径" }
                    },
                    "required": ["path"]
                }),
            },
            category: ToolCategory::FileSystem,
            requires_confirmation: false,
            read_only: true,
        });

        // ---- Web ----
        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "web_search".into(),
                description: "搜索网页。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "搜索查询" }
                    },
                    "required": ["query"]
                }),
            },
            category: ToolCategory::Web,
            requires_confirmation: false,
            read_only: true,
        });

        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "web_fetch".into(),
                description: "抓取网页内容。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "网页 URL" }
                    },
                    "required": ["url"]
                }),
            },
            category: ToolCategory::Web,
            requires_confirmation: false,
            read_only: true,
        });

        // ---- 用户交互 ----
        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "ask_user_question".into(),
                description: "向用户提问，等待用户回答。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "question": { "type": "string", "description": "问题内容" },
                        "options": { "type": "array", "items": { "type": "object" }, "description": "选项列表" }
                    },
                    "required": ["question"]
                }),
            },
            category: ToolCategory::UserInteraction,
            requires_confirmation: false,
            read_only: true,
        });

        // ---- 任务管理 ----
        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "task".into(),
                description: "创建和管理子任务（subagent 调度）。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["create", "list", "cancel", "wait"], "description": "操作类型" },
                        "description": { "type": "string", "description": "任务描述" },
                        "task_type": { "type": "string", "enum": ["explore", "plan", "general-purpose"], "description": "子 agent 类型" }
                    },
                    "required": ["action"]
                }),
            },
            category: ToolCategory::Task,
            requires_confirmation: false,
            read_only: false,
        });

        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "scheduler".into(),
                description: "定时执行命令或监控文件变化。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["create", "delete", "list"], "description": "操作类型" }
                    },
                    "required": ["action"]
                }),
            },
            category: ToolCategory::Task,
            requires_confirmation: true,
            read_only: false,
        });

        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "monitor".into(),
                description: "监控文件变化并报告。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "监控路径" },
                        "action": { "type": "string", "enum": ["start", "stop", "list"], "description": "操作类型" }
                    },
                    "required": ["action"]
                }),
            },
            category: ToolCategory::Task,
            requires_confirmation: false,
            read_only: true,
        });

        // ---- 代码分析 ----
        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "lsp".into(),
                description: "LSP 语言服务（跳转定义、引用查找等）。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "description": "LSP 操作" }
                    },
                    "required": ["action"]
                }),
            },
            category: ToolCategory::CodeAnalysis,
            requires_confirmation: false,
            read_only: true,
        });

        // ---- 记忆 ----
        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "memory_search".into(),
                description: "搜索记忆（短期/长期）。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "搜索查询" }
                    },
                    "required": ["query"]
                }),
            },
            category: ToolCategory::Memory,
            requires_confirmation: false,
            read_only: true,
        });

        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "memory_get".into(),
                description: "获取指定记忆条目的完整内容。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "记忆条目 ID" }
                    },
                    "required": ["id"]
                }),
            },
            category: ToolCategory::Memory,
            requires_confirmation: false,
            read_only: true,
        });

        // ---- 计划模式 ----
        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "enter_plan_mode".into(),
                description: "进入 Plan 模式，Agent 只生成计划不执行。".into(),
                parameters: serde_json::json!({ "type": "object", "properties": {} }),
            },
            category: ToolCategory::UserInteraction,
            requires_confirmation: false,
            read_only: true,
        });

        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "exit_plan_mode".into(),
                description: "退出 Plan 模式，恢复执行。".into(),
                parameters: serde_json::json!({ "type": "object", "properties": {} }),
            },
            category: ToolCategory::UserInteraction,
            requires_confirmation: false,
            read_only: true,
        });

        // ---- TODO/目标 ----
        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "todo".into(),
                description: "管理 TODO 列表。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["add", "complete", "list"], "description": "操作类型" },
                        "content": { "type": "string", "description": "TODO 内容" }
                    },
                    "required": ["action"]
                }),
            },
            category: ToolCategory::Task,
            requires_confirmation: false,
            read_only: false,
        });

        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "update_goal".into(),
                description: "更新当前目标。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "goal": { "type": "string", "description": "目标描述" }
                    },
                    "required": ["goal"]
                }),
            },
            category: ToolCategory::Task,
            requires_confirmation: false,
            read_only: false,
        });

        // ---- 媒体生成 ----
        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "image_gen".into(),
                description: "生成图像。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string", "description": "图像描述" }
                    },
                    "required": ["prompt"]
                }),
            },
            category: ToolCategory::MediaGeneration,
            requires_confirmation: true,
            read_only: false,
        });

        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "video_gen".into(),
                description: "生成视频。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string", "description": "视频描述" }
                    },
                    "required": ["prompt"]
                }),
            },
            category: ToolCategory::MediaGeneration,
            requires_confirmation: true,
            read_only: false,
        });

        // ---- MCP ----
        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "mcp".into(),
                description: "调用 MCP 外部工具。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "MCP 服务名称" },
                        "tool": { "type": "string", "description": "工具名称" },
                        "arguments": { "type": "object", "description": "工具参数" }
                    },
                    "required": ["server", "tool"]
                }),
            },
            category: ToolCategory::Shell,
            requires_confirmation: true,
            read_only: false,
        });
    }

    /// 注册工具
    fn register(&mut self, entry: ToolEntry) {
        let name = entry.definition.name.clone();
        self.entries.insert(name, entry);
    }

    /// 注册工具执行器
    pub fn register_executor(&mut self, executor: Box<dyn ToolExecutor>) {
        self.executors.insert(executor.name().to_string(), executor);
    }

    /// 检查工具是否需要用户确认
    pub fn needs_confirmation(
        &self,
        tool_name: &str,
        permission_mode: PermissionMode,
    ) -> bool {
        match permission_mode {
            PermissionMode::Yolo => false,
            PermissionMode::Ask => true,
            PermissionMode::Trust => {
                // Trust 模式：只读工具自动信任，写操作需要确认
                self.entries
                    .get(tool_name)
                    .map(|e| !e.read_only)
                    .unwrap_or(true)
            }
        }
    }

    /// 执行工具调用
    pub async fn execute(
        &self,
        request: &ToolCallRequest,
    ) -> ToolCallResult {
        let start = Instant::now();

        // 查找工具执行器
        match self.executors.get(&request.tool_name) {
            Some(executor) => {
                // 设置超时
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(self.execution_timeout_secs),
                    executor.execute(&request.arguments),
                )
                .await;

                match result {
                    Ok(Ok(output)) => ToolCallResult {
                        tool_call_id: request.tool_call_id.clone(),
                        output,
                        success: true,
                        duration_ms: start.elapsed().as_millis() as u64,
                        is_partial: false,
                    },
                    Ok(Err(e)) => ToolCallResult {
                        tool_call_id: request.tool_call_id.clone(),
                        output: format!("工具执行错误：{}", e),
                        success: false,
                        duration_ms: start.elapsed().as_millis() as u64,
                        is_partial: false,
                    },
                    Err(_) => ToolCallResult {
                        tool_call_id: request.tool_call_id.clone(),
                        output: format!("工具执行超时（{}秒）", self.execution_timeout_secs),
                        success: false,
                        duration_ms: start.elapsed().as_millis() as u64,
                        is_partial: false,
                    },
                }
            }
            None => ToolCallResult {
                tool_call_id: request.tool_call_id.clone(),
                output: format!("未知工具：{}", request.tool_name),
                success: false,
                duration_ms: 0,
                is_partial: false,
            },
        }
    }

    /// 获取所有工具定义（用于 LLM tools 参数）
    pub fn tool_definitions(&self) -> Vec<&ToolDefinition> {
        self.entries.values().map(|e| &e.definition).collect()
    }

    /// 获取工具定义的 JSON 列表（直接可传给 LLM API）
    pub fn tool_definitions_json(&self) -> Vec<serde_json::Value> {
        self.entries
            .values()
            .map(|e| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": e.definition.name,
                        "description": e.definition.description,
                        "parameters": e.definition.parameters,
                    }
                })
            })
            .collect()
    }

    /// 获取工具条目
    pub fn get_entry(&self, tool_name: &str) -> Option<&ToolEntry> {
        self.entries.get(tool_name)
    }

    /// 已注册工具数量
    pub fn tool_count(&self) -> usize {
        self.entries.len()
    }

    /// 已注册执行器数量
    pub fn executor_count(&self) -> usize {
        self.executors.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_tools_registered() {
        let bridge = ToolBridge::new();
        // 至少 20 个默认工具
        assert!(bridge.tool_count() >= 20, "默认工具数量不足：{}", bridge.tool_count());
    }

    #[test]
    fn test_needs_confirmation_yolo() {
        let bridge = ToolBridge::new();
        // Yolo 模式下所有工具都不需要确认
        assert!(!bridge.needs_confirmation("bash", PermissionMode::Yolo));
        assert!(!bridge.needs_confirmation("read_file", PermissionMode::Yolo));
    }

    #[test]
    fn test_needs_confirmation_ask() {
        let bridge = ToolBridge::new();
        // Ask 模式下所有工具都需要确认
        assert!(bridge.needs_confirmation("bash", PermissionMode::Ask));
        assert!(bridge.needs_confirmation("read_file", PermissionMode::Ask));
    }

    #[test]
    fn test_needs_confirmation_trust() {
        let bridge = ToolBridge::new();
        // Trust 模式：只读工具不需要确认，写操作需要确认
        assert!(!bridge.needs_confirmation("read_file", PermissionMode::Trust));
        assert!(!bridge.needs_confirmation("grep", PermissionMode::Trust));
        assert!(bridge.needs_confirmation("bash", PermissionMode::Trust));
        assert!(bridge.needs_confirmation("write_file", PermissionMode::Trust));
    }

    #[test]
    fn test_unknown_tool() {
        let bridge = ToolBridge::new();
        let request = ToolCallRequest {
            tool_call_id: "test-1".into(),
            tool_name: "nonexistent_tool".into(),
            arguments: serde_json::json!({}),
            agent_id: "agent-1".into(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(bridge.execute(&request));
        assert!(!result.success);
        assert!(result.output.contains("未知工具"));
    }

    #[test]
    fn test_tool_definitions_json() {
        let bridge = ToolBridge::new();
        let defs = bridge.tool_definitions_json();
        assert!(!defs.is_empty());
        // 每个定义应有 type, function, name, description, parameters
        for def in &defs {
            assert_eq!(def["type"], "function");
            assert!(def["function"]["name"].is_string());
            assert!(def["function"]["description"].is_string());
            assert!(def["function"]["parameters"].is_object());
        }
    }
}
