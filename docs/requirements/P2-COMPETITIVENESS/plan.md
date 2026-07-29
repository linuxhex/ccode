# P2 增强竞争力实现计划

**目标：** 补齐 3 项 P2 差距，增强 ccode 安全、调试、容错竞争力

**架构：** 新增网络代理层、Rollout 录制/重放、Rewind 会话回退

**技术栈：** Rust + hudsucker (HTTP代理) + rcgen/rustls (MITM) + uuid v7 + tar/flate2 (bundle)

---

## 交付阶段

### 阶段 A：网络代理层 — 流量更安全
- HTTP/SOCKS5 代理服务器
- 域名规则检查（白名单/黑名单）
- 审计日志
- MITM TLS 拦截（可选）

### 阶段 B：Rollout 重放调试 — 决策可追溯
- Rollout 录制器（Agent 循环埋点）
- Bundle 打包
- 重放播放器 + 分析器
- TUI /replay 命令

### 阶段 C：Rewind 会话回退 — 试错有安全网
- 消息 UUID
- /rewind 命令 + 回退逻辑
- 文件修改撤销
- /rewind-undo 恢复

---

## 文件清单

| 文件 | 操作 | 职责 |
|------|------|------|
| crates/codegen/ccode-http/src/proxy.rs | 新增 | HTTP/SOCKS5 代理服务器 |
| crates/codegen/ccode-http/src/proxy_rules.rs | 新增 | 域名规则检查 |
| crates/codegen/ccode-http/src/proxy_audit.rs | 新增 | 审计日志 |
| crates/codegen/ccode-http/src/mitm.rs | 新增 | MITM TLS 拦截 |
| crates/codegen/ccode-http/src/lib.rs | 修改 | 导出代理模块 |
| crates/codegen/ccode-shell/src/rollout.rs | 新增 | Rollout 录制器 |
| crates/codegen/ccode-shell/src/rollout_player.rs | 新增 | Rollout 重放播放器 |
| crates/codegen/ccode-shell/src/rollout_analyzer.rs | 新增 | Rollout 分析器 |
| crates/codegen/ccode-shell/src/rollout_bundle.rs | 新增 | Bundle 打包/解包 |
| crates/codegen/ccode-shell/src/lib.rs | 修改 | 导出 rollout 模块 |
| crates/codegen/ccode-chat/src/rewind.rs | 新增 | Rewind 回退管理器 |
| crates/codegen/ccode-chat/src/file_undo.rs | 新增 | 文件修改撤销记录 |
| crates/codegen/ccode-chat/src/lib.rs | 修改 | 导出 rewind 模块 |
| crates/codegen/ccode-pager/src/rewind_cmd.rs | 新增 | /rewind 命令处理 |
| crates/codegen/ccode-pager/src/replay_cmd.rs | 新增 | /replay 命令处理 |

---

## 任务拆分

### 任务 1：HTTP/SOCKS5 代理服务器

**目标**：内置代理服务器，拦截 Agent 出站请求

**文件**：
- 新增：`crates/codegen/ccode-http/src/proxy.rs`

**实现要点**：
- `NetworkProxy`：代理服务器主结构
- HTTP 代理：基于 hudsucker 或 hyper，监听本地端口
- SOCKS5 代理：基于 tokio，支持 TCP 连接转发
- 代理启动：作为子进程或 tokio task 启动
- Agent HTTP 客户端配置：设置 HTTP_PROXY/HTTPS_PROXY 环境变量
- 生命周期：随 Agent 启动/关闭

**核心逻辑示意**：
```rust
pub struct NetworkProxy {
    listen_addr: SocketAddr,
    rules: ProxyRules,
    audit: ProxyAudit,
    mode: ProxyMode,  // Off / Whitelist / Blacklist
}
impl NetworkProxy {
    pub async fn start(&self) -> Result<()> { /* 启动代理 */ }
    pub async fn handle_request(&self, req: &HttpRequest) -> ProxyDecision { /* 规则检查 */ }
}
```

---

### 任务 2：域名规则 + 审计日志

**目标**：域名白名单/黑名单检查 + 请求审计日志

**文件**：
- 新增：`crates/codegen/ccode-http/src/proxy_rules.rs`
- 新增：`crates/codegen/ccode-http/src/proxy_audit.rs`

**实现要点**：
- `ProxyRules`：域名规则引擎
  - `check(domain) -> ProxyDecision`：Allow/Deny/Ask
  - 白名单默认域名：api.openai.com, api.anthropic.com, api.deepseek.com 等
  - 支持通配符：*.github.com
- `ProxyAudit`：审计日志记录器
  - 格式：JSONL，每行一条请求记录
  - 记录：timestamp, method, url, status, response_size, decision
  - 存储位置：`~/.ccode/proxy_audit.jsonl`
  - 敏感信息脱敏（复用 ccode-secrets）

---

### 任务 3：MITM TLS 拦截（可选）

**目标**：对 HTTPS 请求做中间人解密，检查请求内容

**文件**：
- 新增：`crates/codegen/ccode-http/src/mitm.rs`

**实现要点**：
- CA 证书生成：rcgen 生成自签名 CA 证书
- 证书存储：`~/.ccode/mitm-ca.crt` + `~/.ccode/mitm-ca.key`
- TLS 拦截：rustls 配置动态证书生成
- 请求检查：解密后检查请求体是否包含敏感信息
- 首次使用提示：引导用户信任 CA 证书
- 默认关闭：需 `network_proxy.mitm = true` 显式启用

---

### 任务 4：Rollout 录制器

**目标**：在 Agent 循环中埋点录制每步决策

**文件**：
- 新增：`crates/codegen/ccode-shell/src/rollout.rs`

**实现要点**：
- `RolloutRecorder`：录制器
- 录制粒度（可配置）：
  - `full`：录制 LLM 请求/响应完整内容
  - `summary`：只录制摘要（工具名+结果摘要）
  - `metrics`：只录制指标（token/耗时）
- 每步记录：`RolloutStep`
  - step_number, timestamp, step_type (llm_call/tool_call/permission/user_input)
  - input_summary, output_summary
  - token_usage, duration_ms
  - tool_name, tool_result_summary（工具调用时）
  - permission_decision（权限审批时）
- 存储格式：JSONL，每行一条 RolloutStep
- 存储位置：`~/.ccode/rollouts/<session-id>.jsonl`
- 在 Agent 循环关键点埋点：LLM 调用前/后、工具执行前/后、权限审批时

---

### 任务 5：Bundle 打包 + 重放播放器

**目标**：打包 rollout 为可分享 bundle，支持重放调试

**文件**：
- 新增：`crates/codegen/ccode-shell/src/rollout_bundle.rs`
- 新增：`crates/codegen/ccode-shell/src/rollout_player.rs`

**实现要点**：
- `RolloutBundle`：打包/解包
  - `pack(rollout_path, project_snapshot, config) -> .tar.gz`
  - `unpack(bundle_path) -> RolloutBundle`
  - 包含：rollout.jsonl + 项目文件快照 + 配置快照
- `RolloutPlayer`：重放播放器
  - `load(bundle) -> Vec<RolloutStep>`
  - `next() / prev() / goto(step_n)`：逐步回放
  - `play(speed)`：自动播放（1x/2x/5x）
  - 显示：每步的 LLM 输入/输出、工具调用/结果、token 用量
  - 断点：在指定步骤暂停
- TUI 集成：`/replay <bundle_path>` 命令进入重放模式

---

### 任务 6：Rollout 分析器

**目标**：分析 rollout 数据，生成统计报告

**文件**：
- 新增：`crates/codegen/ccode-shell/src/rollout_analyzer.rs`

**实现要点**：
- `RolloutAnalyzer`：分析工具
- 统计指标：
  - 总 token 用量（输入/输出/缓存）
  - 各工具调用次数和耗时
  - LLM 调用次数和平均耗时
  - 权限审批统计（allow/deny/ask 比例）
- 异常检测：
  - Doom Loop 迹象（重复相同工具调用 >3 次）
  - Token 浪费（单次工具输出 >5000 token）
  - 长耗时步骤（>30s）
- 导出：Markdown 报告
- TUI 集成：`/replay --analyze` 命令

---

### 任务 7：消息 UUID + Rewind 回退

**目标**：每条消息带 UUID，支持回退到指定检查点

**文件**：
- 新增：`crates/codegen/ccode-chat/src/rewind.rs`
- 修改：`crates/codegen/ccode-chat` 的 ConversationItem 结构

**实现要点**：
- ConversationItem 新增 `uuid: Uuid` 字段（uuid v7，时间排序）
- `RewindManager`：回退管理器
- `rewind_to(conversation, target_uuid) -> RewindResult`：
  1. 找到目标消息
  2. 截断后续消息（标记为 rewound，不删除）
  3. 撤销后续消息中的文件修改
  4. 保留 compactBoundary 压缩摘要
  5. 返回回退后的对话 + 丢弃内容摘要
- `rewind_undo(conversation) -> Conversation`：恢复最近一次回退
- 安全保护：回退前显示丢弃内容摘要，需用户确认

---

### 任务 8：文件修改撤销

**目标**：记录每次文件编辑的旧内容，回退时按逆序撤销

**文件**：
- 新增：`crates/codegen/ccode-chat/src/file_undo.rs`

**实现要点**：
- `FileUndoLog`：文件修改撤销记录
- 每次文件编辑时记录：
  - file_path, old_content_hash, old_content（或 git diff）
  - edit_type：create / modify / delete
- 撤销逻辑：
  - modify → 恢复旧内容
  - create → 删除文件
  - delete → 恢复文件内容
- 撤销顺序：按逆序（最后编辑的先撤销）
- 不可撤销标注：Bash 命令的副作用标注为「不可撤销，需手动检查」
- 与 Git Checkpoint 集成：优先用 git checkout 撤销（更可靠）

---

### 任务 9：TUI /rewind 和 /replay 命令

**目标**：在 TUI 中新增 /rewind 和 /replay 命令

**文件**：
- 新增：`crates/codegen/ccode-pager/src/rewind_cmd.rs`
- 新增：`crates/codegen/ccode-pager/src/replay_cmd.rs`

**实现要点**：
- `/rewind`：回退到上一条用户消息
- `/rewind <uuid>`：回退到指定消息
- `/rewind <n>`：回退 n 步
- `/rewind-undo`：恢复最近一次回退
- `/replay <path>`：加载 rollout bundle 进入重放模式
- `/replay --analyze`：分析当前 rollout
- 重放模式下：next/prev/goto/play/quit 控制键

---

## 验证方案

### 阶段 A 验证（网络代理）
1. 启动网络代理，Agent 请求经过代理
2. 验证白名单模式：只允许 LLM API 域名，其他拒绝
3. 验证审计日志正确记录请求
4. 验证 MITM 模式（需信任 CA 证书）

### 阶段 B 验证（Rollout）
1. 运行 Agent 会话，验证 rollout.jsonl 正确录制
2. 打包 bundle，验证包含完整数据
3. 重放 bundle，验证逐步回放正确
4. 分析 rollout，验证统计报告准确

### 阶段 C 验证（Rewind）
1. 运行多轮对话，执行 /rewind 回退
2. 验证回退后对话状态正确
3. 验证文件修改被正确撤销
4. 验证 /rewind-undo 恢复
5. 验证 Bash 副作用标注为不可撤销
