//! 工具桥接层 - 将消息总线上的工具调用请求转换为实际执行
//!
//! 桥接层的设计思路：
//! 1. 定义工具注册表：每个工具的名称、描述、参数 schema、分类、权限要求
//! 2. 工具分发：根据 tool_name 查找对应的执行器
//! 3. 权限检查：根据 PermissionMode 决定是否需要用户确认
//! 4. 执行 + 结果封装：执行工具 → 封装为 ToolCallResult
//!
//! 注意：ccode 最终会直接依赖 ccode-tools crate，调用其 Tool trait 实现。
//! 工具桥接层：定义 22 个工具元数据 + 执行器接口 + 动态注册 + 超时控制

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::tools::{
    ToolCallRequest, ToolCallResult, ToolDefinition, ToolEntry, ToolCategory,
};
use crate::node::PermissionMode;

/// 工具执行结果（借鉴 Claude Code ToolRunResult）
///
/// 包含两个视图：
/// - output: 干净的工具输出（用于日志、通知）
/// - prompt_text: 格式化为下轮 LLM 输入的文本（包含系统提醒）
#[derive(Debug, Clone)]
pub struct ToolRunResult {
    /// 干净输出
    pub output: String,
    /// LLM 下轮输入格式
    pub prompt_text: String,
}

impl ToolRunResult {
    /// 从简单输出创建（自动生成 prompt_text）
    pub fn from_output(output: impl Into<String>) -> Self {
        let output = output.into();
        let prompt_text = format!("<tool_result>\n{}\n</tool_result>", output);
        Self { output, prompt_text }
    }

    /// 创建带错误的结果
    pub fn from_error(error: impl Into<String>) -> Self {
        let error = error.into();
        let prompt_text = format!("<tool_error>\n{}\n</tool_error>", error);
        Self { output: error, prompt_text }
    }

    /// 追加系统提醒
    pub fn with_reminder(mut self, reminder: &str) -> Self {
        self.prompt_text = format!(
            "{}\n\n<system-reminder>\n{}\n</system-reminder>",
            self.prompt_text, reminder
        );
        self
    }
}

/// 将工具调用结果格式化为 LLM 消息
///
/// 借鉴 Claude Code 的 tool_calls.rs 格式
pub fn format_tool_results_for_prompt(results: &[(&str, ToolRunResult)]) -> Vec<serde_json::Value> {
    results
        .iter()
        .map(|(tool_call_id, result)| {
            serde_json::json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
                "content": result.prompt_text
            })
        })
        .collect()
}

/// 工具执行重试配置（借鉴 Claude Code BackoffConfig）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolRetryConfig {
    /// 最大重试次数
    pub max_retries: u32,
    /// 基础退避延迟（毫秒）
    pub base_delay_ms: u64,
    /// 最大退避延迟（毫秒）
    pub max_delay_ms: u64,
    /// 可重试的工具错误模式（正则表达式，匹配输出文本）
    pub retryable_error_patterns: Vec<String>,
}

impl Default for ToolRetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 30_000,
            retryable_error_patterns: vec![
                "timeout".into(),
                "connection reset".into(),
                "ECONNREFUSED".into(),
                "429".into(),
                "502".into(),
                "503".into(),
                "504".into(),
            ],
        }
    }
}

impl ToolRetryConfig {
    /// 计算指数退避延迟（借鉴 Claude Code BackoffConfig::calculate_delay）
    pub fn calculate_delay(&self, attempt: u32) -> Duration {
        let delay_ms = std::cmp::min(
            self.base_delay_ms * 2u64.pow(attempt.saturating_sub(1)),
            self.max_delay_ms,
        );
        Duration::from_millis(delay_ms)
    }

    /// 检查错误输出是否匹配可重试模式
    fn is_retryable_error(&self, error_output: &str) -> bool {
        self.retryable_error_patterns.iter().any(|pattern| {
            regex::Regex::new(pattern)
                .map(|re| re.is_match(error_output))
                .unwrap_or(false)
        })
    }
}

/// 工具执行器 trait - 每个工具需要实现此 trait
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    /// 执行工具
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String>;

    /// 工具名称
    fn name(&self) -> &str;
}

/// 工具执行后钩子
/// 在工具执行成功后触发，用于增量验证等场景
pub trait PostExecuteHook: Send + Sync {
    /// 是否对该工具触发
    fn should_run(&self, tool_name: &str) -> bool;

    /// 执行钩子，返回追加到工具结果的信息（空字符串表示无追加）
    fn run<'a>(
        &'a self,
        tool_name: &str,
        args: &'a serde_json::Value,
        result: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + 'a>>;
}

/// 工具执行前钩子
/// 在工具执行前触发，用于快照/备份等场景
pub trait PreExecuteHook: Send + Sync {
    /// 是否对该工具触发
    fn should_run(&self, tool_name: &str) -> bool;

    /// 执行钩子，返回追加到工具结果的信息（空字符串表示无追加）
    fn run<'a>(
        &'a self,
        tool_name: &str,
        args: &'a serde_json::Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + 'a>>;
}

use super::sandbox::{ToolSandbox, SandboxProfile, SandboxCheckResult, FileAccessOperation};
use super::permission::{PermissionChecker, PermissionMode as ToolPermissionMode, PermissionDecision};

/// 全局权限检查器（默认 AskUser 模式）
static PERMISSION_CHECKER: std::sync::OnceLock<PermissionChecker> = std::sync::OnceLock::new();

fn get_permission_checker() -> &'static PermissionChecker {
    PERMISSION_CHECKER.get_or_init(|| PermissionChecker::new(ToolPermissionMode::AskUser))
}

/// 工具桥接器 - 管理工具注册、权限检查和分发
pub struct ToolBridge {
    /// 已注册的工具条目
    entries: HashMap<String, ToolEntry>,
    /// 工具执行器（实际执行逻辑，后续集成 ccode 工具时填充）
    executors: HashMap<String, Box<dyn ToolExecutor>>,
    /// 工具执行前钩子（用于快照/备份，如 Write/Edit 前的 git 快照）
    pre_hooks: Vec<Box<dyn PreExecuteHook>>,
    /// 工具执行后钩子（用于增量验证，如 Write/Edit 后的 rustfmt 检查）
    post_hooks: Vec<Box<dyn PostExecuteHook>>,
    /// 工具执行超时（秒）
    execution_timeout_secs: u64,
    /// 工具执行重试配置（借鉴 Claude Code）
    retry_config: ToolRetryConfig,
    /// 工具级沙箱（借鉴 Claude Code，在执行前检查路径/命令/网络权限）
    sandbox: ToolSandbox,
}

impl Default for ToolBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolBridge {
    pub fn new() -> Self {
        let mut bridge = Self {
            entries: HashMap::new(),
            executors: HashMap::new(),
            pre_hooks: Vec::new(),
            post_hooks: Vec::new(),
            execution_timeout_secs: 120,
            retry_config: ToolRetryConfig::default(),
            sandbox: ToolSandbox::new(SandboxProfile::default()),
        };
        bridge.register_defaults();
        // 注册内置工具执行器（bash/read_file/write_file/search_replace/grep/glob/list_dir 等）
        // 以及 post_hook（write_file/search_replace 后的 rustfmt 检查）
        super::builtin::register_builtin_executors(&mut bridge);
        bridge
    }

    /// 注册默认工具集（从 ccode 迁移的 20 个工具）
    fn register_defaults(&mut self) {
        // ---- 文件系统 ----
        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "bash".into(),
                description: r###"执行 shell 命令,用于运行构建/测试命令、git 操作、文件系统操作等。

**何时使用**:
- 运行构建和测试命令 (如 `cargo build`, `npm test`)
- 执行 git 操作 (如 `git status`, `git diff`)
- 文件系统操作 (如 `mkdir`, `cp`, `mv`)
- 运行项目脚本和工具

**何时不使用**:
- 读取文件内容 → 使用 `read_file`
- 搜索代码 → 使用 `grep`
- 查找文件 → 使用 `glob`
- 编辑文件 → 使用 `search_replace` 或 `write_file`

**使用示例**:
构建项目:
command="cargo build --release"

运行测试:
command="cargo test"

Git 操作:
command="git status"
command="git diff HEAD~1"

安装依赖:
command="npm install"

**注意事项**:
- 默认超时为 120 秒,长时间运行的任务应设置 timeout 参数
- 避免在没有用户确认的情况下执行 `rm -rf` 等危险命令
- 工作目录默认为项目根目录
- 支持交互式命令、管道和重定向"###.into(),
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
                description: r###"读取文件内容,用于查看源代码、配置文件、文档等。

**何时使用**:
- 查看源代码文件内容
- 读取配置文件 (如 `Cargo.toml`, `package.json`)
- 查看文档和说明文件
- 检查文件结构

**参数说明**:
- `path`: 文件的绝对路径或相对路径(相对于项目根目录)
- `offset`: 起始行号(从 1 开始),用于分页读取大文件
- `limit`: 最大读取行数,避免一次性读取过多内容

**使用示例**:
读取完整文件:
path="src/main.rs"

读取前 50 行:
path="Cargo.toml"
offset=1
limit=50

读取特定行范围:
path="src/lib.rs"
offset=100
limit=30

**注意事项**:
- 对于大文件,使用 `offset` 和 `limit` 分批读取,避免内存溢出
- 不要读取二进制文件(如图片、可执行文件),可能导致乱码
- 路径区分大小写(在区分大小写的文件系统上)
- 如果文件不存在,会返回错误信息"###.into(),
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
                description: r###"写入文件内容,用于创建新文件或完全替换现有文件。

**何时使用**:
- 创建新的源代码文件
- 生成配置文件
- 创建文档文件
- 完全重写现有文件内容

**何时不使用**:
- 小范围编辑现有文件 → 使用 `search_replace`
- 追加内容到文件末尾 → 使用 `bash` 命令 `echo >> file`
- 只修改几行代码 → 使用 `search_replace` 更安全

**使用示例**:
```bash
创建新的 Rust 文件:
path="src/lib.rs"
content="//! 新库文件..."

生成配置文件:
path="config.json"
content='{"name": "my-project", "version": "1.0.0"}'

创建 README:
path="README.md"
content="项目说明文档"
```

**注意事项**:
- 会完全覆盖现有文件内容,没有警告或确认
- 如果文件不存在,会自动创建(包括必要的目录)
- 适合创建新文件或完全重写文件
- 对于现有文件的小修改,优先使用 `search_replace`
- 写入前确保内容正确,避免数据丢失"###.into(),
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
                description: r###"在文件中搜索文本并替换,用于精确修改现有文件。

**何时使用**:
- 修改现有文件中的特定代码片段
- 更新函数实现
- 重命名变量或函数
- 修复代码中的错误

**何时不使用**:
- 创建新文件 → 使用 `write_file`
- 完全重写文件内容 → 使用 `write_file`
- 批量替换多个文件 → 使用 `bash` 配合 `sed`

**使用示例**:
替换函数实现:
path="src/lib.rs"
search="pub fn old_name() {"
replace="pub fn new_name() {"

更新配置值:
path="config.toml"
search="version = \"1.0.0\""
replace="version = \"2.0.0\""

修复代码错误:
path="src/main.rs"
search="println!(\"Hello\");"
replace="println!(\"Hello, World!\");"

**注意事项**:
- 必须精确匹配要替换的内容,包括空格和缩进
- 如果搜索文本在文件中出现多次,默认会报错；设置 `replace_all=true` 可替换所有匹配项
- 搜索文本不匹配时会返回错误,不会修改文件
- 支持多行搜索,但需要包含完整的文本块
- 匹配会尝试四级模糊阶梯：精确 → 行尾归一(CRLF/LF) → 首尾空白容忍 → Unicode 标点归一
- `mode=insert` 可按行号插入内容（需提供 `after_line` 和 `content`）
- `mode=write` 可完全覆盖文件内容（需提供 `content`）
- 建议先用 `read_file` 查看文件内容,确保搜索文本准确"###.into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "文件路径" },
                        "search": { "type": "string", "description": "要搜索的文本（replace 模式必填）" },
                        "replace": { "type": "string", "description": "替换文本（replace 模式必填）" },
                        "mode": {
                            "type": "string",
                            "enum": ["replace", "insert", "write"],
                            "description": "编辑模式：replace=搜索替换(默认), insert=按行号插入, write=完全覆盖",
                            "default": "replace"
                        },
                        "replace_all": {
                            "type": "boolean",
                            "description": "replace 模式下，是否替换所有匹配项（默认 false，多处匹配时报错）",
                            "default": false
                        },
                        "after_line": {
                            "type": "integer",
                            "description": "insert 模式：在此行号后插入 content（0=文件开头）"
                        },
                        "content": {
                            "type": "string",
                            "description": "insert/write 模式：要插入或写入的内容"
                        }
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
                description: r###"搜索文件内容(基于 ripgrep),用于查找代码、文本模式等。

**何时使用**:
- 在代码库中查找函数定义或使用
- 搜索特定的文本模式
- 查找错误消息或日志
- 定位代码片段

**参数说明**:
- `pattern`: 搜索模式,支持正则表达式(如 `fn \w+\(`, `TODO|FIXME`)
- `path`: 搜索目录,默认为当前工作目录
- `glob`: 文件名过滤,支持通配符(如 `*.rs`, `**/*.js`)

**使用示例**:
搜索函数定义:
pattern="fn main"
glob="*.rs"

查找所有 TODO 注释:
pattern="TODO|FIXME"

在特定目录中搜索:
pattern="import"
path="src/components"
glob="*.tsx"

使用正则表达式:
pattern="function\s+\w+\s*\("
glob="*.js"

**搜索模式示例**:
- 精确匹配: `"exact_text"`
- 正则匹配: `"fn \w+\("` (函数定义)
- 或匹配: `"TODO|FIXME"` (多个关键词)
- 行首匹配: `"^import"` (以 import 开头)
- 大小写不敏感: 使用 `-i` 标志(通过 bash 命令)

**注意事项**:
- 默认区分大小写
- 正则表达式需要转义特殊字符
- 支持大多数 ripgrep 的正则语法
- 对于大型代码库,建议使用 glob 过滤以提高性能
- 返回匹配的文件路径和行号"###.into(),
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
                description: r###"列出目录内容,用于浏览项目结构。

**何时使用**:
- 查看项目目录结构
- 确认文件是否存在
- 浏览文件夹内容
- 查找特定类型的文件

**参数说明**:
- `path`: 目录的绝对路径或相对路径

**使用示例**:
列出项目根目录:
path="."

查看源代码目录:
path="src"

浏览配置目录:
path="config"

检查测试目录:
path="tests"

**输出格式**:
- 显示目录下的所有文件和子目录
- 区分文件和目录(目录会有 `/` 后缀)
- 按字母顺序排序
- 包含隐藏文件(以 `.` 开头)

**注意事项**:
- 路径不存在时会返回错误
- 只显示直接内容,不会递归显示子目录
- 对于大型目录,输出可能较长
- 如果需要递归查找文件,建议使用 `glob` 工具
- 适合快速浏览目录结构,不适合查找特定文件"###.into(),
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

        // glob：按文件名模式查找文件（GlobExecutor 已实现，此前漏了声明导致模型不可见）
        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "glob".into(),
                description: "按 glob 模式查找文件路径，适合按文件名/扩展名定位文件。查找文件内容请用 grep。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "glob 模式，如 **/*.rs" },
                        "path": { "type": "string", "description": "搜索起始目录，默认当前目录" },
                        "exclude": { "type": "string", "description": "要排除的 glob 模式" }
                    },
                    "required": ["pattern"]
                }),
            },
            category: ToolCategory::FileSystem,
            requires_confirmation: false,
            read_only: true,
        });

        // skill：调用预置技能（SkillExecutor 已实现，此前漏了声明导致模型不可见）
        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "skill".into(),
                description: "调用预置技能，执行封装好的领域流程。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "skill": { "type": "string", "description": "技能名称" }
                    },
                    "required": ["skill"]
                }),
            },
            category: ToolCategory::Task,
            requires_confirmation: false,
            read_only: false,
        });

        // tool_search：按需获取其他工具的完整参数 schema，减少每轮工具定义的 token 消耗
        self.register(ToolEntry {
            definition: ToolDefinition {
                name: "tool_search".into(),
                description: "按需获取工具的完整参数定义。当你需要使用某个工具但不确定其参数格式时，调用此工具获取完整 schema。".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "tool_names": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "要获取完整定义的工具名称列表"
                        }
                    },
                    "required": ["tool_names"]
                }),
            },
            category: ToolCategory::Search,
            requires_confirmation: false,
            read_only: true,
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

    /// 注册工具执行后钩子（用于增量验证，如 Write/Edit 后的 rustfmt 检查）
    pub fn register_pre_hook(&mut self, hook: Box<dyn PreExecuteHook>) {
        self.pre_hooks.push(hook);
    }

    pub fn register_post_hook(&mut self, hook: Box<dyn PostExecuteHook>) {
        self.post_hooks.push(hook);
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
            PermissionMode::ReadOnly => {
                // 只读模式：写操作直接拒绝（不需要确认）
                self.entries
                    .get(tool_name)
                    .map(|e| !e.read_only)
                    .unwrap_or(true)
            }
        }
    }

    /// 执行工具调用（带沙箱检查 + 指数退避重试）
    pub async fn execute(
        &self,
        request: &ToolCallRequest,
    ) -> ToolCallResult {
        let start = Instant::now();

        tracing::debug!(
            target: "ccore::tool",
            tool = %request.tool_name,
            "executing tool"
        );

        // ── 沙箱前置检查（借鉴 Claude Code，在执行前拦截危险操作）──
        if let Err(sandbox_denied) = self.check_sandbox(&request.tool_name, &request.arguments) {
            tracing::warn!(
                target: "ccore::tool",
                tool = %request.tool_name,
                error = %sandbox_denied,
                "tool execution failed"
            );
            return ToolCallResult {
                tool_call_id: request.tool_call_id.clone(),
                output: sandbox_denied,
                success: false,
                duration_ms: start.elapsed().as_millis() as u64,
                is_partial: false,
            };
        }

        // ── 权限检查（PermissionChecker，在沙箱检查之后执行）──
        let decision = get_permission_checker().check(&request.tool_name, &request.arguments);
        match decision {
            PermissionDecision::Deny => {
                let err_msg = format!("权限拒绝：工具 {} 被安全策略阻止", request.tool_name);
                tracing::debug!(
                    target: "ccore::tool",
                    tool = %request.tool_name,
                    decision = "deny",
                    "permission check"
                );
                tracing::warn!(
                    target: "ccore::tool",
                    tool = %request.tool_name,
                    error = %err_msg,
                    "tool execution failed"
                );
                return ToolCallResult {
                    tool_call_id: request.tool_call_id.clone(),
                    output: err_msg,
                    success: false,
                    duration_ms: start.elapsed().as_millis() as u64,
                    is_partial: false,
                };
            }
            PermissionDecision::AskUser => {
                tracing::debug!(
                    target: "ccore::tool",
                    tool = %request.tool_name,
                    decision = "ask_user",
                    "permission check"
                );
            }
            PermissionDecision::Allow => {
                tracing::debug!(
                    target: "ccore::tool",
                    tool = %request.tool_name,
                    decision = "allow",
                    "permission check"
                );
            }
        }

        // 查找工具执行器
        let executor = match self.executors.get(&request.tool_name) {
            Some(e) => e,
            None => {
                tracing::warn!(
                    target: "ccore::tool",
                    tool = %request.tool_name,
                    error = "unknown tool",
                    "tool execution failed"
                );
                return ToolCallResult {
                    tool_call_id: request.tool_call_id.clone(),
                    output: format!("未知工具：{}", request.tool_name),
                    success: false,
                    duration_ms: 0,
                    is_partial: false,
                };
            }
        };

        let mut attempt = 0u32;

        // 工具执行前，遍历 pre_hooks 做快照/备份
        // 钩子自身的失败不阻塞工具执行，仅把提示信息追加到结果
        let mut pre_hook_output = String::new();
        for hook in &self.pre_hooks {
            if !hook.should_run(&request.tool_name) {
                continue;
            }
            let info = hook.run(&request.tool_name, &request.arguments).await;
            if !info.is_empty() {
                pre_hook_output.push_str(&info);
            }
        }

        loop {
            attempt += 1;

            // 设置超时执行
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(self.execution_timeout_secs),
                executor.execute(&request.arguments),
            )
            .await;

            match result {
                Ok(Ok(output)) => {
                    let mut final_output = output;
                    // 工具执行成功后，遍历 post_hooks 做增量验证
                    // 钩子自身的失败不阻塞工具执行，仅把警告信息追加到结果
                    for hook in &self.post_hooks {
                        if !hook.should_run(&request.tool_name) {
                            continue;
                        }
                        let extra = hook
                            .run(&request.tool_name, &request.arguments, &final_output)
                            .await;
                        if !extra.is_empty() {
                            final_output.push_str(&extra);
                        }
                    }
                    // 如果经过重试后成功，附加重试信息
                    if attempt > 1 {
                        final_output.push_str(&format!(
                            "\n[重试成功：第 {} 次尝试成功]",
                            attempt
                        ));
                    }
                    // 将 pre_hook 输出（如 git 快照信息）前置到工具结果
                    if !pre_hook_output.is_empty() {
                        final_output = format!("{}{}", pre_hook_output, final_output);
                    }
                    let elapsed = start.elapsed().as_millis() as u64;
                    tracing::debug!(
                        target: "ccore::tool",
                        tool = %request.tool_name,
                        duration_ms = elapsed,
                        success = true,
                        "tool execution completed"
                    );
                    return ToolCallResult {
                        tool_call_id: request.tool_call_id.clone(),
                        output: final_output,
                        success: true,
                        duration_ms: elapsed,
                        is_partial: false,
                    };
                }
                Ok(Err(e)) => {
                    let error_output = format!("工具执行错误：{}", e);
                    // 检查是否为可重试错误，以及是否还有重试次数
                    if attempt <= self.retry_config.max_retries
                        && self.retry_config.is_retryable_error(&error_output)
                    {
                        let delay = self.retry_config.calculate_delay(attempt);
                        tracing::warn!(
                            target: "ccore::tool",
                            tool = %request.tool_name,
                            error = %e,
                            "tool execution failed"
                        );
                        tracing::warn!(
                            "工具 {} 第 {} 次执行失败（{}），将在 {}ms 后重试",
                            request.tool_name,
                            attempt,
                            e,
                            delay.as_millis()
                        );
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    // 不可重试或重试耗尽
                    let retry_info = if attempt > 1 {
                        format!("（已重试 {} 次）", attempt - 1)
                    } else {
                        String::new()
                    };
                    let elapsed = start.elapsed().as_millis() as u64;
                    tracing::warn!(
                        target: "ccore::tool",
                        tool = %request.tool_name,
                        error = %e,
                        "tool execution failed"
                    );
                    return ToolCallResult {
                        tool_call_id: request.tool_call_id.clone(),
                        output: format!("{}{}", error_output, retry_info),
                        success: false,
                        duration_ms: elapsed,
                        is_partial: false,
                    };
                }
                Err(_) => {
                    let error_output = format!("工具执行超时（{}秒）", self.execution_timeout_secs);
                    // 超时也检查是否可重试
                    if attempt <= self.retry_config.max_retries
                        && self.retry_config.is_retryable_error(&error_output)
                    {
                        let delay = self.retry_config.calculate_delay(attempt);
                        tracing::warn!(
                            target: "ccore::tool",
                            tool = %request.tool_name,
                            error = "timeout",
                            "tool execution failed"
                        );
                        tracing::warn!(
                            "工具 {} 第 {} 次执行超时，将在 {}ms 后重试",
                            request.tool_name,
                            attempt,
                            delay.as_millis()
                        );
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    let retry_info = if attempt > 1 {
                        format!("（已重试 {} 次）", attempt - 1)
                    } else {
                        String::new()
                    };
                    let elapsed = start.elapsed().as_millis() as u64;
                    tracing::warn!(
                        target: "ccore::tool",
                        tool = %request.tool_name,
                        error = "timeout",
                        "tool execution failed"
                    );
                    return ToolCallResult {
                        tool_call_id: request.tool_call_id.clone(),
                        output: format!("{}{}", error_output, retry_info),
                        success: false,
                        duration_ms: elapsed,
                        is_partial: false,
                    };
                }
            }
        }
    }

    /// 获取所有工具定义（用于 LLM tools 参数）
    pub fn tool_definitions(&self) -> Vec<&ToolDefinition> {
        self.entries.values().map(|e| &e.definition).collect()
    }

    /// 沙箱前置检查（借鉴 Claude Code，在工具执行前拦截）
    ///
    /// 根据 tool_name 和 arguments 中的路径/命令进行安全检查：
    /// - bash: 检查命令是否在黑名单中
    /// - read/cat/head: 检查读取路径是否被拒绝
    /// - write/edit/search_replace: 检查写入路径是否在工作区内
    /// - web_fetch/curl: 检查网络权限
    fn check_sandbox(&self, tool_name: &str, arguments: &serde_json::Value) -> Result<(), String> {
        match tool_name {
            "bash" => {
                if let Some(cmd) = arguments.get("command").and_then(|v| v.as_str()) {
                    match self.sandbox.check_shell_command(cmd) {
                        SandboxCheckResult::Allowed => Ok(()),
                        SandboxCheckResult::Denied(reason) => {
                            tracing::warn!("沙箱拦截 bash 命令：{}", reason);
                            Err(format!("沙箱拒绝执行：{}", reason))
                        }
                        SandboxCheckResult::RequiresConfirmation(reason) => {
                            tracing::warn!("沙箱提示：{}", reason);
                            Ok(()) // 需确认但不拒绝，由上层权限系统处理
                        }
                    }
                } else {
                    Ok(())
                }
            }
            "read_file" | "cat" | "head" | "tail" => {
                if let Some(path) = arguments.get("path").and_then(|v| v.as_str()) {
                    match self.sandbox.check_file_access(path, FileAccessOperation::Read) {
                        SandboxCheckResult::Allowed => Ok(()),
                        SandboxCheckResult::Denied(reason) => {
                            tracing::warn!("沙箱拦截读取路径：{}", reason);
                            Err(format!("沙箱拒绝访问：{}", reason))
                        }
                        SandboxCheckResult::RequiresConfirmation(_) => Ok(()),
                    }
                } else {
                    Ok(())
                }
            }
            "write_file" | "edit_file" | "search_replace" => {
                if let Some(path) = arguments.get("path").and_then(|v| v.as_str()) {
                    match self.sandbox.check_file_access(path, FileAccessOperation::Write) {
                        SandboxCheckResult::Allowed => Ok(()),
                        SandboxCheckResult::Denied(reason) => {
                            tracing::warn!("沙箱拦截写入路径：{}", reason);
                            Err(format!("沙箱拒绝写入：{}", reason))
                        }
                        SandboxCheckResult::RequiresConfirmation(reason) => {
                            tracing::warn!("沙箱写入提示：{}", reason);
                            Ok(())
                        }
                    }
                } else {
                    Ok(())
                }
            }
            "web_fetch" | "curl" => {
                match self.sandbox.check_network_access() {
                    SandboxCheckResult::Allowed => Ok(()),
                    SandboxCheckResult::Denied(reason) => {
                        tracing::warn!("沙箱拦截网络访问：{}", reason);
                        Err(format!("沙箱拒绝网络访问：{}", reason))
                    }
                    SandboxCheckResult::RequiresConfirmation(_) => Ok(()),
                }
            }
            _ => Ok(()), // 其他工具默认放行
        }
    }

    /// 获取沙箱实例引用
    pub fn sandbox(&self) -> &ToolSandbox {
        &self.sandbox
    }

    /// 获取沙箱实例可变引用（用于设置工作区路径或切换档案）
    pub fn sandbox_mut(&mut self) -> &mut ToolSandbox {
        &mut self.sandbox
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

    /// 获取重试配置
    pub fn retry_config(&self) -> &ToolRetryConfig {
        &self.retry_config
    }

    /// 设置重试配置
    pub fn set_retry_config(&mut self, config: ToolRetryConfig) {
        self.retry_config = config;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_tools_registered() {
        let bridge = ToolBridge::new();
        // 至少覆盖文件读写/搜索/执行等核心能力
        assert!(bridge.tool_count() >= 10, "默认工具数量不足：{}", bridge.tool_count());
    }

    /// 每个声明给模型的工具都必须真能执行。
    ///
    /// 只数工具条数无法暴露「声明了但没有 executor」的断裂——模型会调用一个
    /// 不存在的工具并拿到「未知工具」。这里断言声明与执行能力严格对齐。
    /// `task` / `tool_search` 由 ToolNode 在到达 bridge 前单独拦截，属于例外。
    #[test]
    fn test_every_declared_tool_is_executable() {
        const INTERCEPTED_BY_TOOL_NODE: &[&str] = &["task", "tool_search"];

        let bridge = ToolBridge::new();
        let phantom: Vec<&str> = bridge
            .tool_definitions()
            .iter()
            .map(|d| d.name.as_str())
            .filter(|name| !INTERCEPTED_BY_TOOL_NODE.contains(name))
            .filter(|name| !bridge.executors.contains_key(*name))
            .collect();

        assert!(
            phantom.is_empty(),
            "以下工具声明给了模型但没有 executor，调用必然失败：{phantom:?}"
        );
    }

    /// 有 executor 却没声明的工具，模型永远看不到，等于白实现。
    #[test]
    fn test_every_executor_is_declared() {
        let bridge = ToolBridge::new();
        let declared: std::collections::HashSet<&str> = bridge
            .tool_definitions()
            .iter()
            .map(|d| d.name.as_str())
            .collect();

        let undeclared: Vec<&str> = bridge
            .executors
            .keys()
            .map(|k| k.as_str())
            .filter(|name| !declared.contains(*name))
            .collect();

        assert!(
            undeclared.is_empty(),
            "以下工具有 executor 但未声明，模型不可见：{undeclared:?}"
        );
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
