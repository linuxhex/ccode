# 需求分析（ccode 视角）

## 需求概述
> 基于 grok-build 开源项目，改造为名为 ccode 的终端 AI 编程代理，引入 ROS 式消息总线架构、冷热分层记忆系统、多模型后端支持，实现超越 Claude Code 和 Codex 的能力。

## 业务背景
- grok-build 是 SpaceXAI 开源的终端 AI 编程代理，功能完整但架构为单进程 monolith
- 用户希望改造为类似 Claude Code 的终端交互式 Agent，但不暴露源码
- 核心改造思想：将 ROS 的消息总线发布订阅思想引入 Agent 架构，实现多进程并行、容错、可扩展
- 需要 CLI 脚本安装，核心逻辑以动态库形式分发

## 本项目职责

| 职责项 | 说明 |
|--------|------|
| 消息总线架构 | ZeroMQ 为核心，Kernel 作为 broker，所有功能模块作为独立 Node 进程 |
| 源码保护 | CLI 开源，核心逻辑编译为 libccore.dylib/.so 闭源分发 |
| 记忆系统 | 冷热分层 + 滑动窗口，不做压缩丢弃，用内存级向量库实现 |
| 多模型后端 | 统一 OpenAI Chat Completions 兼容接口，支持 Claude/GPT/GLM/Grok/DeepSeek/Kimi/Qianwen/Qoder 等 |
| 多 Agent 编排 | 主 agent 和子 agent 均通过消息总线通信，支持真正进程级并行 |
| 能力整合 | 从 Claude Code 整合 Plan-Execute 循环、Git Checkpoint、Skill 系统；从 Codex 整合 Patch 编辑、沙箱回滚、自动验证循环 |

## 架构设计

### 进程模型与消息总线

系统由以下 Node 类型组成，每个都是独立进程：

| Node 类型 | 职责 | 数量 |
|-----------|------|------|
| Kernel | 消息总线 broker + Node 注册/发现/健康检查 | 1 |
| TUI | 终端渲染、用户输入 | 1 |
| Agent | 单个 agent 实例（主 agent / 子 agent） | N（动态） |
| Tool | 工具执行（文件读写、bash、grep、MCP…） | 1-N |
| Sampler | LLM API 调用 + 流式返回 | 1-N |
| State | 对话持久化、记忆管理、token 计数 | 1 |
| Plugin | 外部第三方扩展节点 | 0-N |

### 消息协议

Topic 命名规范：`{domain}/{node_type}/{node_id}/{action}`

核心 Topic 表：

| Topic | 发布方 | 订阅方 | 用途 |
|-------|--------|--------|------|
| sys/heartbeat | 所有 Node | Kernel | 心跳保活 |
| sys/register | 新 Node | Kernel | 节点注册 |
| sys/deregister | 退出 Node | Kernel | 节点注销 |
| sys/spawn | Kernel | 所有 Node | 通知新 Node 上线 |
| sys/shutdown | Kernel | 所有 Node | 全局关闭信号 |
| agent/{id}/input | TUI / 其他 Agent | 目标 Agent | 用户输入 / 父 agent 指令 |
| agent/{id}/output | Agent | TUI / 父 Agent | agent 回复流 |
| agent/{id}/tool_call | Agent | Tool Node | 请求执行工具 |
| agent/{id}/tool_result | Tool Node | 发起 Agent | 工具执行结果 |
| agent/{id}/spawn | Agent | Kernel | 请求创建子 agent |
| agent/{id}/event | Agent | State / TUI | 状态事件 |
| sampler/request | Agent | Sampler Node | LLM 采样请求 |
| sampler/{req_id}/stream | Sampler Node | 发起 Agent | LLM 流式返回 |
| state/query | Agent | State Node | 查询对话状态 |
| state/response | State Node | 发起 Agent | 状态查询结果 |
| state/persist | Agent | State Node | 持久化对话 |

消息帧格式：3 帧 ZeroMQ 消息
- Frame 1: topic (UTF-8)
- Frame 2: header (MessagePack) — msg_id, timestamp, src_node, reply_to
- Frame 3: payload (MessagePack) — 业务数据

### 记忆与上下文系统

三层记忆架构：

| 层级 | 名称 | 实现 | 容量 | 内容 |
|------|------|------|------|------|
| L0 | 工作记忆 | LLM context window 内 | context_window_tokens | 热消息原文 + 温消息摘要 + 冷消息占位符 |
| L1 | 短期记忆 | 内存级向量库 (hora HNSW) | 会话级 | 当前会话完整对话历史，永不丢弃 |
| L2 | 长期记忆 | 持久化向量库 (qdrant) + 磁盘缓存 | 无限 | 跨会话知识、项目架构、决策记录、用户偏好 |

冷热评分算法：
- recency: 时间衰减 exp(-λ * elapsed_turns)
- relevance: 与当前任务的语义相似度 (embedding cosine)
- activity: 被引用/被召回的频次
- tool_weight: 工具调用结果权重

滑动窗口更新流程：
1. 新消息入库 L1（完整原文 + embedding，永不丢弃）
2. 计算所有消息的 heat 分数
3. 从热到冷排序，贪心填充 L0 直到 context_window 用满
4. 冷消息替换为摘要向量占位符
5. agent 可通过 recall 工具从 L1/L2 按需取回

Dream 整理：空闲时自动对 L2 知识去重、合并、建立关联

### 多模型后端

所有后端统一为 OpenAI Chat Completions 兼容接口：

| 类型 | 后端 | 说明 |
|------|------|------|
| 原生兼容 | OpenAI, DeepSeek, Qoder | 直接用 /v1/chat/completions |
| 兼容适配 | Claude, GLM, Kimi, Qianwen | 需请求/响应格式转换 |
| xAI 原生 | Grok | 保留 Responses API 支持 |

多模型混排策略：不同子 agent 可用不同模型
- Primary Agent → 强推理模型（如 claude-opus）
- explore 子 agent → 快且便宜的模型（如 deepseek-chat）
- plan 子 agent → 规划能力好的模型（如 o3）

速率限制与重试：每个 Provider 独立令牌桶 + 指数退避 + 自动 fallback

### 源码保护策略

```
ccode-cli (开源 Rust bin)           → 仅做参数解析 + spawn kernel
  └── libccore.dylib/.so (闭源)     → Kernel + Node 框架 + 所有核心逻辑
      └── 复用 grok 的工具实现、采样逻辑、markdown 渲染等
```

### 能力整合清单

从 Claude Code 整合：
- Plan-then-Execute 循环（Plan Mode → 用户审批 → 执行 → 验证）
- Git Checkpoint + 回滚（每次 edit 自动 checkpoint）
- 并行工具调用
- Skill 系统（可复用 prompt 模板）
- Diff 感知编辑（基于 hunk 的精确编辑）

从 Codex 整合：
- Patch 式编辑（已有 apply_patch 实现，复用）
- 沙箱回滚
- 自动验证循环（编辑 → 编译 → 测试 → 修 → 再验证）

ccode 独创：
- 消息总线原生并行（进程级，非线程级）
- 多模型混排
- 外部 Node 插件生态
- Doom Loop 检测（Grok 已有）
- 分布式推理（Sampler Node 可跑在远程 GPU 机器）

## 复用 grok-build 的现有模块

| grok 模块 | ccode 复用方式 |
|-----------|---------------|
| xai-grok-tools | 工具实现迁移到 Tool Node，保留 bash/grep/read/write 等全部工具 |
| xai-grok-sampler | 采样逻辑迁移到 Sampler Node，扩展为多 Provider |
| xai-grok-memory | 嵌入和 RAG 逻辑复用，重构为冷热分层架构 |
| xai-grok-markdown | TUI 渲染复用 |
| xai-grok-hooks | 事件系统迁移到消息总线 topic |
| xai-grok-mcp | MCP 集成作为 Tool Node 的子模块 |
| xai-grok-sandbox | 沙箱执行逻辑复用 |
| xai-grok-compaction | 替换为冷热分层 + 滑动窗口，不使用压缩 |
| xai-grok-config | 配置系统复用，扩展多 Provider 配置 |
| xai-grok-auth | 认证逻辑复用，扩展多 Provider 认证 |
| xai-fast-worktree | Git worktree 复用 |
| xai-grok-shell | ACP 协议和 agent 编排逻辑参考 |
| xai-tool-types | 工具类型定义复用 |
| xai-tool-runtime | 工具运行时复用 |
| xai-tool-protocol | 工具协议参考 |
| xai-circuit-breaker | 熔断器复用，用于 Provider 限流和 fallback |
| xai-chat-state | 对话状态管理参考，重构为 State Node |
| ptyctl | PTY 控制复用 |
| xai-grok-secrets | 密钥脱敏复用 |

## 技术选型

| 领域 | 选型 | 理由 |
|------|------|------|
| 消息总线 | ZeroMQ (zmq crate) | 轻量、低延迟、支持 IPC/TCP/inproc |
| 序列化 | MessagePack (rmp-serde) | 比 JSON 更紧凑，比 protobuf 更灵活 |
| 向量库 (L1) | hora | 纯 Rust HNSW，SIMD 加速，零依赖 |
| 向量库 (L2) | qdrant-client | 持久化向量库，支持混合检索 |
| 嵌入模型 | 本地 ONNX / 远程 API | 语义检索用 |
| 异步运行时 | tokio | grok 已用，继续沿用 |
| 终端渲染 | ratatui + 复用 xai-grok-markdown | grok 已有成熟 TUI |
| 动态库 | cdylib | Rust 编译为 C 动态库，CLI 通过 FFI 调用 |

## 风险与注意

- ⚠️ ZeroMQ 进程管理复杂度：需要仔细设计进程生命周期和错误传播
- ⚠️ 动态库 FFI 边界设计：需明确哪些类型跨 FFI 传递，避免 Rust 类型泄漏
- ⚠️ 向量库嵌入模型选择：本地模型需打包进动态库，增加体积；远程模型增加延迟
- ⚠️ 多 Provider 适配器维护成本：每个后端 API 变更需要及时跟进
- 💡 建议分阶段交付：先跑通 Kernel + Agent + Sampler + TUI 最小闭环，再逐步加 Tool/State/Memory/Plugin
- 💡 子 agent 通信完全通过消息总线，与外部 Plugin 通信方式一致，降低认知负担
