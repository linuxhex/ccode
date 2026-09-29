# CLI Coding Agent 深度源码对比（Phase B/C 维度）

> 对比对象：Codex / Claude Code / OpenCode / deepcode-cli / Harness CLI
> 分析日期：2026-08-26
> 基于 5 个并行分析 agent 的深入源码级调研

---

## 一、沙箱与安全隔离

### 1.1 总览

| 工具 | 沙箱级别 | macOS | Linux | Windows | 网络隔离 | 凭证保护 |
|------|---------|-------|-------|---------|---------|---------|
| **Codex** | ★★★★★ | seatbelt | bwrap+landlock+seccomp | Restricted Token+WFP+ACL | 进程内代理 (HTTP CONNECT/SOCKS5) | shaped dummy 替换 |
| **Claude Code** | ★★★★ | bwrap | bwrap+iptables+ipset | N/A | 代理端口+域名白名单 | JWT-aware+SigV4 |
| **deepcode-cli** | ★★ | 无 OS 沙箱 | 无 | 无 | 无 | 无 |
| **OpenCode** | ★ | 无 | 无 | 无 | 无 | 无 |
| **Harness** | ★★★ | N/A | N/A | N/A | N/A | 0600 文件+DACL |

### 1.2 Codex 沙箱架构（最完整）

**核心抽象**：`SandboxManager` trait + `PermissionProfile { file_system, network }` 单一源

**三平台后端**：
- **macOS**：`/usr/bin/sandbox-exec`（固定路径防注入）+ `.sbpl` 策略文件（base + network + preferences）
- **Linux**：bwrap 构建文件系统视图 → seccomp 安装系统调用过滤 → landlock 作为遗留回退
- **Windows**：持久本地用户 `CodexSandboxOffline/Online` + WFP 过滤器（按 SID 绑定）+ DACL + Job Object

**网络代理**（`network-proxy` crate）：
- 进程内 HTTP CONNECT / SOCKS5 代理（rama 框架）
- 目标 IP 分类器 `is_non_public_ip()`（SSRF 防御）
- **凭证代理**：子进程环境变量中的 `OPENAI_API_KEY` / `GH_TOKEN` 被替换为同长度的 shaped dummy，真实凭证仅在代理层注入请求头
- 域名策略：`None < Allow < Deny` 排序，deny 优先
- Unix socket 白名单（防止绕过代理）

**exec-server 进程隔离**：
- 独立 JSON-RPC 服务器（WebSocket / Noise-NK 加密通道）
- 父进程从不直接执行用户命令
- 文件系统操作通过 fd 传递（子进程打开文件，返回 fd，父进程永远看不到路径）

### 1.3 deepcode-cli 副作用声明制

**10 种副作用范围**：
```
read-in-cwd / read-out-cwd / write-in-cwd / write-out-cwd
delete-in-cwd / delete-out-cwd / query-git-log / mutate-git-log
network / unknown
```

**决策优先级**：deny > ask > allow > defaultMode
- `unknown` 在非 allowAll 模式下始终 ask
- 模型必须在每个 Bash 调用中声明 `sideEffects` 数组
- 无效声明自动降级为 `["unknown"]`

**持久化**：用户选"always allow"时写入 `.deepcode/settings.json` 的 `permissions.allow`

### 1.4 对 Qoder 的启示（Phase B1）

| 借鉴项 | 来源 | 优先级 |
|--------|------|--------|
| `PermissionProfile` 单一声明式结构，三平台后端翻译 | Codex | P0 |
| 网络默认关闭 + 显式 loopback 代理端点 | Codex | P0 |
| 凭证 shaped dummy 替换（子进程看不到真实密钥） | Codex | P1 |
| bwrap 预检（500ms 探测 user namespace） | Codex | P1 |
| 副作用声明制 + `unknown` 安全阀 | deepcode-cli | P0 |
| deny > ask > allow > default 决策优先级 | deepcode-cli | P1 |
| Pre-tool hook 作为轻量替代（无 OS 沙箱时） | Claude Code | P2 |

---

## 二、Sub-Agent 生命周期

### 2.1 总览

| 维度 | Codex V2 | Claude Code | OpenCode | deepcode-cli |
|------|----------|-------------|----------|-------------|
| 工具集 | spawn/send_message/followup_task/wait/list/interrupt | Agent/SendMessage/interrupt | agent（一次性） | 无 |
| 并发上限 | `AgentExecutionLimiter` + `V2Residency` LRU | 默认 20（可配） | 无限制 | N/A |
| 嵌套深度 | 无限制（受 residency 约束） | 默认 3（可配） | 1（不可嵌套） | N/A |
| 上下文继承 | FullHistory / LastNTurns / None | fork = 全量+cache；其他隔离 | 独立空会话 | N/A |
| 通信方式 | Mailbox + `InterAgentCommunication` | SendMessage（跨机器） | 仅最终报告 | N/A |
| 持久化 | SQLite 图存储（parent/child 边+状态） | 会话存储+forkedFrom | ParentSessionID FK | N/A |
| 驻留策略 | LRU 驱逐 + `shutdown_and_wait` | 长期后台会话 | 一次性后回收 | N/A |

### 2.2 Codex V2 架构（最完整）

**图存储**（`agent-graph-store` crate）：
- SQLite 持久化 parent/child 边
- `ThreadSpawnEdgeStatus { Open, Closed }`
- BFS 遍历 `list_thread_spawn_descendants`

**AgentControl**：
- `Arc` 共享 `AgentRegistry` + `V2Residency` + `AgentExecutionLimiter` + `RolloutBudget`
- `Weak<ThreadManagerState>` 避免循环引用
- `SpawnReservation` RAII 原子预留

**V2Residency（LRU 驻留管理）**：
- 有上限的驻留槽位
- 自动驱逐 `Completed|Errored|Interrupted` 且无活跃 turn、无待处理 mailbox 的 agent
- 驱逐时保存 `environment_selections` 以便恢复

**Fork 模式**：
- `FullHistory`：复制父对话完整 rollout（过滤工具调用/安全评分等），保留 prompt cache
- `LastNTurns(n)`：截断最后 N 轮，子 agent 重建上下文
- 继承环境、执行策略、多 agent 版本

**Mailbox 系统**：
- `InputQueue` 带 `Mutex<VecDeque<PendingMailboxCommunication>>` + `watch::Sender<InputQueueActivity>`
- `send_message` = 仅入队（`trigger_turn=false`）
- `followup_task` = 入队+触发 turn（`trigger_turn=true`）
- `wait_agent` 订阅 watch channel 等待活动

### 2.3 Claude Code 独特设计

- **fork subagent**（`subagent_type: "fork"`）：继承完整对话+prompt cache，成本最低
- **跨机器 SendMessage**：@-mention 其他会话名，ListAgents 发现
- **Worktree 隔离**：fork 的会话获得独立的 git worktree
- **模型降级**：org-restricted 模型自动降级到家族中最新允许模型

### 2.4 对 Qoder 的启示（Phase B3）

| 借鉴项 | 来源 | 优先级 |
|--------|------|--------|
| 图存储（parent/child 边+状态，SQLite 持久化） | Codex | P0 |
| SpawnReservation RAII 原子预留 | Codex | P0 |
| V2 Mailbox（trigger_turn 区分 queue-only 和 queue+trigger） | Codex | P0 |
| LRU Residency 驱逐 + 环境快照恢复 | Codex | P1 |
| Fork 模式继承 prompt cache | Claude Code | P1 |
| 深度控制（环境变量/配置旋钮，默认 3） | Claude Code | P1 |
| 并发上限（AtomicUsize + ExecutionGuard Drop 释放） | Codex | P0 |
| OTEL 可观测性（agent_communication 事件） | Codex | P2 |

---

## 三、错误处理与恢复

### 3.1 总览

| 维度 | Codex | Claude Code | OpenCode | deepcode-cli | Harness |
|------|-------|-------------|----------|-------------|---------|
| 错误枚举 | `CodexErrorDetails`（40+ 变体） | 闭源 | 哨兵错误+`FinishReason` | `unknown` | Go `error` |
| LLM 重试 | 可配（request=4, stream=5） | 有（30min Remote Control） | 8 次，2s×2^n±20% | 无 | 无 |
| 退避策略 | `base*2^(n-1)*jitter(0.9-1.1)` | 指数（未公开） | `2000*2^(n-1)*(1±20%)` | N/A | 固定 2s |
| 速率限制 | 解析多窗口 header | Gateway 上游故障转移 | `Retry-After` 覆盖 | 无 | 无 |
| 错误分类 | `is_retryable()` 全枚举 | 瞬态/永久分类 | 按 provider `StatusCode` | `isAbortLikeError` | HTTP 状态码 |
| 传输回退 | WebSocket → HTTPS | 非流式回退 | 无 | 无 | SSE → blob 轮询 |
| 空闲超时 | 300s | 90s（可配） | 无 | 180s keep-alive | 90min (follow) |
| 循环上限 | N/A | N/A | N/A | 80,000 次 | N/A |
| 暂停/恢复 | rollout_trace 持久化 | `--continue/--resume` | Cancel via Map | JSONL 索引+`/resume` | N/A |

### 3.2 Codex 错误分类体系

**可重试**：`Stream`, `Timeout`, `RequestTimeout`, `UnexpectedStatus`, `ResponseStreamFailed`, `ConnectionFailed`, `InternalServerError`, `InternalAgentDied`, `Io`, `Json`, `TokioJoin`

**不可重试**：`TurnAborted`, `SessionBudgetExceeded`, `Interrupted`, `Fatal`, `UsageNotIncluded`, `QuotaExceeded`, `InvalidRequest`, `ContextWindowExceeded`, `ServerOverloaded`, `CyberPolicy`, `MisalignmentPolicyViolation`

**流重连**（`ResponsesStreamRetryState`）：
- 初始延迟 5s，最大 60s，每次翻倍
- 尊重服务端 `retry_delay`
- 超过最大重试 → `try_switch_fallback_transport`（WS→HTTPS）
- `UnboundedConnectionRetries` feature 启用时无限重连

**凭证恢复**：`ModelProvider::recover_from_unauthorized` 透明刷新 token

### 3.3 OpenCode 重试实现

```go
backoffMs := 2000 * (1 << (attempts - 1))   // 2s, 4s, 8s, 16s...
jitterMs := int(float64(backoffMs) * 0.2)    // ±20%
// Retry-After header 覆盖计算值
```

每个 provider 独立的 `shouldRetry`：
- Anthropic: 429, 529
- OpenAI: 429, 500
- Copilot: 429, 500 + 401→刷新 token→重试
- Gemini: 字符串匹配 "rate limit" / "quota exceeded"

### 3.4 deepcode-cli 恢复策略

- **无 LLM 重试**：依赖 OpenAI SDK 内置重试
- **80,000 次循环上限**（"约 1K RMB"）：达到后提示用户是否继续
- **会话状态持久化**：`sessions-index.json` + JSONL，`/resume` 恢复
- **工具调用修复**：中断恢复时插入占位符 `{ error: "Previous tool call did not complete." }`
- **Compaction**：`activeTokens > autoCompactWindow` 时自动摘要压缩

### 3.5 对 Qoder 的启示

| 借鉴项 | 来源 | 优先级 |
|--------|------|--------|
| 显式 `is_retryable()` 错误分类枚举 | Codex | P0 |
| 传输回退（WebSocket → HTTPS） | Codex | P1 |
| `stream_idle_timeout` 流空闲看门狗 | Codex | P0 |
| 凭证透明刷新（`recover_from_unauthorized`） | Codex | P1 |
| 循环上限（成本保护） | deepcode-cli | P1 |
| 中断恢复时工具调用占位符修复 | deepcode-cli | P2 |
| `Retry-After` header 尊重 | OpenCode | P1 |

---

## 四、流式响应与 Prompt 工程

### 4.1 流式协议

| 工具 | 协议 | 空闲超时 | 事件分类 |
|------|------|---------|---------|
| **Codex** | SSE + WebSocket 回退 | 300s | `OutputItemAdded/Done`, `TextDelta`, `ToolCallInputDelta`, `ReasoningSummaryDelta`, `SafetyBuffering`, `Completed` |
| **Claude Code** | SSE（gateway/Bedrock/Vertex） | 90s | `message_delta`, `content_block_delta`, `message_stop` |
| **OpenCode** | SDK SSE（Anthropic/OpenAI/Gemini） | 无 | `ContentStart/Delta/Stop`, `ThinkingDelta`, `ToolUseStart/Delta/Stop`, `Complete`, `Error` |
| **deepcode-cli** | OpenAI SSE（180s keep-alive） | 180s keep-alive | `ChatCompletionChunk`（text, reasoning_content, tool_calls） |
| **Harness** | SSE（pipeline 日志） | 90min | `EvStart`, `EvEnd`, `EvLogLine`, `EvBlob` |

### 4.2 Prompt 构造对比

| 工具 | 系统提示结构 | Fragment 化 |
|------|------------|------------|
| **Codex** | 多个 `ContextualUserFragment`（BaseInstructions / DeveloperInstructions / ManagedDeveloperInstructions / TokenBudgetContext / CompactionSummary 等），XML 标记分隔 | ★★★★★ 完全 fragment 化 |
| **Claude Code** | 模块化 XML 片段（`<frontend_aesthetics>`, `<coding_guidelines>`, `<tool_behavior>`），6 段式规范 | ★★★★ 模块化 |
| **OpenCode** | 单字符串：`CoderPrompt(provider)` + `envInfo` + `lspInformation` + 项目上下文 | ★★ 单一字符串 |
| **deepcode-cli** | 系统消息列表（工具 prompt → 运行时上下文 → AGENTS.md → plan-mode → 用户消息 → skills → skill catalog） | ★★★ 有序列表 |
| **Harness** | 无 LLM prompt（YAML spec 驱动命令语法） | N/A |

### 4.3 Prompt Cache 策略

| 工具 | 缓存机制 | 断点布局 | 关键优化 |
|------|---------|---------|---------|
| **Codex** | `prompt_cache_key` = session_id / thread_id | 服务端自动 | World State diff 保持前缀字节不变；工具 schema 字节稳定 |
| **Claude Code** | Anthropic `cache_control` | 4 断点（system / tools / last large msg / current user） | 1h TTL；日期移出 system；工具描述字节稳定；fan-out 交错 `CLAUDE_CODE_WORKFLOW_PREFIX_STAGGER_MS` |
| **OpenCode** | Anthropic `cache_control: ephemeral` | 3 断点（system / last tool / last 3 messages） | 基本布局 |
| **deepcode-cli** | 无前缀缓存提示 | 无 | 依赖稳定排序 + 服务端隐式缓存 |

### 4.4 Token 预算管理

| 工具 | 来源 | 自动压缩 | 预算提醒 |
|------|------|---------|---------|
| **Codex** | 服务端 `usage`（cached_tokens, cache_write_tokens, reasoning_tokens） | 本地 `compact.rs` + 远程 `compact_remote*.rs` | `TokenBudgetReminder` / `TokenBudgetRemainingContext` 注入 developer fragment |
| **Claude Code** | 服务端 per-block final usage | 服务端/客户端混合 | 未知（闭源） |
| **OpenCode** | 服务端 `InputTokens/OutputTokens/CacheCreationTokens` | 无（手动触发 Summarizer agent） | 无 |
| **deepcode-cli** | 服务端 `total_tokens/prompt_cache_hit/miss` | `autoCompactWindow` 阈值触发 | 无 |

### 4.5 对 Qoder 的启示

| 借鉴项 | 来源 | 优先级 |
|--------|------|--------|
| World State diff（仅重发变化的 fragment） | Codex | P1 |
| 字节稳定工具 schema（不随会话变化） | Claude Code | P0 |
| `TokenBudgetRemainingContext` 注入提醒模型剩余空间 | Codex | P1 |
| Fan-out 交错启动（后续 agent 命中缓存前缀） | Claude Code | P2 |
| `prompt_cache_key` 共享（sub-agent 继承 session_id） | Codex | P0 |
| 流空闲超时 + 传输回退 | Codex | P1 |

---

## 五、配置与插件系统

### 5.1 总览

| 维度 | Codex | Claude Code | OpenCode | deepcode-cli | Harness |
|------|-------|-------------|----------|-------------|---------|
| 配置格式 | TOML | JSON | JSON | JSON | YAML |
| 配置层级 | 9 层（-10 到 50 优先级） | 企业→用户→项目→flag | env→local→global→默认 | 默认→用户→项目→env | profile+embedded+home specs |
| 插件清单 | `plugin.json`（Agent Plugins v1 + 遗留） | `.claude-plugin/plugin.json` + marketplace | 无 | statusline provider in settings | `.spec.yaml` |
| 安装方式 | marketplace/npm/git/remote | marketplace（git/zip/command/gitlab） | 无 | 放文件+声明 | `harness install plugin` |
| Hook 事件 | 11 个 | ~15 个（含 asyncRewake） | 无 | 无 | 无 |
| Skill 系统 | 完全（BM25+n-gram 动态选择器） | 完全（3 级渐进披露） | 无 | 5 级扫描+LLM 匹配 | 无 |
| 项目记忆 | `.codex/AGENTS.md` | `CLAUDE.md` | 多文件名自动检测 | `AGENTS.md` | 无 |

### 5.2 Codex 配置层（最复杂）

```
PackagedDefaults (-10)
  < Mdm (0)
    < System (10)
      < EnterpriseManaged (15)
        < User { profile } (20/21)
          < Project .codex/ (25)
            < SessionFlags (30)
              < LegacyManagedConfigTomlFromFile (40)
                < LegacyManagedConfigTomlFromMdm (50)
```

每层独立版本化，可单独禁用。

### 5.3 Codex 插件生态

**安装源**：bundled / git marketplace / npm / remote catalog / local bundle

**跨工具兼容**：识别 `.codex-plugin/` / `.claude-plugin/` / `.cursor-plugin/` 三种清单路径

**Skill 动态选择器**：
- 字段化 BM25 + 字符 n-gram
- 多查询词法匹配
- LRU+character / LRU+lexical 路由
- RRF lexical/char 加权

**Hook 信任**：`HookStateToml { enabled, trusted_hash }`，`allowManagedHooksOnly` 模式

### 5.4 Claude Code 插件市场

**13 官方插件**：agent-sdk-dev, claude-opus-4-5-migration, code-review, commit-commands, explanatory-output-style, feature-dev, frontend-design, hookify, learning-output-style, plugin-dev, pr-review-toolkit, ralph-wiggum, security-guidance

**市场源**：GitHub / GitLab / archive（zip+SHA-252 pinning）/ command

**asyncRewake**：Stop hook 长时间运行后唤醒 Claude，注入 `rewakeMessage` 作为后续 prompt

**插件本地设置**：`.claude/<plugin-name>.local.md`（YAML frontmatter + markdown，per-project，gitignored）

### 5.5 Harness 自描述插件

**安装流程**：
1. 获取二进制
2. **身份门控**：运行 `<binary> --identity`，验证 `harness_` 前缀哨兵
3. 安装到 `~/.harness/bin/`
4. 捕获语法：`<binary> --spec`
5. 写入 `~/.harness/spec/<name>.spec.yaml`

**自描述查询**：`--identity`（身份）+ `--spec`（语法转储），无需单独注册

### 5.6 对 Qoder 的启示

| 借鉴项 | 来源 | 优先级 |
|--------|------|--------|
| 配置分层（至少 user/project/session 三级） | Codex | P0 |
| 跨工具插件清单兼容（.codex-plugin/.claude-plugin） | Codex | P2 |
| asyncRewake hook（长时间后台任务完成后唤醒） | Claude Code | P1 |
| 插件本地设置（per-project, gitignored） | Claude Code | P2 |
| 自描述插件（--identity + --spec） | Harness | P1 |
| Skill 动态选择器（BM25+embedding 混合） | Codex | P2 |
| `enabledSkills` per-skill 开关 | deepcode-cli | P1 |

---

## 六、综合优先级矩阵

### Phase B 建议实施顺序

| 序号 | 改进项 | 复杂度 | 收益 | 来源 |
|------|--------|--------|------|------|
| B1 | 错误分类枚举 + `is_retryable()` | 低 | 高 | Codex |
| B2 | 流空闲超时 + 传输回退 | 低 | 高 | Codex |
| B3 | `prompt_cache_key` 共享 + 字节稳定 schema | 中 | 高 | Codex/Claude |
| B4 | Sub-Agent 图存储 + 生命周期完善 | 高 | 高 | Codex |
| B5 | 副作用声明制权限完善 | 中 | 中 | deepcode-cli |
| B6 | 平台沙箱（macOS seatbelt 优先） | 高 | 高 | Codex |
| B7 | 凭证 shaped dummy + 网络代理 | 高 | 高 | Codex |
| B8 | 配置分层完善（9 层→至少 4 层） | 中 | 中 | Codex |
| B9 | Hook asyncRewake | 中 | 中 | Claude Code |
| B10 | 自描述插件系统 | 中 | 中 | Harness |

### 关键架构决策

1. **沙箱路线选择**：
   - 路线 A：直接借鉴 Codex 三平台沙箱（工作量大，安全等级最高）
   - 路线 B：先实现副作用声明制 + Pre-tool hook（工作量小，覆盖 80% 用例）
   - **建议**：先 B 后 A

2. **Sub-Agent 演进路线**：
   - 先实现图存储 + Mailbox + 并发上限（V2 核心）
   - 再实现 LRU Residency + Fork 继承（性能优化）
   - 最后实现跨机器 SendMessage（分布式）

3. **Prompt Cache 优化**：
   - 最优先：工具 schema 字节稳定（消除会话内缓存失效）
   - 其次：`prompt_cache_key` sub-agent 共享
   - 再次：World State diff 仅重发变化 fragment

---

## 七、代码定位索引

### Codex 关键文件

| 模块 | 路径 |
|------|------|
| 沙箱管理器 | `codex-rs/sandboxing/src/manager.rs` |
| macOS seatbelt | `codex-rs/sandboxing/src/seatbelt.rs` |
| Linux bwrap | `codex-rs/linux-sandbox/src/bwrap.rs` |
| Linux landlock | `codex-rs/linux-sandbox/src/landlock.rs` |
| Windows 沙箱 | `codex-rs/windows-sandbox-rs/src/` |
| 网络代理 | `codex-rs/network-proxy/src/` |
| exec-server | `codex-rs/exec-server/src/` |
| Agent 控制 | `codex-rs/core/src/agent/control.rs` |
| Agent 图存储 | `codex-rs/agent-graph-store/src/` |
| V2 Residency | `codex-rs/core/src/agent/control/residency.rs` |
| 重试策略 | `codex-rs/codex-client/src/retry.rs` |
| 错误类型 | `codex-rs/protocol/src/error.rs` |
| 流重连 | `codex-rs/core/src/responses_retry.rs` |
| 配置层 | `codex-rs/config/src/state.rs` |
| 插件管理 | `codex-rs/core-plugins/src/manager.rs` |
| Hook 引擎 | `codex-rs/hooks/src/engine/dispatcher.rs` |
| Skill 选择器 | `codex-rs/ext/skills/dynamic_skill_selector/` |
| Prompt fragment | `codex-rs/core/src/context/` |

### Claude Code 关键文件

| 模块 | 路径 |
|------|------|
| 市场清单 | `.claude-plugin/marketplace.json` |
| 沙箱设置 | `examples/settings/settings-bash-sandbox.json` |
| 防火墙脚本 | `.devcontainer/init-firewall.sh` |
| Hook 示例 | `plugins/security-guidance/hooks/hooks.json` |
| 插件结构文档 | `plugins/plugin-dev/skills/plugin-structure/SKILL.md` |
| Hook 开发文档 | `plugins/plugin-dev/skills/hook-development/SKILL.md` |
| Gateway 部署 | `examples/gateway/aws/` |

### deepcode-cli 关键文件

| 模块 | 路径 |
|------|------|
| 副作用解析 | `packages/core/src/common/permissions.ts::parseBashSideEffects` |
| 权限决策 | `packages/core/src/common/permissions.ts::evaluatePermissionScopes` |
| 会话主循环 | `packages/core/src/session.ts` |
| Skill 扫描 | `packages/core/src/session.ts::getSkillScanRoots` |
| 错误描述 | `packages/core/src/common/llm-error.ts` |

### OpenCode 关键文件

| 模块 | 路径 |
|------|------|
| 重试逻辑 | `internal/llm/provider/anthropic.go::shouldRetry` |
| 权限服务 | `internal/permission/permission.go` |
| Agent 工具 | `internal/llm/agent/agent-tool.go` |
| 持久化 Shell | `internal/llm/tools/shell/shell.go::PersistentShell` |
| Prompt Cache | `internal/llm/provider/anthropic.go` (cache_control) |
