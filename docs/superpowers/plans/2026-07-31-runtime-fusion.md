# Runtime Fusion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将生产运行时能力并入 ccore，删除 SessionActor 双路径，最终只保留一套由 ROS 风格消息总线拉起的 Node 协作运行时。

**Architecture:** Kernel 拉起 Thinker / Sampler / Tool / State / TUI / Acp；Node 只经 topic 协作；ThinkerNode 做决策 loop，不内嵌工具/持久化；生产能力迁入对应 Node 后删除 shell 主循环与 MessageBusBridge。

**Tech Stack:** Rust、tokio、zeromq (ZMTP)、ccore Node trait、现有 `ccode-tools` / `ccode-sampler` / `ccode-chat` / `ccode-compaction` / ACP 类型（迁入或被 Node 调用）。

**Spec:** `docs/superpowers/specs/2026-07-31-runtime-fusion-design.md`

**硬约束（贯彻全程）：**

1. 接真实模块、真实 topic、真实总线 API——不做假 ZMQ / 假 LLM。
2. **阶段 1–3：只写逻辑，不执行 `cargo build` / `cargo test` / 跑二进制。**
3. **阶段 4：才编译运行验证。**
4. 跨 Node 禁止直接调内部 API；共享库可复用。
5. 不保留 local fallback。

**逻辑自检（阶段 1–3 每步替代“跑测试”）：**

- 对照本 Task 的订阅/发布 topic 表，确认无遗漏。
- 确认无 `MessageBusBridge` / `run_turn_local_fallback` 新引用。
- 确认跨 Node 调用均为 `transport.publish` / `send_message`，非直接函数跨 Node。

---

## File Structure (锁定分解)

| 路径 | 职责 |
|------|------|
| `crates/codegen/ccore/src/message/topic.rs` | 补齐 permission / cancel / compact / sampler_cancel topic |
| `crates/codegen/ccore/src/message/payloads.rs` **(新建)** | 总线 payload 类型（PermissionRequest/Response、Cancel、CompactRequest 等） |
| `crates/codegen/ccore/src/node/acp.rs` **(新建)** | `AcpNode`：ACP/stdio/IDE ↔ `agent/*/input\|output\|permission\|cancel` |
| `crates/codegen/ccore/src/node/mod.rs` | 注册 `Acp` NodeType；`pub mod acp` |
| `crates/codegen/ccore/src/kernel/launcher.rs` | spawn 6 Node：+AcpNode |
| `crates/codegen/ccore/src/node/sampler.rs` | 吸收生产采样能力（重试/取消/与 `ccode-sampler` 对齐） |
| `crates/codegen/ccore/src/node/tool.rs` | 改为使用生产级工具栈（迁入或依赖 `ccode-tools`） |
| `crates/codegen/ccore/src/tools/` | 用生产工具实现替换薄 builtin（或变为对迁入代码的 thin wrapper） |
| `crates/codegen/ccore/src/node/state.rs` | 吸收 ChatState + JSONL + compaction host |
| `crates/codegen/ccore/src/node/thinker.rs` | 吸收 SessionActor turn/goal/doom-loop；只经总线调 Sampler/Tool/State |
| `crates/codegen/ccode-cli/src/main.rs` 或 `ccode-pager-bin` | **单一入口**启动 Kernel |
| `crates/codegen/ccode-shell/src/session/message_bus_bridge.rs` | **删除** |
| `crates/codegen/ccode-shell/src/session/acp_session_impl/{turn,sampler_turn,tool_calls}.rs` 等 | **删除主循环双路径**（能力已迁走） |

共享库可继续以 crate 形式存在（`ccode-tools`、`ccode-compaction` 等），但**编排入口只在 ccore Kernel**。

---

## Phase 1 — 契约与骨架

### Task 1: 补齐 Topic 契约

**Files:**
- Modify: `crates/codegen/ccore/src/message/topic.rs`
- Create: `crates/codegen/ccore/src/message/payloads.rs`
- Modify: `crates/codegen/ccore/src/message/mod.rs`

- [ ] **Step 1: 在 `topic.rs` 增加生产协作所需 factory**

在 `Topic` impl 的 Agent / Sampler / State 段追加（保持现有风格）：

```rust
/// Agent 权限请求（ToolNode → Acp/TUI）
pub fn agent_permission(agent_id: &str) -> Self {
    Self::new(format!("agent/{agent_id}/permission"))
}

/// Agent 取消（Acp/TUI → Thinker/Sampler/Tool）
pub fn agent_cancel(agent_id: &str) -> Self {
    Self::new(format!("agent/{agent_id}/cancel"))
}

/// 取消进行中的采样
pub fn sampler_cancel(request_id: &str) -> Self {
    Self::new(format!("sampler/{request_id}/cancel"))
}

/// 请求压缩会话上下文
pub fn state_compact() -> Self {
    Self::new("state/compact")
}
```

在 `TopicPattern` 中如有 `all_sampler_streams` 同类 helper，追加：

```rust
pub fn all_agent_permissions() -> Self {
    Self::new("agent/*/permission")
}

pub fn all_agent_cancels() -> Self {
    Self::new("agent/*/cancel")
}
```

- [ ] **Step 2: 新建 `payloads.rs`（真实可序列化类型，供各 Node 共用）**

```rust
//! 总线 payload（MessagePack / JSON 均可，与现有 FrameCodec 一致）
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub agent_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionResponse {
    pub agent_id: String,
    pub tool_call_id: String,
    pub allowed: bool,
    pub remember: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelRequest {
    pub agent_id: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactRequest {
    pub session_id: String,
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactResult {
    pub session_id: String,
    pub ok: bool,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub error: Option<String>,
}
```

- [ ] **Step 3: 在 `message/mod.rs` 导出**

```rust
pub mod payloads;
pub use payloads::{
    CancelRequest, CompactRequest, CompactResult, PermissionRequest, PermissionResponse,
};
```

- [ ] **Step 4: 逻辑自检（不编译）**

确认 design §3 topic 表中的 permission / cancel / compact / sampler cancel 均有 factory。

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/ccore/src/message/topic.rs \
        crates/codegen/ccore/src/message/payloads.rs \
        crates/codegen/ccore/src/message/mod.rs
git commit -m "$(cat <<'EOF'
feat(ccore): add fusion topic contracts and bus payloads

EOF
)"
```

---

### Task 2: `AcpNode` 骨架（真实 ACP 接线逻辑，逻辑层完整）

**Files:**
- Create: `crates/codegen/ccore/src/node/acp.rs`
- Modify: `crates/codegen/ccore/src/node/mod.rs`

- [ ] **Step 1: 扩展 `NodeType`**

在 `node/mod.rs` 的 `NodeType` 增加变体并更新 `as_str`：

```rust
// 在 Thinker 旁增加
Acp,

// as_str:
Self::Acp => "acp",
```

增加：

```rust
pub mod acp;
```

- [ ] **Step 2: 实现 `AcpNode`（订阅/发布契约完整；stdio/ACP 协议逻辑从 shell 迁入的入口标清）**

```rust
//! AcpNode — IDE/stdio ACP client 与消息总线之间的边界
//!
//! 职责：
//! - 把 ACP `session/prompt` 转为 `agent/{id}/input`
//! - 把 `agent/{id}/output` 流转为 ACP session updates
//! - 处理 `agent/{id}/permission` ↔ ACP permission request
//! - 发布 `agent/{id}/cancel`
//!
//! 实现来源：从 `ccode-shell` MvpAgent/ACP gateway 迁入，不在此 Node 内跑 agentic loop。

use async_trait::async_trait;
use crate::message::payloads::{CancelRequest, PermissionResponse};
use crate::message::{Message, Topic};
use crate::node::transport::NodeTransportHandle;
use crate::node::{Node, NodeContext, NodeId, NodeType};

pub struct AcpNode {
    id: NodeId,
    /// 当前绑定的 primary agent id（与 ThinkerNode 一致）
    primary_agent_id: String,
}

impl AcpNode {
    pub fn new(id: NodeId, primary_agent_id: impl Into<String>) -> Self {
        Self {
            id,
            primary_agent_id: primary_agent_id.into(),
        }
    }

    pub fn set_primary_agent(&mut self, agent_id: impl Into<String>) {
        self.primary_agent_id = agent_id.into();
    }

    /// ACP prompt → bus input（真实调用点：后续接 agent-client-protocol 读写）
    async fn publish_user_input(
        &self,
        text: &str,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        let topic = Topic::agent_input(&self.primary_agent_id);
        // 使用与 ThinkerNode/TUINode 相同的 FrameCodec 构造 Message
        let msg = crate::message::frame::FrameCodec::new_message(
            topic,
            self.id.as_str(),
            serde_json::json!({ "text": text }),
        );
        transport.publish(msg).await?;
        Ok(())
    }

    async fn publish_cancel(
        &self,
        reason: Option<String>,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        let payload = CancelRequest {
            agent_id: self.primary_agent_id.clone(),
            reason,
        };
        let msg = crate::message::frame::FrameCodec::new_message(
            Topic::agent_cancel(&self.primary_agent_id),
            self.id.as_str(),
            payload,
        );
        transport.publish(msg).await?;
        Ok(())
    }

    async fn publish_permission_response(
        &self,
        response: PermissionResponse,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        // 回复走同一 permission topic，body 带 allowed（或单独 response topic；此处约定同 topic + type 字段）
        let msg = crate::message::frame::FrameCodec::new_message(
            Topic::agent_permission(&response.agent_id),
            self.id.as_str(),
            response,
        );
        transport.publish(msg).await?;
        Ok(())
    }
}

#[async_trait]
impl Node for AcpNode {
    fn node_type(&self) -> NodeType {
        NodeType::Acp
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!(node_id = %self.id, agent = %self.primary_agent_id, "AcpNode started");
        // TODO(fusion-migrate): 在此启动 ACP stdio/server accept loop（从 ccode-shell gateway 迁入）
        Ok(())
    }

    async fn handle_message(
        &mut self,
        msg: Message,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        let topic = msg.topic.as_str();
        if topic.ends_with("/output") {
            // TODO(fusion-migrate): 转发为 ACP session update
            let _ = transport;
            return Ok(());
        }
        if topic.ends_with("/permission") {
            // 入站若是 ToolNode 的 PermissionRequest → 转 ACP；出站由 publish_permission_response
            return Ok(());
        }
        if topic == Topic::sys_shutdown().as_str() {
            return self.stop().await;
        }
        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec![
            Topic::sys_shutdown().as_str().to_string(),
            Topic::agent_output(&self.primary_agent_id).as_str().to_string(),
            Topic::agent_permission(&self.primary_agent_id).as_str().to_string(),
            Topic::agent_event(&self.primary_agent_id).as_str().to_string(),
        ]
    }

    fn published_topics(&self) -> Vec<String> {
        vec![
            Topic::agent_input(&self.primary_agent_id).as_str().to_string(),
            Topic::agent_cancel(&self.primary_agent_id).as_str().to_string(),
            Topic::agent_permission(&self.primary_agent_id).as_str().to_string(),
        ]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn graceful_stop(
        &mut self,
        _transport: Option<&NodeTransportHandle>,
    ) -> anyhow::Result<()> {
        self.stop().await
    }
}
```

> 注：`TODO(fusion-migrate)` 标记的是「从 shell 搬具体 ACP IO 循环」的挂点；topic 契约与 Node 生命周期本 Task 必须完整。下一 Task 迁入真实 IO 时填满，不得另起假协议。

- [ ] **Step 3: 逻辑自检（不编译）**

对照 design §2：AcpNode 无 agentic loop；只做 bus client。

- [ ] **Step 4: Commit**

```bash
git add crates/codegen/ccore/src/node/acp.rs crates/codegen/ccore/src/node/mod.rs
git commit -m "$(cat <<'EOF'
feat(ccore): add AcpNode skeleton on message bus

EOF
)"
```

---

### Task 3: Launcher 拉起 6 Node（含 Acp）

**Files:**
- Modify: `crates/codegen/ccore/src/kernel/launcher.rs`
- Modify: `crates/codegen/ccore/src/node/mod.rs` 注释（5→6 Node）

- [ ] **Step 1: 更新 `spawn_initial_set` 文档与 imports**

```rust
use crate::node::acp::AcpNode;
```

将注释改为：

```text
仿生架构 + ACP：6 个 Node
- Sampler / State / Tool / Thinker / TUI / Acp
```

- [ ] **Step 2: 在 spawn 中增加 AcpNode，并使 Thinker 与 Acp 共享同一 `primary_agent_id`**

关键逻辑（插入到 TUI spawn 旁；`thinker_id` 已有）：

```rust
// Thinker 的 node id 即 primary agent id（或显式生成 agent_session_id）
let primary_agent_id = thinker_id_clone.as_str().to_string();

// 让 TUI 绑定同一 agent
// 若 TUINode::set_primary_agent 已存在则调用：
// tui.set_primary_agent(primary_agent_id.clone());

let acp_ctx = self.node_context();
let acp_id = NodeId::new();
let acp_id_clone = acp_id.clone();
let acp = AcpNode::new(acp_id.clone(), primary_agent_id.clone());

// tokio::join! 增加第 6 个 spawn：
tokio::spawn(async move {
    if let Err(e) = run_node(acp, acp_ctx).await {
        tracing::error!("Acp Node 异常退出：{}", e);
    }
}),
```

同步更新 `launched_nodes` push 与返回的 `NodeDescriptor` 列表，包含 `NodeType::Acp`。

- [ ] **Step 3: 确保 ThinkerNode 构造使用的 agent id 与 Acp/TUI 的 `primary_agent_id` 一致**

若当前 `ThinkerNode::new(thinker_id, …)` 已用 `thinker_id` 作为 `agent/{id}/…` 前缀，则 Acp/TUI 必须用同一字符串；否则在 Thinker 内增加显式 `agent_id` 字段并在三处对齐。

- [ ] **Step 4: 逻辑自检（不编译）**

Kernel 启动路径只经 `NodeLauncher::spawn_initial_set`；无 SessionActor。

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/ccore/src/kernel/launcher.rs crates/codegen/ccore/src/node/mod.rs
git commit -m "$(cat <<'EOF'
feat(ccore): launch AcpNode in initial node set

EOF
)"
```

---

## Phase 2 — 生产能力迁入 ccore

### Task 4: SamplerNode 对齐生产采样能力

**Files:**
- Modify: `crates/codegen/ccore/src/node/sampler.rs`
- Modify: `crates/codegen/ccore/Cargo.toml`（若改为依赖 `ccode-sampler` / `ccode-sampling-types`）
- Reference: `crates/codegen/ccode-sampler/src/handle.rs`（`submit` / `cancel` / `submit_and_collect`）

- [ ] **Step 1: 确定集成方式（本融合选定）**

**选定：SamplerNode 内部持有与生产等价的采样执行器。** 优先路径：

1. 若 crate 依赖允许：`ccore` 依赖 `ccode-sampler`，Node 内 `SamplerHandle`。
2. 若循环依赖：把 `ccode-sampler` 的执行核心下沉到共享 crate，或把生产重试/取消逻辑**复制迁入** `ccore::sampler`（与现有 ProviderRouter 合并），删除平行实现中较弱的一侧。

在 `sampler.rs` 顶部写清选定路径注释：

```rust
//! Fusion: 生产 SamplerActor 能力并入本 Node。
//! 总线契约不变：sub sampler/request + sampler/*/cancel；pub sampler/{id}/stream
```

- [ ] **Step 2: 订阅 cancel；实现取消传播**

`subscriptions()` 增加：

```rust
"sampler/*/cancel".to_string(),
```

`handle_message` 分支：

```rust
if topic.starts_with("sampler/") && topic.ends_with("/cancel") {
    let request_id = topic.split('/').nth(1).unwrap_or_default();
    // 取消进行中的采样（对齐 SamplerHandle::cancel）
    self.cancel_request(request_id).await?;
    return Ok(());
}
```

实现 `cancel_request`：停止对应 in-flight stream，并向 `sampler/{id}/stream` 发最终 error/cancelled 事件（真实错误类型，与现有 stream 事件枚举一致）。

- [ ] **Step 3: 对齐生产重试 / fallback provider**

确保 `try_stream_with_fallback`（或迁入的 `SamplerActor` 路径）覆盖：超时、可重试 HTTP 错误、provider fallback；失败时 stream 上发 error，**不 panic**。

- [ ] **Step 4: 逻辑自检（不编译）**

对照 design §4.2 Sampler 行；确认无 shell `MessageBusBridge::send_llm_request` 新依赖。

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/ccore/src/node/sampler.rs crates/codegen/ccore/Cargo.toml crates/codegen/ccore/src/sampler/
git commit -m "$(cat <<'EOF'
feat(ccore): fold production sampler capabilities into SamplerNode

EOF
)"
```

---

### Task 5: ToolNode 吸收生产工具栈（权限 / MCP / hooks）

**Files:**
- Modify: `crates/codegen/ccore/src/node/tool.rs`
- Modify: `crates/codegen/ccore/src/tools/`（替换或包裹薄 bridge）
- Modify: `crates/codegen/ccore/Cargo.toml`
- Reference: `crates/codegen/ccode-tools/src/bridge.rs`
- Reference: shell 权限流 `acp_session_impl/tool_calls.rs`

- [ ] **Step 1: 选定集成方式并写注释**

**选定：ToolNode 使用生产级 `ccode-tools::ToolBridge`（或将其源码迁入 `ccore::tools` 后删除薄实现）。**  
薄 `ccore::tools::bridge::ToolBridge::new()` 内置 20+ 工具在迁入完成后删除或变为 deprecated wrapper。

```rust
//! Fusion: ToolNode 以生产 ccode-tools 能力为准。
//! Ask 模式：发布 agent/{id}/permission，等待 PermissionResponse 再执行。
```

- [ ] **Step 2: 改写 Ask 模式——真实 permission 往返（替换当前直接 deny）**

将 `tool.rs` 中 `PermissionMode::Ask` 直接拒绝的逻辑改为：

```rust
PermissionMode::Ask => {
    let req = crate::message::payloads::PermissionRequest {
        agent_id: request.agent_id.clone(),
        tool_call_id: request.tool_call_id.clone(),
        tool_name: request.tool_name.clone(),
        arguments: request.arguments.clone(),
        reason: None,
    };
    let msg = FrameCodec::new_message(
        Topic::agent_permission(&request.agent_id),
        self.id.as_str(),
        &req,
    );
    transport.publish(msg).await?;
    // 等待同 tool_call_id 的 PermissionResponse（oneshot/pending map）
    let allowed = self.wait_permission(&request.tool_call_id).await?;
    if !allowed {
        // 发布 denied tool_result，return Ok(())
    }
}
```

在 `ToolNode` 增加：

```rust
pending_permissions: Mutex<HashMap<String, oneshot::Sender<bool>>>,
```

并在 `subscriptions` 增加 `agent/*/permission`，于 `handle_message` 完成 oneshot。

- [ ] **Step 3: 接入 MCP / hooks**

从生产路径迁入：

- MCP 工具注册 → 启动时或 `tool/register` 广播完整 definitions
- hooks（pre/post tool）→ 执行前后调用，与 shell 行为对齐

具体符号从 `ccode-tools` / `ccode-hooks` 迁入或依赖；**禁止**在 ThinkerNode 内执行工具。

- [ ] **Step 4: 逻辑自检（不编译）**

ToolNode 是唯一执行点；permission 经总线；无 SessionActor `execute_tool_calls` 双路径新增。

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/ccore/src/node/tool.rs crates/codegen/ccore/src/tools/ crates/codegen/ccore/Cargo.toml
git commit -m "$(cat <<'EOF'
feat(ccore): absorb production tool stack into ToolNode

EOF
)"
```

---

### Task 6: StateNode 吸收会话真相源 + compaction

**Files:**
- Modify: `crates/codegen/ccore/src/node/state.rs`
- Possibly create: `crates/codegen/ccore/src/node/state_chat.rs`（ChatState 适配）
- Modify: `crates/codegen/ccore/Cargo.toml`
- Reference: `crates/codegen/ccode-chat/src/actor/mod.rs`、`persistence.rs`
- Reference: `crates/codegen/ccode-shell/src/session/persistence.rs`、`chat_persistence.rs`
- Reference: `crates/common/ccode-compaction/`

- [ ] **Step 1: 定义 StateNode 为唯一会话真相源**

文件头注释：

```rust
//! Fusion: 唯一会话真相源。
//! 吸收 ChatStateActor 语义 + shell JSONL persistence + ccode-compaction。
//! Topics: state/persist, state/query → state/response, state/compact, agent/*/event
```

- [ ] **Step 2: 挂载持久化与 ChatState 语义**

StateNode 字段改为持有：

```rust
// 伪结构——实现时用真实类型名
chat: ChatStateHandle,              // 或内嵌等价状态机
persistence: Box<dyn ChatPersistence>, // Channel/JSONL 实现从 shell 迁入 ccore::persistence
compactor: CompactionHost,          // 包装 ccode-compaction
```

`handle_message`：

- `state/persist` → `persist_message` / flush  
- `state/query` → 构建 snapshot / `build_request` 所需视图，reply `state/response`  
- `state/compact` → 调 `ccode-compaction`，回 `CompactResult`（可 pub 到 `state/response` 或 compact 专用 reply）

- [ ] **Step 3: 订阅 `state/compact`**

```rust
fn subscriptions(&self) -> Vec<String> {
    vec![
        Topic::state_persist().as_str().to_string(),
        Topic::state_query().as_str().to_string(),
        Topic::state_compact().as_str().to_string(),
        "agent/*/event".to_string(),
        Topic::sys_shutdown().as_str().to_string(),
    ]
}
```

- [ ] **Step 4: 逻辑自检（不编译）**

确认 shell JSONL 不再是第二真相源；Thinker 只通过 `state/*` 读写。

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/ccore/src/node/state.rs crates/codegen/ccore/src/node/state_chat.rs \
        crates/codegen/ccore/src/persistence/ crates/codegen/ccore/Cargo.toml
git commit -m "$(cat <<'EOF'
feat(ccore): make StateNode canonical session store with compaction

EOF
)"
```

---

### Task 7: ThinkerNode 吸收 SessionActor 决策 loop

**Files:**
- Modify: `crates/codegen/ccore/src/node/thinker.rs`
- Reference: `crates/codegen/ccode-shell/src/session/acp_session_impl/turn.rs`
- Reference: `crates/codegen/ccore/src/agent/doom_loop.rs`、`loop_state`

- [ ] **Step 1: 明确 Thinker 职责边界（写在模块文档）**

```rust
//! Fusion: 唯一决策 Node（不是上帝进程）。
//! - 拥有：感知(Ear/Eye/Nose/Skin 内置)、agentic loop、doom-loop、max_turns、goal
//! - 不拥有：工具执行、JSONL 持久化实现、LLM HTTP
//! - 协作：sampler/request、agent/{id}/tool_call、state/persist|query|compact、agent/{id}/output
```

- [ ] **Step 2: 迁入 turn loop 控制流（真实逻辑，经总线）**

将 `turn.rs` 中的核心循环改写为 bus 版（结构必须存在于 `thinker.rs` 或 `thinker/turn.rs`）：

```rust
async fn run_agentic_loop(&mut self, transport: &NodeTransportHandle) -> anyhow::Result<()> {
    let mut turns = 0u32;
    let max_turns = self.config.max_turns.unwrap_or(u32::MAX);
    loop {
        if self.cancel_requested {
            self.persist_partial(transport).await?;
            break;
        }
        // 1) state/query 取 ConversationRequest 视图
        // 2) publish sampler/request
        // 3) 收集 sampler/{req}/stream 直到完成
        // 4) 若 tool_calls：逐个 publish agent/{id}/tool_call，等待 tool_result
        // 5) state/persist 中间结果
        // 6) 若 EndTurn 或 doom_loop escape 或 turns>=max_turns：publish output，break
        turns += 1;
        if turns >= max_turns {
            break;
        }
    }
    Ok(())
}
```

从生产迁入并对接：

- doom loop 检测（`ccore::agent::doom_loop`）
- circuit breaker / token budget（原 `ccore_integration`）
- cancel：订阅 `agent/{id}/cancel`，设 `cancel_requested`，并向 `sampler/{req}/cancel` 转发

- [ ] **Step 3: 订阅补齐**

```rust
fn subscriptions(&self) -> Vec<String> {
    vec![
        Topic::agent_input(self.id.as_str()).as_str().to_string(),
        Topic::agent_tool_result(self.id.as_str()).as_str().to_string(),
        Topic::agent_cancel(self.id.as_str()).as_str().to_string(),
        "sampler/*/stream".to_string(),
        Topic::tool_register().as_str().to_string(),
        Topic::sys_shutdown().as_str().to_string(),
    ]
}
```

（若 agent id ≠ node id，用 `self.agent_id`。）

- [ ] **Step 4: 逻辑自检（不编译）**

Thinker 内无 `ToolBridge::call`、无直接 `ChatPersistence`、无 `SamplerActor::submit`——仅 bus。

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/ccore/src/node/thinker.rs crates/codegen/ccore/src/node/thinker/
git commit -m "$(cat <<'EOF'
feat(ccore): move agentic loop decisions into ThinkerNode over the bus

EOF
)"
```

---

### Task 8: 填满 AcpNode 真实 ACP IO + TUI 对齐

**Files:**
- Modify: `crates/codegen/ccore/src/node/acp.rs`
- Modify: `crates/codegen/ccore/src/node/tui.rs`
- Reference: `crates/codegen/ccode-shell/src/agent/mvp_agent/acp_agent.rs`
- Reference: `crates/codegen/ccode-acp/`（若存在）

- [ ] **Step 1: 从 shell 迁入 ACP stdio/server accept 与 prompt 处理到 `AcpNode::start`**

- `session/prompt` → `publish_user_input`
- cancel → `publish_cancel`
- permission request from bus → ACP permission RPC → `publish_permission_response`

- [ ] **Step 2: TUINode 与 AcpNode 共用同一 topic 契约**

确认 TUI 发布 `agent/{id}/input`、订阅 `output`/`event`/`tool_call`（展示用）；不跑 loop。

- [ ] **Step 3: 逻辑自检（不编译）**

应用面无采样/工具决策。

- [ ] **Step 4: Commit**

```bash
git add crates/codegen/ccore/src/node/acp.rs crates/codegen/ccore/src/node/tui.rs
git commit -m "$(cat <<'EOF'
feat(ccore): wire real ACP IO into AcpNode; align TUINode topics

EOF
)"
```

---

## Phase 3 — 删除旧生产路径 + 单一入口

### Task 9: 单一产品入口启动 Kernel

**Files:**
- Modify: `crates/codegen/ccode-pager-bin/src/main.rs` **或** 选定 `ccode-cli` 为唯一 `ccode` 二进制并更新 workspace
- Modify: 相应 `Cargo.toml` `[[bin]]` 名称
- Remove/redirect: 旧 `ccode_pager::app::run` 中 SessionActor/leader 拉起路径

- [ ] **Step 1: 选定唯一二进制名 `ccode`**

**选定：保留用户面对的 `ccode` 命令；实现改为启动 `ccore::kernel::Kernel`（与现 `ccode-cli` 相同核心）。**  
`ccode-cli` 与 `ccode-pager-bin` 合并：一个 bin，内部 `Kernel::new` → `set_ccode_config` → `run`。

示例入口（替换 pager-bin / cli 分裂）：

```rust
fn main() -> anyhow::Result<()> {
    let config = /* 加载 CcodeConfig，兼容原 CLI args 中仍需要的部分 */;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let kernel_config = ccore::kernel::KernelConfig {
            router_addr: "ipc:///tmp/ccode-router".into(),
            pub_addr: "ipc:///tmp/ccode-pub".into(),
            working_dir: std::env::current_dir()?.to_string_lossy().into(),
            ..Default::default()
        };
        let mut kernel = ccore::kernel::Kernel::new(kernel_config);
        kernel.set_ccode_config(config);
        kernel.run().await
    })
}
```

- [ ] **Step 2: 标记删除旧 leader/SessionActor 启动路径**

在 `ccode-shell` agent app 入口加 `#[deprecated]` 或直接删除调用点，确保**无代码路径**再 `spawn_session_actor` 作为主产品路径。

- [ ] **Step 3: 逻辑自检（不编译）**

全仓搜索产品 main：仅 Kernel 启动。

- [ ] **Step 4: Commit**

```bash
git add crates/codegen/ccode-pager-bin crates/codegen/ccode-cli crates/codegen/ccode-shell/src/agent/
git commit -m "$(cat <<'EOF'
feat: unify product entry to Kernel-only startup

EOF
)"
```

---

### Task 10: 删除 SessionActor 双路径与 MessageBusBridge

**Files:**
- Delete: `crates/codegen/ccode-shell/src/session/message_bus_bridge.rs`
- Modify/Delete: `crates/codegen/ccode-shell/src/session/acp_session_impl/sampler_turn.rs` 中 bus/local 双路径
- Modify/Delete: `crates/codegen/ccode-shell/src/session/acp_session_impl/tool_calls.rs` 双路径
- Modify/Delete: `crates/codegen/ccode-shell/src/session/acp_session_impl/turn.rs` 主 loop（能力已在 Thinker）
- Modify: `crates/codegen/ccode-shell/src/session/ccore_integration.rs`（能力已沉入 Node 则删除桥）
- Modify: `crates/codegen/ccode-shell/src/session/mod.rs` 等导出

- [ ] **Step 1: 全仓搜索并删除**

搜索并清除：

```text
MessageBusBridge
run_turn_local_fallback
run_turn_via_message_bus
execute_tool_calls_via_message_bus
use_message_bus
```

- [ ] **Step 2: 删除或掏空 `SessionActor` 作为编排器的代码**

若 ACP 测试仍依赖 shell 类型，仅保留**纯协议/测试夹具**；不得保留第二套 agentic loop。

- [ ] **Step 3: 删除薄 `ccore::tools` 中已被生产栈替换的重复 builtin（若已完全吸收）**

- [ ] **Step 4: 逻辑自检（不编译）**

`rg` 确认无双路径符号；design §3.2 删除清单逐条勾掉。

- [ ] **Step 5: Commit**

```bash
git add -A crates/codegen/ccode-shell/src/session crates/codegen/ccore/src/tools
git commit -m "$(cat <<'EOF'
refactor: remove SessionActor dual-path and MessageBusBridge

EOF
)"
```

---

## Phase 4 — 编译运行验证（此前禁止）

### Task 11: 首次全量编译与修复

**Files:** 编译器指出的所有文件

- [ ] **Step 1: 构建**

```bash
cargo build -p ccore 2>&1 | tee /tmp/ccore-build.log
cargo build -p ccode-cli 2>&1 | tee /tmp/cli-build.log
# 以及合并后的产品 bin crate
```

Expected: 修复至 exit code 0（允许分多轮 commit 修编译）。

- [ ] **Step 2: 单元/集成测试（契约与 Node）**

```bash
cargo test -p ccore --test integration_test 2>&1 | tee /tmp/ccore-test.log
```

补充/迁移测试（若尚未存在则本步创建）：

- topic factory：permission/cancel/compact
- ToolNode permission oneshot
- Thinker cancel 传播

- [ ] **Step 3: Commit 编译修复**

```bash
git add -A
git commit -m "$(cat <<'EOF'
fix: make fused ccore runtime compile and pass unit tests

EOF
)"
```

---

### Task 12: 丝滑回归（功能完整）

**Files:** 测试与缺陷修复涉及文件

- [ ] **Step 1: 手工/自动化场景清单（必须全绿）**

| # | 场景 | 期望 |
|---|------|------|
| 1 | TUI 输入一轮 → 采样 → 输出 | 有完整回复 |
| 2 | 模型请求工具 → ToolNode 执行 → 继续 | tool_result 回到 Thinker |
| 3 | Ask 权限：拒绝 | denied result，不执行副作用 |
| 4 | Ask 权限：允许 | 执行成功 |
| 5 | Cancel 中途 | 采样/工具停，状态 partial persist |
| 6 | Sampler provider 失败 fallback | 不崩 Kernel，可恢复错误或成功 fallback |
| 7 | Compaction 触发 | state/compact 后上下文仍可用 |
| 8 | ACP client prompt（若适用） | 与 TUI 同契约 |
| 9 | MCP 工具（若配置） | 出现在 tool/register 并可调用 |
| 10 | max_turns / doom loop | 有序 EndTurn，无死循环 |

- [ ] **Step 2: 修复失败项直至全绿**

- [ ] **Step 3: 最终 Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
test: verify fused bus runtime feature-complete smooth path

EOF
)"
```

---

## Spec Coverage Checklist

| Spec 要求 | Task |
|-----------|------|
| ROS 风格总线协作中枢 | 1–3, 全程硬约束 |
| Thinker 决策非上帝 | 7 |
| Sampler/Tool/State/TUI/Acp Nodes | 2–8 |
| 生产工具/权限/MCP/hooks → ToolNode | 5 |
| ChatState/JSONL/compaction → StateNode | 6 |
| Session turn/doom-loop → Thinker | 7 |
| ACP 为 bus client | 2, 8 |
| 删除 MessageBusBridge / fallback / 双路径 | 10 |
| 单一 Kernel 入口 | 9 |
| 真实实现 + 阶段 1–3 不编译 | 全文硬约束 + Task 11–12 |
| 功能完整丝滑 | 12 |

---

## Self-Review Notes

- 无 TBD 作为任务终点；`TODO(fusion-migrate)` 仅出现在 Task 2 骨架，并由 Task 8 关闭。
- 类型名：`PermissionRequest` / `PermissionResponse` / `CancelRequest` / `CompactRequest` 在 Task 1 定义，后续 Task 复用。
- `NodeType::Acp` 在 Task 2 引入，Task 3 launcher 使用。
- 阶段 1–3 不跑 cargo；与用户约束一致。
