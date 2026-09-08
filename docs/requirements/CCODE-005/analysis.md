# 需求分析（ccode 视角）

## 需求概述
> 单架构收敛 + TUI 升级：消除 ccode 双引擎分裂，以 shell 路径为唯一产品主线，单进程内集成 agent 引擎与 ratatui TUI，交互体验对齐 Claude Code（信息丰富规整），同时吸收 codex / grok-build / claude-code / opencode 四方架构优势。

## 业务背景
- ccode 存在两条独立 agent 引擎：ccore 消息总线（入口启动但功能简陋，3.9 万行）和 shell 单体（功能完整但入口不调用，89 万行）
- TUI 现状：ccore TUINode 仅 993 行三段式（对话+状态栏+输入框），信息密度低；ccode-pager 是完整 TUI 但走多进程 leader/client 架构，入口空壳未接
- 目标形态：单进程 + ratatui TUI，交互体验对齐 Claude Code（对话流、工具状态、diff 预览、权限弹窗、spinner、markdown 渲染等）
- 实现参考：codex 的 tui crate（27 万行 ratatui 架构）同为 Rust/ratatui，可直接借鉴布局与组件模式

## 本服务职责

| 职责项 | 说明 |
|--------|------|
| 架构收敛 | 消除双引擎分裂，建立唯一的产品级 agent 引擎 |
| 能力通电 | 将 ccore 中已实现但未接入主线的能力接入 shell 路径 |
| 死代码清理 | 删除 ccore 中确定死的消息总线 / Node 框架代码 |
| 目录重划分 | 参考 codex 细粒度 crate 模式，重划 crate 边界 |
| 竞品优势吸收 | 从四方开源项目吸收可落地的架构与实现思路 |

## 依赖关系

### 调用方（谁调用我）
| 调用方 | 接口 | 用途 |
|--------|------|------|
| CLI 用户 | ccode binary | 启动 agent 会话 |
| TUI (pager) | ACP / leader 协议 | 渲染界面、接收输入 |

### 被调用方（我调用谁）
| 服务 | 接口 | 用途 | 关键字段 |
|------|------|------|---------|
| ccode-sampler | /v1/messages, /v1/chat/completions | LLM 调用 | model, messages, tools |
| ccode-tools | 22+ 内置工具 | 工具执行 | tool_name, params |
| ccode-mcp | MCP 协议 | 外部工具 | server_name, tool_name |
| ccode-workspace | FS / Git API | 文件操作 | path, content |

## 改动范围
- [ ] 阶段 1：通电 + 清理（CCODE-004 死能力迁主线 + ccore 死骨架删除）
- [ ] 阶段 2：结构收敛（目录重划分 + crate 拆分 + compaction 合并）
- [ ] 阶段 3：吸收增强（remote compact v2 + rollout + 沙箱分层）

## 风险与注意

- ⚠️ **风险 1：改动面大**——涉及 30+ crate 的移动/重命名，容易引入回归
- 💡 策略：三阶段渐进，阶段 1 只迁能力不改名，先验证效果
- ⚠️ **风险 2：测试覆盖不足**——shell 路径有大量测试，但 ccore 侧测试可能不完整
- 💡 策略：迁能力时同步搬运测试，用逻辑推演替代人工跑测
- ⚠️ **风险 3：命名/路径变更影响外部依赖**——crate 改名会破 workspace 内引用
- 💡 策略：保留旧 crate 名做 re-export 过渡，或一次性全量改
- ⚠️ **风险 4：删除消息总线可能影响远期分布式计划**——如果未来还想做多进程分布式
- 💡 策略：确认当前无此需求，删除；真需要时再基于主线重构，不背着死骨架前进
