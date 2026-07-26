# Agent 全面增强实现计划

## 目标
将 Claude Code 源码中提取的优秀设计模式融合到 ccode，实现 8 大增强模块。

## 架构概述
在现有 ccode-tools/ccode-hooks/ccode-compaction/ccode-memory 基础上，新增权限规则引擎、搜索工具、上下文智能、微压缩、会话持久化、子代理白名单、自动记忆整合。

## 文件清单

| 文件 | 操作 | 职责 |
|------|------|------|
| crates/codegen/ccode-tools/src/implementations/ccode_build/github_search/mod.rs | 新增 | GitHub 代码搜索工具 |
| crates/codegen/ccode-tools/src/implementations/ccode_build/deps_search/mod.rs | 新增 | 依赖搜索工具 |
| crates/codegen/ccode-hooks/src/permission_rules.rs | 新增 | 权限规则引擎 |
| crates/codegen/ccode-hooks/src/matcher.rs | 新增 | Hook matcher 过滤 |
| crates/codegen/ccode-compaction/src/micro_compact.rs | 新增 | 微压缩层 |
| crates/codegen/ccode-shell/src/session/storage.rs | 修改 | JSONL 会话持久化 |
| crates/codegen/ccode-shell/src/session/rewind.rs | 新增 | 会话回退 |
| crates/codegen/ccode-tools/src/implementations/ccode_build/context_layer/mod.rs | 新增 | 上下文冷热分层 |
| crates/codegen/ccode-tools/src/implementations/ccode_build/context_layer/types.rs | 新增 | 上下文类型定义 |
| crates/codegen/ccode-memory/src/auto_extract.rs | 新增 | 自动记忆提取 |
| crates/codegen/ccode-memory/src/consolidation.rs | 新增 | 记忆整合 |
| crates/codegen/ccode-tools/src/implementations/ccode_build/agent_whitelist.rs | 新增 | 子代理工具白名单 |

---

## 任务拆分

### 任务 1：权限规则引擎

**目标**：实现 allow/deny/ask 规则引擎，支持通配符匹配

**文件**：
- 新增：`crates/codegen/ccode-hooks/src/permission_rules.rs`
- 修改：`crates/codegen/ccode-hooks/src/lib.rs`

**实现要点**：
- PermissionRule 枚举：Allow/Deny/Ask，各带 pattern 字段
- Pattern 匹配：支持 `Bash(git *)`、`Write(src/**)`、`Read(*)` 语法
- 规则评估：按优先级 deny > ask > allow 评估
- JSON 配置：从 `~/.ccode/rules.json` 或 `.ccode/rules/` 加载

**核心逻辑示意**：
```rust
pub enum PermissionRule {
    Allow { pattern: ToolPattern },
    Deny  { pattern: ToolPattern },
    Ask   { pattern: ToolPattern },
}
pub fn evaluate(rules: &[PermissionRule], tool: &str, input: &Value) -> PermissionDecision
```

---

### 任务 2：GitHub 代码搜索工具

**目标**：Agent 能搜索 GitHub 上的代码实现

**文件**：
- 新增：`crates/codegen/ccode-tools/src/implementations/ccode_build/github_search/mod.rs`
- 修改：`crates/codegen/ccode-tools/src/implementations/ccode_build/mod.rs`

**实现要点**：
- 输入参数：query（搜索关键词）、language（语言过滤）、sort（stars/updated）
- 调用 GitHub Search API：`https://api.github.com/search/code`
- 结果筛选：stars 数、更新时间、license
- 输出 Top5 推荐 + 理由 + 适配性分析
- 无 token 时降级为 WebSearch

**核心逻辑示意**：
```rust
pub struct GitHubSearchInput { query: String, language: Option<String>, sort: Option<String> }
pub struct GitHubSearchResult { items: Vec<CodeResult>, recommendation: String }
```

---

### 任务 3：依赖搜索工具

**目标**：搜索 crates.io/npm/pypi 找现成依赖

**文件**：
- 新增：`crates/codegen/ccode-tools/src/implementations/ccode_build/deps_search/mod.rs`
- 修改：`crates/codegen/ccode-tools/src/implementations/ccode_build/mod.rs`

**实现要点**：
- 输入参数：query、registry（crates/npm/pypi）、limit
- 调用对应 registry API 搜索
- 结果评估：下载量、最近更新、license、安全告警
- 输出 Top3 推荐 + 理由

**核心逻辑示意**：
```rust
pub struct DepsSearchInput { query: String, registry: Registry, limit: Option<usize> }
pub enum Registry { Crates, Npm, Pypi }
```

---

### 任务 4：上下文冷热分层 + 滑动窗口

**目标**：实现 Hot/Warm/Cold 三层上下文调度 + 错误纠正标注

**文件**：
- 新增：`crates/codegen/ccode-tools/src/implementations/ccode_build/context_layer/mod.rs`
- 新增：`crates/codegen/ccode-tools/src/implementations/ccode_build/context_layer/types.rs`

**实现要点**：
- ContextZone 枚举：Hot（最近 N 轮完整保留）、Warm（压缩摘要）、Cold（长期记忆按需检索）
- 滑动窗口：Hot 区固定大小，超出按语义价值分流
- 错误纠正标注：CorrectedKnowledge 类型，标注 `⚠️ 已纠正：原说法→正确说法`
- 语义价值评估：Decision/Correction = 高价值，Chat/Trial = 低价值

**核心逻辑示意**：
```rust
pub enum ContextZone { Hot { window_size: usize }, Warm, Cold }
pub struct CorrectedKnowledge { original: String, corrected: String, reason: String }
pub fn classify_message(msg: &Message) -> SemanticValue { /* 高/中/低 */ }
```

---

### 任务 5：微压缩层（MicroCompact）

**目标**：按时间清除旧工具结果，保留语义

**文件**：
- 新增：`crates/codegen/ccode-compaction/src/micro_compact.rs`
- 修改：`crates/codegen/ccode-compaction/src/lib.rs`

**实现要点**：
- COMPACTABLE_TOOLS 白名单：FileRead、Bash、Grep、Glob、WebSearch、WebFetch、FileEdit、FileWrite
- 按时间清除策略：超过 N 分钟的工具结果替换为 `[Old tool result content cleared]`
- 保留语义摘要：只清除原始输出，保留工具调用的意图和结论
- 集成到现有压缩管道

**核心逻辑示意**：
```rust
const COMPACTABLE_TOOLS: &[&str] = &["FileRead", "Bash", "Grep", "Glob", "WebSearch", "WebFetch", "FileEdit", "FileWrite"];
pub fn micro_compact(messages: &[Message], max_age: Duration) -> Vec<Message>
```

---

### 任务 6：会话持久化 + Rewind

**目标**：JSONL 会话存储 + uuid 消息标识 + rewind 截断

**文件**：
- 修改：`crates/codegen/ccode-shell/src/session/storage/mod.rs`
- 新增：`crates/codegen/ccode-shell/src/session/rewind.rs`

**实现要点**：
- 每条消息带 uuid 标识
- JSONL 格式存储：`~/.ccode/projects/<project-path>/sessions/<session-id>.jsonl`
- rewind：找到指定 uuid，截断后续消息，保留压缩摘要
- resume：加载 JSONL 文件恢复完整对话历史

**核心逻辑示意**：
```rust
pub fn save_message(session_id: &str, msg: &Message) -> Result<()>
pub fn rewind_to(session_id: &str, target_uuid: &str) -> Result<Vec<Message>>
pub fn resume_session(session_id: &str) -> Result<Vec<Message>>
```

---

### 任务 7：自动记忆整合

**目标**：会话结束时自动提取关键知识 + 后台定期整合

**文件**：
- 新增：`crates/codegen/ccode-memory/src/auto_extract.rs`
- 新增：`crates/codegen/ccode-memory/src/consolidation.rs`

**实现要点**：
- auto_extract：会话结束时扫描对话，提取决策/约束/已排除方案/用户偏好
- consolidation：跨会话去重、提炼摘要，写入 `~/.ccode/projects/<path>/memory/`
- PID 文件锁防止多实例并发整合
- 互斥检测：如果主 Agent 已写记忆文件，跳过提取

**核心逻辑示意**：
```rust
pub fn extract_knowledge(messages: &[Message]) -> Vec<KnowledgeItem>
pub async fn consolidate_memories(project_path: &Path) -> Result<()>
```

---

### 任务 8：子代理工具白名单

**目标**：子代理不能递归创建子代理、不能交互式提问

**文件**：
- 新增：`crates/codegen/ccode-tools/src/implementations/ccode_build/agent_whitelist.rs`
- 修改：`crates/codegen/ccode-tools/src/implementations/ccode_build/mod.rs`

**实现要点**：
- AGENT_DISALLOWED_TOOLS：AgentTool、ExitPlanMode、EnterPlanMode、AskUserQuestion、TaskStop
- ASYNC_AGENT_ALLOWED_TOOLS：FileRead、WebSearch、TodoWrite、Grep、WebFetch、Glob、Shell、FileEdit、FileWrite、Skill
- 工具注册时声明 tool_level：ReadWrite / Interactive / Orchestrator
- 过滤函数：filter_tools_for_agent(agent_type, all_tools) -> Vec<Tool>

**核心逻辑示意**：
```rust
pub enum ToolLevel { ReadWrite, Interactive, Orchestrator }
pub fn filter_tools_for_agent(level: ToolLevel, tools: &[ToolMeta]) -> Vec<ToolMeta>
```
