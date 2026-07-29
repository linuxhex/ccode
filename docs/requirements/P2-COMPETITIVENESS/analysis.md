# 需求分析（P2 增强竞争力）

## 需求概述
> 补齐 ccode 对标 Claude Code 和 Codex CLI 的 3 项 P2 差距：网络代理层、Rollout 重放调试、Rewind 会话回退。增强 ccode 在安全、调试、容错方面的竞争力。

## 业务背景
- Codex 有内置网络代理层（MITM 代理），可检查和过滤出站流量，ccode 无此能力
- Codex 有 Rollout 追踪/重放，支持 bundle 重放调试，ccode 无此能力
- Claude Code 有 /rewind 回退到检查点，ccode 无此能力
- 这三项虽非核心缺失，但显著影响安全性和开发体验

## 本项目职责

| 职责项 | 说明 |
|--------|------|
| 网络代理层 | 内置 HTTP/SOCKS5 代理，检查/过滤 Agent 出站流量，防止数据泄露 |
| Rollout 重放 | 会话执行轨迹录制 + bundle 重放调试，支持事后分析 Agent 决策 |
| Rewind 回退 | 会话回退到指定检查点，丢弃后续修改，安全网 |

## 详细设计

### 1. 网络代理层

**对标**：Codex 的 codex-network-proxy

**架构**：
```
Agent 出站请求 → 网络代理 → 域名规则检查 → 允许/拒绝/修改 → 目标服务器
                     ↓
              审计日志（请求/响应摘要）
```

**核心能力**：
- **HTTP 代理**：拦截 HTTP/HTTPS 请求
- **SOCKS5 代理**：拦截任意 TCP 连接
- **MITM TLS 拦截**：对 HTTPS 请求做中间人解密，检查请求内容
- **域名规则**：
  - 白名单模式：只允许指定域名（如 api.openai.com, api.anthropic.com）
  - 黑名单模式：禁止指定域名
  - 默认：白名单模式，只允许已知 LLM API 域名
- **请求检查**：
  - 检查请求体是否包含敏感信息（复用 ccode-secrets 检测）
  - 检查请求 URL 是否符合策略
  - 记录审计日志
- **配置**：
  - `network_proxy.mode`：off / whitelist / blacklist
  - `network_proxy.allowed_domains`：白名单域名列表
  - `network_proxy.blocked_domains`：黑名单域名列表
  - `network_proxy.audit_log`：审计日志路径
  - `network_proxy.mitm`：是否启用 MITM（默认 false，需用户信任 CA 证书）

**实现要点**：
- 基于 `hudsucker` 或自研 HTTP 代理（Rust 异步实现）
- 代理作为子进程启动，Agent 的 HTTP 客户端配置代理
- MITM 需生成自签名 CA 证书，用户需手动信任
- 审计日志格式：JSONL，每行一条请求记录

### 2. Rollout 重放调试

**对标**：Codex 的 codex-rollout

**架构**：
```
Agent 执行 → RolloutRecorder → 录制每步决策 → rollout bundle (.jsonl)
                                                          ↓
重放调试 ← RolloutPlayer ← 加载 bundle ← 逐步回放每步决策
```

**核心能力**：
- **录制**：
  - 每步 Agent 决策：LLM 请求/响应、工具调用/结果、权限审批
  - 每步带时间戳、token 用量、耗时
  - 存储格式：JSONL，每行一条步骤记录
  - 存储位置：`~/.ccode/rollouts/<session-id>.jsonl`
- **Bundle 打包**：
  - 将 rollout + 相关文件快照打包为 `.tar.gz`
  - 包含：rollout.jsonl + 项目文件快照 + 配置快照
  - 用于分享和离线分析
- **重放**：
  - 加载 rollout bundle
  - 逐步回放：next/prev/goto_step
  - 显示每步的：LLM 输入/输出、工具调用/结果、token 用量
  - 支持断点：在指定步骤暂停
- **分析**：
  - 统计：总 token 用量、各工具调用次数、耗时分布
  - 异常检测：Doom Loop 迹象、重复工具调用、token 浪费
  - 导出：Markdown 报告

**实现要点**：
- `RolloutRecorder`：在 Agent 循环中埋点录制
- `RolloutPlayer`：TUI 中新增 `/replay` 命令启动重放模式
- `RolloutAnalyzer`：分析工具，生成统计报告
- 与 ccode-telemetry 集成：rollout 数据可上报到 OTel

### 3. Rewind 会话回退

**对标**：Claude Code 的 /rewind

**架构**：
```
会话历史：[msg1, msg2, msg3, msg4, msg5]
                                    ↑
                          /rewind msg3
                                    ↓
回退后：[msg1, msg2, msg3]  +  [msg4, msg5] 标记为 rewound
```

**核心能力**：
- **消息 UUID**：每条消息带唯一 UUID
- **/rewind 命令**：
  - `/rewind`：回退到上一条用户消息
  - `/rewind <uuid>`：回退到指定消息
  - `/rewind <n>`：回退 n 步
- **回退逻辑**：
  1. 找到目标消息 UUID
  2. 截断后续消息（标记为 rewound，不删除）
  3. 撤销后续消息中的文件修改（git checkout 或反向编辑）
  4. 保留压缩摘要（compactBoundary）避免丢失上下文
- **安全保护**：
  - 回退前显示将丢弃的内容摘要
  - 需用户确认（Y/n）
  - rewound 消息可恢复（/rewind-undo）
- **文件撤销**：
  - 记录每次文件编辑的旧内容
  - 回退时按逆序撤销编辑
  - 无法撤销的编辑（如 Bash 命令的副作用）标注为「不可撤销」

**实现要点**：
- 在 ccode-chat 的 ConversationItem 中新增 `uuid` 字段
- 新增 `RewindManager`：管理回退逻辑和文件撤销
- TUI 新增 `/rewind` 命令
- 与 Git Checkpoint 集成：回退时优先用 git checkout 撤销

## 复用现有模块

| 现有模块 | 复用方式 |
|----------|----------|
| ccode-http | 网络代理基于 HTTP 客户端扩展 |
| ccode-secrets | 网络代理检查请求体中的敏感信息 |
| ccode-sandbox | 网络代理与沙箱网络策略协同 |
| ccode-telemetry | Rollout 数据上报到 OTel |
| ccode-chat | Rewind 修改 ConversationItem 结构 |
| ccode-compaction | Rewind 保留 compactBoundary |
| ccode-hooks | Rollout 录制埋点 |
| ccode-pager | TUI 新增 /replay 和 /rewind 命令 |

## 技术选型

| 领域 | 选型 | 理由 |
|------|------|------|
| 网络代理 | hudsucker 或自研 | Rust 异步 HTTP 代理库 |
| MITM TLS | rcgen + rustls | 自签名 CA 证书生成 |
| Rollout 存储 | JSONL | 与会话存储格式一致 |
| Bundle 打包 | tar + flate2 | 标准压缩格式 |
| 消息 UUID | uuid v7 | 时间排序 + 唯一 |

## 风险与注意

- ⚠️ MITM 代理需用户手动信任 CA 证书，体验有摩擦，默认关闭
- ⚠️ 网络代理与沙箱网络策略需协同，避免冲突
- ⚠️ Rollout 录制有性能开销，需控制录制粒度（可配置只录关键步骤）
- ⚠️ Rewind 撤销文件修改有风险：Bash 命令副作用无法撤销，需明确标注
- ⚠️ Rewind 与 MicroCompact 交互：回退后压缩摘要可能不一致，需重新压缩
- 💡 网络代理可渐进上线：先只做审计日志（不拦截），验证后再启用拦截
- 💡 Rollout 重放可先做录制+分析，重放 UI 后续迭代
