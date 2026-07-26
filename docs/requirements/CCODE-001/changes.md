# ccode 改动简述

## 改动概述
将 ccode 改造为 ccode 终端 AI 编程代理，引入 ZeroMQ 消息总线、冷热分层记忆、多模型后端。

## 新增文件
- ccore 动态库 crate：kernel/broker, node trait, message 协议, memory 三层, sampler 多 Provider, agent 编排, tools 桥接, ffi 导出, config 配置
- ccode-cli：CLI 入口
- install 脚本

## 修改文件
- 根 Cargo.toml：新增 ccore 和 ccode-cli workspace member

## 复用 ccode 模块
- ccode-tools → Tool Node 工具桥接
- ccode-sampler → Sampler Node 采样逻辑
- ccode-memory → 记忆系统 embedding/RAG
- ccode-markdown → TUI 渲染
- ccode-hooks → 事件迁移到消息总线
- ccode-mcp → MCP 集成
- ccode-sandbox → 沙箱执行
- ccode-config → 配置系统
- ccode-auth → 认证逻辑
- ccode-fast-worktree → Git worktree
- ccode-circuit-breaker → 熔断器
- ptyctl → PTY 控制

## 方案审查

### 业务逻辑推演
- 业务流程推演：✓ 主流程闭环
- 业务规则推演：✓ 权限/Doom Loop/Plan-Execute 逻辑完整
- 业务状态推演：✓ Agent 状态机完整
- 业务数据推演：✗ 冷消息占位符 token 估算需实际验证
- 业务异常推演：✗ 子 Agent 崩溃后主 Agent 超时/重试策略未定义（已补充）
- 业务边界推演：✓ 配置项预留
- 业务依赖关系：✓ MVP 依赖链清晰
- 业务异常恢复：✗ Kernel 崩溃后 Registry 丢失（minor，阶段 F 补充）

### 技术方案审查
- 文件路径正确：✓
- 依赖关系合理：✗ zmq crate 不活跃 → 已改用 zeromq (纯 Rust ZMTP)
- 技术方案可行：✓
- 接口契约一致：✓
- 配置项完整：✓

### 执行可行性审查
- 步骤无遗漏：✓ 19 个任务
- 步骤无冲突：✓
- 资源可获取：✓
- 环境可支持：✓

### 审查结论
- 发现问题：4 个
  - [critical] zmq crate 不活跃有内存安全问题 → 已改用 zeromq
  - [critical] 子 Agent 崩溃后主 Agent 无超时/重试策略 → 补充 agent/{sub_id}/event topic 通知崩溃
  - [minor] L0 冷消息占位符 token 估算 → 实际可测后调整
  - [minor] Kernel 崩溃后 Registry 丢失 → 阶段 F 补充 Registry 持久化

## 执行进度（阶段 A：MVP 骨架）

### 已完成任务

| 任务 | 状态 | 说明 |
|------|------|------|
| 任务1：创建 ccore crate 骨架 | ✓ | Cargo.toml + lib.rs，cdylib/rlib 双输出 |
| 任务2：定义消息协议 | ✓ | Topic 命名/通配符匹配、3帧 MessagePack 编解码、含单元测试 |
| 任务3：定义 Node trait | ✓ | NodeId/NodeType/NodeConfig/PermissionMode/NodeContext/Node trait + async_trait |
| 任务4：实现 Kernel Broker | ✓ | Broker 路由逻辑（identity/subscribe/route_message）+ Registry + 健康检查 + spawn 子 Agent + 含单元测试 |
| 任务5：实现 Sampler Node | ✓ | 多 Provider 注册/路由/fallback 逻辑 |
| 任务6：实现 Agent Node | ✓ | 完整 Agent 循环：input→sample→tool_call→tool_result→output，含 L0/L1 记忆、Doom Loop 检测、子 Agent 崩溃处理 |
| 任务7：实现 TUI Node | ✓ | 基础骨架，订阅 Agent output/event |
| 任务8：实现 CLI + FFI | ✓ | ccode-cli (clap 参数解析) + ccore FFI (ccore_start/ccore_stop/ccore_version) |
| 任务9：实现配置系统 | ✓ | CcodeConfig + ProviderConfig + MemoryConfig + TOML 序列化 |
| 任务10：集成测试 MVP | 待后续 | ZMQ 实际连接集成 |

### 额外完成的模块

| 模块 | 说明 |
|------|------|
| memory/working | L0 工作记忆：Hot/Warm/Cold 条目 + token 预算管理 |
| memory/short_term | L1 短期记忆：完整对话历史存储 + embedding + recall 计数 |
| memory/long_term | L2 长期记忆：跨会话知识持久化（qdrant 待集成） |
| memory/heat | 冷热评分算法：recency+relevance+activity+tool_weight，含单元测试 |
| memory/window | 滑动窗口更新：每轮重算热度，贪心填充 L0，冷消息替换为占位符 |
| memory/dream | Dream 整理：空闲时去重/合并/关联 |
| memory/recall | recall 工具：从 L1/L2 按需取回冷记忆 |
| agent/prompt | Agent prompt 模板：primary + explore/plan/general-purpose/codex 子 prompt |
| agent/subagent | 子 Agent 定义/生命周期/崩溃事件 |
| agent/orchestrator | 子 Agent 编排器：注册/状态更新/完成检测 |
| agent/doom_loop | Doom Loop 检测：工具调用签名去重 + 阈值判定 |
| agent/plan_execute | Plan-Execute 循环：Plan 创建/审批/执行/验证 |
| agent/skills | Skill 系统：可复用 prompt 模板 + 工具配置 |
| tools/bridge | 工具桥接：20 个默认工具注册 |
| tools/checkpoint | Git Checkpoint：编辑前自动 checkpoint + 回滚 |
| tools/verify | 自动验证：编辑后编译/测试验证循环 |
| node/tool | Tool Node：订阅 agent/*/tool_call，执行并返回结果 |
| node/state | State Node：对话持久化 + L1/L2 记忆管理 |

### 代码质量检查
- ✓ 清理未使用的 import（Topic, PermissionMode, ShortTermEntry 等）
- ✓ 修复 TUI Node struct 命名 typo
- ✓ _entry_id 抑制 unused 警告
- ✓ 所有模块有详细的中文注释说明职责和设计思路
- ✓ 核心逻辑有单元测试（topic 匹配、消息编解码、broker 路由、冷热评分）
