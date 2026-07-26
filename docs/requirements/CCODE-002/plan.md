# 精华融合 + Skill-Model 切换实现计划

**目标：** 清理8个重复模块，将3个真正缺失能力（权限决策链、Hook改写、循环状态机）融入现有架构，实现技能调用时模型切换

**架构：** 删除5个完全重复模块，精简3个模块为接口层集成到现有代码，新增技能模型切换桥接

**技术栈：** Rust，基于现有 ccode-agent / ccode-hooks / ccode-shell / ccode-tools crate

---

## 文件清单

| 文件 | 操作 | 职责 |
|------|------|------|
| ccode-agent/src/fork.rs | 删除 | 与现有 AgentDefinition + 子代理系统完全重复 |
| ccode-agent/src/coordinator.rs | 删除 | 与现有 subagent 编排系统完全重复 |
| ccode-agent/src/task_state.rs | 删除 | 与现有 TaskTool + ccode-subagent 完全重复 |
| ccode-agent/src/background.rs | 删除 | 与现有后台任务 + 通知系统完全重复 |
| ccode-agent/src/prompt_template.rs | 删除 | 与现有 prompt::context 系统大部分重复 |
| ccode-agent/src/loop_state.rs | 修改 | 精简为 LoopStateMachine 核心类型，集成到 agent 模块 |
| ccode-hooks/src/permission_chain.rs | 修改 | 集成到现有 permission_rules，保留决策链和替代方案 |
| ccode-hooks/src/hook_rewrite.rs | 修改 | 集成到现有 hook dispatcher，保留改写和上下文注入 |
| ccode-agent/src/lib.rs | 修改 | 更新模块声明，移除已删除模块 |
| ccode-hooks/src/lib.rs | 修改 | 确认模块声明正确 |
| ccode-tools/src/implementations/ccode_build/skill/mod.rs | 修改 | 技能执行时传递 model 字段 |
| ccode-agent/src/prompt/skills.rs | 修改 | 技能列表注入时包含 model 信息 |

---

## 任务拆分

### 任务 1：删除完全重复的5个模块

**目标：** 移除与现有代码完全重复的 fork、coordinator、task_state、background、prompt_template

**文件：**
- 删除：`ccode-agent/src/fork.rs`
- 删除：`ccode-agent/src/coordinator.rs`
- 删除：`ccode-agent/src/task_state.rs`
- 删除：`ccode-agent/src/background.rs`
- 删除：`ccode-agent/src/prompt_template.rs`

**实现要点：**
- 删除5个文件
- 在 `lib.rs` 中移除对应 mod 声明
- 确认无其他代码引用这些模块的公开类型
- 如果有引用，替换为现有代码中的等价类型

**核心逻辑示意：**
```rust
// lib.rs 修改：移除5个重复模块声明
// 删除: pub mod fork;
// 删除: pub mod coordinator;
// 删除: pub mod task_state;
// 删除: pub mod background;
// 删除: pub mod prompt_template;
```

---

### 任务 2：精简 loop_state.rs 为核心状态机类型

**目标：** 保留循环状态机的核心决策逻辑，移除与现有代码重复的辅助类型

**文件：**
- 修改：`ccode-agent/src/loop_state.rs`

**修改内容：**
- 保留 `AgentLoopStateMachine` 核心类型和 `transition` 方法
- 保留 `LoopState`、`LoopEvent`、`LoopAction`、`FinishReason` 枚举
- 保留 `ErrorKind` 及其退避策略
- 保留 `TokenUsage` 及其 auto-compact 判定
- 保留 `DeniedToolCall` 和 deny recovery
- 移除 `ToolCall` 和 `ToolResult` 辅助类型（与 `ccode_sampling_types::ToolCall` 重复）
- 修改 `LLMResponse` 事件：将 `tool_calls: Vec<ToolCall>` 改为 `tool_calls: Vec<(String, String, serde_json::Value)>`（id, name, input 三元组）
- 修改 `ToolExecutionCompleted/Failed`：移除 `ToolResult`，直接用 `(tool_use_id, content)` 元组

**核心逻辑示意：**
```rust
// LLMResponse 事件改用内联元组，不再依赖自定义 ToolCall
LoopEvent::LLMResponse {
    stop_reason: String,
    tool_calls: Vec<(String, String, serde_json::Value)>,  // (id, name, input)
    token_used: u64,
}
```

---

### 任务 3：集成 permission_chain.rs 到现有权限系统

**目标：** 将权限决策链作为现有 `permission_rules` 的增强层，而非独立系统

**文件：**
- 修改：`ccode-hooks/src/permission_chain.rs`

**修改内容：**
- 保留 `PermissionChainResult` 类型（含 decision、source、alternatives、retryable）
- 保留 `DecisionSource` 枚举（PreFilter、Hook、RuleEngine、UserConfirmation、Default）
- 保留 `pre_filter` 函数（危险命令预过滤）
- 保留 `evaluate_permission_chain` 函数（完整决策链）
- 修改 `evaluate_permission_chain` 使其接受 `&PermissionRuleSet` 引用（已与现有类型对齐）
- 确认 `HookDecision` 引用来自 `crate::result`（已对齐）
- 确认 `PermissionDecision` 引用来自 `crate::permission_rules`（已对齐）

**核心逻辑示意：**
```rust
// 已对齐现有类型，无需额外适配
pub fn evaluate_permission_chain(
    tool_name: &str,
    tool_input: &serde_json::Value,
    rules: &PermissionRuleSet,      // ← 来自 crate::permission_rules
    hook_decision: Option<HookDecision>,  // ← 来自 crate::result
    auto_mode: bool,
) -> PermissionChainResult
```

---

### 任务 4：集成 hook_rewrite.rs 到现有 hook 系统

**目标：** 将 hook 改写逻辑作为 dispatcher 的增强，而非独立系统

**文件：**
- 修改：`ccode-hooks/src/hook_rewrite.rs`

**修改内容：**
- 保留 `HookRewriteResult` 类型
- 保留 `parse_hook_output` 函数
- 保留 `inject_additional_context` 函数
- 修改 `inject_additional_context` 的签名：`Vec<serde_json::Value>` → 更通用的 `&mut Vec<serde_json::Value>`（已如此，无需改动）
- 确认与现有 `dispatcher` 的集成点：`parse_hook_output` 的输出应被 dispatcher 调用后传递给工具执行层

**核心逻辑示意：**
```rust
// dispatcher 中调用 hook_rewrite 的示例流程：
// let hook_output = run_hook(hook, event).await;
// let rewrite = parse_hook_output(&hook_output);
// if rewrite.blocked { return deny; }
// if let Some(updated) = rewrite.updated_input { tool_input = updated; }
// if let Some(ctx) = rewrite.additional_context { inject_additional_context(&mut context, &ctx); }
```

---

### 任务 5：更新 ccode-agent/src/lib.rs 模块声明

**目标：** 反映模块删除和保留

**文件：**
- 修改：`ccode-agent/src/lib.rs`

**修改内容：**
- 移除已删除模块声明：`fork`、`coordinator`、`task_state`、`background`、`prompt_template`
- 保留：`loop_state`
- 保留其他所有现有模块

**核心逻辑示意：**
```rust
// 移除5个重复模块，保留 loop_state
pub mod agent;
// pub mod background;     ← 删除
pub mod builder;
pub mod compaction;
pub mod config;
// pub mod coordinator;    ← 删除
pub mod discovery;
pub mod error;
// pub mod fork;           ← 删除
pub mod loop_state;         // ← 保留（精简后）
pub mod plugins;
pub mod prompt;
// pub mod prompt_template; ← 删除
pub mod repo;
pub mod system_reminder;
// pub mod task_state;     ← 删除
pub mod timing;
```

---

### 任务 6：实现技能调用时模型切换

**目标：** 技能声明 `model` 字段后，调用时自动切换会话模型，执行后恢复

**文件：**
- 修改：`ccode-tools/src/implementations/ccode_build/skill/mod.rs`
- 修改：`ccode-agent/src/prompt/skills.rs`

**实现要点：**
1. 技能列表注入时，在 system prompt 中标注技能的模型需求
2. 技能执行前，如果 `skill.model.is_some()`，向 shell 发送 `SetSessionModel` 请求
3. 技能执行后，发送 `SetSessionModel` 恢复原模型
4. 模型不可用时，记录 warn 日志并继续使用当前模型执行

**核心逻辑示意：**
```rust
// 技能调用时的模型切换流程
// if let Some(ref model_id) = skill.model {
//     let original_model = session.model_id();
//     session.set_model(model_id).await;  // 切换
//     let result = execute_skill(skill).await;  // 执行
//     session.set_model(&original_model).await;  // 恢复
//     result
// } else {
//     execute_skill(skill).await  // 无需切换
// }
```

---

### 任务 7：验证静态一致性

**目标：** 确认所有改动后代码的静态一致性

**文件：**
- 全项目

**实现要点：**
- 检查所有删除模块的公开类型是否仍有引用
- 检查 loop_state 精简后的类型是否自洽
- 检查 permission_chain 和 hook_rewrite 与现有类型的对齐
- 检查 lib.rs 声明与实际文件的一致性

**注意：** 不做编译，仅静态分析
