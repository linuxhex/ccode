# [CCODE-005] 单架构收敛 & 竞品优势整合 实现计划

**目标：** 消除 ccode 双引擎分裂，以 shell 路径为唯一产品主线，单进程内集成 agent 引擎与 ratatui TUI，交互体验对齐 Claude Code，同时吸收 codex / grok-build / claude-code / opencode 四方架构优势。

**架构：** 三阶段渐进推进 — 阶段 1 入口接通 + 能力通电 → 阶段 2 TUI 升级（对齐 Claude Code 交互）+ 结构收敛 → 阶段 3 吸收增强（搬高级特性）。

**技术栈：** Rust, tokio, ratatui, crossterm
**TUI 参考：** Claude Code 交互体验（信息密度、规整度）+ codex tui crate（27 万行 ratatui 实现，同为 Rust 可借鉴架构）

---

## 文件清单（阶段 1 细化，阶段 2/3 方向级）

| 文件 | 操作 | 职责 |
|------|------|------|
| `crates/codegen/ccode-shell/src/session/acp_session_impl/turn.rs` | 修改 | 接入工具输出预算、稳定前缀锁定 |
| `crates/codegen/ccode-shell/src/agent/mvp_agent/mod.rs` | 修改 | 接入工具延迟加载（summary + tool_search） |
| `crates/codegen/ccode-shell/src/agent/subagent/` | 修改 | 子 Agent 摘要返回 |
| `crates/codegen/ccode-sampler/src/` | 修改 | cache_control 正确序列化 |
| `crates/codegen/ccode-shell/src/session/ccore_integration.rs` | 修改 | 5 个桥接能力真接入主线 |
| `crates/codegen/ccore/src/kernel/` | 删除 | 删除死骨架（消息总线 Broker + NodeLauncher） |
| `crates/codegen/ccore/src/node/` | 部分删除 | 删除 6 Node 框架，保留可复用能力 |
| `crates/codegen/ccore-advanced/` | 拆分删除 | 能力并入 ccore，删除 crate |
| `crates/codegen/ccode-shell/src/session/acp_session.rs` | 修改 | CcoreSessionState 字段改为真使用 |
| `...`（阶段 2/3 后续补充） | — | — |

---

## 阶段 1：通电 + 清理（先把已有能力用起来）

### 任务 0：入口切换——CLI 从 kernel 改为 shell 启动（P0 前置）

**目标**：让 ccode binary 真正启动 shell + pager 引擎，而不是启动 ccore 的简化 TUINode。这是所有后续能力接入的前提。

**文件**：
- 重写：`crates/codegen/ccode-cli/src/main.rs`
- 修改：`crates/codegen/ccode-cli/Cargo.toml`（确认依赖）

**实现要点**：
1. 从 `kernel.run()` 改为调用 `ccode_shell::agent::app::*`
2. 保留并验证以下模式：
   - **TUI 模式**（默认）：启动 leader server + pager 客户端
   - **stdio 模式**：`run_stdio_agent`，兼容 ACP stdio
   - **headless 模式**：`-p` 参数，`run_headless`
3. 原 CLI 参数全部支持：`-m` 模型、`-p` prompt、`--agent` 类型、`--permission`、`--max-turns`、`--reasoning-effort`、`--config`
4. 删除 ffi_mode 分支（ccore FFI 无意义）
5. API 预热逻辑保留，移到合适位置
6. **验证标准**：`cargo run -- -p "hello"` 能正常返回结果（走 LLM + shell 路径）

**核心风险**：
- shell 启动路径依赖 pager 二进制，需确认 pager 是否在同一进程还是子进程
- 如果 pager 需要独立启动，需要类似 grok-build 的 spawn 机制

---

### 任务 1：工具延迟加载迁主线（P0）

**目标**：将 CCODE-004 中做在 ccore 死路径上的「工具延迟加载」能力，迁到 shell 主线的 MvpAgent 上，使其真正生效。

**文件**：
- 修改：`crates/codegen/ccode-shell/src/agent/mvp_agent/mod.rs`
- 修改：`crates/codegen/ccode-sampling-types/src/conversation.rs`（ToolSpec 增加 summary 字段）
- 新增：`crates/codegen/ccode-shell/src/agent/tool_search.rs`（tool_search 执行器）
- 修改：`crates/codegen/ccode-shell/src/session/acp_session_impl/turn.rs`（工具定义注入点）

**实现要点**：
1. `ToolSpec` 增加 `summary: Option<String>` 字段
2. `MvpAgent` 构建 tools 列表时，默认只注入 name + summary（精简 schema，约 50 token/工具）
3. 新增 `tool_search` 内置工具（参数 `tool_names: Vec<String>`），返回完整 schema
4. 模型调用 `tool_search` 后，将返回的完整工具定义追加到下一轮请求的 tools 中
5. 为每个内置工具编写 summary（中文，≤30 字）
6. 复用 ccore 中已有的 `tool_search` 逻辑（从 `ccore/src/tools/bridge.rs` 搬）

**核心逻辑示意**：
```rust
// turn.rs 构建 LLM 请求时
let tools = if self.tools_fully_loaded {
    full_tool_specs()
} else {
    summary_tool_specs()  // 只有 name + summary + tool_search 完整定义
};
```

---

### 任务 2：工具输出预算控制迁主线（P0）

**目标**：将工具输出截断逻辑从 ccore 迁到 shell 主线 turn 循环中。

**文件**：
- 修改：`crates/codegen/ccode-shell/src/session/acp_session_impl/turn.rs`（tool call 后处理）

**实现要点**：
1. 工具执行完成后，检查 `result.output.len()`，超过 24000 字符时截断
2. 保留前 10000 字符 + 截断提示 + 后 10000 字符
3. 对 `task`（子 agent）和 `tool_search` 不截断
4. UTF-8 安全截断（按 char 边界切）
5. 复用 ccore `node/tool.rs` 中已有的截断逻辑

**核心逻辑示意**：
```rust
// 工具执行完成后
if output.len() > TOOL_OUTPUT_BUDGET_CHARS
    && tool_name != "task"
    && tool_name != "tool_search"
{
    let head = char_truncate(&output, 10_000, true);
    let tail = char_truncate(&output, 10_000, false);
    result.output = format!("{head}\n\n[... 截断，共 {len} 字，用 read_file 查看 ...]\n\n{tail}");
}
```

---

### 任务 3：稳定前缀 + cache_control 序列化迁主线（P0）

**目标**：修复 cache_control 断点在 shell 主路径不生效的问题。

**文件**：
- 修改：`crates/codegen/ccode-sampler/src/claude_compat.rs`
- 修改：`crates/codegen/ccode-sampler/src/openai_compat.rs`
- 修改：`crates/codegen/ccode-shell/src/session/acp_session_impl/turn.rs`（工具排序 + 前缀锁定时机）

**实现要点**：
1. 工具定义按 name 字母序排序，确保跨轮次稳定
2. Claude provider：为 messages 和 tools 数组写入 `cache_control: {type: "ephemeral"}` 断点
3. OpenAI provider：为 messages 写入 cache_control（OpenAI 仅支持 message 级别）
4. system prompt + 工具定义在第一轮后锁定为稳定前缀
5. 复用 ccode `ccore/src/sampler/claude_compat.rs` 中已有的序列化逻辑

**注意**：ccode-sampler 是真被主线调用的，需要确认现有实现是否已支持 cache_control。

---

### 任务 4：子 Agent 摘要返回迁主线（P1）

**目标**：子 agent 完成后只向父上下文注入摘要，保护父上下文 token 预算。

**文件**：
- 修改：`crates/codegen/ccode-shell/src/agent/subagent/handle_request.rs`
- 修改：`crates/codegen/ccode-shell/src/agent/subagent/mod.rs`

**实现要点**：
1. 子 agent 输出超过 2000 字符时生成摘要
2. 摘要 = 前 200 字 + ... + 后 200 字 + 提取的文件路径列表
3. 完整输出写入 `.ccode/state/subagent-{id}.md`
4. 父上下文只注入摘要
5. 摘要 ≤ 500 token（约 2000 字符）

---

### 任务 5：推理力度分级接入主线（P1）

**目标**：将 MetaCognitiveController 的难度评估映射到 reasoning_effort，简单任务省 token。

**文件**：
- 修改：`crates/codegen/ccode-shell/src/session/ccore_integration.rs`（真接入 meta_cognitive）
- 修改：`crates/codegen/ccode-shell/src/session/acp_session_impl/turn.rs`（turn 开始时评估难度，设置 reasoning_effort）

**实现要点**：
1. `CcoreSessionState.meta_cognitive` 真使用（目前零调用）
2. 每轮 turn 开始时，根据任务复杂度评估难度等级
3. 将难度映射到 reasoning_effort（Trivial→off, Moderate→0.5, Complex→0.8, Extreme→1.0）
4. 简单任务直接省 50%+ 推理 token
5. 复用 ccore 中 `ccore-advanced/src/agent/meta_cognitive.rs` 的 `to_reasoning_effort()` 方法

---

### 任务 6：CcoreSessionState 5 项桥接能力真接入（P1）

**目标**：让 `ccore_integration.rs` 中 5 个零调用的能力真正接入主线。

**文件**：
- 修改：`crates/codegen/ccode-shell/src/session/ccore_integration.rs`（补充调用入口）
- 修改：`crates/codegen/ccode-shell/src/session/acp_session_impl/turn.rs`（接入 cache_tracker / token_budget）
- 修改：`crates/codegen/ccode-shell/src/session/memory_state.rs`（接入 episodic_memory）
- 修改：`crates/codegen/ccode-shell/src/agent/tools_executor.rs`（接入 read_tracker）

**5 项能力**：
1. `SessionCacheTracker` — 缓存命中率追踪
2. `SessionReadTracker` — 文件读取追踪（防止重复读）
3. `SessionTokenBudget` — token 预算管理
4. `SessionMetaCognitive` — 难度评估（任务 5 已接）
5. `EpisodicMemory` — 情景记忆存储与检索

**实现要点**：
- 逐个接入，每个能力一个 PR 级改动
- 接入后验证：日志埋点可见、状态正常更新
- 不改变主循环结构，只增加旁路能力

---

### 任务 7：删除 ccore 死骨架（P0 清理）

**目标**：删除 ccore 中确认死的消息总线和 Node 框架代码，减少维护负担。任务 0 完成后（入口已切换到 shell）执行。

**文件**：
- 删除：`crates/codegen/ccore/src/kernel/`（消息总线 Broker + NodeLauncher）
- 删除：`crates/codegen/ccore/src/node/transport.rs`（Node ZMQ 传输层）
- 删除：`crates/codegen/ccore/src/node/tui/`（简化 TUI，pager 是真 TUI）
- 删除：`crates/codegen/ccore/src/node/acp.rs`（简化 ACP 桥，shell 有完整 ACP）
- 保留改造：`crates/codegen/ccore/src/node/thinker/`（提取压缩/记忆逻辑后删 Node 框架）
- 保留改造：`crates/codegen/ccore/src/node/tool.rs`（提取工具执行逻辑后删 Node 框架）
- 保留改造：`crates/codegen/ccore/src/node/sampler.rs`（提取采样逻辑后删 Node 框架）
- 待确认：`crates/codegen/ccore/src/message/`（如果只有 kernel 用就一起删）
- 待确认：`crates/codegen/ccore/src/config/`（如果 shell 侧不依赖就删）

**实现要点**：
1. **删除前全量 grep 确认**：对每个待删模块，全工程 grep 引用，确认无其他调用方
2. 提取可复用的纯函数到独立模块（工具截断、缓存断点构造等）
3. 删除 Node 框架代码（spawn、run、消息循环），保留纯能力函数
4. 更新 Cargo.toml，移除对应依赖（如 zmq）

---

### 任务 8：ccore-advanced 拆分并入 ccore（P1 清理）

**目标**：ccore-advanced 里的三大循环（GoalLoop/ScheduleLoop/ProactiveLoop）已经移回 ccore，删除这个过渡 crate。

**文件**：
- 删除：`crates/codegen/ccore-advanced/` 整个 crate
- 确认：所有能力已在 ccore 中存在对应实现

---

## 阶段 2：结构收敛（目录重划分）

> 阶段 1 完成后细化。以下为方向级计划。

### 任务 9：ccore 拆分为细粒度能力 crate
### 任务 10：三套 compaction 合并为一套
### 任务 11：agent 引擎从 shell 抽出为独立 crate
### 任务 12：统一命名规范（消除 ccode-/xai-grok-/ccore 混用）
### 任务 13：crate 分层整理（core / agent / tui / tools / infra / cli）

## 阶段 3：吸收增强（从四方搬高级特性）

> 阶段 2 完成后细化。以下为方向级计划。

### 任务 14：远程压缩 v2（参考 codex compact_remote_v2）
### 任务 15：rollout / feature flag 系统（参考 codex rollout + grok-build）
### 任务 16：沙箱分层安全（参考 codex linux-sandbox / bwrap）
### 任务 17：session_prefix 稳定前缀管理（参考 codex session_prefix）
### 任务 18：App Server 模式（参考 codex app-server，远期可选）

---

**注意**：不需要编写测试用例，测试由后续的逻辑推演替代。
