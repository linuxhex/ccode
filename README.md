# ccode

终端 AI 编程代理，基于 Rust 构建的自主代码智能体。

## 架构概览

ccode 采用 **微内核 + 插件式 Node** 架构，借鉴 ROS（Robot Operating System）消息总线思想：

```
┌─────────────────────────────────────────────────────────┐
│                      Kernel (微内核)                      │
│  ┌─────────┐  ┌─────────┐  ┌──────────┐  ┌───────────┐ │
│  │ Broker  │  │ Router  │  │ Param    │  │ Service   │ │
│  │ (路由)  │  │ (转发)  │  │ Server   │  │ Registry  │ │
│  └─────────┘  └─────────┘  └──────────┘  └───────────┘ │
│         ▲         ▲              ▲              ▲       │
│         │  控制面  │              │              │       │
│    ┌────┴─────────┴──────────────┴──────────────┴───┐   │
│    │              ZeroMQ 消息总线                    │   │
│    └────▲─────────▲──────────────▲──────────────▲───┘   │
│         │  数据面  │              │              │       │
│    ┌────┴──┐  ┌───┴───┐   ┌─────┴────┐   ┌─────┴────┐  │
│    │Agent  │  │CLI    │   │Explorer  │   │Worker    │  │
│    │Node   │  │Node   │   │Node      │   │Node      │  │
│    └───────┘  └───────┘   └──────────┘   └──────────┘  │
└─────────────────────────────────────────────────────────┘
```

**核心设计理念**：
- **控制面**：Kernel 集中管理注册、发现、心跳、参数（类似 ROS Master）
- **数据面**：Node 之间点对点直连，Topic 发布/订阅、Service 请求/响应不经 Kernel（类似 ROS 1 的数据面）
- **双面分离**：控制流与数据流物理隔离，Kernel 不是性能瓶颈

## 模块地图

### 自研核心模块

| 模块 | 路径 | 职责 |
|------|------|------|
| **ccore** | `crates/codegen/ccore` | 微内核消息总线：Kernel、Broker、Node trait、Message、Config、Sampler、Agent、Memory、Tools、FFI |
| **ccode-cli** | `crates/codegen/ccode-cli` | CLI 入口，参数解析 + 启动 kernel（FFI/Direct 两种模式） |

### Agent 系统（源自开源 Grok Build）

| 模块 | 路径 | 职责 |
|------|------|------|
| **ccode-shell** | `crates/codegen/ccode-shell` | 核心 Agent 运行时：会话管理、认证、工具调用、MCP 集成、子代理调度、扩展系统 |
| **ccode-agent** | `crates/codegen/ccode-agent` | Agent 抽象层：构建器、压缩策略、发现、信任插件、prompt 模板 |
| **ccode-subagent** | `crates/codegen/ccode-subagent` | 子代理管理：配置、上下文隔离、恢复、类型定义 |
| **ccode-chat** | `crates/codegen/ccode-chat` | Actor 模型的聊天状态机：命令、事件、持久化、用量追踪 |
| **ccode-sampler** | `crates/codegen/ccode-sampler` | LLM 采样器：流式推理、重试、doom-loop 防护、指标 |
| **ccode-sampling-types** | `crates/codegen/ccode-sampling-types` | 采样层纯数据类型：API 请求/响应、token 计算 |
| **ccode-session** | `crates/codegen/ccode-session` | 会话抽象：生命周期、状态管理 |

### TUI 渲染系统（源自开源 Grok Build）

| 模块 | 路径 | 职责 |
|------|------|------|
| **ccode-pager** | `crates/codegen/ccode-pager` | 终端 UI 主程序：视图、输入、搜索、斜杠命令、语音、MCP 命令 |
| **ccode-pager-bin** | `crates/codegen/ccode-pager-bin` | TUI 二进制入口 |
| **ccode-pager-render** | `crates/codegen/ccode-pager-render` | 渲染引擎 |
| **ccode-pager-pty** | `crates/codegen/ccode-pager-pty` | PTY 管理：内容、环境、Leader 选举、屏幕控制 |
| **ccode-pager-minimal** | `crates/codegen/ccode-pager-minimal` | 精简 UI 模式：认证、计划、待办面板 |
| **ccode-ratatui** | `crates/codegen/ccode-ratatui` | ratatui 定制分支：scrollback、segment、resize |
| **ccode-textarea** | `crates/codegen/ccode-textarea` | 终端文本编辑器组件 |
| **ccode-markdown** | `crates/codegen/ccode-markdown` | Markdown 渲染：解析、语法高亮、Mermaid 图、LaTeX、URL 检测 |
| **ccode-markdown-core** | `crates/codegen/ccode-markdown-core` | Markdown 核心解析逻辑 |

### 工具系统（源自开源 Grok Build）

| 模块 | 路径 | 职责 |
|------|------|------|
| **ccode-tools** | `crates/codegen/ccode-tools` | 工具注册中心：Computer Use、类型系统、持久化、重试、二进制管理 |
| **ccode-tools-api** | `crates/codegen/ccode-tools-api` | Protobuf 工具 API 定义 |
| **ccode-mcp** | `crates/codegen/ccode-mcp` | MCP 协议实现：传输、凭证、OAuth、服务器管理 |
| **ccode-marketplace** | `crates/codegen/ccode-marketplace` | 插件市场：发现、安装、更新 |

### Hub 工具平台（源自开源 Grok Build）

| 模块 | 路径 | 职责 |
|------|------|------|
| **ccode-hub-core** | `crates/common/ccode-hub-core` | Hub 核心：传输抽象、工具注册表、解析器、本地/远程分发 |
| **ccode-hub-sdk** | `crates/common/ccode-hub-sdk` | Hub SDK：连接池、透明重连、工具 harness、通知、OIDC |
| **ccode-hub-mcp** | `crates/common/ccode-hub-mcp` | MCP→Hub 桥接：将 MCP 工具注册为原生 Hub 工具 |
| **ccode-tool-protocol** | `crates/common/ccode-tool-protocol` | 工具线协议：JSON-RPC 信封、握手、注册、会话事件 |
| **ccode-tool-runtime** | `crates/common/ccode-tool-runtime` | 统一 Tool trait：调度、错误分类、通知、搜索索引 |
| **ccode-tool-types** | `crates/common/ccode-tool-types` | 工具描述类型：Schema 定义、任务类型 |

### 基础设施（源自开源 Grok Build）

| 模块 | 路径 | 职责 |
|------|------|------|
| **ccode-auth** | `crates/codegen/ccode-auth` | 认证：OAuth/OIDC、API Key、JWT、认证提供者 |
| **ccode-config** | `crates/codegen/ccode-config` | 配置加载：层级合并（requirements > user > managed）、TOML/JSON |
| **ccode-config-types** | `crates/codegen/ccode-config-types` | 配置类型定义：Feature flags、MCP 配置、Pool 配置 |
| **ccode-env** | `crates/codegen/ccode-env` | 环境变量集中管理 |
| **ccode-memory** | `crates/codegen/ccode-memory` | 长期记忆：嵌入、BM25/MMR 搜索、分块、Dream 整合 |
| **ccode-compaction** | `crates/common/ccode-compaction` | 上下文压缩：Code/Intra/Inter 三级压缩、Prompt 模板 |
| **ccode-sandbox** | `crates/codegen/ccode-sandbox` | 沙箱：文件系统隔离、网络过滤、deny glob |
| **ccode-secrets** | `crates/codegen/ccode-secrets` | 密钥管理：检测和脱敏 |
| **ccode-telemetry** | `crates/codegen/ccode-telemetry` | 遥测：事件追踪、OpenTelemetry、Sentry |
| **ccode-tracing** | `crates/common/ccode-tracing` | 分布式追踪：fastrace、gRPC/HTTP 上报 |
| **ccode-circuit-breaker** | `crates/common/ccode-circuit-breaker` | 熔断器：状态机、半开探测、观察者、重试策略 |

### 文件/Git 集成（源自开源 Grok Build）

| 模块 | 路径 | 职责 |
|------|------|------|
| **ccode-fs** | `crates/codegen/ccode-fs` | 文件系统：事件追踪、GCS/S3 上传 |
| **ccode-fsnotify** | `crates/codegen/ccode-fsnotify` | 文件变更监听：递归 watch、路径过滤 |
| **ccode-git** | `crates/codegen/ccode-git` | Git 操作封装 |
| **ccode-worktree** | `crates/codegen/ccode-worktree` | Git Worktree 管理：隔离工作目录 |
| **ccode-hunk** | `crates/codegen/ccode-hunk` | 代码变更追踪：Diff、Git 集成、LOC 统计 |
| **ccode-graph** | `crates/codegen/ccode-graph` | 代码图谱：语言解析（TS）、导航、符号索引 |

### 生命周期/通信（源自开源 Grok Build）

| 模块 | 路径 | 职责 |
|------|------|------|
| **ccode-lifecycle** | `crates/codegen/ccode-lifecycle` | 进程生命周期：启动、关闭、信号处理 |
| **ccode-shell-base** | `crates/codegen/ccode-shell-base` | Shell 基础工具：环境变量、路径 |
| **ccode-acp** | `crates/codegen/ccode-acp` | ACP 协议：Gateway 通信、消息规范化 |
| **ccode-hooks** | `crates/codegen/ccode-hooks` | 生命周期钩子：发现、匹配、分发（HTTP/Shell） |
| **ccode-hooks-types** | `crates/codegen/ccode-hooks-types` | 钩子类型定义 |
| **ccode-interjection** | `crates/common/ccode-interjection` | 插入消息：缓冲、格式化、事件 |
| **ccode-http** | `crates/codegen/ccode-http` | HTTP 客户端封装 |
| **ccode-update** | `crates/codegen/ccode-update` | 自动更新检查和安装 |
| **ccode-voice** | `crates/codegen/ccode-voice` | 语音输入：音频管道、STT 集成 |

### 辅助模块（源自开源 Grok Build）

| 模块 | 路径 | 职责 |
|------|------|------|
| **ccode-models** | `crates/codegen/ccode-models` | LLM 模型配置和列表 |
| **ccode-prompt** | `crates/codegen/ccode-prompt` | Prompt 组装和模板 |
| **ccode-tokens** | `crates/codegen/ccode-tokens` | Token 计数 |
| **ccode-version** | `crates/codegen/ccode-version` | 版本信息（build.rs 生成） |
| **ccode-announcements** | `crates/codegen/ccode-announcements` | 公告系统 |
| **ccode-crash** | `crates/codegen/ccode-crash` | 崩溃处理：符号化、格式化、终端恢复 |
| **ccode-journal** | `crates/codegen/ccode-journal` | 日志/日记 |
| **ccode-power** | `crates/codegen/ccode-power` | 电源管理（Linux/macOS/Windows） |
| **ccode-tty** | `crates/codegen/ccode-tty` | TTY 管理 |
| **ccode-paths** | `crates/codegen/ccode-paths` | 路径常量 |
| **ccode-shared** | `crates/codegen/ccode-shared` | 共享类型：Session 信息、剪贴板、UI 配置 |
| **ccode-mermaid** | `crates/codegen/ccode-mermaid` | Mermaid 图渲染：纯 Rust/子进程/引擎模式 |
| **ccode-analytics** | `crates/codegen/ccode-analytics` | 分析统计 |
| **ccode-workspace** | `crates/codegen/ccode-workspace` | 工作空间管理 |
| **ccode-workspace-client** | `crates/codegen/ccode-workspace-client` | 工作空间客户端 |
| **ccode-workspace-types** | `crates/codegen/ccode-workspace-types` | 工作空间类型 |
| **ccode-test-support** | `crates/codegen/ccode-test-support` | 测试基础设施 |
| **ccode-test-utils** | `crates/common/ccode-test-utils` | 测试工具：环境变量、Git fixture、图像 |
| **ptyctl** | `crates/codegen/ptyctl` | PTY 控制库 |
| **ptyctl-cli** | `crates/codegen/ptyctl-cli` | PTY 控制命令行工具 |

### 构建/第三方

| 模块 | 路径 | 职责 |
|------|------|------|
| **ccode-proto-build** | `crates/build/ccode-proto-build` | Protobuf 代码生成 |
| **mermaid-to-svg** | `third_party/mermaid-to-svg` | Mermaid→SVG 转换 |
| **dagre_rust** | `third_party/dagre_rust` | Dagre 有向图布局 |
| **graphlib_rust** | `third_party/graphlib_rust` | 图数据结构 |
| **ordered_hashmap** | `third_party/ordered_hashmap` | 有序 HashMap |
| **cli-chat-proxy-types** | `prod/mc/cli-chat-proxy-types` | CLI Chat Proxy 协议类型 |

---

## 源自开源的能力清单

以下能力源自 [Grok Build](https://github.com/openai/codex)（OpenAI Codex CLI 的早期版本/变体），经品牌融合后成为 ccode 的基础：

### Agent 核心能力

| 能力 | 来源 | 说明 |
|------|------|------|
| **Agentic Loop** | Grok Build | 想→做→看循环：收集上下文→调用 LLM→执行工具→验证结果 |
| **多 Provider 采样** | Grok Build | 支持 OpenAI/Ollama/LM Studio/Bedrock 等多 LLM 提供商 |
| **流式推理** | Grok Build | SSE/Streaming 响应，支持中途取消 |
| **Doom-Loop 防护** | Grok Build | 检测 Agent 陷入重复无效循环并自动中断 |
| **Token 预算管理** | Grok Build | 跟踪输入/输出 token 用量，控制成本 |

### 代码编辑能力

| 能力 | 来源 | 说明 |
|------|------|------|
| **文件读写** | Grok Build | Read/Write/Edit 工具，支持行范围读取、精确字符串替换 |
| **多文件编辑** | Grok Build | 跨文件协调修改 |
| **代码搜索** | Grok Build | Glob 模式匹配、ripgrep 内容搜索 |
| **Shell 执行** | Grok Build | Bash 命令执行，支持超时、后台运行 |

### 上下文管理

| 能力 | 来源 | 说明 |
|------|------|------|
| **三级压缩** | Grok Build | Code Compaction（代码级）、Intra-Compaction（轮内）、Inter-Compaction（跨轮） |
| **Auto-Compact** | Grok Build | Token 接近上限时自动触发摘要压缩 |
| **Prompt 模板** | Grok Build | 可定制的系统/用户 prompt 模板 |

### 上下文智能管理（需增强）

当前上下文管理只做了压缩（省空间），缺少智能调度（用空间）。需要引入冷热分层 + 滑动窗口理论，将有限的上下文空间用到极致：

**核心模型：冷热分层 + 滑动窗口**

```
┌─────────────────────────────────────────────────────────┐
│                   上下文空间（有限 Token 预算）            │
│                                                         │
│  ┌────────────────────────────────────────────────────┐ │
│  │  热区（Hot）— 滑动窗口                              │ │
│  │  最近 N 轮对话 + 当前任务 + 活跃代码文件             │ │
│  │  ✅ 始终完整保留，零损失                             │ │
│  └────────────────────────────────────────────────────┘ │
│                                                         │
│  ┌────────────────────────────────────────────────────┐ │
│  │  温区（Warm）— 压缩摘要                             │ │
│  │  早期对话的精炼摘要 + 关键决策记录 + 错误纠正标注    │ │
│  │  ✅ 保留语义，丢弃细节                               │ │
│  └────────────────────────────────────────────────────┘ │
│                                                         │
│  ┌────────────────────────────────────────────────────┐ │
│  │  冷区（Cold）— 长期记忆                             │ │
│  │  跨会话的项目知识 / 用户偏好 / 已验证的代码模式      │ │
│  │  ✅ 按需检索，不常驻上下文                           │ │
│  └────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────┘
```

**需要增强的 6 项能力**：

| 能力 | 说明 | 优先级 |
|------|------|--------|
| **错误纠正标注** | 用户纠正/否定过的内容标记为「反向知识」，进入上下文时标注 `⚠️ 已纠正：原说法→正确说法`，防止 Agent 重复犯错 | P0 |
| **反事实过滤** | 识别用户说错或已撤回的内容，不进入热区；若必须保留则降级为温区并标注「已否定」 | P0 |
| **滑动窗口调度** | 热区固定窗口大小（如最近 10 轮），超出的内容按语义价值决定：高价值→温区摘要，低价值→丢弃 | P0 |
| **语义价值评估** | 对每轮对话打分：决策/纠正/约束 = 高价值，闲聊/试错/重复 = 低价值，据此决定保留/压缩/丢弃 | P1 |
| **会话级记忆** | 当前会话的完整决策链、约束条件、已排除方案，确保 Agent 不「遗忘」已做过的事 | P0 |
| **长记忆检索** | 冷区（长期记忆）按语义相关性动态检索注入热区，而非全量常驻；用完即回收，释放空间 | P1 |

### 认证与安全

| 能力 | 来源 | 说明 |
|------|------|------|
| **OAuth/OIDC** | Grok Build | 完整的 OAuth 2.0 + OIDC 认证流程 |
| **API Key** | Grok Build | 支持 API Key 和 JWT 双重认证 |
| **沙箱** | Grok Build | 文件系统隔离、网络过滤、deny glob 规则 |
| **密钥脱敏** | Grok Build | 自动检测和脱敏日志中的密钥/Token |

### 工具系统

| 能力 | 来源 | 说明 |
|------|------|------|
| **MCP 协议** | Grok Build | Model Context Protocol 客户端，接入外部工具服务器 |
| **Hub 工具平台** | Grok Build | 统一工具分发：本地/远程、连接池、透明重连 |
| **工具搜索** | Grok Build | BM25 语义搜索，当工具数量多时快速定位 |
| **工具持久化** | Grok Build | 工具配置跨会话持久化 |

### 会话与记忆

| 能力 | 来源 | 说明 |
|------|------|------|
| **会话持久化** | Grok Build | JSONL 格式会话存储、fork/merge |
| **长期记忆** | Grok Build | 嵌入向量 + BM25/MMR 搜索、Dream 整合 |
| **子代理** | Grok Build | 独立上下文的子代理，支持配置覆盖和恢复 |

### TUI 渲染

| 能力 | 来源 | 说明 |
|------|------|------|
| **ratatui TUI** | Grok Build | 基于 ratatui 的全屏终端 UI |
| **Markdown 渲染** | Grok Build | 语法高亮、Mermaid 图、LaTeX、URL 检测 |
| **语音输入** | Grok Build | 麦克风录音→STT 文本输入 |
| **斜杠命令** | Grok Build | /compact、/model、/mcp 等交互命令 |

### 分布式/部署

| 能力 | 来源 | 说明 |
|------|------|------|
| **Leader 选举** | Grok Build | 多实例 Leader 锁，防止重复启动 |
| **自动更新** | Grok Build | 版本检查和自动安装 |
| **遥测** | Grok Build | 事件追踪、OpenTelemetry、Sentry 错误上报 |
| **熔断器** | Grok Build | API 调用熔断：状态机、半开探测、观察者模式 |

---

## ccode 自研能力

以下能力是 ccode 在 Grok Build 基础上自主开发的：

| 能力 | 模块 | 说明 |
|------|------|------|
| **ROS 风格消息总线** | ccore | 微内核 Kernel + Broker，控制面集中管理、数据面点对点直连 |
| **ZeroMQ 通信** | ccore | ZMQ ROUTER/DEALER + PUB/SUB 双面通信 |
| **Node trait** | ccore | 统一 Agent/CLI/Explorer/Worker 等 Node 接口 |
| **Service 机制** | ccore | ROS 风格 Service：注册→发现→直连调用 |
| **Parameter Server** | ccore | ROS 风格参数服务器：全局参数共享 |
| **心跳机制** | ccore | 10 秒间隔心跳，检测 Node 存活 |
| **FFI 接口** | ccore | C FFI 导出，支持动态库方式启动 |
| **CLI Node** | ccode-cli | 替代 TUI 的 stdin/stdout 交互模式 |

---

## 与行业标杆对比及增强方向

### 对比：OpenAI Codex CLI

| 维度 | ccode 现状 | Codex CLI | 差距/增强方向 |
|------|-----------|-----------|-------------|
| **会话持久化** | JSONL 文件存储 | SQLite Thread，跨进程恢复/fork/rollback | **需要增强**：引入 SQLite 会话存储，支持 resume/fork/rollback |
| **沙箱** | 文件系统隔离 + deny glob | OS 内核级沙箱（Linux bubblewrap/macOS Seatbelt/Windows 受限令牌） | **重大差距**：需实现 OS 级沙箱，模型无法绕过 |
| **Patch 解析** | 字符串替换（Edit 工具） | Lark 形式语法解析 apply_patch | **需要增强**：引入结构化 Patch 解析器，降低畸形 Patch 损坏文件风险 |
| **记忆系统** | 嵌入 + BM25/MMR 搜索 + Dream | 两阶段 AI 记忆管线（提取→合并），上限 5000 tokens | **需要增强**：增加 AI 驱动的记忆提取和合并管线 |
| **多 Agent 协作** | 子代理（独立上下文） | spawn_agent/wait_agent/send_input/close_agent + Git Worktree 隔离 | **需要增强**：Agent 间消息传递 + Worktree 并行隔离 |
| **MCP** | 仅客户端 | 客户端 + 服务端 | **需要增强**：支持 MCP Server 模式，让 ccode 可被其他 Agent 调用 |
| **Exec Policy** | 无 | DSL 规则文件，细粒度命令审批 | **需要增强**：引入基于规则的命令审批引擎 |
| **JS REPL** | 无 | 持久化 Node.js 内核 | **可选增强**：嵌入 JS 运行时用于数据转换 |
| **Voice** | 已有 | 已有 | 持平 |
| **架构语言** | Rust | Rust | 持平 |

### 对比：Anthropic Claude Code

| 维度 | ccode 现状 | Claude Code | 差距/增强方向 |
|------|-----------|-------------|-------------|
| **上下文窗口** | 受限于 Provider | 1M token（Opus 4.6+） | 模型层限制，需多 Provider 支持 |
| **压缩策略** | 三级压缩（Code/Intra/Inter） | 五层压缩管道（Budget→Snip→Microcompact→Collapse→Auto） | **需要增强**：增加 Microcompact 和 Context Collapse 层 |
| **Plan Mode** | 无 | EnterPlanMode/ExitPlanMode 工具 | **需要增强**：只读规划模式，先想清楚再动手 |
| **Extended Thinking** | 无 | 深度推理链 + Ultrathink | **需要增强**：支持 reasoning_effort 参数，深度推理模式 |
| **权限模型** | 简单的 PermissionMode 枚举 | 7 种权限模式 + ML 分类器 Auto Mode + Hook 钩子 | **重大差距**：需实现 Auto Mode + 规则引擎 |
| **Hook 系统** | 生命周期钩子（HTTP/Shell） | 8 种事件 × 5 种处理器（command/http/mcp_tool/prompt/agent） | **需要增强**：增加 PreToolUse 拦截、PostToolUse 反馈 |
| **CLAUDE.md** | 无 | 层级化指令文件 + auto-memory + @import | **需要增强**：实现 CCODE.md 层级指令 + 自动记忆写入 |
| **子代理嵌套** | 1 层子代理 | 最多 5 层嵌套，20 并发 | **需要增强**：增加嵌套深度和并发数 |
| **工具数量** | ~15 个内置工具 | ~45 个内置工具 | **需要增强**：TodoWrite/TodoRead、AskUserQuestion、DesignSync 等 |
| **Rewind** | 无 | /rewind 回退到检查点 | **需要增强**：会话回退能力 |
| **Artifacts** | 无 | 会话输出→可分享页面 | **可选增强** |
| **Dynamic Workflows** | 无 | 从脚本编排数十到数百个子代理 | **远期增强** |

### 对比：代码搜索与外部知识获取

| 维度 | ccode 现状 | 行业最佳 | 差距/增强方向 |
|------|-----------|---------|-------------|
| **Web 搜索** | 已有 `WebSearch` 工具 | Codex/Claude 均有 | 持平，需确保默认启用 |
| **Web 抓取** | 已有 `WebFetch` 工具（含 SSRF 防护） | Claude 有，Codex 无内置 | 持平，已领先于 Codex |
| **GitHub 代码搜索** | 无（需通过 Bash 调用 `gh` 命令） | 无竞品原生支持 | **需要增强**：新增 `github_code_search` 工具，调用 GitHub Search API 搜代码实现 |
| **GitHub Repo 浏览** | 无（`WebFetch` 对 GitHub 私有仓库不适用） | Claude 通过 MCP 集成 | **需要增强**：新增 `github_repo` 工具，浏览文件树/读取文件/搜索代码 |
| **文档检索** | 无 | Claude 通过 MCP 集成 | **需要增强**：新增 `doc_search` 工具，搜索技术文档（MDN/rustdoc/cppreference 等） |
| **依赖查找** | 无 | 无竞品原生支持 | **需要增强**：新增 `crate_search` 工具，搜索 crates.io/npm/pypi 找现成依赖 |
| **代码参考集成** | 无 | 无竞品原生支持 | **需要增强**：搜索到的代码可自动提取为参考片段注入 Agent 上下文 |
| **筛选与评估** | 无 | 无竞品原生支持 | **需要增强**：对搜索结果自动评估质量（stars/更新频率/issue 数/license/安全告警），过滤低质量/过时/有风险的选项 |
| **总结与推荐** | 无 | 无竞品原生支持 | **需要增强**：将多个搜索结果精炼为推荐摘要（Top3 推荐 + 理由 + 适配性分析），而非罗列原始结果 |

> **核心原则**：用现成的不等于照搬。搜索只是第一步，筛选总结才是关键——Agent 必须能判断"哪个值得用"和"怎么用"，而非把搜索结果一股脑塞给用户。

### 增强优先级排序

**P0（核心缺失，必须补齐）**：

1. **OS 级沙箱** — 安全是基础，应用层隔离不够
2. **SQLite 会话持久化** — resume/fork/rollback 是生产必需
3. **CCODE.md 层级指令** — 项目级持久记忆是效率关键
4. **Auto Mode 权限** — ML 分类器替代人工确认，实现真正的自主
5. **GitHub 代码搜索** — Agent 写代码必须能找到现成实现，调用 GitHub Search API 搜代码/repo/issue
6. **依赖查找** — 搜索 crates.io/npm/pypi 找现成依赖，学会用现成的而非从零写
7. **筛选与总结** — 搜索结果自动评估质量（stars/更新/issue/license/安全）并精炼为 Top3 推荐 + 理由 + 适配性分析，而非罗列原始结果
8. **错误纠正标注** — 用户纠正/否定过的内容标记为「反向知识」，防止 Agent 重复犯错
9. **滑动窗口调度** — 热区固定窗口，超出的内容按语义价值决定：高价值→温区摘要，低价值→丢弃
10. **会话级记忆** — 完整决策链、约束条件、已排除方案，确保 Agent 不遗忘已做过的事

**P1（显著提升体验）**：

11. **结构化 Patch 解析** — 降低代码损坏风险
12. **Plan Mode** — 先想后做，减少无效修改
13. **冷热分层上下文** — 热/温/冷三层调度，有限空间内保留最关键信息
14. **反事实过滤** — 识别用户说错/撤回的内容，不进热区或标注「已否定」
15. **语义价值评估** — 对每轮对话打分：决策/纠正=高价值，闲聊/试错=低价值
16. **长记忆按需检索** — 冷区按语义相关性动态注入热区，用完回收释放空间
17. **Extended Thinking** — 深度推理，处理复杂任务
18. **PreToolUse/PostToolUse Hook** — 精细控制工具行为
19. **GitHub Repo 浏览** — 浏览开源项目的文件树和代码，学习架构和实现
20. **文档检索** — 搜索 MDN/rustdoc/cppreference 等技术文档，确保 API 用法正确

**P2（增强竞争力）**：

21. **AI 驱动记忆管线** — 跨会话知识积累
22. **MCP Server 模式** — 让 ccode 可被编排
23. **Exec Policy DSL** — 规则引擎，团队级安全策略
24. **Agent 间消息传递** — 复杂多 Agent 编排
25. **Git Worktree 并行隔离** — 多 Agent 并行工作
26. **代码参考集成** — 搜索到的代码自动提取为参考片段注入 Agent 上下文

**P3（锦上添花）**：

27. **Rewind 回退** — 安全网，试错后恢复
28. **JS REPL** — 数据转换/脚本执行
29. **Dynamic Workflows** — 大规模 Agent 编排
30. **Artifacts 分享** — 会话输出可视化

---

## Claude Code 源码融合分析

基于泄露的 Claude Code 源码（`claude-source-code/`），提取以下可融合的设计模式和具体实现方案。

### 1. Hook 系统（20 种事件 × 4 种处理器）

**Claude Code 实现**：完整的声明式 Hook 系统，定义在 `schemas/hooks.ts` 中：

```
事件类型（20 种）：
  PreToolUse / PostToolUse / PostToolUseFailure  — 工具调用前后拦截
  UserPromptSubmit / Stop / StopFailure           — 用户交互和停止
  SessionStart / SessionEnd                        — 会话生命周期
  SubagentStart / SubagentStop                     — 子代理生命周期
  PreCompact / PostCompact                         — 压缩前后
  PermissionRequest / PermissionDenied             — 权限决策
  Notification / Setup                             — 通知和初始化
  TeammateIdle / TaskCreated / TaskCompleted       — 团队协作
  Elicitation                                      — SDK 交互

处理器类型（4 种）：
  command — Shell 命令（支持 bash/powershell、超时、once、async、asyncRewake）
  prompt  — LLM prompt（注入额外指令到模型上下文）
  agent   — 子代理（fork 当前会话执行任务）
  http    — HTTP 请求（Webhook）
```

**关键设计**：
- `matcher` 字段按工具名过滤（如 `matcher: "Bash"` 只在 Bash 工具时触发）
- `if` 条件字段支持权限规则语法（如 `if: "Bash(git *)"` 只在 git 命令时触发）
- `async` + `asyncRewake` 支持后台执行 + 错误时唤醒模型
- Hook 返回可改写工具入参（`updatedInput`）、决策权限（allow/deny/ask/defer）

**ccode 融合方案**：当前 `ccode-hooks` 只有生命周期钩子（HTTP/Shell），需要升级为完整的 4 处理器 × N 事件模型。优先实现 `PreToolUse`（拦截+改写）和 `PostToolUse`（反馈+审计）。

### 2. 微压缩管道（MicroCompact）

**Claude Code 实现**：`services/compact/microCompact.ts`

```
五层压缩管道：
  1. Budget Reduction  — 压缩工具输出的 token 预算
  2. Snip              — 截断超长输出（FileRead/Bash/Grep）
  3. MicroCompact      — 按时间清除旧工具结果，保留语义
  4. Context Collapse  — 大范围摘要压缩
  5. Auto-Compact      — Token 接近上限时自动触发全量压缩
```

**MicroCompact 核心逻辑**：
- `COMPACTABLE_TOOLS` 白名单：FileRead、Bash、Grep、Glob、WebSearch、WebFetch、FileEdit、FileWrite
- 按时间清除旧工具结果（`TIME_BASED_MC_CLEARED_MESSAGE`）
- 支持缓存感知压缩（`cachedMicrocompact`），避免重复压缩已缓存的内容
- `CacheEdits` 机制：压缩结果通过 `cache_edits` 参数注入 API 请求，利用 prompt cache 节省 token

**ccode 融合方案**：当前只有三级压缩（Code/Intra/Inter），需要增加 MicroCompact 层。实现 `COMPACTABLE_TOOLS` 白名单 + 按时间清除策略。

### 3. 自动记忆整合（AutoDream）

**Claude Code 实现**：`services/autoDream/` + `services/extractMemories/`

```
记忆管线：
  extractMemories — 每次会话结束时提取关键知识，写入 ~/.claude/projects/<path>/memory/
  autoDream      — 后台定期整合多个会话的记忆，使用 forked agent + consolidation lock
  /dream 命令    — 手动触发记忆整合
```

**关键设计**：
- `runForkedAgent` 模式：fork 当前会话作为子代理执行记忆提取，共享 prompt cache
- `consolidationLock`：基于 PID 文件的分布式锁，防止多个 ccode 实例同时整合
- 互斥检测：如果主 Agent 已经写了记忆文件，`extractMemories` 跳过（避免重复）
- 记忆格式：Markdown 文件存储在 `~/.claude/projects/<project-path>/memory/` 目录

**ccode 融合方案**：当前有 `ccode-memory` 模块但缺少自动提取和整合。需要实现：
1. 会话结束时自动提取关键知识（决策、约束、已排除方案）
2. 后台定期整合（跨会话去重、提炼摘要）
3. 记忆按语义相关性动态注入上下文（冷区→热区）

### 4. 工具白名单与子代理隔离

**Claude Code 实现**：`constants/tools.ts`

```
工具权限分级：
  ALL_AGENT_DISALLOWED_TOOLS    — 所有子代理禁止：AgentTool、ExitPlanMode、EnterPlanMode、AskUserQuestion、TaskStop
  ASYNC_AGENT_ALLOWED_TOOLS     — 异步代理允许：FileRead、WebSearch、TodoWrite、Grep、WebFetch、Glob、Shell、FileEdit、FileWrite、Skill
  IN_PROCESS_TEAMMATE_ALLOWED_TOOLS — 团队成员允许：TaskCreate/Get/List/Update、SendMessage、CronCreate/Delete/List
  COORDINATOR_MODE_ALLOWED_TOOLS — 协调者允许：AgentTool、TaskStop、SendMessage、SyntheticOutput
```

**关键设计**：
- 子代理不能递归创建子代理（防止失控）
- 异步代理只能用只读+写入工具，不能交互式提问
- 团队成员有额外的任务管理和消息发送能力
- 协调者模式只做编排不做执行

**ccode 融合方案**：当前子代理没有工具白名单隔离。需要实现：
1. `AgentDisallowedTools` 集合：禁止递归 Agent、禁止 Plan 模式、禁止用户交互
2. `AsyncAgentAllowedTools` 白名单：只允许搜索/读写/执行工具
3. 工具注册时声明 `tool_level: readwrite | interactive | orchestrator`

### 5. 完整工具清单对比

**Claude Code 内置工具（~35 个）**：

| ccode 已有 | Claude 独有 | 融合优先级 |
|-----------|------------|-----------|
| Bash | TodoWrite / TodoRead | P0 — Agent 任务管理必备 |
| FileRead | AskUserQuestion | P0 — Agent 与用户交互必备 |
| FileEdit | EnterPlanMode / ExitPlanMode | P1 — 只读规划模式 |
| FileWrite | Task (Agent) | P1 — 子代理 |
| Glob | Skill | P2 — 技能系统 |
| Grep | SendMessage | P1 — Agent 间通信 |
| WebSearch | TaskCreate/Get/Update/List | P2 — 高级任务管理 |
| WebFetch | ToolSearch | P2 — 动态工具发现 |
| — | LSPTool | P1 — 代码智能（跳转/引用/诊断） |
| — | Brief | P2 — 设计简报 |
| — | EnterWorktree / ExitWorktree | P2 — Git Worktree 隔离 |
| — | Workflow | P3 — 动态工作流 |
| — | CronCreate / CronDelete / CronList | P3 — 定时触发 |
| — | NotebookEdit | P3 — Jupyter 笔记本 |
| — | Tungsten (终端) | P3 — 虚拟终端 |

### 6. 权限模型对比

**Claude Code 实现**：

```
权限模式（7 种）：
  default    — 默认，每次询问
  auto-edit  — 编辑自动允许，执行需确认
  full-auto  — 全自动（有 ML 分类器）
  plan       — 只读规划，不执行
  bypass     — 跳过所有权限（管理员）
  yolo       — 无限制
  ask        — 总是询问

权限决策流程：
  1. pre-filtering（工具级快速拒绝：已知危险命令直接 deny）
  2. PreToolUse hook（用户自定义拦截）
  3. Rule evaluation（.claude/rules/ 路径规则 + allow/deny/ask 列表）
  4. Permission handler（弹出确认对话框）
  5. PostToolUse hook（审计+反馈）

权限规则语法：
  allow: ["Bash(git *)", "Read(*)", "Write(src/**)"]
  deny:  ["Bash(rm -rf *)", "Write(.env)"]
  ask:   ["Bash(docker *)"]
```

**ccode 融合方案**：当前只有 `PermissionMode` 枚举（Yolo/Trust/Ask），需要升级为完整的规则引擎。优先实现 `allow/deny/ask` 规则列表 + 路径通配符匹配。

### 7. 会话持久化与 Rewind

**Claude Code 实现**：

```
会话存储：JSONL 格式（每行一条消息）
  ~/.claude/projects/<project-path>/sessions/<session-id>.jsonl

Rewind 实现：
  - 每条消息带 uuid
  - /rewind 找到指定 uuid，截断后续消息
  - 保留压缩摘要（compactBoundary）避免丢失上下文

Session Resume：
  - 加载 JSONL 文件恢复完整对话历史
  - 自动重新执行工具结果（或标记为 stale）
```

**ccode 融合方案**：需要实现 JSONL 会话存储 + uuid 消息标识 + rewind 截断。

---

## 技术栈

- **语言**：Rust（~95%）、TypeScript（npm 包装层）、Python（Hook 示例）
- **消息总线**：ZeroMQ（ROUTER/DEALER + PUB/SUB）
- **序列化**：serde_json、Protobuf（tonic/prost）
- **终端 UI**：ratatui + crossterm
- **AI SDK**：async-openai（支持 Responses API）
- **存储**：SQLite（rusqlite）、JSONL 会话
- **网络**：reqwest + axum + tokio-tungstenite
- **可观测性**：fastrace + OpenTelemetry + Sentry
- **协议**：ACP、MCP、Protobuf

## 开发

```bash
# 构建
cargo build

# 运行 CLI
cargo run -p ccode-cli

# 运行 TUI
cargo run -p ccode-pager-bin

# 静态分析
cargo clippy --all-targets
```

## 许可证

Apache-2.0
