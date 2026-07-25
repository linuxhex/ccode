# ccode 改造实现计划

**目标：** 将 grok-build 改造为名为 ccode 的终端 AI 编程代理，引入 ZeroMQ 消息总线、冷热分层记忆、多模型后端，实现进程级并行和多 Agent 编排

**架构：** 微内核 + ZeroMQ PUB/SUB + REQ/REP 消息总线，所有功能模块作为独立 Node 进程，子 Agent 通过消息总线与主 Agent 通信，核心逻辑编译为 libccore 动态库保护源码

**技术栈：** Rust + zeromq (纯 Rust ZMTP) + MessagePack + hora (HNSW) + qdrant + tokio + ratatui

---

## 交付阶段

### 阶段 A：最小闭环（MVP）—— 能对话
- Kernel 消息总线
- Agent Node（单 agent，复用 grok 的 prompt/工具逻辑）
- Sampler Node（单 Provider：xAI Grok）
- TUI Node（复用 grok 的 markdown 渲染）
- CLI 入口 + 动态库骨架

### 阶段 B：工具 + 状态 —— 能干活
- Tool Node（复用 grok 全部工具）
- State Node（对话持久化 + 基础滑动窗口）

### 阶段 C：记忆系统 —— 能记住
- L1 短期记忆（hora 向量库）
- 冷热评分 + 滑动窗口上下文更新
- recall 工具

### 阶段 D：多 Agent 编排 —— 能并行
- 子 Agent spawn/通信
- Agent 类型定义（explore/plan/general-purpose）
- Doom Loop 检测

### 阶段 E：多模型后端 —— 能混排
- OpenAI 兼容适配器框架
- Claude 适配器
- DeepSeek / GLM / Kimi / Qianwen / Qoder 适配器
- 多模型路由 + fallback

### 阶段 F：高级能力 —— 能超越
- Plan-Execute 循环
- Git Checkpoint + 回滚
- 自动验证循环
- Skill 系统
- L2 长期记忆 + Dream 整理
- Plugin Node 外部扩展

---

## 文件清单

| 文件 | 操作 | 职责 |
|------|------|------|
| crates/codegen/ccore/Cargo.toml | 新增 | libccore 动态库 crate 定义 |
| crates/codegen/ccore/src/lib.rs | 新增 | 动态库导出入口 |
| crates/codegen/ccore/src/kernel/mod.rs | 新增 | Kernel 主逻辑 |
| crates/codegen/ccore/src/kernel/broker.rs | 新增 | ZeroMQ ROUTER/DEALER broker |
| crates/codegen/ccore/src/kernel/registry.rs | 新增 | Node 注册/发现 |
| crates/codegen/ccore/src/kernel/health.rs | 新增 | 心跳/健康检查 |
| crates/codegen/ccore/src/node/mod.rs | 新增 | Node trait 定义 |
| crates/codegen/ccore/src/node/agent.rs | 新增 | Agent Node |
| crates/codegen/ccore/src/node/sampler.rs | 新增 | Sampler Node |
| crates/codegen/ccore/src/node/tool.rs | 新增 | Tool Node |
| crates/codegen/ccore/src/node/state.rs | 新增 | State Node |
| crates/codegen/ccore/src/node/tui.rs | 新增 | TUI Node |
| crates/codegen/ccore/src/message/mod.rs | 新增 | 消息帧定义 |
| crates/codegen/ccore/src/message/topic.rs | 新增 | Topic 命名与路由 |
| crates/codegen/ccore/src/message/frame.rs | 新增 | 3 帧编解码（MessagePack） |
| crates/codegen/ccore/src/memory/working.rs | 新增 | L0 工作记忆 |
| crates/codegen/ccore/src/memory/short_term.rs | 新增 | L1 短期记忆（hora） |
| crates/codegen/ccore/src/memory/long_term.rs | 新增 | L2 长期记忆（qdrant） |
| crates/codegen/ccore/src/memory/heat.rs | 新增 | 冷热评分算法 |
| crates/codegen/ccore/src/memory/window.rs | 新增 | 滑动窗口更新 |
| crates/codegen/ccore/src/memory/dream.rs | 新增 | Dream 整理 |
| crates/codegen/ccore/src/memory/recall.rs | 新增 | recall 工具 |
| crates/codegen/ccore/src/sampler/provider.rs | 新增 | Provider trait 定义 |
| crates/codegen/ccore/src/sampler/openai_compat.rs | 新增 | OpenAI 兼容适配器 |
| crates/codegen/ccore/src/sampler/claude_adapter.rs | 新增 | Claude 适配器 |
| crates/codegen/ccore/src/sampler/glm_adapter.rs | 新增 | GLM 适配器 |
| crates/codegen/ccore/src/sampler/router.rs | 新增 | 模型路由 + fallback |
| crates/codegen/ccore/src/sampler/pool.rs | 新增 | 连接池 + 令牌桶限流 |
| crates/codegen/ccore/src/agent/prompt.rs | 新增 | Agent prompt 模板 |
| crates/codegen/ccore/src/agent/subagent.rs | 新增 | 子 Agent 定义 |
| crates/codegen/ccore/src/agent/orchestrator.rs | 新增 | Agent 编排器 |
| crates/codegen/ccore/src/agent/doom_loop.rs | 新增 | Doom Loop 检测 |
| crates/codegen/ccore/src/agent/plan_execute.rs | 新增 | Plan-Execute 循环 |
| crates/codegen/ccore/src/agent/skills.rs | 新增 | Skill 系统 |
| crates/codegen/ccore/src/tools/bridge.rs | 新增 | 工具桥接（调用 grok 工具实现） |
| crates/codegen/ccore/src/tools/checkpoint.rs | 新增 | Git Checkpoint |
| crates/codegen/ccore/src/tools/verify.rs | 新增 | 自动验证循环 |
| crates/codegen/ccore/src/ffi/mod.rs | 新增 | C FFI 导出接口 |
| crates/codegen/ccode-cli/Cargo.toml | 新增 | CLI crate 定义 |
| crates/codegen/ccode-cli/src/main.rs | 新增 | CLI 入口（参数解析 + spawn kernel） |
| crates/codegen/ccore/src/config/mod.rs | 新增 | ccode 配置（Provider、模型、权限等） |
| crates/codegen/ccore/src/config/provider.rs | 新增 | Provider 配置结构 |
| crates/codegen/ccore/src/config/memory.rs | 新增 | 记忆系统配置 |

---

## 任务拆分

### 任务 1：创建 ccore crate 骨架

**目标**：建立 libccore 动态库 crate，定义核心 trait 和类型

**文件**：
- 新增：`crates/codegen/ccore/Cargo.toml`
- 新增：`crates/codegen/ccore/src/lib.rs`

**实现要点**：
- Cargo.toml 配置 crate-type = ["cdylib", "rlib"]
- 依赖：zeromq (纯 Rust ZMTP), rmp-serde, tokio, serde, anyhow
- lib.rs 导出核心模块：kernel, node, message, memory, sampler, agent, tools, ffi, config

**核心逻辑示意**：
```rust
// ccode 核心库入口，编译为 cdylib 供 CLI 调用
pub mod kernel;
pub mod node;
pub mod message;
pub mod ffi;
```

---

### 任务 2：定义消息协议

**目标**：实现 3 帧消息格式、Topic 路由、MessagePack 编解码

**文件**：
- 新增：`crates/codegen/ccore/src/message/mod.rs`
- 新增：`crates/codegen/ccore/src/message/topic.rs`
- 新增：`crates/codegen/ccore/src/message/frame.rs`

**实现要点**：
- Topic 类型：sys/*, agent/*, sampler/*, state/*, tool/*
- 消息帧：Frame{topic, header, payload}，header 含 msg_id/timestamp/src_node/reply_to
- MessagePack 序列化/反序列化
- Topic 匹配支持通配符（agent/*/output 匹配所有 agent 输出）

**核心逻辑示意**：
```rust
pub struct Message {
    pub topic: String,
    pub header: MessageHeader,
    pub payload: Vec<u8>,
}
pub fn topic_matches(pattern: &str, topic: &str) -> bool { /* 通配符匹配 */ }
```

---

### 任务 3：定义 Node trait

**目标**：统一所有 Node 的生命周期接口

**文件**：
- 新增：`crates/codegen/ccore/src/node/mod.rs`

**实现要点**：
- Node trait：start/stop/handle_message/subscriptions/node_type/node_id
- NodeId：UUID 格式
- NodeType 枚举：Kernel/Agent/Tool/Sampler/State/TUI/Plugin
- NodeConfig：启动参数、模型配置、权限模式

**核心逻辑示意**：
```rust
pub trait Node: Send + Sync {
    fn node_type(&self) -> NodeType;
    async fn start(&mut self, ctx: NodeContext) -> Result<()>;
    async fn handle_message(&mut self, msg: Message) -> Result<()>;
    fn subscriptions(&self) -> Vec<String>;
}
```

---

### 任务 4：实现 Kernel Broker

**目标**：ZeroMQ ROUTER/DEALER + PUB/SUB broker，Node 注册/发现/健康检查

**文件**：
- 新增：`crates/codegen/ccore/src/kernel/mod.rs`
- 新增：`crates/codegen/ccore/src/kernel/broker.rs`
- 新增：`crates/codegen/ccore/src/kernel/registry.rs`
- 新增：`crates/codegen/ccore/src/kernel/health.rs`

**实现要点**：
- ROUTER socket 接收 Node REQ 消息，DEALER 分发
- PUB socket 广播系统事件（spawn/shutdown），SUB 接收 Node 心跳
- Registry 维护 NodeId → (node_type, subscriptions, last_heartbeat) 映射
- 健康检查：超时未心跳的 Node 标记为 dead，通知依赖方
- Node spawn：收到 agent/{id}/spawn topic → fork 子进程 → 注册到 Registry

**核心逻辑示意**：
```rust
pub struct Kernel {
    broker: Broker,       // ZMQ ROUTER/DEALER
    publisher: Publisher,  // ZMQ PUB
    registry: Registry,    // Node 注册表
}
impl Kernel {
    pub async fn run(&mut self) -> Result<()> { /* 事件循环 */ }
}
```

---

### 任务 5：实现 Sampler Node（单 Provider）

**目标**：Sampler Node 接收采样请求，调用 xAI Grok API，流式返回

**文件**：
- 新增：`crates/codegen/ccore/src/node/sampler.rs`
- 新增：`crates/codegen/ccore/src/sampler/provider.rs`
- 新增：`crates/codegen/ccore/src/sampler/openai_compat.rs`

**实现要点**：
- Provider trait：send_streaming / send / model_list
- OpenAI Compat 实现：调用 /v1/chat/completions，SSE 流式解析
- 从 grok 的 xai-grok-sampler 迁移采样逻辑
- Sampler Node 订阅 sampler/request topic，发布 sampler/{req_id}/stream
- 流式返回：逐 token 发送 text channel，完成时发 usage

**核心逻辑示意**：
```rust
pub trait Provider: Send + Sync {
    async fn stream(&self, req: SampleRequest) -> Result<Pin<Box<dyn Stream<Item = StreamChunk>>>>;
}
```

---

### 任务 6：实现 Agent Node（基础版）

**目标**：单 agent 循环：接收 input → 采样 → 解析工具调用 → 发送 tool_call → 收 tool_result → 继续采样

**文件**：
- 新增：`crates/codegen/ccore/src/node/agent.rs`
- 新增：`crates/codegen/ccore/src/agent/prompt.rs`

**实现要点**：
- Agent 循环：input → sampler/request → 解析 response → tool_call 或 output
- 从 grok 的 xai-grok-agent 迁移 agent 逻辑
- 复用 grok 的 prompt.md 模板
- 订阅：agent/{self.id}/input, agent/{self.id}/tool_result, sampler/{req_id}/stream
- 发布：sampler/request, agent/{self.id}/tool_call, agent/{self.id}/output

**核心逻辑示意**：
```rust
pub struct AgentNode {
    id: NodeId,
    model: String,
    context: Vec<Message>,  // L0 工作记忆
}
impl AgentNode {
    pub async fn run_loop(&mut self, bus: &MessageBus) -> Result<()> { /* agent 循环 */ }
}
```

---

### 任务 7：实现 TUI Node

**目标**：终端渲染 + 用户输入，复用 grok 的 ratatui + markdown 渲染

**文件**：
- 新增：`crates/codegen/ccore/src/node/tui.rs`

**实现要点**：
- 从 grok 的 xai-grok-pager 迁移 TUI 渲染逻辑
- 订阅 agent/{primary_id}/output 渲染 agent 回复
- 用户输入发布到 agent/{primary_id}/input
- 渲染工具调用状态（等待/执行中/完成）

---

### 任务 8：实现 CLI 入口 + FFI

**目标**：ccode-cli 解析参数，通过 FFI 调用 libccore 启动 Kernel

**文件**：
- 新增：`crates/codegen/ccode-cli/Cargo.toml`
- 新增：`crates/codegen/ccode-cli/src/main.rs`
- 新增：`crates/codegen/ccore/src/ffi/mod.rs`

**实现要点**：
- CLI 参数：--model, --agent, --yolo, --max-turns, --config, -p (headless)
- FFI 导出：ccore_start(config_json) → Kernel 启动
- CLI 调用 FFI 启动 Kernel，Kernel spawn 初始 Node 集合

**核心逻辑示意**：
```rust
// FFI 导出
#[no_mangle]
pub extern "C" fn ccore_start(config_json: *const c_char) -> i32 { /* 启动 kernel */ }

// CLI 入口
fn main() {
    let config = parse_args();
    unsafe { ccore_start(config.to_json().as_ptr()); }
}
```

---

### 任务 9：实现配置系统

**目标**：~/.ccode/config.toml 配置文件，支持多 Provider、模型、权限配置

**文件**：
- 新增：`crates/codegen/ccore/src/config/mod.rs`
- 新增：`crates/codegen/ccore/src/config/provider.rs`
- 新增：`crates/codegen/ccore/src/config/memory.rs`

**实现要点**：
- 从 grok 的 xai-grok-config 迁移配置逻辑
- Provider 配置：api_key, base_url, adapter, models
- 记忆配置：L1 容量、L2 路径、热度权重、滑动窗口策略
- 权限配置：allow/deny 规则、yolo 模式

---

### 任务 10：集成测试——MVP 闭环

**目标**：验证 Kernel + Agent + Sampler + TUI 最小闭环可运行

**文件**：
- 修改：`crates/codegen/ccore/src/kernel/mod.rs`
- 修改：`crates/codegen/ccore/src/node/agent.rs`

**实现要点**：
- 启动 Kernel → spawn Sampler/Agent/TUI → 用户输入 → Agent 回复 → 渲染
- 验证消息总线通信正确
- 验证流式输出正常
- 修复集成问题

---

### 任务 11：实现 Tool Node

**目标**：Tool Node 接收工具调用请求，执行并返回结果

**文件**：
- 新增：`crates/codegen/ccore/src/node/tool.rs`
- 新增：`crates/codegen/ccore/src/tools/bridge.rs`

**实现要点**：
- 从 grok 的 xai-grok-tools 迁移全部工具实现
- Tool Node 订阅 agent/{any}/tool_call，发布 agent/{src}/tool_result
- 工具桥接：将 grok 的 Tool trait 实现适配为 ccode 的工具调用协议
- 支持并行工具调用

---

### 任务 12：实现 State Node + 基础滑动窗口

**目标**：对话持久化 + 基础滑动窗口上下文管理

**文件**：
- 新增：`crates/codegen/ccore/src/node/state.rs`
- 新增：`crates/codegen/ccore/src/memory/working.rs`
- 新增：`crates/codegen/ccore/src/memory/window.rs`

**实现要点**：
- State Node 维护对话历史，持久化到磁盘
- 滑动窗口：按 token 预算保留最近 N 轮，超出部分标记为冷
- 此阶段不做向量检索，仅做简单的 FIFO 滑动窗口

---

### 任务 13：实现 L1 短期记忆 + 冷热评分

**目标**：hora 向量库存储会话完整历史，冷热评分驱动滑动窗口

**文件**：
- 新增：`crates/codegen/ccore/src/memory/short_term.rs`
- 新增：`crates/codegen/ccore/src/memory/heat.rs`
- 新增：`crates/codegen/ccore/src/memory/recall.rs`

**实现要点**：
- 每条消息 → embedding (ONNX/远程) + 存入 hora HNSW 索引
- 冷热评分：recency + relevance + activity + tool_weight
- 滑动窗口更新：每轮重算热度，冷消息替换为占位符
- recall 工具：从 L1 检索相关历史消息，标记为热，纳入 L0

---

### 任务 14：实现子 Agent 编排

**目标**：主 Agent 可 spawn 子 Agent，通过消息总线通信

**文件**：
- 新增：`crates/codegen/ccore/src/agent/subagent.rs`
- 新增：`crates/codegen/ccore/src/agent/orchestrator.rs`
- 新增：`crates/codegen/ccore/src/agent/doom_loop.rs`

**实现要点**：
- 主 Agent 发布 agent/{sub_id}/spawn → Kernel fork 新 Agent 进程
- 子 Agent 类型：explore（只读搜索）、plan（架构规划）、general-purpose（通用）
- 子 Agent 输出通过 agent/{sub_id}/output 返回给主 Agent
- Doom Loop 检测：检测 agent 循环（重复相同工具调用），自动终止
- 子 Agent 可用不同模型

---

### 任务 15：实现多模型后端

**目标**：支持 Claude/GPT/DeepSeek/GLM/Kimi/Qianwen/Qoder 等多 Provider

**文件**：
- 新增：`crates/codegen/ccore/src/sampler/claude_adapter.rs`
- 新增：`crates/codegen/ccore/src/sampler/glm_adapter.rs`
- 新增：`crates/codegen/ccore/src/sampler/router.rs`
- 新增：`crates/codegen/ccore/src/sampler/pool.rs`

**实现要点**：
- Claude 适配器：system 参数独立传递、tool_use 格式转换、extended thinking 支持
- GLM/Kimi/Qianwen/Qoder 适配器：请求/响应格式转换
- 路由器：按 agent 类型 + 模型配置路由到对应 Provider
- 连接池 + 令牌桶限流 + 指数退避重试
- 自动 fallback：Provider A 限流 → 切到 Provider B 同能力模型

---

### 任务 16：实现 Plan-Execute 循环

**目标**：Plan Mode → 用户审批 → 执行 → 验证

**文件**：
- 新增：`crates/codegen/ccore/src/agent/plan_execute.rs`

**实现要点**：
- Plan 模式：Agent 只生成计划，不执行工具，等待用户审批
- 执行模式：按计划顺序执行，每步验证结果
- 用户可 approve/reject/edit 计划

---

### 任务 17：实现 Git Checkpoint + 自动验证

**目标**：每次编辑自动 checkpoint，编辑后自动编译/测试验证

**文件**：
- 新增：`crates/codegen/ccore/src/tools/checkpoint.rs`
- 新增：`crates/codegen/ccore/src/tools/verify.rs`

**实现要点**：
- Checkpoint：每次文件编辑前 git stash / commit，支持回滚
- 自动验证：编辑 → cargo check / npm run build → 测试 → 修 → 再验证
- 验证失败自动重试（最多 3 次）

---

### 任务 18：实现 Skill 系统 + L2 长期记忆

**目标**：可复用 prompt 模板 + 跨会话知识持久化

**文件**：
- 新增：`crates/codegen/ccore/src/agent/skills.rs`
- 新增：`crates/codegen/ccore/src/memory/long_term.rs`
- 新增：`crates/codegen/ccore/src/memory/dream.rs`

**实现要点**：
- Skill：可复用的 prompt 模板 + 工具配置，用户可自定义
- L2 长期记忆：qdrant 持久化向量库，存储跨会话知识
- Dream 整理：空闲时自动去重、合并、建立知识关联

---

### 任务 19：安装脚本

**目标**：curl | sh 一键安装 ccode

**文件**：
- 新增：`scripts/install.sh`
- 新增：`scripts/install.ps1`

**实现要点**：
- 检测平台和架构
- 下载 libccore + ccode-cli 二进制
- 安装到 ~/.ccode/bin/
- 添加到 PATH
