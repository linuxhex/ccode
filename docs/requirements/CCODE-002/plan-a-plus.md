# Agent 写代码能力 A+ 提升 实现计划

**目标：** 实现 5 个关键缺口，将 agent 写代码能力从 A- 提升到 A+

**架构：** 在工具执行层添加 post_hook 和事务机制，在 turn 结束层添加编译反馈和 review，在 agent 层添加 Doom Loop 逃脱策略

**技术栈：** Rust, tokio, serde, ccore/ccode-shell

---

## 文件清单

| 文件 | 操作 | 职责 |
|------|------|------|
| `ccore/src/tools/compile_feedback.rs` | 新增 | cargo check 错误解析 + 注入 |
| `ccore/src/tools/file_transaction.rs` | 新增 | 文件级备份 + 回滚事务 |
| `ccore/src/tools/builtin.rs` | 修改 | Write/Edit 集成 post_hook + 事务 |
| `ccore/src/tools/bridge.rs` | 修改 | post_execute_hooks 机制 |
| `ccore/src/tools/mod.rs` | 修改 | 导出新模块 |
| `ccore/src/agent/doom_loop.rs` | 修改 | 扩展 DoomLoopResult + EscapeAction |
| `ccore/src/node/agent.rs` | 修改 | Doom Loop 逃脱执行 + 工具禁用 |
| `ccode-shell/src/session/acp_session_impl/turn.rs` | 修改 | handle_turn_end 集成编译反馈 |
| `ccore/src/config/mod.rs` | 修改 | 添加 auto_review 配置项 |

---

## 任务拆分

### 任务 1：文件事务管理器（多文件事务）

**目标**：实现文件级备份 + 失败回滚的事务机制

**文件**：
- 新增：`crates/codegen/ccore/src/tools/file_transaction.rs`
- 修改：`crates/codegen/ccore/src/tools/mod.rs`

**实现要点**：
- FileTransaction 结构体：管理事务内的文件备份
- `begin()` 开启事务，`backup(path)` 备份文件原始内容，`commit()` 提交，`rollback()` 回滚
- 备份存储在内存 HashMap<PathBuf, Option<Vec<u8>>>（None 表示文件原本不存在）
- rollback 逐文件恢复：有备份则写回，无备份则删除

**核心逻辑示意**：
```rust
pub struct FileTransaction {
    backups: HashMap<PathBuf, Option<Vec<u8>>>,
    active: bool,
}
// backup: 读原始内容存入 backups
// rollback: 遍历 backups，恢复每个文件
```

---

### 任务 2：编译反馈模块（编译反馈闭环）

**目标**：解析 cargo check 输出，格式化为可注入 agent 上下文的错误信息

**文件**：
- 新增：`crates/codegen/ccore/src/tools/compile_feedback.rs`
- 修改：`crates/codegen/ccore/src/tools/mod.rs`

**实现要点**：
- `run_cargo_check(workdir) -> Result<CompileReport>` 异步运行 cargo check
- 解析 `--message-format=json` 输出，提取 error/warning 级别消息
- CompileReport 包含 errors 列表（文件:行号:消息）
- `format_for_injection(report) -> String` 格式化为 system prompt 注入文本
- 轻量检查：`run_rustfmt_check(filepath) -> Result<Option<String>>` 单文件检查

**核心逻辑示意**：
```rust
pub struct CompileReport {
    pub errors: Vec<CompileError>,
    pub success: bool,
}
// run_cargo_check: tokio::Command::new("cargo").args(["check","--message-format=json"])
// format_for_injection: "编译发现以下错误:\n1. src/main.rs:10 类型不匹配..."
```

---

### 任务 3：post_hook 机制 + 增量验证

**目标**：在工具执行后添加钩子，Write/Edit 后自动轻量检查

**文件**：
- 修改：`crates/codegen/ccore/src/tools/bridge.rs`
- 修改：`crates/codegen/ccore/src/tools/builtin.rs`

**实现要点**：
- ToolBridge 添加 `post_hooks: Vec<Box<dyn PostExecuteHook>>`
- PostExecuteHook trait：`fn should_run(&self, tool_name: &str) -> bool` + `fn run(&self, result: &ToolResult) -> HookOutput`
- RustfmtHook：对 Write/Edit 工具的结果，提取文件路径，运行 rustfmt --check
- 钩子输出追加到工具结果，反馈给 agent

**核心逻辑示意**：
```rust
pub trait PostExecuteHook: Send + Sync {
    fn should_run(&self, tool_name: &str) -> bool;
    fn run(&self, result: &ToolResult) -> String;
}
// RustfmtHook: 解析 Write 的 file_path 参数，运行 rustfmt --check
```

---

### 任务 4：Doom Loop 逃脱策略

**目标**：检测到循环后注入提示 + 禁用重复工具 + 降级模型

**文件**：
- 修改：`crates/codegen/ccore/src/agent/doom_loop.rs`
- 修改：`crates/codegen/ccore/src/node/agent.rs`

**实现要点**：
- DoomLoopResult 添加 `escape_action: Option<EscapeAction>` 字段
- EscapeAction 枚举：`InjectHint(String)` + `DisableTool(String)` + `DegradeModel`
- detect() 检测到循环时，生成逃脱动作
- agent.rs 中检测到 doom loop 后：
  1. 注入提示到 working_memory
  2. 从 tool_definitions 中移除重复工具
  3. 降低 reasoning_effort（High → Medium → Low）

**核心逻辑示意**：
```rust
pub enum EscapeAction {
    InjectHint(String),
    DisableTool(String),
    DegradeModel,
}
// detect: 返回 escape_action 列表
// agent: 应用所有 escape_action
```

---

### 任务 5：编译反馈集成到 turn 主循环

**目标**：在 handle_turn_end 中集成完整 cargo check，错误注入下一轮

**文件**：
- 修改：`crates/codegen/ccode-shell/src/session/acp_session_impl/turn.rs`

**实现要点**：
- handle_turn_end 中：检查 turn 是否有 Write/Edit 调用
- 有则异步运行 `run_cargo_check(workdir)`
- 解析 CompileReport，若有 errors，格式化为 system prompt
- 注入到下一轮 LLM 推理的 working_memory
- 自动 review（可配置）：若 auto_review=true，turn 结束后触发 review 技能

**核心逻辑示意**：
```rust
// handle_turn_end:
if has_write_edit {
    let report = run_cargo_check(workdir).await;
    if !report.success {
        let hint = format_for_injection(report);
        inject_to_next_turn(hint);
    }
}
```

---

### 任务 6：配置项 + 集成验证

**目标**：添加 auto_review 配置项，验证全工程编译

**文件**：
- 修改：`crates/codegen/ccore/src/config/mod.rs`

**实现要点**：
- AgentConfig 或 CcodeConfig 添加 `auto_review: bool`（默认 false）
- 添加 `cargo_check_on_turn_end: bool`（默认 true）
- 添加 `rustfmt_on_write: bool`（默认 true）

**注意**：不需要编写测试用例，测试由后续的逻辑推演替代。
