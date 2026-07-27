# 9 项 Claude Code 能力端到端推演验证

## 推演验证方法

对每项能力，模拟完整运行路径：用户输入 → Agent 处理 → 模型调用 → 工具执行 → 结果返回，检查每一步代码是否可达、逻辑是否正确。

---

## 1. Hook 输入改写/上下文注入

### 推演路径
1. Hook runner 执行命令 → 输出 JSON → `parse_hook_output` 提取 `updatedInput` / `additionalContext`
2. `run_command_hook` 返回 `HookRewriteInfo { updated_input, additional_context }`
3. `dispatch_pre_tool_use` 累积所有 hook 的 rewrite 到 `combined_rewrite`
4. `tool_calls.rs` 中检查 `combined_rewrite.updated_input` → 替换 raw_input
5. `tool_calls.rs` 中检查 `combined_rewrite.additional_context` → 注入对话

### 潜在问题
- [minor] fileCache 使用 `prepared.parsed_args` 获取文件路径，但 `prepared` 在 dispatch 结果处理循环中可用。如果 prepared 被移动或消费，可能编译失败。

**结论**：✅ 功能可达，逻辑正确

---

## 2. permission_chain 预过滤

### 推演路径
1. `dispatch_pre_tool_use` 被调用，传入 `rules=PermissionRuleSet::load_from_dirs()`
2. `evaluate_permission_chain` 执行：pre_filter → hook → rule → default
3. chain_result 如果是 Deny → 拒绝工具调用

### 潜在问题
- [minor] `PermissionRuleSet::load_from_dirs()` 每次工具调用都执行，I/O 开销大。应该缓存。
- [minor] `load_from_dirs` 的搜索目录列表需要确认是否包含项目根目录

**结论**：✅ 功能可达，需优化 I/O 缓存

---

## 3. 显式状态机 LoopStateMachine

### 推演路径
1. turn.rs 中 `let mut loop_sm = LoopStateMachine::new()`
2. 每次 `process_conversation_turn_with_recovery` 前后，状态机记录 debug 日志
3. 如果模型返回 refusal，状态机 transition 到 Done

### 潜在问题
- **[critical] 状态机只用于日志记录，没有真正驱动循环决策**。当前代码：
  ```rust
  let mut loop_sm = LoopStateMachine::new();
  loop {
      tracing::debug!(loop_state = ?loop_sm.state(), ...);
      let round = self.process_conversation_turn_with_recovery(...).await;
      // 只在 refusal 时 transition
      if let Ok(TurnOutcome::Completed { refusal: Some(_), .. }) = &round {
          let _ = loop_sm.transition(...);  // 返回值被忽略！
      }
      break round;
  }
  ```
  问题：
  1. `transition` 的返回值 `LoopAction` 被丢弃（`let _ =`），状态机没有驱动任何决策
  2. 只处理了 refusal 这一个事件，正常流程（tool_use、end_turn）没有 feed 给状态机
  3. 状态机永远停留在 Idle 或 CallingLLM，从未进入 ExecutingTools
- **影响**：状态机形同虚设，没有实现"可观测、可中断、可恢复"的目标

**结论**：❌ 集成深度不足，状态机只做日志，未驱动决策

---

## 4. deny-first 权限模型

### 推演路径
1. 非 auto_mode → evaluate_permission_chain → 无规则匹配 → 默认 Deny
2. auto_mode → safe_tools（Read/Grep/Glob/LS 等）→ Allow，其余 → Ask
3. pre_filter 拦截 30+ 危险命令

### 潜在问题
- [minor] safe_tools 列表中没有 `Bash`、`Write`、`Edit`，这些在 auto_mode 下会走 Ask 路径，可能导致每次操作都需要确认
- [minor] `Glob` 在 safe_tools 中但可能被滥用（如 `Glob("**/*")` 扫描全盘）

**结论**：✅ 功能可达，deny-first 语义正确

---

## 5. 技能模型切换

### 推演路径
1. 技能加载 → `parsed_skills.first().model/effort` 非空
2. 调用 `handle_lightweight_model_switch(model, effort)`
3. `reconstruct_full_config()` 获取当前完整配置
4. 修改 model 和 reasoning_effort
5. 调用 `handle_set_session_model` 完成切换

### 潜在问题
- **[critical] `reconstruct_full_config` 方法是否存在？** 在 model_switch.rs 中搜索不到此方法定义。如果该方法不存在，编译会失败。
- [minor] 技能模型切换后，system prompt 不会自动更新（`apply_prompt_override=false`）

**结论**：⚠️ 需确认 `reconstruct_full_config` 是否存在

---

## 6. fileCache

### 推演路径
1. Read 工具执行成功 → 从 `prepared.parsed_args` 提取 `file_path`
2. 缓存到 `file_cache: HashMap<PathBuf, String>`
3. 后续读取同一文件时……**问题：缓存没有被使用！**

### 潜在问题
- **[critical] fileCache 只写不读**。代码中只做了 `file_cache.insert()`，但没有在后续 Read 工具调用前检查 `file_cache.get()`。缓存写入了但从未被读取，完全是无效代码。

**结论**：❌ 集成不完整，fileCache 只写不读

---

## 7. toolCallBudget

### 推演路径
1. `tool_call_budget = 50`
2. 每次工具调用前 `tool_call_budget -= 1`
3. budget 耗尽 → push budget_exhausted 消息 → `final_result = Some(ToolLoop::Continue)`

### 潜在问题
- [minor] budget 耗尽后 `continue` 而非 `break`，剩余的工具调用会继续执行 budget=0 的检查，效率略低但不影响正确性
- [minor] DEFAULT_TOOL_CALL_BUDGET 硬编码为 50，不可配置

**结论**：✅ 功能可达，逻辑正确

---

## 8. 循环检测

### 推演路径
1. 每次工具调用前，计算 `(call.function.name, hash(call.function.arguments))`
2. push 到 recent_calls（VecDeque，容量 5）
3. 如果所有 recent_calls 都相同 → 熔断

### 潜在问题
- [minor] `call.function.arguments` 是 String 类型，对其做 hash 可能在不同 JSON 序列化顺序下产生不同 hash。例如 `{"a":1,"b":2}` 和 `{"b":2,"a":1}` 语义相同但 hash 不同。这会导致循环检测漏报。
- [minor] 检测窗口为 5，但 `recent_calls` 是 VecDeque，pop_front 后容量始终为 5。当第 6 次调用时，窗口内只有最近 5 次。逻辑正确。

**结论**：✅ 功能可达，JSON 序列化顺序可能导致漏报（minor）

---

## 9. 连续失败熔断

### 推演路径
1. 工具执行失败 → `consecutive_failures += 1`
2. 达到 `MAX_CONSECUTIVE_TOOL_FAILURES = 3` → 熔断
3. 成功时 `consecutive_failures = 0`

### 潜在问题
- 无。逻辑正确，与循环检测互补。

**结论**：✅ 功能可达，逻辑正确

---

## 汇总

| # | 能力 | 状态 | 问题 |
|---|------|------|------|
| 1 | Hook 输入改写 | ✅ | 无 |
| 2 | permission_chain 预过滤 | ✅ | I/O 缓存待优化 |
| 3 | 显式状态机 | ❌ critical | 只做日志，未驱动决策 |
| 4 | deny-first 权限 | ✅ | safe_tools 列表需调整 |
| 5 | 技能模型切换 | ⚠️ critical | reconstruct_full_config 可能不存在 |
| 6 | fileCache | ❌ critical | 只写不读 |
| 7 | toolCallBudget | ✅ | 不可配置 |
| 8 | 循环检测 | ✅ | JSON 序列化顺序漏报 |
| 9 | 连续失败熔断 | ✅ | 无 |

**需要修复的 3 个 critical 问题**：
1. 状态机未真正驱动决策
2. 技能模型切换的 `reconstruct_full_config` 可能不存在
3. fileCache 只写不读
