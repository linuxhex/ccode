# [CCODE-005 续] 工具层名实对齐 实现计划

**目标：** 修复 ccore 工具层「声明 ≠ 注册」的主路径断裂，让模型看到的每个工具都能真实执行。

**架构：** 以 `bridge.rs` 的 `tool_definitions()` 为唯一权威契约（已被 `thinker` 的 `CORE_TOOLS` 白名单和全部测试印证），executor 侧向其对齐；无法兑现的声明从契约中移除。

**技术栈：** Rust

---

## 缺陷证据链（全部实测）

```
ToolNode.sys/spawn → bridge.tool_definitions()  [24 个定义]
  → 发布 tool/register (tool.rs:712-728)
    → ThinkerNode 接收 → config.tools (thinker/mod.rs:1853-1856)
      → 发给模型（CORE_TOOLS 白名单同用 read_file/write_file/search_replace，thinker/mod.rs:432-434）
        → 模型调用 read_file
          → ToolNode.handle_tool_call → bridge.execute("read_file") (tool.rs:547)
            → executors.get("read_file") → None
              → 返回「未知工具：read_file」success=false (bridge.rs:957-973)
```

关键事实：`bridge.rs:844-846` 的 `register_executor` 直接以 `executor.name()` 为 key，**无任何别名/规范化层**（已 grep 确认 alias/normalize/canonical 全空）。

## 缺陷清单

| # | 级别 | 缺陷 | 证据 |
|---|------|------|------|
| 1 | critical | `read_file`/`write_file`/`search_replace` 声明了但 executor 名为 `read`/`write`/`edit`，模型调用必失败 | `builtin.rs:365,444,487` vs `bridge.rs:272,322,373` |
| 2 | critical | `search_replace` 参数契约断裂：声明 `search`/`replace`，executor 读 `old_text`/`new_text` | `bridge.rs:413-414` vs `builtin.rs:493` |
| 3 | critical | 12 个工具空声明（无 executor 且无 ToolNode 拦截） | 见下方明细 |
| 4 | minor | `glob`/`skill` 有 executor 但未在 definitions 中，模型永不调用 | `builtin.rs:811`, `skill.rs:92` |
| 5 | minor | `thinker/mod.rs:441` `&summary[..77]` 按字节切，中文描述会 panic | 工程内已有 `floor_char_boundary` 惯例 |
| 6 | minor | `prompts.rs:258-284` 第三套定义仍用旧名 `read`/`write`/`edit`，与权威契约冲突 | `prompts.rs` `get_tool_definitions()` |
| 7 | critical | `git_snapshot_hook`/`rustfmt_hook` 的 `should_run` 按旧名 `write`/`edit` 判断，改名后静默失效 | 推演收敛第 2 轮自问自答发现 |

**缺陷 3 明细**——声明了但完全无法执行的 12 个：
`ask_user_question`、`enter_plan_mode`、`exit_plan_mode`、`todo`、`memory_search`、`memory_get`、`update_goal`、`scheduler`、`monitor`、`mcp`、`image_gen`、`video_gen`

（`tool_search`、`task` 由 `node/tool.rs:468,509` 单独拦截，正常可用，不在此列）

## 决策记录

用户离席，按推荐方案决策，待事后确认：

| 决策点 | 选择 | 理由 |
|--------|------|------|
| 缺陷 1 修法 | 改 executor 的 `name()` 对齐声明名 | `bridge.rs:1287-1297`、`node/tool.rs:814-839` 全部测试用 `read_file`/`write_file`/`search_replace`，`thinker` 的 `CORE_TOOLS` 白名单同理——设计意图就是这套名，executor 才是偏离 |
| 缺陷 2 修法 | executor 兼容两套参数名（`search`/`replace` 优先，回退 `old_text`/`new_text`） | 声明是权威，但回退保证既有调用方不炸 |
| 缺陷 3 修法 | 从 definitions 移除空声明 | 让名实相符，不骗模型；需要哪个再实现哪个 |

---

## 文件清单

| 文件 | 操作 | 职责 |
|------|------|------|
| `crates/codegen/ccore/src/tools/builtin.rs` | 修改 | 3 个 executor 改名 + `search_replace` 参数兼容 |
| `crates/codegen/ccore/src/tools/bridge.rs` | 修改 | 移除 12 个空声明 + 补 `glob`/`skill` 声明 |
| `crates/codegen/ccore/src/tools/prompts.rs` | 修改 | 第三套定义对齐权威名 |
| `crates/codegen/ccore/src/node/thinker/mod.rs` | 修改 | UTF-8 安全截断 |

---

## 任务拆分

### 任务 1：executor 改名对齐声明（critical）

**目标**：`read`→`read_file`、`write`→`write_file`、`edit`→`search_replace`

**文件**：修改 `crates/codegen/ccore/src/tools/builtin.rs`

**实现要点**：
- `ReadExecutor::name()` 返回 `"read_file"`
- `WriteExecutor::name()` 返回 `"write_file"`
- `EditExecutor::name()` 返回 `"search_replace"`
- 同步更新各 executor 内部错误信息里的工具名前缀（`edit: 缺少 path` → `search_replace: 缺少 path`），保持报错可追溯
- `permission.rs:205-228` 已同时匹配新旧两套名，无需改动（已确认）

---

### 任务 2：search_replace 参数契约对齐（critical）

**目标**：让声明的 `search`/`replace` 真实生效

**文件**：修改 `crates/codegen/ccore/src/tools/builtin.rs` 的 `EditExecutor::execute_replace`

**实现要点**：
- 参数读取改为：先取 `search`，缺失时回退 `old_text`；先取 `replace`，缺失时回退 `new_text`
- 两者都缺失才报错，错误信息提示权威参数名 `search`/`replace`
- `mode` 参数保留（声明中无 `mode`，默认 `replace` 即声明行为，向后兼容 insert/write 模式）

**核心逻辑示意**：
```rust
let old_text = args["search"].as_str()
    .or_else(|| args["old_text"].as_str())
    .ok_or_else(|| anyhow::anyhow!("search_replace: 缺少 search 参数"))?;
```

---

### 任务 3：移除空声明 + 补齐遗漏声明（critical + minor）

**目标**：definitions 与可执行能力严格一致

**文件**：修改 `crates/codegen/ccore/src/tools/bridge.rs`

**实现要点**：
- 移除 12 个无 executor 的声明（`ask_user_question`、`enter_plan_mode`、`exit_plan_mode`、`todo`、`memory_search`、`memory_get`、`update_goal`、`scheduler`、`monitor`、`mcp`、`image_gen`、`video_gen`）
- 新增 `glob` 声明（executor 在 `builtin.rs:748`，参数需与 `GlobExecutor` 实读一致）
- 新增 `skill` 声明（executor 在 `skill.rs:92`）
- 保留 `tool_search`、`task`（由 ToolNode 单独拦截，可用）
- 移除前对每个工具名全工程 grep，确认无生产代码依赖该声明

---

### 任务 4：UTF-8 安全截断（minor）

**目标**：消除中文工具描述触发 panic 的风险

**文件**：修改 `crates/codegen/ccore/src/node/thinker/mod.rs:440-441`

**实现要点**：
- `&summary[..77]` 改为 `&summary[..summary.floor_char_boundary(77)]`
- 与工程既有惯例一致（`node/tool.rs:559`、`prompt/agents_md.rs:101` 均用此法）

---

### 任务 5：prompts.rs 第三套定义对齐（minor）

**目标**：消除三套定义并存导致的 system prompt 与实际工具名不一致

**文件**：修改 `crates/codegen/ccore/src/tools/prompts.rs`

**实现要点**：
- `get_tool_definitions()` 中 `read`/`write`/`edit` 改为 `read_file`/`write_file`/`search_replace`
- 同步更新 `prompts.rs:436-438` 的断言
- 该函数被 `prompt/context.rs:488` 用于生成 system prompt 的 Tools section，名字必须与真实契约一致

---

**注意**：不需要编写测试用例，测试由后续的逻辑推演替代。

---

### 任务 6：修复钩子按旧名判断（critical，推演收敛阶段发现）

**目标**：让 `git_snapshot_hook`（写前 git 快照）和 `rustfmt_hook`（写后格式化）在改名后仍能触发

**文件**：
- 修改：`crates/codegen/ccore/src/tools/git_snapshot_hook.rs:107-109`
- 修改：`crates/codegen/ccore/src/tools/rustfmt_hook.rs:19-21`

**实现要点**：
- `should_run` 从 `"write"`/`"edit"` 改为 `"write_file"`/`"search_replace"`
- 这类缺陷编译和测试都不报——钩子只是静默不触发，`git_snapshot_hook` 失效意味着**丢失写操作前的回滚保护**

---

## 排除项（核实后确认不需改动）

| 位置 | 判定 | 理由 |
|------|------|------|
| `permission.rs:205-206,228` | 无需改 | 新旧名都已覆盖，新名分级正确 |
| `builtin.rs:482` `"write" =>` | 无需改 | 这是 `mode` 参数值（写入模式），不是工具名 |
| `hook_adapter.rs:318` | 无需改 | 测试内自造 payload，与 bridge 注册无关 |
| `ccore-advanced/mcp_server/tool_registry.rs:99,113,129` | **不动** | 独立自洽契约：自己声明 `read`/`write`/`edit`，自己分派同名，不经过 `ccore::tools::bridge`。这是对外 MCP server 接口，改动会破坏外部 MCP 客户端 |
| `read_tracker` | 无需改 | 按文件路径追踪，不按工具名 |
| `can_spawn_concurrent` | 无需改 | 走 `bridge.get_entry()` 查 definitions，用的就是权威名 |

## 验证结果

| 项 | 结果 |
|----|------|
| `cargo check -p ccore` | 通过（1 个 warning，来自本次未触及的 `tui/mod.rs:23` unused import） |
| `cargo test -p ccore --lib` | **348 passed / 0 failed** |
| `cargo check --workspace` | 通过，零错误 |
| 声明↔executor 对齐 | 差集仅剩 `task`/`tool_search`（ToolNode 拦截）+ `nonexistent_tool`（`#[cfg(test)]` 内） |

**新增的契约不变式测试**（防止此类断裂复发）：
- `test_every_declared_tool_is_executable` — 每个声明给模型的工具都必须有 executor
- `test_every_executor_is_declared` — 每个 executor 都必须被声明，否则模型看不见

原 `test_default_tools_registered` 断言 `tool_count() >= 20` 已改为 `>= 10`：该断言只数条数，**此前 24 个里有 12 个是不可执行的空声明，断言通过恰恰掩盖了主路径断裂**。
