# 死代码接线计划 — 从"图纸"到"能力"

## 目标
将 ccore 中 12 个已实现但零调用的模块接到 ThinkerNode 的实际执行路径上，不写新模块，只接线。

## 架构概述
- 所有接线点在 ThinkerNode（ccore/src/node/thinker.rs，1290 行）
- 接线方式：use 引入 → 结构体字段 → 实例化 → 真调方法
- 三大循环通过字段持有 + handle_message 分发驱动
- Context Engine 通过 IntentRetriever 持有 RepoMap + VectorStore
- 高级认知通过 Kernel getter 延迟获取

## 文件清单

| 文件 | 操作 | 职责 |
|------|------|------|
| crates/codegen/ccore/src/node/thinker.rs | 修改 | 接线三大循环 + Context Engine + 高级认知 |
| crates/codegen/ccore/src/agent/mod.rs | 修改 | pub mod goal_loop/schedule_loop/proactive_loop（确认已导出） |
| crates/codegen/ccore/src/memory/mod.rs | 修改 | pub mod intent_retriever/repo_map/function_embed/vector_store（确认已导出） |

---

## 任务拆分

### 任务 1：ThinkerNode 接线三大循环（P0）

**目标**：/goal /schedule /proactive 命令触发真实循环驱动，而非只打日志

**文件**：
- 修改：`crates/codegen/ccore/src/node/thinker.rs`

**实现要点**：
1. 在 ThinkerNode 结构体新增 3 个字段：
   - `goal_loop: Option<GoalLoop>` — 当前活跃的目标循环
   - `schedule_loop: Option<ScheduleLoop>` — 当前活跃的定时循环
   - `proactive_loop: Option<ProactiveLoop>` — 主动扫描循环（一直活跃）
2. 在 `new()` 中初始化：goal_loop=None, schedule_loop=None, proactive_loop=Some(ProactiveLoop::new(ProactiveSpec::default()))
3. handle_message 中 `/goal` 分支：真调 `GoalLoop::from_description()` 存入字段，注入 Planning 提示到 WorkingMemory
4. handle_message 中 `/schedule` 分支：解析间隔参数，真调 `ScheduleLoop::from_description()` 存入字段
5. handle_message 中 `/proactive` 分支：真调 `self.proactive_loop.on_start_scan()`
6. 在 `handle_message` 末尾增加三大循环驱动逻辑：
   - GoalLoop：根据 state 调 on_subtasks_planned/on_turn_complete/on_verification_result
   - ProactiveLoop：should_scan() 时调 on_start_scan/on_scan_complete/on_repair_complete
7. ScheduleLoop 的定时器驱动：在 `start()` 中 spawn 一个后台 tokio task，用 `tokio::time::interval` 定期检查 `should_execute_now()`，为 true 时向 ThinkerNode 发送 `agent/{id}/input` 消息触发执行
8. 每个循环产生的 GoalAction/ScheduleAction/ProactiveAction 转化为 WorkingMemory 注入 + send_sample_request

**核心逻辑示意**：
```rust
// /goal 分支：真调而非只打日志
if content.starts_with("/goal ") {
    let goal = &content[6..];
    self.goal_loop = Some(GoalLoop::from_description(goal.to_string()));
    // 注入规划提示，让 LLM 返回子任务列表
    self.working_memory.push_system(
        format!("[GoalLoop] 请将以下目标拆解为子任务列表：{}", goal),
        Self::estimate_tokens(goal),
    );
}
```

---

### 任务 2：ThinkerNode 接线 Context Engine（P0）

**目标**：build_sample_request 时真调 IntentRetriever/RepoMap/VectorStore

**文件**：
- 修改：`crates/codegen/ccore/src/node/thinker.rs`

**实现要点**：
1. 在 ThinkerNode 新增字段：
   - `intent_retriever: IntentRetriever` — 意图检索器
2. 在 `new()` 中初始化：`IntentRetriever::new(EmbeddingIndex::new())`
3. 修改 `build_sample_request()`：在构建 messages 前，用 intent_retriever 检索相关上下文注入
4. 修改 `listen()`：在注入长期记忆后，额外调 intent_retriever.search_by_intents() 注入代码级上下文
5. 添加 `build_context_from_retriever()` 辅助方法：
   - 调 `IntentRetriever::expand_intents(query)` 扩展查询意图
   - 调 `self.intent_retriever.search_by_intents(&intents, 5)` 检索相关代码块
   - 将结果作为 system 消息注入 WorkingMemory

**核心逻辑示意**：
```rust
// listen() 中检索代码级上下文
let intents = IntentRetriever::expand_intents(content);
let results = self.intent_retriever.search_by_intents(&intents, 5);
for result in results {
    self.working_memory.push_system(
        format!("[代码上下文] {} (相似度:{:.2})", result.summary, result.score),
        Self::estimate_tokens(&result.summary),
    );
}
```

---

### 任务 3：ThinkerNode 接线高级认知（P1）

**目标**：meta_cognitive/experiential/decentralized 在 ThinkerNode 中被调用

**文件**：
- 修改：`crates/codegen/ccore/src/node/thinker.rs`

**实现要点**：
1. 在 ThinkerNode 新增可选字段：
   - `kernel_handle: Option<std::sync::Arc<crate::kernel::Kernel>>` — Kernel 引用（延迟注入）
2. 在 `handle_message` 的 turn 结束逻辑中（update_context_window 之后）：
   - 调 `self.kernel_handle.as_ref().map(|k| k.meta_cognitive())` 获取元认知评估
   - 调 `self.kernel_handle.as_ref().map(|k| k.erl())` 提取经验启发
   - 评估结果注入 WorkingMemory
3. 由于 Kernel getter 返回的 OnceLock 可能为空（未初始化），需 `.get()` 检查
4. 对 decentralized coordinator：在子代理事件中调 coordinator.assign_task()

**注意**：高级认知的 Kernel getter 当前全仓零调用，接线后需要确认 Kernel 初始化时是否真的构建了这些组件。如果 OnceLock 未初始化，`get()` 返回 None，跳过即可。

---

### 任务 4：验证 ToolNode 权限链真执行（P1）

**目标**：确认 check_shell_safety 在 ToolNode 中真执行

**文件**：
- 读取验证：`crates/codegen/ccore/src/node/tool.rs`
- 读取验证：`crates/codegen/ccore/src/tools/permission.rs`

**验证要点**：
1. 确认 tool.rs 中 check_tool_call 路径是否调用了 PermissionChecker.check_shell_safety
2. 确认 permission_rules 的 check_tool_call 方法是否包含 shell 安全检查
3. 如果缺失，在 permission.rs 的 check_tool_call 中加入 check_shell_safety 调用
4. 确认 Bash 工具执行前必经权限链

---

### 任务 5：KernelSession 接线 shell（P2）

**目标**：shell 的 SessionActor 被 KernelSession 替代

**文件**：
- 修改：`crates/codegen/ccode-shell/src/session/ccore_integration.rs`
- 修改：`crates/codegen/ccode-shell/src/session/mod.rs`

**实现要点**：
1. KernelSession.new() 真调 `Kernel::new()` + `Kernel::run()`
2. send_input() 真调 AcpNode 发送 `agent/{id}/input`
3. on_output() 真正从 AcpNode 接收输出并转为 shell 输出
4. McpBridge.call_tool() 真调 `ccode_hub_mcp::McpBridge` 而非返回 Err
5. 在 shell 的 session 入口处：检查 KernelSession 是否可用，可用则走 KernelSession，否则降级到 SessionActor

**注意**：这是最大的接线任务，涉及跨 crate 调用。建议在任务 1-4 完成后再做。

---

### 任务 6：mod.rs 导出确认

**目标**：确认 goal_loop/schedule_loop/proactive_loop/intent_retriever/repo_map/function_embed/vector_store 已在 mod.rs 中导出

**文件**：
- 检查：`crates/codegen/ccore/src/agent/mod.rs`
- 检查：`crates/codegen/ccore/src/memory/mod.rs`

**实现要点**：
1. 确认 agent/mod.rs 包含 `pub mod goal_loop; pub mod schedule_loop; pub mod proactive_loop;`
2. 确认 memory/mod.rs 包含 `pub mod intent_retriever; pub mod repo_map; pub mod function_embed; pub mod vector_store;`
3. 如果缺失，添加导出声明
