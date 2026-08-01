# 冲刺 100+ 分 实现计划

**目标：** 激活 4 层循环（GoalLoop+DoomLoop+LoopStateMachine 全闭环）+ 打磨体验到 9 分

**架构：** 在 ThinkerNode 中闭合 GoalLoop，打磨流式/MCP/文档同步

---

## 任务拆分

### 任务 1：ThinkerNode 闭合 GoalLoop

**目标：** 将 GoalLoop 接入 ThinkerNode 的 turn 循环，激活 4 层循环

**文件**：
- 修改：ccore/src/node/thinker.rs

**实现要点**：
- ThinkerNode 新增 `goal_loop: Option<GoalLoop>` 字段
- 收到 `agent/{id}/goal` 消息时创建 GoalLoop 并进入 Planning 状态
- Turn 完成后调用 `goal_loop.on_turn_complete(success)`
- 验证完成后调用 `goal_loop.on_verification_result(passed)`
- GoalAction 驱动后续行为：ExecuteSubTask 注入工作记忆，GoalComplete 结束目标
- 多 Agent 维度评分预期 6→8

---

### 任务 2：DoomLoop 逃脱动作闭环

**目标：** DoomLoop 检测到的逃脱动作真正生效

**文件**：
- 修改：ccore/src/node/thinker.rs

**实现要点**：
- DoomLoop 检测到循环后，执行 EscapeAction 列表：
  - InjectHint → 注入工作记忆作为 system message
  - DisableTool → 设置 disabled_tool_next_round
  - DegradeModel → 降低 reasoning_effort
- 逃脱动作生效后，继续循环而非直接 EndTurn（给 Agent 一次逃脱机会）

---

### 任务 3：LoopStateMachine ERL 轨迹提取闭环

**目标：** 循环结束时自动提取 TaskTrajectory 供 ERL 使用

**文件**：
- 修改：ccore/src/node/thinker.rs

**实现要点**：
- Turn 结束时调用 `loop_state_machine.extract_trajectory()`
- 通过 bus 发送 `cortex/erl_trajectory` 消息给 Kernel
- Kernel 接收后调用 `erl.extract_heuristics(trajectory)`
- ERL 提取的 heuristics 注入工作记忆

---

### 任务 4：流式渲染 channel 接通

**目标：** sampler_turn → StreamingRenderer channel 真正接通

**文件**：
- 修改：ccode-pager/src/app/acp_handler/mod.rs

**实现要点**：
- ACP 收到 AgentMessageChunk 时，调用 feed_streaming_update
- 确认 channel 从 SamplerNode → ThinkerNode → ACP → StreamingRenderer 全链路

---

### 任务 5：文档同步

**目标：** 修复 workflow-state.json 和关键文档与代码的脱节

**文件**：
- 修改：docs/ 下相关文件

**实现要点**：
- 更新 workflow-state.json 的 tasks_completed
- 确保代码质量维度评分 8→9
