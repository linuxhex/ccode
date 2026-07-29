# 需求分析：Agent 写代码能力 A+ 提升（ccode 视角）

## 需求概述
> 将 agent 写代码能力从 A- 提升到 A+，通过实现 5 个关键缺口：编译反馈闭环、Doom Loop 逃脱策略、增量验证、代码审查自动化、多文件事务。

## 业务背景
- 当前 agent 有完整工具集（Bash/Read/Write/Edit/Grep/Glob）和 ReAct 循环
- 5 大基础设施模块（持久化/可观测性/错误恢复/性能优化/配置热更新）已集成
- 但 agent 写代码的"闭环能力"缺失：写完代码不自动编译验证、陷入循环无法逃脱、多文件修改无事务保障
- 这些缺口导致 agent 写代码能力停留在 A-，无法达到 A+

## 本服务职责

| 职责项 | 说明 |
|--------|------|
| 编译反馈闭环 | turn 结束后自动 cargo check，解析错误注入下一轮 |
| 增量验证 | Write/Edit 后轻量检查（rustfmt --check） |
| Doom Loop 逃脱 | 检测到循环后注入提示 + 禁用重复工具 + 降级模型 |
| 代码审查自动化 | 可配置的自动 review（默认关闭） |
| 多文件事务 | 文件级备份 + 失败回滚 |

## 改动范围
- [ ] 新增：`ccore/src/tools/compile_feedback.rs` — cargo check 错误解析器
- [ ] 新增：`ccore/src/tools/file_transaction.rs` — 文件事务管理器
- [ ] 修改：`ccore/src/tools/builtin.rs` — Write/Edit 添加 post_hook + 事务
- [ ] 修改：`ccore/src/tools/bridge.rs` — post_execute_hooks 机制
- [ ] 修改：`ccore/src/agent/doom_loop.rs` — 逃脱策略
- [ ] 修改：`ccore/src/node/agent.rs` — Doom Loop 逃脱执行
- [ ] 修改：`ccode-shell/src/session/acp_session_impl/turn.rs` — handle_turn_end 集成
- [ ] 修改：配置项添加 auto_review

## 决策记录

| 决策 | 选择 | 理由 |
|------|------|------|
| 编译反馈触发时机 | 混合方案 | Write/Edit 后轻量检查 + turn 结束后完整 cargo check |
| Doom Loop 逃脱策略 | 提示+禁用工具+降级模型 | 最激进方案，有效打破循环 |
| 代码审查自动化 | 可配置（默认关闭） | 避免增加延迟，用户按需开启 |
| 多文件事务 | 文件级备份 | 不依赖 git，内存备份，简单可靠 |

## 风险与注意
- ⚠️ cargo check 可能耗时较长（大型工程 30s+），需异步执行不阻塞主循环
- ⚠️ Doom Loop 降级模型可能降低代码质量，需设置降级上限
- ⚠️ 文件备份在内存中，大文件可能占用过多内存
- 💡 rustfmt --check 可能不适用于非 Rust 工程，需检测工程类型
