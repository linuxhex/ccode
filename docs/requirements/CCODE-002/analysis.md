# 需求分析（ccode 视角）— 第二轮：6 项 Claude Code 优秀设计集成

## 需求概述
> 将 Claude Code 的 6 项核心架构能力完整集成到 ccode，消除安全缺陷和功能缺口

## 业务背景
- 第一轮已完成：hook_rewrite 接入、permission_chain 接入、循环失败熔断、loop_state 删除、技能 model/effort 日志记录
- 经推演模拟分析，仍有 6 项 Claude Code 优秀设计未集成，其中 3 项为 P0 级安全/核心能力缺口

## 6 项缺口分析

### 缺口 1：显式状态机 queryLoop（P0）

**现状**：`handle_prompt` 中使用隐式 `loop {}` 驱动主循环，无统一状态对象，无可观测性
**风险**：无法断点恢复、无法 deny recovery、无法跨轮次追踪状态
**目标**：引入 `LoopState` 枚举 + `LoopAction` 驱动主循环，使状态变迁可观测、可中断、可恢复

### 缺口 2：deny-first 权限模型（P0）

**现状**：`permission_chain` 已接入但默认 allow-first，YOLO 模式下 `rm -rf /` 等危险命令可直达执行
**风险**：安全漏洞，危险命令绕过所有检查
**目标**：实现 deny-first 语义——不在安全白名单中的操作默认 Deny，逐层放行

### 缺口 3：技能模型切换执行（P0）

**现状**：`SkillInfo.model/effort` 已读取但只记日志，未触发 `handle_set_session_model`
**风险**：技能声明的模型切换功能形同虚设
**目标**：技能加载后通过 `SetSessionModel` 命令完成完整模型切换

### 缺口 4：7 层上下文压缩管线（P1）

**现状**：有 3 级压缩（Code/Intra/Inter），缺 fileCache 缓存和渐进裁剪
**风险**：重复读取消耗 Token，大项目上下文利用率低
**目标**：增加 fileCache 层，避免重复读文件；增加 compactSnapshots 断点快照

### 缺口 5：toolCallBudget 配额扣减（P1）

**现状**：有 token 预算和 max_turns，但无单轮工具调用配额
**风险**：模型可在单轮中无限调用工具，成本失控
**目标**：引入 `tool_call_budget`，每次工具调用扣减配额，耗尽时结束 turn

### 缺口 6：循环检测（P1）

**现状**：只检测"连续失败"，不检测"相同工具调用循环"
**风险**：模型反复执行同一命令但不失败时（如反复读同一文件），不会熔断
**目标**：检测连续 N 次相同（tool_name + 相似 input）的工具调用，触发熔断

## 改动范围

| 模块 | 缺口 | 操作 |
|------|------|------|
| ccode-agent | 显式状态机 | 新增 loop_state.rs |
| ccode-shell/turn.rs | 显式状态机 | 重构主循环为状态机驱动 |
| ccode-hooks/permission_chain.rs | deny-first | 修改默认决策为 Deny |
| ccode-hooks/permission_rules.rs | deny-first | 新增安全白名单 |
| ccode-hooks/pre_filter.rs | deny-first | 增强预过滤规则 |
| ccode-shell/turn.rs | 技能模型切换 | 加载技能后调用模型切换 |
| ccode-shell/model_switch.rs | 技能模型切换 | 新增简化模型切换入口 |
| ccode-shell/compaction.rs | 7 层压缩 | 新增 fileCache 层 |
| ccode-shell/tool_calls.rs | toolCallBudget | 新增配额扣减逻辑 |
| ccode-shell/tool_calls.rs | 循环检测 | 扩展熔断检测逻辑 |

## 风险与注意
- ⚠️ 显式状态机重构涉及主循环核心代码，需确保不破坏现有 happy path
- ⚠️ deny-first 改为默认拒绝可能影响现有用户体验，需保留 auto_mode 快速放行
- ⚠️ 技能模型切换需要完整 SamplerConfig，需通过现有 `chat_state_handle` 获取
- 💡 7 层压缩和 toolCallBudget 为 P1，可在 P0 完成后独立迭代
