# 对标 Claude Code 三大差距补齐 实现计划

**目标：** 补齐 MCP Server(2→8)、流式渲染(4→7)、Hook 接线(5→8)，总分 76→88

**架构：** MCP Server 在 ccore 新增 mcp_server 模块桥接 rmcp Server 端；流式渲染在 ccode-pager 新增 StreamingRenderer 做增量刷新；Hook 接线将 ccode-hooks dispatcher 注入 ccore ToolNode

**技术栈：** rmcp SDK（MCP 协议）、ratatui（终端渲染）、ccode-hooks（已有）

---

## 文件清单

| 文件 | 操作 | 职责 |
|------|------|------|
| ccore/src/mcp_server/mod.rs | 新增 | MCP Server 模块入口 |
| ccore/src/mcp_server/transport.rs | 新增 | stdio/SSE 双传输层 |
| ccore/src/mcp_server/tool_registry.rs | 新增 | 工具注册到 MCP Server |
| ccore/src/mcp_server/handler.rs | 新增 | JSON-RPC 请求处理 |
| ccore/src/kernel/mod.rs | 修改 | Kernel 集成 MCP Server 启动 |
| ccore/src/node/tool.rs | 修改 | ToolNode 注入 HookDispatcher |
| ccore/src/tools/bridge.rs | 修改 | 桥接内置工具到 MCP Server |
| ccore/src/lib.rs | 修改 | 导出 mcp_server 模块 |
| ccode-pager/src/app/agent_view/streaming.rs | 新增 | StreamingRenderer 增量渲染 |
| ccode-pager/src/app/agent_view/mod.rs | 修改 | AgentView 集成 streaming |
| ccode-pager/src/app/agent_view/render.rs | 修改 | 支持 token append 触发局部刷新 |

---

## 任务拆分

### 任务 1：MCP Server — 模块骨架 + 传输层

**目标：** 在 ccore 中新增 mcp_server 模块，实现 stdio 和 SSE 双传输

**文件**：
- 新增：`crates/codegen/ccore/src/mcp_server/mod.rs`
- 新增：`crates/codegen/ccore/src/mcp_server/transport.rs`
- 修改：`crates/codegen/ccore/src/lib.rs`

**实现要点**：
- mod.rs 声明子模块，定义 McpServerConfig（transport 类型、工具列表）
- transport.rs 实现 StdioTransport（stdin/stdout JSON-RPC）和 SseTransport（HTTP 长连接）
- 两个 transport 都实现 `Transport` trait（来自 rmcp），负责 JSON-RPC 消息的读写
- lib.rs 新增 `pub mod mcp_server;`

**核心逻辑示意**：
```rust
pub enum McpTransportKind { Stdio, Sse { port: u16 } }
pub struct McpServerConfig { pub transport: McpTransportKind, pub name: String }
```

---

### 任务 2：MCP Server — 工具注册 + JSON-RPC Handler

**目标：** 实现工具注册到 MCP Server 和 JSON-RPC 请求处理

**文件**：
- 新增：`crates/codegen/ccore/src/mcp_server/tool_registry.rs`
- 新增：`crates/codegen/ccore/src/mcp_server/handler.rs`
- 修改：`crates/codegen/ccore/src/tools/bridge.rs`

**实现要点**：
- tool_registry.rs 维护 `HashMap<String, McpToolDef>`，McpToolDef 包含 name/description/inputSchema/handler
- 从 bridge.rs 的内置工具（read/write/edit/bash/glob/grep）注册到 registry
- handler.rs 实现 JSON-RPC 2.0 处理：initialize（返回 capabilities）、tools/list（返回工具列表）、tools/call（调用工具并返回结果）
- handler 使用 tokio 异步处理，工具调用通过 mpsc channel 发送到 ToolNode

**核心逻辑示意**：
```rust
// tools/call 处理
async fn handle_tools_call(params: CallToolParams) -> JsonRpcResult {
    let tool = registry.get(&params.name)?;
    let result = tool.handler.call(params.arguments).await?;
    Ok(CallToolResult { content: vec![Content::text(result)] })
}
```

---

### 任务 3：MCP Server — Kernel 集成 + 启动入口

**目标：** Kernel 启动时可选启动 MCP Server，与消息总线协同

**文件**：
- 修改：`crates/codegen/ccore/src/kernel/mod.rs`

**实现要点**：
- Kernel 新增 `mcp_server: Option<McpServerHandle>` 字段
- KernelConfig 新增 `mcp_server_enabled: bool` 和 `mcp_transport: McpTransportKind`
- 启动时如果 mcp_server_enabled，spawn MCP Server 的 tokio task
- MCP Server 的工具调用通过消息总线发送到 ToolNode（agent/{id}/tool_call topic）
- 工具结果通过 bus 回传到 MCP Server handler

**核心逻辑示意**：
```rust
if self.config.mcp_server_enabled {
    let server = McpServer::new(config, tool_registry, bus_sender);
    tokio::spawn(async move { server.run().await });
}
```

---

### 任务 4：流式 Token 渲染 — StreamingRenderer

**目标：** 在 ccode-pager 中实现 token-by-token 增量渲染

**文件**：
- 新增：`crates/codegen/ccode-pager/src/app/agent_view/streaming.rs`
- 修改：`crates/codegen/ccode-pager/src/app/agent_view/mod.rs`
- 修改：`crates/codegen/ccode-pager/src/app/agent_view/render.rs`

**实现要点**：
- StreamingRenderer 维护增量缓冲区 `pending_tokens: String` 和 `is_streaming: bool`
- `append_token(token: &str)` 方法：追加 token 到缓冲区，标记需要刷新
- `flush()` 方法：计算 ratatui 的局部区域，只刷新变更部分（避免全屏重绘闪烁）
- `finish()` 方法：流式结束，切换到完整渲染模式
- 工具调用块用独立组件 ToolCallBlock 渲染，支持 spinner 动画
- mod.rs 和 render.rs 集成：AgentView 持有 StreamingRenderer，render 时优先使用流式模式

**核心逻辑示意**：
```rust
pub struct StreamingRenderer {
    pending_tokens: String,
    tool_blocks: Vec<ToolCallBlock>,
    is_streaming: bool,
    dirty: bool,
}
impl StreamingRenderer {
    pub fn append_token(&mut self, token: &str) { self.pending_tokens.push_str(token); self.dirty = true; }
    pub fn is_dirty(&self) -> bool { self.dirty }
}
```

---

### 任务 5：流式渲染 — 接入 sampler_turn token 流

**目标：** 将 LLM 的 SSE token 流接入 StreamingRenderer

**文件**：
- 修改：`crates/codegen/ccode-shell/src/session/acp_session_impl/sampler_turn.rs`

**实现要点**：
- 在处理 SSE chunk 时，除了当前的 streaming_capture 逻辑，额外通知 StreamingRenderer
- 通过 mpsc channel 或 watch channel 传递 token 到渲染层
- token 到达 → StreamingRenderer.append_token() → 标记 dirty → 下次 ratatui event loop 触发局部刷新
- 工具调用开始/结束通过专门的 channel 事件通知

**核心逻辑示意**：
```rust
// SSE chunk 处理中
if let Some(text) = chunk.delta_text() {
    streaming_tx.send(StreamingEvent::Token(text)).ok();
}
if chunk.is_tool_call() {
    streaming_tx.send(StreamingEvent::ToolCallStart(tool_name)).ok();
}
```

---

### 任务 6：Hook 系统接线 — ToolNode 集成

**目标：** 将 ccode-hooks dispatcher 桥接到 ccore 的 ToolNode，工具执行前后触发 hooks

**文件**：
- 修改：`crates/codegen/ccore/src/node/tool.rs`

**实现要点**：
- ToolNode 新增 `hook_dispatcher: Option<Arc<HookDispatcher>>` 字段
- 工具执行前调用 `dispatch_pre_tool_use(registry, envelope)` → 检查 decision：
  - Allow → 继续执行
  - Deny → 返回权限拒绝错误
  - Rewrite → 使用 updatedInput 替换工具参数
- 工具执行后调用 `dispatch_post_tool_use(registry, envelope)` → 记录结果
- HookDispatcher 通过 FFI 或 trait object 从 ccode-shell 传入，ccore 不直接依赖 ccode-hooks

**核心逻辑示意**：
```rust
// 工具执行前
if let Some(ref dispatcher) = self.hook_dispatcher {
    let result = dispatcher.dispatch_pre_tool_use(&registry, &envelope).await;
    match result.decision {
        HookDecision::Deny => return Err(PermissionDenied),
        HookDecision::Allow => {},
    }
}
```

---

### 任务 7：Hook 接线 — Shell → ccore 桥接

**目标：** 从 ccode-shell 传递 HookRegistry 到 ccore 的 ToolNode

**文件**：
- 修改：`crates/codegen/ccode-shell/src/agent/init.rs`

**实现要点**：
- ccode-shell 启动 agent 时，创建 HookRegistry（从 config 加载 hooks）
- 将 HookRegistry 包装为 trait object（ccore 定义的 HookDispatcher trait）
- 通过 NodeContext 或 Kernel 配置传递给 ToolNode
- 确保失败安全：HookRegistry 创建失败时 ToolNode 仍可正常工作（无 hook 模式）

**核心逻辑示意**：
```rust
let hook_registry = HookRegistry::from_config(&config)?;
let dispatcher = Arc::new(HookDispatcherAdapter::new(hook_registry));
tool_node.set_hook_dispatcher(dispatcher);
```
