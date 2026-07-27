# Claude Code 6 项优秀设计集成计划

**目标：** 将 6 项 Claude Code 架构能力完整集成到 ccode，消除安全缺陷和功能缺口

**架构：** 在现有 ccode-shell/ccode-hooks/ccode-agent 架构上增量集成，不破坏现有运行路径

**技术栈：** Rust / Tokio async / ccode-sampler / ccode-hooks

---

## 文件清单

| 文件 | 操作 | 职责 |
|------|------|------|
| crates/codegen/ccode-agent/src/loop_state.rs | 新增 | 显式状态机定义 |
| crates/codegen/ccode-agent/src/lib.rs | 修改 | 导出 loop_state 模块 |
| crates/codegen/ccode-shell/src/session/acp_session_impl/turn.rs | 修改 | 主循环改为状态机驱动 |
| crates/codegen/ccode-hooks/src/permission_chain.rs | 修改 | deny-first 默认决策 |
| crates/codegen/ccode-hooks/src/permission_rules.rs | 修改 | 安全白名单 + deny-first 规则 |
| crates/codegen/ccode-hooks/src/pre_filter.rs | 修改 | 增强预过滤（危险命令列表） |
| crates/codegen/ccode-shell/src/session/acp_session_impl/turn.rs | 修改 | 技能模型切换执行 |
| crates/codegen/ccode-shell/src/session/acp_session_impl/model_switch.rs | 修改 | 新增简化模型切换入口 |
| crates/codegen/ccode-shell/src/session/acp_session_impl/tool_calls.rs | 修改 | toolCallBudget + 循环检测 |

---

## 任务拆分

### 任务 1：显式状态机 queryLoop（P0）

**目标**：引入 LoopState 枚举驱动主循环，使状态可观测、可中断

**文件**：
- 新增：`crates/codegen/ccode-agent/src/loop_state.rs`
- 修改：`crates/codegen/ccode-agent/src/lib.rs`
- 修改：`crates/codegen/ccode-shell/src/session/acp_session_impl/turn.rs`

**实现要点**：
- 定义 `LoopState` 枚举：Idle → CallingLLM → ExecutingTools → WaitingPermission → Done
- 定义 `LoopEvent` 枚举：LLMResponse、ToolExecutionCompleted、PermissionDenied、BudgetExhausted、UserCancelled
- 定义 `LoopAction` 枚举：CallLLM、ExecuteTool、WaitForPermission、EndTurn、ContinueLoop
- 实现 `LoopStateMachine::new()` + `transition(event) -> LoopAction`
- 在 turn.rs 的 `loop {}` 中使用状态机驱动：每次迭代根据当前状态决定动作
- **不改变现有的 process_conversation_turn_with_recovery 调用**，只在循环层包装状态机

**核心逻辑示意**：
```rust
pub enum LoopState { Idle, CallingLLM, ExecutingTools, WaitingPermission, Done }
pub enum LoopAction { CallLLM, ExecuteTool { .. }, WaitForPermission, EndTurn }
impl LoopStateMachine {
    pub fn transition(&mut self, event: LoopEvent) -> LoopAction { ... }
}
```

---

### 任务 2：deny-first 权限模型（P0）

**目标**：默认 Deny，逐层放行，危险命令自动阻断

**文件**：
- 修改：`crates/codegen/ccode-hooks/src/permission_chain.rs`
- 修改：`crates/codegen/ccode-hooks/src/permission_rules.rs`
- 修改：`crates/codegen/ccode-hooks/src/pre_filter.rs`

**实现要点**：
- 在 `evaluate_permission_chain` 中，当规则匹配和 hook 都无决策时，默认返回 Deny（而非 Allow）
- 新增 `PermissionRuleSet::default_deny_rules()` 生成默认 deny 规则
- 增强 `pre_filter` 的危险命令列表：`rm -rf /`、`mkfs`、`dd if=`、`format`、`shutdown`、`reboot`
- 在 `dispatch_pre_tool_use` 中，当 `chain_result` 无明确 Allow 时，应用 deny-first 语义
- 保留 auto_mode 的快速放行路径（白名单内的 Read/Grep 等不受影响）

---

### 任务 3：技能模型切换执行（P0）

**目标**：技能声明 model/effort 后，运行时完成完整模型切换

**文件**：
- 修改：`crates/codegen/ccode-shell/src/session/acp_session_impl/turn.rs`
- 修改：`crates/codegen/ccode-shell/src/session/acp_session_impl/model_switch.rs`

**实现要点**：
- 在 `model_switch.rs` 中新增 `handle_lightweight_model_switch(&self, model: String, effort: Option<String>)` 方法
- 该方法从 `chat_state_handle.get_sampling_config()` 获取当前配置，只修改 model 和 reasoning_effort
- 调用现有的 `handle_set_session_model` 完成完整切换
- 在 turn.rs 中，技能加载后检查 model/effort，调用 `handle_lightweight_model_switch`

**核心逻辑示意**：
```rust
pub async fn handle_lightweight_model_switch(&self, model: String, effort: Option<String>) -> Result<(), acp::Error> {
    let current = self.chat_state_handle.get_sampling_config();
    let mut new_config = current.to_sampler_config();
    new_config.model = model;
    if let Some(e) = effort { new_config.reasoning_effort = parse_effort(&e); }
    self.handle_set_session_model(new_config, false, true, false, 80).await?;
    Ok(())
}
```

---

### 任务 4：7 层上下文压缩 — fileCache 层（P1）

**目标**：避免重复读取消耗 Token，已读文件内容缓存

**文件**：
- 修改：`crates/codegen/ccode-shell/src/session/acp_session_impl/tool_calls.rs`

**实现要点**：
- 在 `execute_tool_calls` 中，当工具是 Read/Bash（cat/head）且返回文件内容时，缓存到 `file_cache: HashMap<PathBuf, String>`
- 后续同一 turn 中再次读取同一文件时，从缓存返回，不触发实际工具调用
- 在 turn 开始时清空 file_cache
- 缓存容量限制（如 50 个文件），超出时 LRU 淘汰

---

### 任务 5：toolCallBudget 配额扣减（P1）

**目标**：单轮工具调用配额，防止成本失控

**文件**：
- 修改：`crates/codegen/ccode-shell/src/session/acp_session_impl/tool_calls.rs`

**实现要点**：
- 在 `execute_tool_calls` 中新增 `tool_call_budget: u32`（默认值从 session config 读取，如 50）
- 每次工具执行前检查 budget > 0，否则跳过并推入 budget_exhausted 消息
- 每次工具执行后 budget -= 1
- budget 耗尽时设置 `final_result = Some(ToolLoop::Continue)`，让模型根据 budget_exhausted 消息决定下一步

---

### 任务 6：循环检测（P1）

**目标**：检测连续相同工具调用循环，触发熔断

**文件**：
- 修改：`crates/codegen/ccode-shell/src/session/acp_session_impl/tool_calls.rs`

**实现要点**：
- 扩展现有 `consecutive_failures` 机制，新增 `recent_tool_calls: VecDeque<(String, u64)>` 记录最近 N 次工具调用的 (tool_name, input_hash)
- 每次工具调用前，计算 (tool_name, input_hash) 并与最近 5 次比较
- 如果连续 5 次完全相同，触发循环检测熔断
- 熔断时推入 loop_detected 消息，建议模型换一种方式

**核心逻辑示意**：
```rust
const LOOP_DETECTION_WINDOW: usize = 5;
let mut recent_calls: VecDeque<(String, u64)> = VecDeque::with_capacity(LOOP_DETECTION_WINDOW);
// 在每次工具调用前：
let input_hash = calculate_hash(&raw_input);
let call_sig = (resolved_tool_name.clone(), input_hash);
recent_calls.push_back(call_sig.clone());
if recent_calls.len() >= LOOP_DETECTION_WINDOW && recent_calls.iter().all(|c| c == &call_sig) {
    // 循环检测熔断
}
```
