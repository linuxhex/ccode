# 需求分析（ccode 视角）

## 需求概述
> 精华融合：清理8个重复模块，将真正缺失的能力融入现有架构；实现 Skill-Model 切换功能

## 业务背景
- 前一轮创建了8个借鉴 Claude Code 架构的模块（loop_state、fork、coordinator、task_state、background、prompt_template、permission_chain、hook_rewrite）
- 这些模块与现有代码存在严重类型重复（AgentDefinition、AgentType 等）
- SkillInfo 已有 model 字段但未与运行时模型切换集成
- 用户期望技能调用时可切换模型

## 本服务职责

| 职责项 | 说明 |
|--------|------|
| 清理重复模块 | 删除8个模块中与现有代码重复的类型和函数，保留真正缺失的能力 |
| 融合缺失能力 | 将状态机、权限链、hook改写等真正缺失能力集成到现有代码 |
| Skill-Model切换 | 技能声明模型 → 技能执行前切换会话模型 → 执行后恢复 |

## 重复问题分析

### 1. fork.rs — 严重重复
| 重复类型 | 新模块定义 | 现有代码 |
|----------|-----------|---------|
| AgentDefinition | `fork::AgentDefinition`（5字段简化版） | `config::AgentDefinition`（30+字段完整版） |
| AgentType | `fork::AgentType`（6变体枚举） | `config::BuiltinAgentName`（12变体枚举） |
| load_agent_definitions | `fork::load_agent_definitions`（简单读文件） | `discovery::by_name_in_cwd_with_plugins`（完整发现机制） |
| build_fork_prompt | `fork::build_fork_prompt` | `prompt::subagent_prompts`（已有完整子代理 prompt 系统） |
| build_fresh_agent_prompt | `fork::build_fresh_agent_prompt` | 已由 TaskTool + agent definition 系统完全覆盖 |

**结论：fork.rs 完全重复，应删除**

### 2. coordinator.rs — 重复
| 重复类型 | 新模块定义 | 现有代码 |
|----------|-----------|---------|
| Coordinator + AgentSlot | `coordinator::Coordinator` | 已由 `ccode-shell::subagent` 模块完全覆盖 |
| AgentStatus | `coordinator::AgentStatus` | 已由 `ccode-subagent::types` 中的状态管理覆盖 |
| TaskStatus | `coordinator::TaskStatus` | 已由 TaskTool 的 task 生命周期管理覆盖 |

**结论：coordinator.rs 完全重复，应删除**

### 3. task_state.rs — 重复
| 重复类型 | 新模块定义 | 现有代码 |
|----------|-----------|---------|
| Task + TaskManager | `task_state::TaskManager` | 已由 `ccode-subagent::config` + TaskTool 系统覆盖 |
| claim 机制 | `task_state::TaskManager::claim_task` | 已由 `ccode-subagent` 的原子性任务认领覆盖 |

**结论：task_state.rs 完全重复，应删除**

### 4. background.rs — 重复
| 重复类型 | 新模块定义 | 现有代码 |
|----------|-----------|---------|
| BackgroundAgent | `background::BackgroundAgent` | 已由 `ccode-tools::task` 的后台任务系统覆盖 |
| NotificationManager | `background::NotificationManager` | 已由 `ccode-shell::extensions::notification` 覆盖 |
| AgentNotification | `background::AgentNotification` | 已由 TaskTool 的 `get_task_output` 机制覆盖 |

**结论：background.rs 完全重复，应删除**

### 5. prompt_template.rs — 部分重复
| 重复类型 | 新模块定义 | 现有代码 |
|----------|-----------|---------|
| PromptTemplateEngine | `prompt_template::PromptTemplateEngine` | 部分由 `prompt::context::PromptContext` 覆盖 |
| AgentPromptDef | `prompt_template::AgentPromptDef` | 已由 `config::AgentDefinition` 覆盖 |
| generate_agent_tool_prompt | 自定义 prompt 生成 | 已由 `prompt::context` 系统覆盖 |

**结论：prompt_template.rs 大部分重复，应删除**

### 6. permission_chain.rs — 真正缺失，需保留并集成
- 现有 `permission_rules` 只有简单的 allow/deny/ask 三值决策
- `permission_chain` 增加了：预过滤 → Hook拦截 → 规则引擎 → 用户确认 的完整链路
- 增加了 `DecisionSource`（决策来源追踪）和 `alternatives`（替代方案）
- 需要集成到现有权限系统中

### 7. hook_rewrite.rs — 真正缺失，需保留并集成
- 现有 hook 系统只有简单的 allow/deny 决策
- `hook_rewrite` 增加了：工具输入改写（updatedInput）、上下文注入（additionalContext）、阻止（blocked）
- 需要集成到现有 hook dispatcher 中

### 8. loop_state.rs — 真正缺失，需保留并集成
- 现有 agent 循环分散在 `ccode-shell/src/session/acp_session_impl/run_loop.rs`
- `loop_state` 将循环建模为显式状态机，增加了：
  - 错误重试退避策略（RateLimit 指数退避、ServerError 线性退避）
  - auto-compact 触发判定
  - deny recovery（权限被拒后调整策略）
  - maxTurns 限制
- 需要作为现有循环的状态管理组件集成

## Skill-Model 切换需求

### 现有基础
1. `SkillInfo.model: Option<String>` — 技能可声明模型（frontmatter `model: xxx`）
2. `ModelOverride` — 代理定义的模型覆盖（Inherit / Override）
3. `model_switch.rs::apply()` — 会话级模型切换的完整实现
4. `ccode-shell::subagent` — 子代理创建时可指定模型

### 缺失环节
1. 技能调用时读取 `skill.model` 字段
2. 技能执行前切换会话模型
3. 技能执行后恢复原模型
4. 模型不可用时的降级处理

## 改动范围
- [ ] 删除：fork.rs（与现有代码完全重复）
- [ ] 删除：coordinator.rs（与现有代码完全重复）
- [ ] 删除：task_state.rs（与现有代码完全重复）
- [ ] 删除：background.rs（与现有代码完全重复）
- [ ] 删除：prompt_template.rs（与现有代码大部分重复）
- [ ] 修改：permission_chain.rs → 集成到 ccode-hooks 权限系统
- [ ] 修改：hook_rewrite.rs → 集成到 ccode-hooks dispatcher
- [ ] 修改：loop_state.rs → 集成到 ccode-agent 状态管理
- [ ] 新增：技能执行时模型切换逻辑
- [ ] 修改：lib.rs 声明（删除已删除模块，保留融合后的模块）

## 风险与注意
- ⚠️ 删除模块前需确认无其他代码引用
- ⚠️ permission_chain 和 hook_rewrite 集成时需保持与现有权限规则的兼容
- ⚠️ loop_state 集成时需确认现有 run_loop 的状态流转与新状态机不冲突
- 💡 Skill-Model 切换应作为可选功能，不影响不声明模型的技能
