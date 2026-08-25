# CLI Coding Agent 源码对比分析

> 对比对象：Claude Code / Codex / OpenCode / deepcode-cli / Qoder(ccode) / Harness CLI
> 分析日期：2026-08-25

---

## 一、基础技术栈对比

| 维度 | Claude Code | Codex | OpenCode | deepcode-cli | Qoder(ccode) | Harness CLI |
|------|-------------|-------|----------|--------------|-------------|-------------|
| **语言** | TypeScript (闭源) | **Rust** | Go | TypeScript | **Rust** | Go |
| **TUI** | 自有 (闭源) | **ratatui** | **BubbleTea** | **React+Ink** | **ratatui** | **BubbleTea** |
| **构建** | npm bundle | Bazel+Cargo | GoReleaser | esbuild | Cargo | GoReleaser+Taskfile |
| **存储** | 文件系统 | SQLite | SQLite | JSONL+Git | ZMQ IPC+文件 | 无状态 |
| **开源** | 否 (仅文档) | Apache 2.0 | 是 | 是 | 私有 | Apache 2.0 |

**关键发现**：
- Rust 阵营（Codex + Qoder）选择 ratatui 做 TUI，性能最优但开发成本高
- Go 阵营（OpenCode + Harness）选择 BubbleTea，Elm 架构清晰
- TypeScript 阵营分化：Claude Code 闭源，deepcode-cli 走 React+Ink 路线

---

## 二、核心架构模式对比

### 2.1 Agent Loop 实现

| 工具 | Loop 模式 | 并发模型 | 关键特征 |
|------|----------|---------|---------|
| **Claude Code** | 标准 agentic loop | 并行 tool call | Auto-mode 权限分类器；Fork subagent 继承 prompt cache |
| **Codex** | SQ/EQ 异步协议 | 并行 tool call + 并行 sub-agent | Submission/Event 队列解耦 UI 和引擎；World State 追踪 |
| **OpenCode** | 经典 for{} 无限循环 | 顺序 tool call | 全服务 PubSub 事件驱动；SQLite 持久化每条消息 |
| **deepcode-cli** | 单线程顺序 loop | 无并发 | 循环上限 80,000 次；遇权限/用户问题暂停-恢复 |
| **Qoder** | **状态机驱动** | 读写分离并发 | LoopStateMachine 显式状态转换；ROS 1 双平面消息总线 |
| **Harness** | 无 Agent Loop | N/A | 声明式 YAML 驱动，不是 Agent |

**差异核心**：
- Codex 和 Qoder 都走向了"引擎与 UI 完全解耦"的路线，但实现方式不同：Codex 用 SQ/EQ channel，Qoder 用 ZMQ 消息总线
- deepcode-cli 是最简模型：单线程、暂停-恢复、无并发
- Qoder 的状态机是唯一将 loop 显式建模为 `Idle→Thinking→AwaitingApproval→ToolCalling→Done` 的

### 2.2 架构范式

```
Claude Code:    Plugin Platform（插件平台 + 闭源引擎）
Codex:          Extension Registry（扩展注册表 + 沙箱隔离）
OpenCode:       PubSub Monolith（发布订阅单体）
deepcode-cli:   Single-Agent Sequential（单 Agent 顺序）
Qoder:          Biomimetic Microkernel（仿生微内核）
Harness:        Spec-Driven Declarative（规格驱动声明式）
```

---

## 三、工具系统对比

| 维度 | Claude Code | Codex | OpenCode | deepcode-cli | Qoder | Harness |
|------|-------------|-------|----------|--------------|-------|---------|
| **工具定义** | 编译进二进制 | ToolSpec + JSON Schema | BaseTool interface | OpenAI function schema | ToolExecutor trait | YAML spec |
| **内置工具数** | ~15 | ~25+ | ~13 | 10 | ~15+ | 0 (不是 Agent) |
| **编辑模式** | Edit/MultiEdit | **apply_patch** (diff) | edit (字符串替换) | **snippet-scoped** | search_replace | N/A |
| **Shell** | 沙箱内执行 | **exec-server 进程隔离** | **持久化单例 shell** | 持久化 session | 安全缓存 | N/A |
| **动态发现** | MCP deferred | tool_search | - | - | - | 自描述查询 |
| **验证** | 无 | Schema 验证 | 无 | **Zod v4 强验证** | Schema 验证 | YAML 校验 |

**独特设计**：
- **deepcode-cli Snippet 系统**：必须先 read 再 edit，编辑绑定 snippet_id + 文件版本号，过期自动拒绝。这是最严格的编辑安全机制
- **Codex apply_patch**：用 unified diff 而非字符串替换，避免歧义匹配
- **Qoder 读写并发分离**：只读工具并发执行（Semaphore 20），写工具串行

---

## 四、权限与安全对比

| 维度 | Claude Code | Codex | OpenCode | deepcode-cli | Qoder | Harness |
|------|-------------|-------|----------|--------------|-------|---------|
| **权限模型** | 三层 ask/deny/allow | 四级审批模式 | PubSub 请求-响应 | 10 种作用域 | 四模式 + 14 项检查 | RBAC + PAT/SAT |
| **沙箱** | **bwrap + iptables** | **seatbelt/bwrap/landlock/Windows** | 无 | 无 | 无 | 无 |
| **网络隔离** | iptables/ipset + proxy | NetworkSandboxPolicy | 无 | 无 | 无 | N/A |
| **凭证保护** | JWT-aware + SigV4 | - | - | - | - | 0600 文件权限 |
| **Auto 模式** | AI 分类器自动审批 | OnRequest 模型决定 | - | 副作用声明制 | 14 项安全检查 | - |

**差异核心**：
- **Codex 沙箱最强**：四平台原生沙箱（macOS seatbelt / Linux bwrap+landlock / Windows restricted token），加上 exec-server 进程隔离
- **Claude Code 次之**：bwrap + iptables 网络隔离 + 凭证掩码
- **Qoder 走不同路线**：不做 OS 级沙箱，而是 14 项 Shell 安全检查（注入检测、路径穿越、Unicode 欺骗等）
- **deepcode-cli 创新**：让模型自己声明 sideEffects，权限基于声明而非命令解析

---

## 五、上下文管理对比

| 维度 | Claude Code | Codex | OpenCode | deepcode-cli | Qoder | Harness |
|------|-------------|-------|----------|--------------|-------|---------|
| **Compaction 级别** | 自动 + 手动 /compact | 自动 + 服务端 v2 | 单级 summarization | 单级 compact | **4 级渐进压缩** | N/A |
| **触发阈值** | 接近窗口上限 | 预采样检查 | 95% | autoCompactWindow | 85%/90%/95%/99% | N/A |
| **Prompt Cache** | **共享 prefix + fork 继承** | Context Fragment 管理 | 最近 3 条消息 + 系统提示 | **缓存感知布局** | Stable Prefix 保护 | N/A |
| **记忆系统** | 无 | World State 追踪 | 文件版本历史 | 会话状态重建 | **3 层记忆 (L0/L1/L2)** | N/A |

**差异核心**：
- **Qoder 记忆系统最复杂**：L0 工作记忆（Hot→Warm→Cold）、L1 短期记忆（反射信号）、L2 情景记忆（跨会话关键词索引）
- **Qoder 4 级压缩**：Snip → MicroCompact → AutoCompact → ReactiveCompact，逐级升级
- **Claude Code Prompt Cache 最优**：fork subagent 继承完整 prompt cache，auto-mode 权限检查复用缓存前缀
- **deepcode-cli 缓存感知**：系统提示在前、变量内容在后，最大化 DeepSeek 缓存命中

---

## 六、Sub-Agent 对比

| 维度 | Claude Code | Codex | OpenCode | deepcode-cli | Qoder |
|------|-------------|-------|----------|--------------|-------|
| **支持** | 是 | 是 (V1+V2) | 是 | **否** | 是 |
| **嵌套深度** | 3 层 (可配) | 多层 | 1 层 | N/A | 支持嵌套 |
| **继承模式** | Fork (继承完整对话+cache) | ForkSnapshot 快照 | 独立新会话 | N/A | 共享 Sampler+Tool |
| **并发上限** | 20 (可配) | 多线程并行 | 单 agent | N/A | 资源限制 |
| **通信** | SendMessage | InterAgentMessage | 仅最终结果 | N/A | 消息总线 |
| **隔离** | git worktree | 独立 thread | 独立 session | N/A | tokio task 隔离 |

**差异核心**：
- **Claude Code fork 模式最独特**：sub-agent 继承父对话的完整上下文和 prompt cache，成本最低
- **Codex 最完整**：V1/V2 两代实现，spawn/wait/send/interrupt/list/resume 全套生命周期
- **deepcode-cli 是唯一不支持 sub-agent 的**

---

## 七、MCP 集成对比

| 维度 | Claude Code | Codex | OpenCode | deepcode-cli | Qoder | Harness |
|------|-------------|-------|----------|--------------|-------|---------|
| **传输** | stdio/SSE/HTTP/WS | rmcp | stdio/SSE | stdio | rmcp (quarantine) | 无 |
| **工具命名** | `mcp__plugin_server__tool` | 标准转换 | `server_tool` | `mcp__server__tool` | 标准 | N/A |
| **连接管理** | 懒加载 + 按需连接 | McpRuntime Projection | 每次新建连接 | 崩溃重连 | 健康监控 | N/A |
| **双向** | 是 (也可做 MCP server) | 是 (mcp-server crate) | 否 | 否 | 是 (ccore-advanced) | N/A |

---

## 八、独特功能矩阵

| 功能 | Claude Code | Codex | OpenCode | deepcode-cli | Qoder | Harness |
|------|:-----------:|:-----:|:--------:|:------------:|:-----:|:-------:|
| 插件市场 | **13 官方插件** | Extension Registry | - | - | - | Plugin 二进制 |
| Hook 系统 | **15 事件 + asyncRewake** | Pre/PostToolUse | - | - | **15 事件 + 5 阶段链** | - |
| Skill 系统 | **3 级渐进披露** | Skills 注入 | - | 动态 skill 加载 | .skill/*.md | - |
| 反射弧 | - | - | - | - | **3 级 (L0/L1/L2)** | - |
| Doom Loop 检测 | - | - | - | - | **4 级降级策略** | - |
| 元认知控制 | - | - | - | - | **难度自评 + 策略适配** | - |
| 经验学习 (ERL) | - | - | - | - | **多角色反思 + 启发式池** | - |
| 编辑版本追踪 | - | - | 文件版本 | **Snippet 版本** | Git snapshot hook | - |
| 多格式输出 | - | - | - | - | - | **6 种格式** |
| Agent 检测 | - | - | - | - | - | **9 种 Agent 识别** |
| 加密 Prompt | - | - | - | - | **XOR 混淆模板** | - |
| 自描述查询 | - | - | - | - | - | **list/get --matrix** |

---

## 九、架构成熟度评估

| 维度 | Claude Code | Codex | OpenCode | deepcode-cli | Qoder | Harness |
|------|:-----------:|:-----:|:--------:|:------------:|:-----:|:-------:|
| 工程完整度 | ★★★★★ | ★★★★★ | ★★★★ | ★★★ | ★★★★ | ★★★★ |
| 架构创新 | ★★★ | ★★★★ | ★★★ | ★★ | ★★★★★ | ★★★★ |
| 安全隔离 | ★★★★★ | ★★★★★ | ★★ | ★★ | ★★★ | ★★★ |
| 可扩展性 | ★★★★★ | ★★★★★ | ★★★ | ★★ | ★★★★ | ★★★★★ |
| 代码可读性 | ★ (闭源) | ★★★★ | ★★★★★ | ★★★★ | ★★★ | ★★★★★ |
| 性能潜力 | ★★★ | ★★★★★ | ★★★★ | ★★★ | ★★★★★ | ★★★★ |

---

## 十、对 Qoder(ccode) 的启示

### Qoder 已领先的领域
1. **微内核架构** — ROS 1 双平面消息总线，Node 间解耦最彻底
2. **记忆系统** — 3 层记忆 + 4 级压缩，其他工具望尘莫及
3. **元认知 + ERL** — 难度自评、策略适配、经验学习，学术前沿落地
4. **反射弧** — 3 级快速响应，无需 LLM 即可处理简单信号

### Qoder 需要补强的领域
1. **沙箱隔离** — Codex 四平台原生沙箱 + exec-server 进程隔离值得借鉴
2. **Prompt Cache 优化** — Claude Code 的 fork 继承 + 权限检查缓存复用
3. **Sub-Agent 完善** — Codex V2 的完整生命周期管理（spawn/wait/send/interrupt/list/resume）
4. **编辑安全** — deepcode-cli 的 Snippet 版本追踪机制值得参考
5. **插件生态** — Claude Code 的插件市场 + 3 级 Skill 渐进披露
6. **声明式扩展** — Harness 的 YAML spec 驱动，新增 CRUD 零代码

### 可快速借鉴的低垂果实
1. **deepcode-cli 的副作用声明制** — 让模型声明 bash 命令的 sideEffects，简化权限判断
2. **Codex 的 Context Fragment** — 将系统提示拆为独立可管理的片段，便于 compaction 时精确控制
3. **Harness 的 Agent 检测** — 通过环境变量检测驱动 Agent，用于遥测和适配
4. **OpenCode 的持久化 Shell** — 单例 shell 保持环境变量和工作目录，减少上下文丢失
