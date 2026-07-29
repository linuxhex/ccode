//! 工具级详细提示模板（借鉴 Claude Code prompt.ts）
//!
//! 每个工具有独立的 LLM 面向描述，告诉 LLM：
//! - 工具做什么
//! - 何时使用/何时不使用
//! - 参数说明
//! - 注意事项和最佳实践

/// Bash 工具提示
///
/// 借鉴 Claude Code BashTool/prompt.ts 的核心内容
pub const BASH_PROMPT: &str = r#"
Executes a given bash command and returns its output.

The working directory persists between commands, but shell state does not. The shell environment is initialized from the user's profile (bash or zsh).

IMPORTANT: Avoid using this tool to run `find`, `grep`, `cat`, `head`, `tail`, `sed`, `awk`, or `echo` commands, unless explicitly instructed or after you have verified that a dedicated tool cannot accomplish your task. Instead, use the appropriate dedicated tool as this will provide a much better experience for the user:

- File search: Use glob (NOT find or ls)
- Content search: Use grep (NOT grep or rg)
- Read files: Use read (NOT cat/head/tail)
- Edit files: Use edit (NOT sed/awk)
- Write files: Use write (NOT echo >/cat <<EOF)
- Communication: Output text directly (NOT echo/printf)

# Instructions

- If your command will create new directories or files, first use this tool to run `ls` to verify the parent directory exists and is the correct location.
- Always quote file paths that contain spaces with double quotes in your command (e.g., cd "path with spaces/file.txt")
- Try to maintain your current working directory throughout the session by using absolute paths and avoiding usage of `cd`. You may use `cd` if the User explicitly requests it.
- You may specify an optional timeout in milliseconds (up to 600000ms / 10 minutes). By default, your command will timeout after 120000ms (2 minutes).
- When issuing multiple commands:
  - If the commands are independent and can run in parallel, make multiple bash tool calls in a single message. Example: if you need to run "git status" and "git diff", send a single message with two bash tool calls in parallel.
  - If the commands depend on each other and must run sequentially, use a single bash call with '&&' to chain them together.
  - Use ';' only when you need to run commands sequentially but don't care if earlier commands fail.
  - DO NOT use newlines to separate commands (newlines are ok in quoted strings).
- For git commands:
  - Prefer to create a new commit rather than amending an existing commit.
  - Before running destructive operations (e.g., git reset --hard, git push --force, git checkout --), consider whether there is a safer alternative that achieves the same goal.
  - Never skip hooks (--no-verify) or bypass signing (--no-gpg-sign) unless the user has explicitly asked for it.
- Avoid unnecessary `sleep` commands:
  - Do not sleep between commands that can run immediately — just run them.
  - If your command is long running and you would like to be notified when it finishes — use `run_in_background`. No sleep needed.
  - Do not retry failing commands in a sleep loop — diagnose the root cause.

# Security

- NEVER run commands that could destroy data (rm -rf /, mkfs, etc.)
- Be careful with sudo commands
- Don't pipe curl/wget output directly to shell
- NEVER update the git config
- NEVER run destructive git commands (push --force, reset --hard, checkout ., restore ., clean -f, branch -D) unless the user explicitly requests these actions
- NEVER commit changes unless the user explicitly asks you to
"#;

/// Read 工具提示
///
/// 借鉴 Claude Code FileReadTool/prompt.ts 的核心内容
pub const READ_PROMPT: &str = r#"
Reads a file from the local filesystem. You can access any file directly by using this tool.
Assume this tool is able to read all files on the machine. If the User provides a path to a file assume that path is valid. It is okay to read a file that does not exist; an error will be returned.

Usage:
- The file_path parameter must be an absolute path, not a relative path
- By default, it reads up to 2000 lines starting from the beginning of the file
- You can optionally specify a line offset and limit (especially handy for long files), but it's recommended to read the whole file by not providing these parameters
- When you already know which part of the file you need, only read that part. This can be important for larger files.
- Results are returned using cat -n format, with line numbers starting at 1
- This tool can only read files, not directories. To read a directory, use an ls command via the bash tool.
- You will regularly be asked to read screenshots. If the user provides a path to a screenshot, ALWAYS use this tool to view the file at the path.
- If you read a file that exists but has empty contents you will receive a system reminder warning in place of file contents.

Parameters:
- path: The absolute path to the file to read
- offset: The line number to start reading from (1-based, optional)
- limit: The number of lines to read (default: 2000)
"#;

/// Write 工具提示
///
/// 借鉴 Claude Code FileWriteTool/prompt.ts 的核心内容
pub const WRITE_PROMPT: &str = r#"
Writes a file to the local filesystem.

Usage:
- This tool will overwrite the existing file if there is one at the provided path.
- If this is an existing file, you MUST use the read tool first to read the file's contents. This tool will fail if you did not read the file first.
- Prefer the Edit tool for modifying existing files — it only sends the diff. Only use this tool to create new files or for complete rewrites.
- NEVER create documentation files (*.md) or README files unless explicitly requested by the User.
- Only use emojis if the user explicitly requests it. Avoid writing emojis to files unless asked.

Parameters:
- path: The absolute path to the file to write (must be absolute, not relative)
- content: The content to write to the file
"#;

/// Edit 工具提示
///
/// 借鉴 Claude Code FileEditTool/prompt.ts 的核心内容
pub const EDIT_PROMPT: &str = r#"
Performs exact string replacements in files.

Usage:
- You must use your `read` tool at least once in the conversation before editing. This tool will error if you attempt an edit without reading the file.
- When editing text from Read tool output, ensure you preserve the exact indentation (tabs/spaces) as it appears AFTER the line number prefix. The line number prefix format is: spaces + line number + arrow. Everything after that is the actual file content to match. Never include any part of the line number prefix in the old_string or new_string.
- ALWAYS prefer editing existing files in the codebase. NEVER write new files unless explicitly required.
- Only use emojis if the user explicitly requests it. Avoid adding emojis to files unless asked.
- The edit will FAIL if `old_string` is not unique in the file. Either provide a larger string with more surrounding context to make it unique or use `replace_all` to change every instance of `old_string`.
- Use `replace_all` for replacing and renaming strings across the file. This parameter is useful if you want to rename a variable for instance.

Parameters:
- path: The absolute path to the file to edit
- old_text: The text to search for (must be unique in the file unless replace_all is true)
- new_text: The text to replace it with
- replace_all: If true, replace all occurrences (default: false)
- mode: Edit mode - "replace", "insert", or "write" (default: "replace")
"#;

/// Grep 工具提示
///
/// 借鉴 Claude Code GrepTool/prompt.ts 的核心内容
pub const GREP_PROMPT: &str = r#"
A powerful search tool built on ripgrep

  Usage:
  - ALWAYS use grep for search tasks. NEVER invoke `grep` or `rg` as a bash command. The grep tool has been optimized for correct permissions and access.
  - Supports full regex syntax (e.g., "log.*Error", "function\s+\w+")
  - Filter files with glob parameter (e.g., "*.js", "**/*.tsx") or type parameter (e.g., "js", "py", "rust")
  - Output modes: "content" shows matching lines, "files_with_matches" shows only file paths (default), "count" shows match counts
  - Pattern syntax: Uses ripgrep (not grep) - literal braces need escaping (use `interface\{\}` to find `interface{}` in Go code)
  - Multiline matching: By default patterns match within single lines only. For cross-line patterns like `struct \{[\s\S]*?field`, use `multiline: true`

Parameters:
- pattern: The regular expression pattern to search for
- path: The directory to search in (default: current directory)
- case_insensitive: Whether to search case-insensitively (default: false)
- context: Number of context lines to show around matches (default: 0)
- file_type: Filter by file type (e.g., "js", "py", "rust")
- include: Glob pattern for files to include (e.g., "*.js")
- exclude: Glob pattern for files to exclude
"#;

/// Glob 工具提示
///
/// 借鉴 Claude Code GlobTool/prompt.ts 的核心内容
pub const GLOB_PROMPT: &str = r#"
- Fast file pattern matching tool that works with any codebase size
- Supports glob patterns like "**/*.js" or "src/**/*.ts"
- Returns matching file paths sorted by modification time
- Use this tool when you need to find files by name patterns
- When you are doing an open ended search that may require multiple rounds of globbing and grepping, use the Agent tool instead

Parameters:
- pattern: The glob pattern to match files against (e.g., "*.js", "**/*.ts")
- path: The directory to search in (default: current directory)
- exclude: Glob pattern for files to exclude
"#;

/// WebSearch 工具提示
///
/// 借鉴 Claude Code WebSearchTool/prompt.ts 的核心内容
pub const WEB_SEARCH_PROMPT: &str = r#"
- Allows Claude to search the web and use the results to inform responses
- Provides up-to-date information for current events and recent data
- Returns search result information formatted as search result blocks, including links as markdown hyperlinks
- Use this tool for accessing information beyond Claude's knowledge cutoff
- Searches are performed automatically within a single API call

CRITICAL REQUIREMENT - You MUST follow this:
  - After answering the user's question, you MUST include a "Sources:" section at the end of your response
  - In the Sources section, list all relevant URLs from the search results as markdown hyperlinks: [Title](URL)
  - This is MANDATORY - never skip including sources in your response

Usage notes:
  - Domain filtering is supported to include or block specific websites
  - Use the correct year in search queries for recent information

Parameters:
- query: The search query string
"#;

/// WebFetch 工具提示
///
/// 借鉴 Claude Code WebFetchTool/prompt.ts 的核心内容
pub const WEB_FETCH_PROMPT: &str = r#"
- Fetches content from a specified URL and returns its contents in a readable markdown format
- Use this tool when you need to retrieve and analyze web content
- The URL must be a fully-formed valid URL
- HTTP URLs will be automatically upgraded to HTTPS
- This tool is read-only and will not work for requests intended to have side effects
- Results may be truncated if the content is very large
- Includes a self-cleaning 15-minute cache for faster responses when repeatedly accessing the same URL
- For GitHub URLs, prefer using the gh CLI via Bash instead (e.g., gh pr view, gh issue view, gh api)

Parameters:
- url: The URL to fetch content from
"#;

/// LSP 工具提示
pub const LSP_PROMPT: &str = r#"
Language Server Protocol tool for code intelligence.

**Actions:**
- goto_definition: Jump to the definition of a symbol at the given position
- find_references: Find all references to a symbol
- get_diagnostics: Get compiler/linter diagnostics for a file
- hover: Get hover information at a position

**Parameters:**
- action: One of "goto_definition", "find_references", "get_diagnostics", "hover"
- file_path: Absolute path to the file
- line: Line number (1-based)
- column: Column number (1-based)

**Usage notes:**
- Use this tool when you need code navigation or analysis
- Falls back to grep-based search if no LSP server is available
"#;

/// Skill 工具提示
pub const SKILL_PROMPT: &str = r#"
Execute a predefined skill (automated workflow).

**Available skills:**
- commit: Analyze git changes and create a commit
- test: Run tests and analyze results
- review: Code review of current changes
- fix: Auto-fix lint/check errors
- refactor: Analyze code and suggest refactoring

**Parameters:**
- skill: Name of the skill to execute

**Usage notes:**
- Skills provide structured prompts for common workflows
- The LLM follows the skill's prompt template to complete the task
"#;

/// 获取所有工具定义（LLM 面向的 JSON Schema）
///
/// 返回包含 name、description、parameters 的工具定义数组
pub fn get_tool_definitions() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "name": "bash",
            "description": BASH_PROMPT,
            "parameters": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "要执行的 shell 命令" },
                    "timeout": { "type": "number", "description": "超时秒数，默认 120" },
                    "working_dir": { "type": "string", "description": "工作目录" }
                },
                "required": ["command"]
            }
        }),
        serde_json::json!({
            "name": "read",
            "description": READ_PROMPT,
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件的绝对路径" },
                    "offset": { "type": "number", "description": "起始行号（从 1 开始）" },
                    "limit": { "type": "number", "description": "最大读取行数" },
                    "show_line_numbers": { "type": "boolean", "description": "是否显示行号" }
                },
                "required": ["path"]
            }
        }),
        serde_json::json!({
            "name": "write",
            "description": WRITE_PROMPT,
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件的绝对路径" },
                    "content": { "type": "string", "description": "文件内容" }
                },
                "required": ["path", "content"]
            }
        }),
        serde_json::json!({
            "name": "edit",
            "description": EDIT_PROMPT,
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件的绝对路径" },
                    "old_text": { "type": "string", "description": "要搜索的文本" },
                    "new_text": { "type": "string", "description": "替换文本" },
                    "replace_all": { "type": "boolean", "description": "是否替换所有匹配" },
                    "mode": { "type": "string", "enum": ["replace", "insert", "write"], "description": "编辑模式" }
                },
                "required": ["path"]
            }
        }),
        serde_json::json!({
            "name": "grep",
            "description": GREP_PROMPT,
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "搜索模式（支持正则）" },
                    "path": { "type": "string", "description": "搜索目录" },
                    "case_insensitive": { "type": "boolean", "description": "是否忽略大小写" },
                    "context": { "type": "number", "description": "上下文行数" },
                    "file_type": { "type": "string", "description": "文件类型过滤" },
                    "include": { "type": "string", "description": "包含的文件模式" },
                    "exclude": { "type": "string", "description": "排除的文件模式" }
                },
                "required": ["pattern"]
            }
        }),
        serde_json::json!({
            "name": "glob",
            "description": GLOB_PROMPT,
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "glob 模式" },
                    "path": { "type": "string", "description": "搜索目录" },
                    "exclude": { "type": "string", "description": "排除的文件模式" }
                },
                "required": ["pattern"]
            }
        }),
        serde_json::json!({
            "name": "web_search",
            "description": WEB_SEARCH_PROMPT,
            "parameters": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "搜索查询字符串" }
                },
                "required": ["query"]
            }
        }),
        serde_json::json!({
            "name": "web_fetch",
            "description": WEB_FETCH_PROMPT,
            "parameters": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "要抓取的 URL" }
                },
                "required": ["url"]
            }
        }),
        serde_json::json!({
            "name": "lsp",
            "description": LSP_PROMPT,
            "parameters": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["goto_definition", "find_references", "get_diagnostics", "hover"], "description": "LSP 操作" },
                    "file_path": { "type": "string", "description": "文件绝对路径" },
                    "line": { "type": "number", "description": "行号（从 1 开始）" },
                    "column": { "type": "number", "description": "列号（从 1 开始）" }
                },
                "required": ["action", "file_path"]
            }
        }),
        serde_json::json!({
            "name": "skill",
            "description": SKILL_PROMPT,
            "parameters": {
                "type": "object",
                "properties": {
                    "skill": { "type": "string", "enum": ["commit", "test", "review", "fix", "refactor"], "description": "技能名称" }
                },
                "required": ["skill"]
            }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_prompts_non_empty() {
        assert!(!BASH_PROMPT.is_empty());
        assert!(!READ_PROMPT.is_empty());
        assert!(!WRITE_PROMPT.is_empty());
        assert!(!EDIT_PROMPT.is_empty());
        assert!(!GREP_PROMPT.is_empty());
        assert!(!GLOB_PROMPT.is_empty());
        assert!(!WEB_SEARCH_PROMPT.is_empty());
        assert!(!WEB_FETCH_PROMPT.is_empty());
        assert!(!LSP_PROMPT.is_empty());
        assert!(!SKILL_PROMPT.is_empty());
    }

    #[test]
    fn test_prompts_contain_key_instructions() {
        // Bash: 应包含工具偏好指引
        assert!(BASH_PROMPT.contains("Avoid using this tool"));
        assert!(BASH_PROMPT.contains("File search: Use glob"));
        assert!(BASH_PROMPT.contains("Content search: Use grep"));

        // Read: 应包含先读后写约束提示
        assert!(READ_PROMPT.contains("2000 lines"));
        assert!(READ_PROMPT.contains("line numbers starting at 1"));

        // Write: 应包含先读后写约束
        assert!(WRITE_PROMPT.contains("MUST use the read tool first"));

        // Edit: 应包含先读后写约束
        assert!(EDIT_PROMPT.contains("must use your `read` tool at least once"));

        // Grep: 应包含 ripgrep 提示
        assert!(GREP_PROMPT.contains("ripgrep"));

        // Glob: 应包含 glob 模式提示
        assert!(GLOB_PROMPT.contains("glob patterns"));

        // WebSearch: 应包含 Sources 要求
        assert!(WEB_SEARCH_PROMPT.contains("Sources:"));

        // WebFetch: 应包含 URL 要求
        assert!(WEB_FETCH_PROMPT.contains("fully-formed valid URL"));
    }

    #[test]
    fn test_get_tool_definitions() {
        let defs = get_tool_definitions();
        assert_eq!(defs.len(), 10);

        let names: Vec<&str> = defs
            .iter()
            .filter_map(|d| d["name"].as_str())
            .collect();
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"read"));
        assert!(names.contains(&"write"));
        assert!(names.contains(&"edit"));
        assert!(names.contains(&"grep"));
        assert!(names.contains(&"glob"));
        assert!(names.contains(&"web_search"));
        assert!(names.contains(&"web_fetch"));
        assert!(names.contains(&"lsp"));
        assert!(names.contains(&"skill"));
    }

    #[test]
    fn test_tool_definitions_have_required_fields() {
        for def in get_tool_definitions() {
            assert!(def["name"].is_string(), "Missing name in {:?}", def);
            assert!(def["description"].is_string(), "Missing description in {:?}", def);
            assert!(def["parameters"].is_object(), "Missing parameters in {:?}", def);
            assert!(def["parameters"]["properties"].is_object(), "Missing properties in {:?}", def);
        }
    }
}
