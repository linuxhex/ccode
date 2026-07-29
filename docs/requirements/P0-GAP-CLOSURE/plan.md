# P0 核心差距补齐实现计划

**目标：** 补齐 ccode 对标 Claude Code 和 Codex CLI 的 4 项 P0 核心差距，使 ccode 在体验层达到竞品同等水平

**架构：** 在现有 ccode-compaction/ccode-memory/ccode-mcp/ccode-config 基础上，新增 MicroCompact 压缩层、CCODE.md 层级指令、记忆自动提取管线、MCP Server 模式

**技术栈：** Rust + rmcp (MCP Server) + ccode-memory (BM25+embedding) + ccode-fsnotify (文件监听)

---

## 交付阶段

### 阶段 A：MicroCompact 压缩层 — 省空间
- Budget Reduction（工具输出 token 预算）
- Snip（超长输出截断）
- MicroCompact（按时间清除旧工具结果）
- Cache-Aware 压缩
- Auto-Compact 触发阈值

### 阶段 B：CCODE.md 层级指令 — 记知识
- 层级收集算法（全局→项目→.ccode→子目录）
- @import 引用展开
- 自动记忆写入
- 文件变更监听热更新

### 阶段 C：记忆自动提取管线 — 积知识
- 会话结束自动提取（fork 子代理）
- 后台定期整合（去重+提炼）
- 冷区→热区动态注入
- 与现有 Dream 整合统一

### 阶段 D：MCP Server 模式 — 被编排
- MCP Server 逻辑（工具暴露+请求处理）
- stdio/SSE 传输层
- 安全控制（沙箱+白名单+权限规则）
- CLI 启动入口

---

## 文件清单

| 文件 | 操作 | 职责 |
|------|------|------|
| crates/common/ccode-compaction/src/micro_compact.rs | 新增 | MicroCompact 按时间清除旧工具结果 |
| crates/common/ccode-compaction/src/budget.rs | 新增 | Budget Reduction 工具输出 token 预算 |
| crates/common/ccode-compaction/src/snip.rs | 新增 | Snip 超长输出截断 |
| crates/common/ccode-compaction/src/context_collapse.rs | 新增 | Context Collapse 大范围摘要压缩 |
| crates/common/ccode-compaction/src/cache_aware.rs | 新增 | Cache-Aware 压缩 + cache_edits |
| crates/common/ccode-compaction/src/compactable.rs | 新增 | COMPACTABLE_TOOLS 白名单定义 |
| crates/common/ccode-compaction/src/lib.rs | 修改 | 导出新增模块 |
| crates/codegen/ccode-config/src/claude_md.rs | 新增 | CCODE.md 层级收集 + @import 展开 |
| crates/codegen/ccode-config/src/lib.rs | 修改 | 导出 claude_md 模块 |
| crates/codegen/ccode-memory/src/auto_extract.rs | 修改 | 完善自动记忆提取逻辑 |
| crates/codegen/ccode-memory/src/consolidation.rs | 修改 | 完善后台定期整合逻辑 |
| crates/codegen/ccode-memory/src/inject.rs | 新增 | 冷区→热区动态注入 |
| crates/codegen/ccode-memory/src/lib.rs | 修改 | 导出 inject 模块 |
| crates/codegen/ccode-mcp/src/server.rs | 新增 | MCP Server 逻辑 |
| crates/codegen/ccode-mcp/src/server_transport.rs | 新增 | stdio/SSE 传输层 |
| crates/codegen/ccode-mcp/src/server_tools.rs | 新增 | MCP Server 暴露的工具定义 |
| crates/codegen/ccode-mcp/src/lib.rs | 修改 | 导出 server 模块 |
| crates/codegen/ccode-cli/src/main.rs | 修改 | 新增 --mcp-server 参数 |

---

## 任务拆分

### 任务 1：COMPACTABLE_TOOLS 白名单 + Budget Reduction

**目标**：定义可压缩工具白名单，实现工具输出 token 预算

**文件**：
- 新增：`crates/common/ccode-compaction/src/compactable.rs`
- 新增：`crates/common/ccode-compaction/src/budget.rs`
- 修改：`crates/common/ccode-compaction/src/lib.rs`

**实现要点**：
- `COMPACTABLE_TOOLS` 常量集合：FileRead, Bash, Grep, Glob, WebSearch, WebFetch, FileEdit, FileWrite
- `ToolBudget` 结构体：tool_name → max_output_tokens 映射
- 默认预算：FileRead=4000, Bash=8000, Grep=4000, Glob=2000, WebSearch=2000, WebFetch=4000
- `budget_check(tool_name, output_tokens) → bool`：超预算返回 false，触发截断
- `truncate_output(output, budget) → (truncated, saved_tokens)`：截断并标注

**核心逻辑示意**：
```rust
pub const COMPACTABLE_TOOLS: &[&str] = &[
    "FileRead", "Bash", "Grep", "Glob",
    "WebSearch", "WebFetch", "FileEdit", "FileWrite",
];

pub struct ToolBudget {
    limits: HashMap<String, usize>,
}

impl ToolBudget {
    pub fn default_budgets() -> Self { /* 默认预算 */ }
    pub fn check(&self, tool: &str, tokens: usize) -> BudgetResult { /* 预算检查 */ }
    pub fn truncate(&self, output: &str, budget: usize) -> String { /* 截断 */ }
}
```

---

### 任务 2：Snip 超长输出截断

**目标**：截断超长的工具输出，保留首尾

**文件**：
- 新增：`crates/common/ccode-compaction/src/snip.rs`

**实现要点**：
- `SnipConfig`：每工具的截断阈值（行数/字符数/匹配数）
- 默认阈值：FileRead=2000行, Bash=10000字符, Grep=500匹配, Glob=1000条目
- 截断策略：保留首 N 行/字符 + 尾 M 行/字符，中间用 `[... K lines/chars omitted ...]` 标注
- `snip(tool_name, output, config) -> SnipResult`：执行截断

**核心逻辑示意**：
```rust
pub struct SnipConfig {
    pub file_read_max_lines: usize,    // 2000
    pub bash_max_chars: usize,         // 10000
    pub grep_max_matches: usize,       // 500
    pub glob_max_entries: usize,       // 1000
}

pub struct SnipResult {
    pub output: String,
    pub original_tokens: usize,
    pub saved_tokens: usize,
}

pub fn snip(tool: &str, output: &str, config: &SnipConfig) -> SnipResult { /* 截断 */ }
```

---

### 任务 3：MicroCompact 按时间清除

**目标**：按时间清除旧工具结果，保留最近 K 轮完整结果

**文件**：
- 新增：`crates/common/ccode-compaction/src/micro_compact.rs`

**实现要点**：
- `MicroCompactConfig`：hot_turns（保留最近 K 轮，默认 5）、time_threshold（超过 N 轮清除，默认 10）
- `micro_compact(conversation, config) -> CompactedConversation`：
  1. 遍历对话历史中的工具调用结果
  2. 只处理 COMPACTABLE_TOOLS 中的工具
  3. 最近 K 轮内的结果保留完整
  4. 超过 K 轮但 < N 轮的，替换为摘要占位符：`[MicroCompact: {tool_name} result, {original_tokens} tokens saved]`
  5. 超过 N 轮的，直接清除
- 保留非 COMPACTABLE 工具的完整结果（如 Agent、Task、TodoWrite 等）

**核心逻辑示意**：
```rust
pub struct MicroCompactConfig {
    pub hot_turns: usize,         // 5 — 最近 5 轮保留完整
    pub time_threshold: usize,    // 10 — 超过 10 轮直接清除
}

pub fn micro_compact(
    items: &[ConversationItem],
    current_turn: usize,
    config: &MicroCompactConfig,
) -> Vec<ConversationItem> { /* 按时间清除 */ }
```

---

### 任务 4：Cache-Aware 压缩 + Context Collapse

**目标**：利用 prompt cache 避免重复压缩，超长对话做 LLM 摘要

**文件**：
- 新增：`crates/common/ccode-compaction/src/cache_aware.rs`
- 新增：`crates/common/ccode-compaction/src/context_collapse.rs`

**实现要点**：

**Cache-Aware**：
- `CacheAwareCompactor`：跟踪已压缩的内容 hash
- `cached_micro_compact(items, cache_state) -> (compacted, cache_edits)`：
  - 已缓存的内容跳过压缩
  - 新压缩的内容通过 `cache_edits` 参数注入 API 请求
  - 利用 Anthropic prompt cache 节省 token

**Context Collapse**：
- `ContextCollapseConfig`：collapse_threshold（触发阈值，默认 85% context window）、max_summary_tokens（摘要预算，默认 2000）
- `context_collapse(conversation, llm_client, config) -> CollapsedConversation`：
  1. 当 MicroCompact 后仍超 token 预算
  2. 将对话历史分为：保留段（最近 K 轮）+ 压缩段（早期轮次）
  3. 对压缩段调用 LLM 生成摘要
  4. 摘要保留：关键决策、错误纠正、约束条件、已排除方案
  5. 摘要丢弃：闲聊、试错、重复
  6. 返回：摘要 + 保留段

**Auto-Compact 触发**：
- 在 Agent 循环中检查 token 使用率
- >85% 时自动触发：Snip → MicroCompact → Cache-Aware → Context Collapse
- 压缩前执行 PreCompact 钩子，压缩后执行 PostCompact 钩子

---

### 任务 5：CCODE.md 层级收集 + @import

**目标**：实现层级化指令文件收集和引用展开

**文件**：
- 新增：`crates/codegen/ccode-config/src/claude_md.rs`
- 修改：`crates/codegen/ccode-config/src/lib.rs`

**实现要点**：
- `ClaudeMdCollector`：层级收集器
- `collect(workspace_root, current_dir, home_dir) -> Vec<ClaudeMdFile>`：
  1. 加载全局 `~/.ccode/CLAUDE.md`
  2. 从项目根到当前目录，收集所有 `CLAUDE.md` 和 `.ccode/CLAUDE.md`
  3. 按层级排序：全局 → 项目根 → .ccode → 子目录
- `expand_imports(content, base_dir, depth) -> String`：
  - 解析 `@filename.md` 语法
  - 递归展开引用文件内容
  - 深度限制（max_depth=5）防止循环引用
  - 循环检测：维护已访问文件集合
- `merge(files) -> String`：合并所有层级内容，后者覆盖前者
- 注入位置：系统提示的 `project_instructions` 段

**核心逻辑示意**：
```rust
pub struct ClaudeMdFile {
    pub path: PathBuf,
    pub level: ClaudeMdLevel,  // Global / ProjectRoot / ProjectLocal / Subdirectory
    pub content: String,
}

pub struct ClaudeMdCollector {
    max_import_depth: usize,  // 5
}

impl ClaudeMdCollector {
    pub fn collect(&self, workspace_root: &Path, current_dir: &Path, home_dir: &Path) -> Vec<ClaudeMdFile> { /* 层级收集 */ }
    pub fn expand_imports(&self, content: &str, base_dir: &Path) -> Result<String> { /* @import 展开 */ }
    pub fn merge(&self, files: &[ClaudeMdFile]) -> String { /* 合并 */ }
}
```

---

### 任务 6：CCODE.md 热更新 + 自动记忆写入

**目标**：文件变更时自动重新加载，Agent 可写入记忆到 CCODE.md

**文件**：
- 修改：`crates/codegen/ccode-config/src/claude_md.rs`（新增热更新逻辑）
- 复用：`crates/codegen/ccode-fsnotify`（文件变更监听）

**实现要点**：
- `ClaudeMdWatcher`：监听所有 CLAUDE.md 文件变更
- 文件变更时触发重新收集和合并
- 新内容注入 Agent 上下文（下一轮生效）
- 自动记忆写入：
  - 写入位置：`~/.ccode/projects/<blake3(workspace_path)>/memory/`
  - 格式：Markdown + frontmatter（name, description, metadata.type）
  - 写入时机：Agent 学习到项目知识时自动写入，或用户 `/memory` 命令手动写入

---

### 任务 7：会话结束自动记忆提取

**目标**：会话结束时 fork 子代理提取关键知识

**文件**：
- 修改：`crates/codegen/ccode-memory/src/auto_extract.rs`
- 修改：`crates/codegen/ccode-memory/src/lib.rs`

**实现要点**：
- `AutoExtractor`：自动提取器
- `extract_on_session_end(session, config) -> Result<Vec<RawMemory>>`：
  1. 触发时机：SessionEnd 钩子 或 `/dream` 命令
  2. 构建提取 prompt：从对话历史中提取关键知识
  3. Fork 当前会话作为子代理执行提取
  4. 提取内容：决策、约束、已排除方案、错误纠正、用户偏好、架构发现
  5. 产出：`raw_memory` Markdown 文件
  6. 写入：`~/.ccode/projects/<hash>/memory/raw/<timestamp>.md`
- 提取 prompt 模板（复用 extraction_prompt.md）
- 互斥检测：如果主 Agent 已写了记忆文件，跳过提取

**核心逻辑示意**：
```rust
pub struct AutoExtractor {
    memory_dir: PathBuf,
    max_extract_tokens: usize,  // 2000
}

impl AutoExtractor {
    pub async fn extract_on_session_end(&self, session: &Session) -> Result<Vec<RawMemory>> { /* fork 子代理提取 */ }
    pub async fn extract_manual(&self, session: &Session) -> Result<Vec<RawMemory>> { /* /dream 手动触发 */ }
}
```

---

### 任务 8：后台定期整合 + 冷区→热区注入

**目标**：后台定期整合记忆，按需注入热区

**文件**：
- 修改：`crates/codegen/ccode-memory/src/consolidation.rs`
- 新增：`crates/codegen/ccode-memory/src/inject.rs`

**实现要点**：

**后台整合**：
- `Consolidator`：后台整合器
- `consolidate(memory_dir, lock) -> Result<ConsolidationReport>`：
  1. 获取分布式锁（复用 dream_lock）
  2. 扫描所有 `raw_memory` 文件
  3. BM25 + embedding 混合检索找相似记忆
  4. 语义相似度 > 阈值的记忆合并
  5. 多条相关记忆提炼为更精炼摘要
  6. 写入整合后的 `MEMORY.md`
  7. 释放锁
- 触发时机：空闲时自动触发，或 `/dream` 命令

**冷区→热区注入**：
- `MemoryInjector`：记忆注入器
- `injectrelevant(query, memory_store, token_budget) -> Vec<RecalledMemory>`：
  1. 从长期记忆 BM25+embedding 检索与 query 相关的记忆
  2. 按 MMR 去重排序
  3. 贪心填充直到 token 预算用完
  4. 返回注入内容
- 注入位置：系统提示的 `recalled_memory` 段
- 用完即回收：注入的记忆在下一轮不自动保留

---

### 任务 9：MCP Server 逻辑 + 工具暴露

**目标**：ccode 自身可作为 MCP 服务器

**文件**：
- 新增：`crates/codegen/ccode-mcp/src/server.rs`
- 新增：`crates/codegen/ccode-mcp/src/server_tools.rs`
- 修改：`crates/codegen/ccode-mcp/src/lib.rs`

**实现要点**：
- `CcodeMcpServer`：MCP Server 主结构
- 暴露的工具：
  - `ccode_read`：读取文件（复用 FileRead 工具）
  - `ccode_write`：写入文件（复用 FileWrite 工具）
  - `ccode_edit`：编辑文件（复用 SearchReplace 工具）
  - `ccode_search`：搜索代码（复用 Grep/Glob 工具）
  - `ccode_bash`：执行命令（默认不暴露，需显式配置）
  - `ccode_memory`：检索记忆（复用 ccode-memory search）
  - `ccode_code_graph`：代码图谱查询（复用 ccode-graph）
- 每个工具定义：name, description, inputSchema (JSON Schema)
- 请求处理：接收 MCP tool_call → 调用对应 ccode 工具 → 返回结果

**核心逻辑示意**：
```rust
pub struct CcodeMcpServer {
    tools: HashMap<String, McpToolDef>,
    sandbox: SandboxConfig,
    permissions: PermissionRules,
}

impl CcodeMcpServer {
    pub async fn handle_tool_call(&self, name: &str, input: Value) -> Result<McpToolResult> { /* 分发到对应工具 */ }
    pub async fn list_tools(&self) -> Vec<McpToolDef> { /* 返回工具列表 */ }
}
```

---

### 任务 10：MCP Server 传输层 + CLI 入口

**目标**：stdio/SSE 传输层 + CLI 启动参数

**文件**：
- 新增：`crates/codegen/ccode-mcp/src/server_transport.rs`
- 修改：`crates/codegen/ccode-cli/src/main.rs`

**实现要点**：

**stdio 传输**：
- 标准输入/输出通信，适用于本地 MCP 客户端调用
- JSON-RPC 消息格式
- 生命周期：initialize → tools/list → tools/call → shutdown

**SSE 传输**：
- HTTP SSE 通信，适用于远程调用
- 端点：`/sse`（SSE 连接）、`/messages`（发送消息）
- 需指定端口：`--mcp-server --port 8080`

**CLI 入口**：
- `ccode --mcp-server`：以 MCP Server 模式启动（stdio 传输）
- `ccode --mcp-server --port 8080`：以 MCP Server 模式启动（SSE 传输）
- `ccode mcp serve`：子命令形式

**安全控制**：
- MCP Server 模式默认使用最严格沙箱（workspace-write）
- `ccode_bash` 工具默认不暴露，需 `--allow-bash` 显式启用
- 权限规则复用 permission_rules 引擎
- 连接认证：API Key（`--mcp-api-key`）或 OAuth

---

## 验证方案

### 阶段 A 验证（MicroCompact）
1. 构造 50 轮对话历史，验证 MicroCompact 正确清除旧工具结果
2. 验证最近 5 轮结果完整保留
3. 验证 Snip 截断超长输出（>2000行文件读取）
4. 验证 Budget Reduction 超预算截断
5. 验证 Auto-Compact 在 85% token 阈值自动触发

### 阶段 B 验证（CCODE.md）
1. 创建多层级 CLAUDE.md 文件，验证收集顺序正确
2. 验证 @import 引用展开，包括嵌套引用
3. 验证循环引用检测（A→B→A 报错而非死循环）
4. 验证文件变更时热更新生效
5. 验证内容正确注入系统提示

### 阶段 C 验证（记忆提取）
1. 模拟会话结束，验证自动提取产出 raw_memory 文件
2. 验证分布式锁防止多实例同时整合
3. 验证整合后 MEMORY.md 内容正确（去重+提炼）
4. 验证冷区→热区注入：查询相关问题时记忆被检索并注入
5. 验证用完即回收：注入的记忆在下一轮不自动保留

### 阶段 D 验证（MCP Server）
1. 启动 `ccode --mcp-server`，用 MCP 客户端连接
2. 验证 `tools/list` 返回正确的工具列表
3. 验证 `ccode_read` 正确读取文件
4. 验证 `ccode_search` 正确搜索代码
5. 验证沙箱限制：MCP 模式下不能读取工作区外文件
6. 验证 SSE 传输模式远程调用
