# Codex vs ccode 架构对比报告

> 基于 OpenAI Codex 官方开源版（Rust），日期：2026-08-25

---

## 一、整体架构对比

### 分层对比

| 层级 | **Codex**（130+ crate） | **ccode**（63 codegen crate） |
|---|---|---|
| **入口层** | `cli`（主 binary）+ `tui`（TUI binary）+ `app-server-daemon`（后台服务） | `ccode-cli`（单 binary，启动 kernel） |
| **核心层** | `core`（agent 引擎 + 工具调用 + 压缩 + 状态 + 记忆 + session）+ `core-api` + `core-plugins` | `ccore`（消息总线 + 6 Node，3.9万行，但工具/记忆等能力弱） |
| **能力层** | 独立 crate：`tools` / `config` / `state` / `rollout` / `history` / `mcp` / `hooks` / `memory*` / `skills` / `sandboxing` / `exec` / `thread-manager` / ... | 混合：ccore 一套（能力弱但接入总线）+ ccode-shell 一套（能力强但不在主线） |
| **TUI 层** | `tui` crate（27.6万行 ratatui，独立进程） | 两套：ccore TUINode（993行） + ccode-pager（42.7万行，多进程架构） |
| **基础层** | `utils/*`（20+ 小工具 crate）+ `protocol` + `model-provider` | `common/`（11个 crate）+ 零散 crate |

### 架构哲学对比

| 维度 | Codex | ccode（当前） |
|---|---|---|
| **核心引擎** | 单 `core` crate，内聚 19.9 万行，职责清晰 | 双引擎（ccore + shell），功能重叠，70% 代码不进主流程 |
| **TUI** | 独立进程，通过 app-server 或 stdio 与 core 通信 | 两套：ccore 集成简易 TUI + pager 独立 TUI（空壳） |
| **工具** | core 内置 + core-plugins + ext/* 插件化扩展 + MCP | 两套：ccore 16个工具 + ccode-tools 12.5万行工具集 |
| **扩展机制** | ext/ 细粒度插件 + plugin crate + skill 系统 | hooks 系统 + skills + marketplace（shell 侧） |
| **部署模式** | CLI / TUI / app-server daemon / IDE 插件 | 仅 CLI（TUI 为内置简化版） |

---

## 二、TUI 架构对比

### Codex TUI（27.6万行，ratatui）

**顶层模块结构（按规模）：**

| 模块 | 行数 | 职责 |
|---|---|---|
| `resume_picker.rs` | 6911 | 会话选择器（启动时选历史会话） |
| `app_server_session.rs` | 3865 | app-server 会话管理 |
| `keymap.rs` | 3642 | 键位映射系统（庞大） |
| `app_command.rs` | 3401 | 命令系统 |
| `markdown_render.rs` | 2915 | Markdown 渲染（ratatui 适配） |
| `diff_render.rs` | 2559 | Diff 渲染（红绿对比） |
| `chatwidget.rs` | 2039 | 聊天组件（对话流主体） |
| `pager_overlay.rs` | 1800 | 分页覆盖层（全屏查看代码等） |
| `dynamic_tools.rs` | 1506 | 动态工具 UI（MCP 工具等） |
| `markdown_stream.rs` | 898 | 流式 Markdown 渲染 |
| `tui.rs` | 1224 | TUI 主循环 |
| `app/` 子目录 | 50+ 文件 | 应用状态、事件分发、会话管理、权限、历史、agent 导航等 |

**关键架构特点：**

1. **事件驱动**：`app_event.rs` + `app_event_sender.rs` 定义完整事件系统，各组件通过事件通信
2. **App 状态集中**：`app/` 目录 50+ 文件管理所有状态（会话、agent、历史、权限、配置...）
3. **组件丰富**：resume_picker / diff_render / pager_overlay / dynamic_tools / multi_agents 等高级组件
4. **键位系统复杂**：3642 行 keymap.rs + 1952 行 keymap_setup.rs，支持高度定制
5. **服务端会话**：`app_server_session.rs` 表明 TUI 既能本地模式也能连远程 app-server

### ccode TUI 现状

| 路径 | 规模 | 状态 |
|---|---|---|
| ccore TUINode | 993 行 | ✅ 真跑，ratatui 三段式（对话+状态栏+输入框） |
| ccode-pager | 42.7 万行 | ❌ 空壳入口，多进程架构，未接入主线 |

### 可借鉴的 TUI 特性（优先级排序）

1. **diff_render** — 代码 diff 红绿对比渲染（ccode 已有 ccode-pager 的 diff 能力，但主线没有）
2. **pager_overlay** — 全屏查看长代码/大输出（按某个键放大查看）
3. **dynamic_tools UI** — 工具调用的动态渲染（展开/收起、参数、结果）
4. **resume_picker** — 启动时选择历史会话
5. **markdown_stream** — 流式 Markdown 渲染优化（ccode 已有 ccode-markdown，但可借鉴流式增量渲染）
6. **multi_agents** — 多 agent 状态展示
7. **完善的 keymap 系统**

---

## 三、Agent 引擎对比

### Codex Core（19.9万行）

**核心对象：`CodexThread`（954 行）**

| 方法 | 说明 |
|---|---|
| `submit(op: Op)` | 提交操作（用户输入、取消、steer 等） |
| `start_or_steer_turn()` | 开始或引导一个 turn |
| `next_event()` | 拉取下一个事件（流式输出、工具调用、状态变化等） |
| `agent_status()` | 获取 agent 状态 |
| `token_usage_info()` | token 使用统计 |

**事件驱动架构**：TUI 通过 `next_event()` 拉取事件（类似订阅模式的 pull 版本），事件类型包括：
- 流式文本输出
- 工具调用/工具结果
- 状态变化
- 终端输出
- 权限请求

**核心模块（按重要性）：**

| 模块 | 规模 | 职责 |
|---|---|---|
| `codex_thread.rs` | 954 行 | 主循环 / turn 管理 / 事件流 |
| `codex_delegate.rs` | 368 行 | 运行器（interactive / one_shot 模式） |
| `agent/` | — | agent 角色、注册、控制、状态 |
| `compact*.rs`（10+ 文件） | ~3000 行 | 压缩系统（本地 + 远程 v1/v2） |
| `session_prefix.rs` | 50 行 | 稳定前缀管理（简单但关键） |
| `thread_manager.rs` | — | 多线程管理 |
| `context/` + `context_manager/` | — | 上下文管理 |
| `mcp_tool_call/` + `mcp_tool_exposure/` | — | MCP 工具集成 |
| `guardian/` | — | 安全防护（沙箱策略） |
| `exec/` + `exec_env/` + `exec_policy/` | — | 代码执行 / 沙箱 |
| `rollout.rs` + `rollout_budget.rs` | — | 特性灰度发布 |
| `skills.rs` | — | Skill 系统 |
| `state/`（独立 crate） | — | 状态持久化 |

### ccode 引擎对比

| 引擎 | 规模 | 状态 | 主循环 |
|---|---|---|---|
| ccore ThinkerNode | 2275 行 | ✅ 总线接入，能力弱 | 消息驱动，有完整的 turn 循环 + 工具调用 + 流式 |
| ccode-shell MvpAgent | 33 万行 | ❌ 未接入主线，独立架构 | ACP 协议驱动，功能完整 |

### 关键差异

| 特性 | Codex Core | ccore ThinkerNode | ccode-shell MvpAgent |
|---|---|---|---|
| 主循环模式 | 事件驱动（pull 模式） | 消息驱动（push 模式，总线） | ACP 协议驱动 |
| 工具数量 | ~几十个内置 + MCP + ext | 16 个 | ~22 个内置 + MCP + hooks |
| 压缩系统 | compact_remote_v2（远程压缩，1191行）+ 本地 + token budget | working_memory 4级压缩（自实现） | 自有 compaction + common/ccode-compaction |
| 子 Agent | agent/role + agent/registry（完整体系） | Orchestrator + SubAgentNode（简单） | subagent 完整体系 |
| 记忆 | memories/read + memories/write 独立 crate + thread-store | episodic memory + repo map（零调用） | memory_state（shell 自有） |
| 沙箱 | linux-sandbox + windows-sandbox + bwrap（三层） | 简单 sandbox crate | ccode-sandbox + exec policy |
| Rollout/特性开关 | rollout + rollout-trace（完整系统） | 无（硬编码） | 部分有 |
| 状态持久化 | state crate + state_db_bridge | persistence 模块（已通电） | shell 自有持久化 |

---

## 四、工具系统对比

### Codex 工具架构（四层）

```
┌─────────────────────────────────────────────┐
│  ext/* 插件（goal/memory/skills/mcp/...）   │  扩展层
├─────────────────────────────────────────────┤
│  core-plugins（核心插件）                   │  插件层
├─────────────────────────────────────────────┤
│  core/tools + core/mcp_tool_call            │  工具运行层
├─────────────────────────────────────────────┤
│  tools crate（schema/dynamic tool）         │  工具定义层
└─────────────────────────────────────────────┘
```

**特点：**
- `tools` crate（6744 行）只定义工具 schema 和动态工具接口
- 实际工具实现在 `core/src/tools/`、`core-plugins/`、`ext/*/` 各处
- MCP 深度集成（`mcp_tool_call.rs` / `mcp_tool_exposure.rs`）
- 工具权限：与 exec_policy / sandboxing 深度整合
- 动态工具运行时注册

### ccode 工具架构（两套，互不连通）

```
ccore/tools/ (21文件, 7927行)    ccode-tools/ (225文件, 12.5万行)
       ↓                                    ↓
  ToolNode（总线）                    MvpAgent / shell 路径
  （16个工具）                       （完整工具集 + MCP + hooks）
```

### 可借鉴点

1. **工具延迟加载**（Codex 也做了，但方式不同）
2. **MCP 深度集成**（ccode-tools 已有，但主线不通）
3. **工具 + 沙箱 + 权限 三层防护**（codex 做得更深）
4. **插件化 ext/* 扩展机制**（比 ccode 的 hooks 更灵活）

---

## 五、压缩 / Prompt Cache 对比

### Codex 压缩系统（10 文件，~3000 行）

| 模块 | 行数 | 作用 |
|---|---|---|
| `compact_remote_v2.rs` | 1191 | 远程压缩 v2（核心） |
| `compact.rs` | 799 | 本地压缩 |
| `compact_remote.rs` | 521 | 远程压缩 v1 |
| `compact_token_budget.rs` | 93 | token 预算管理 |
| `compact_model_fallback.rs` | 64 | 模型降级时的压缩策略 |
| `compact_remote_history.rs` | 56 | 历史压缩管理 |
| `compact_remote_v2_attempt.rs` | 142 | v2 单轮压缩尝试 |
| `compact_remote_v2_images.rs` | 100 | 图像预算 |
| `compact_remote_request.rs` | 102 | 压缩请求构造 |

**核心思路：**
- **compact_remote_v2**：用独立的 LLM 请求做摘要压缩（类似 Claude Code 的压缩方式），不是在当前轮里让模型自己压缩
- 有完整的 token 预算管理
- 支持模型降级时的压缩适配
- 支持图像预算控制

### ccode 压缩系统（三套）

| 路径 | 实现 | 状态 |
|---|---|---|
| ccore working_memory | 4 级压缩（hot/warm/cold/archive） | ❌ 死代码 |
| ccode-shell compaction | 自有实现 | ❌ 未接入主线 |
| common/ccode-compaction | 共享库（7990 行） | 部分使用 |

### 关键差距

- codex 的 **remote_v2 压缩**（独立 LLM 请求做摘要）比 ccore 的 4 级自管理更成熟、效果更好
- codex 的 **session_prefix** 稳定前缀简单有效（50 行），ccore 有 `lock_stable_prefix` 但从未调用
- **prompt cache**：codex 深度整合在 API 层，ccore 有 cache_control 类型但未在主线生效

---

## 六、配置 & Rollout 对比

| 特性 | Codex | ccode |
|---|---|---|
| 配置大小 | config crate 2.5 万行 | config crate 1 万行 |
| 特性开关 | rollout + rollout-trace 完整系统 | 无统一系统 |
| 灰度发布 | 服务端拉取 + 本地覆盖 | 无 |
| 配置层 | 默认 → 用户配置 → managed config → rollout | 默认 → 配置文件 → 环境变量 → CLI 参数 |
| 配置验证 | 有（config_schema） | 不确定 |

---

## 七、总结：ccode 应该从 codex 吸收什么？

### 高价值、低工作量（先做）

1. **session_prefix 稳定前缀** — codex 只有 50 行，ccore 已有 `lock_stable_prefix` 但没接入主循环，接通就行
2. **工具输出预算控制** — codex 有独立的 `output-truncation` util crate，ccode 已有思路，接进主线即可
3. **工具延迟加载** — codex + claude-code 都有，ccore 有实现但在死路径上
4. **cache_control + 稳定前缀** — codex 深度整合，ccode 类型已定义但未生效

### 中价值、中工作量

5. **远程压缩 v2 思路** — 用独立 LLM 请求做摘要，效果好但要设计好触发时机
6. **工具侧栏 UI** — codex 的 dynamic_tools UI 模式可以借鉴
7. **diff_render** — 代码 diff 可视化，pager 已有能力，迁进主线即可
8. **rollout 特性开关系统** — 长期有益，但需要服务端配合

### 高价值、高工作量（长期）

9. **沙箱分层安全** — linux-sandbox + bwrap + exec_policy，codex 做得很深
10. **多 agent 体系** — agent/role + agent/registry + inter-agent communication
11. **app-server 模式** — 后台 daemon + 多会话管理（远期目标）
12. **ext/* 插件化架构** — 比 hooks 更灵活的扩展机制

### 确认的架构共识

- **单 core 内聚**：codex 的 core crate 19.9 万行，所有核心能力都在这里，ccore 现在只有 3.9 万行且能力分散
- **细粒度工具 crate**：codex 把工具相关的拆得很细（tools / tool-protocol / tool-runtime / tool-types），ccode 也有类似拆分但层级不清晰
- **TUI 独立**：codex 的 TUI 是独立进程、独立 crate，通过事件/协议与 core 通信
- **状态外置**：state 是独立 crate，core 通过 state_db_bridge 访问
