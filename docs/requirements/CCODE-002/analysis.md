# 需求分析（仿生架构重构）

## 需求概述
> 参考"人有眼睛耳朵嘴巴鼻子和手"的仿生学原理，将现有 agent 架构重构为感官-运动-反射闭环系统，让 agent 具备自主感知、反射响应、闭环学习的能力。

## 业务背景
- 当前 agent 是"只有大脑"的生物——没有感官，没有反射弧，全靠 LLM 轮询处理一切
- 消息总线已存在（ROS 1 风格），但仅做控制面路由，Agent 业务不经过总线
- A+ 增强模块（持久化/可观测性/错误恢复/性能/配置热更新）已实现但集成深度不足

## 核心设计决策

| 决策 | 结论 | 理由 |
|------|------|------|
| 范围 | 全器官 + 反射弧 | 用户明确选择 |
| 反射弧位置 | Kernel 内 ReflexRouter | 集中管理，可访问全局状态 |
| 运动器官 | HandNode + LimbNode 两 Node | 用户明确选择 |
| 信号协议 | 各器官自定义 topic | 用户明确选择 |
| A+ 融合 | 能力分配到对应器官 | 消除重复，架构统一 |
| LLM 参与准则 | L0 反射/L1 本能/L2 思考 三级 | 明确哪些不经 LLM |
| 学习闭环 | 预置规则 + 成功率计数器 + 经历回放 | 务实，无 ML |

## LLM 参与准则

| 级别 | 定义 | 处理方式 |
|------|------|---------|
| L0 反射 | 确定性高、风险低、模式固定 | 脊髓反射，不经 LLM |
| L1 本能 | 需简单判断，有固定模式 | 器官内置规则，不经 LLM 但记录供审查 |
| L2 思考 | 不确定性高、需理解上下文 | 必须经 LLM |

### 信号分级

| 信号 | 级别 |
|------|------|
| 缺分号/括号/引号 | L0 |
| 缺 import/模块引用 | L0 |
| 文件变化 → 重索引 | L0 |
| 心跳超时 → 重启 | L0 |
| 内存压力 → 回收 | L0 |
| 简单类型不匹配 | L1 |
| 变量未定义（拼写错误） | L1 |
| 测试失败 → 重跑 | L1 |
| Git merge 简单冲突 | L1 |
| 复杂编译错误 | L2 |
| 修改业务逻辑 | L2 |
| 编写新功能 | L2 |
| 重构代码 | L2 |

### 反射弧路由

```
sensor signal → Kernel ReflexRouter
  ├─ L0 → 直接发 motor 指令（不通知 ThinkerNode）
  ├─ L1 → 发 motor 指令 + 发 sensory/summary 给 ThinkerNode
  └─ L2 → 转发给 ThinkerNode
```

约束：L1 同一问题最多 3 次，超过升级为 L2

## 器官映射表

| 器官 | Node | 功能 | A+ 融合 | Topics |
|------|------|------|---------|--------|
| 眼睛 | EyeNode | 读代码/看文件变化/观终端 | config hot-reload | eye/observe, eye/file_changed, eye/terminal_output |
| 耳朵 | EarNode | 听用户指令/收通知/收心跳 | — | ear/hear, ear/notification, ear/heartbeat |
| 鼻子 | NoseNode | 嗅编译错误/测试失败/性能退化 | — | nose/smell, nose/compile_error, nose/test_failure |
| 皮肤 | SkinNode | 触觉反馈：工具结果/进程退出/内存压力 | persistence（触觉记忆） | skin/touch, skin/process_exit, skin/memory_pressure |
| 嘴巴 | MouthNode | 写代码/生成回复/报告状态 | tracing/json_formatter | mouth/speak, mouth/code_write, mouth/status |
| 手 | HandNode | 编辑文件/搜索定位/重构移动 | degradation → ReflexRouter | hand/edit, hand/search, hand/restructure |
| 肢体 | LimbNode | 运行命令/构建测试/Git 操作 | retry/backoff（肌肉记忆） | limb/execute, limb/build, limb/git |
| 大脑皮层 | ThinkerNode（重构 AgentNode） | 规划/推理/决策 | metrics → 各器官 | cortex/think, cortex/plan, cortex/decide |
| 脊髓反射 | Kernel::ReflexRouter | 模式匹配 → 自动响应 | degradation 策略 | 内部路由 |
| 自主神经 | Kernel::AutonomicNervousSystem | 心跳/健康/内存回收 | SelfHealingManager | 内部管理 |

## 记忆架构

| 层级 | 人体 | 对应组件 | 实现 |
|------|------|---------|------|
| 感觉记忆 | 各感官缓冲 | 各 Sensory Node 内部缓冲 | 瞬时（<1s） |
| 工作记忆 | 前额叶皮层 | ThinkerNode WorkingMemory | 当前对话上下文 |
| 短期记忆 | 海马体 | SkinNode ShortTermMemory | 近期工具结果/编译输出 |
| 长期记忆 | 大脑皮层 | StateNode（海马体） | 跨会话快照/规则/知识 |
| 肌肉记忆 | 小脑/基底节 | LimbNode | 重复操作模式记忆 |
| 反射记忆 | 脊髓 | Kernel ReflexRouter | L0/L1 反射规则库 |

## 学习闭环（务实版）

1. **预置规则模板**：出厂自带常见 L0/L1 规则（reflex_rules.toml，人类可读可编辑）
2. **成功率计数器**：每个规则记 use_count / success_count / consecutive_fails，consecutive_fails >= 2 → 禁用 + 升级 L2
3. **经历回放提规则**：会话结束扫描 L2 经历，同类问题成功 3 次 → 提议新 L1_trial 规则，确认 3 次 → 升级 L1 正式

## 改动范围
- [ ] 新增：6 个器官 Node（Eye/Ear/Nose/Skin/Mouth/Hand/Limb = 7 个，Thinker 重构自 AgentNode）
- [ ] 新增：Kernel::ReflexRouter + Kernel::AutonomicNervousSystem
- [ ] 新增：ReflexRule 数据结构 + 规则加载/匹配/统计
- [ ] 新增：ExperienceLog 经历记录 + 回放提规则
- [ ] 重构：AgentNode → ThinkerNode
- [ ] 重构：SelfHealingManager → AutonomicNervousSystem
- [ ] 融合：A+ 模块能力分配到对应器官
- [ ] 新增：仿生 Topic 命名（eye/*, ear/*, nose/*, skin/*, mouth/*, hand/*, limb/*, cortex/*）
- [ ] 更新：NodeType 枚举新增器官类型

## 风险与注意
- ⚠️ 大规模重构风险：7 个新 Node + 2 个新 Kernel 模块，需分阶段实施
- ⚠️ 与现有 session 路径兼容：ccode-shell 的 session/turn.rs 路径需保留，双轨过渡
- ⚠️ 反射规则的副作用：L0/L1 自动操作可能引入错误，必须有回退机制
- 💡 先实现器官 + 反射弧骨架，再逐步填充器官的具体感官/运动逻辑
