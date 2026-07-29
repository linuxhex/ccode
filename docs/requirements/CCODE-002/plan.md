# 仿生架构重构 实现计划

**目标：** 将 agent 重构为感官-运动-反射闭环系统，7 个器官 Node + Kernel 反射路由 + 闭环学习

**架构：** 脊髓（Kernel ReflexRouter）负责 L0/L1 反射弧，大脑皮层（ThinkerNode）负责 L2 思考，器官 Node 各司其职

**技术栈：** Rust + tokio + ZeroMQ + serde + notify + crossbeam-queue

---

## 文件清单

| 文件 | 操作 | 职责 |
|------|------|------|
| ccore/src/node/eye.rs | 新增 | EyeNode：读代码/看文件变化/观终端 |
| ccore/src/node/ear.rs | 新增 | EarNode：听用户指令/收通知/收心跳 |
| ccore/src/node/nose.rs | 新增 | NoseNode：嗅编译错误/测试失败/性能退化 |
| ccore/src/node/skin.rs | 新增 | SkinNode：触觉反馈 + 短期记忆 |
| ccore/src/node/mouth.rs | 新增 | MouthNode：写代码/生成回复 |
| ccore/src/node/hand.rs | 新增 | HandNode：编辑文件/搜索定位 |
| ccore/src/node/limb.rs | 新增 | LimbNode：运行命令/构建测试/Git |
| ccore/src/node/thinker.rs | 新增 | ThinkerNode（从 AgentNode 重构） |
| ccore/src/kernel/reflex.rs | 新增 | ReflexRouter：反射规则匹配 + 路由 |
| ccore/src/kernel/autonomic.rs | 新增 | AutonomicNervousSystem（从 self_healing 重构） |
| ccore/src/kernel/experience.rs | 新增 | ExperienceLog + 经历回放提规则 |
| ccore/src/kernel/reflex_rules.toml | 新增 | 预置反射规则模板 |
| ccore/src/node/mod.rs | 修改 | NodeType 新增器官类型 |
| ccore/src/message/topic.rs | 修改 | 新增仿生 Topic 命名 |
| ccore/src/kernel/mod.rs | 修改 | 集成 ReflexRouter + AutonomicNervousSystem |
| ccore/src/lib.rs | 修改 | 新增器官模块导出 |
| ccore/src/node/agent.rs | 修改 | 保留但标记为 deprecated，指向 ThinkerNode |
| ccore/src/kernel/launcher.rs | 修改 | spawn_initial_set 包含器官 Node |

---

## 任务拆分

### 任务 1：基础设施 — 仿生 Topic + NodeType + ReflexRule 数据结构

**目标**：建立仿生架构的命名和数据结构基础

**文件**：
- 修改：`ccore/src/node/mod.rs`
- 修改：`ccore/src/message/topic.rs`
- 新增：`ccore/src/kernel/reflex.rs`

**实现要点**：
- NodeType 枚举新增：Eye, Ear, Nose, Skin, Mouth, Hand, Limb, Thinker
- Topic 新增仿生命名方法：eye_observe, ear_hear, nose_smell 等
- ReflexRule 数据结构：id, pattern(正则), level(L0/L1_trial/L1_formal), action, params, stats(use_count/success_count/consecutive_fails)
- ReflexRouter 结构体：rules(HashMap), load_from_toml, match_signal, record_result, promote/demote 规则

**核心逻辑示意**：
```rust
pub enum ReflexLevel { L0, L1Trial, L1Formal }
pub struct ReflexRule {
    pub id: String,
    pub pattern: String,        // 正则匹配信号内容
    pub level: ReflexLevel,
    pub action: String,         // 目标 motor topic
    pub params: serde_json::Value,
    pub use_count: u32,
    pub success_count: u32,
    pub consecutive_fails: u32,
}
```

---

### 任务 2：Kernel ReflexRouter 实现

**目标**：在 Kernel 内实现反射弧路由

**文件**：
- 新增：`ccore/src/kernel/reflex.rs`（任务 1 已创建，此处填充逻辑）
- 修改：`ccore/src/kernel/mod.rs`
- 新增：`ccore/src/kernel/reflex_rules.toml`

**实现要点**：
- ReflexRouter::new() 从 reflex_rules.toml 加载预置规则
- ReflexRouter::route(signal_topic, signal_payload) → Option<ReflexAction>
  - 遍历规则，正则匹配 signal_payload
  - L0 → 返回 ReflexAction::Direct { action, params }
  - L1_trial → 返回 ReflexAction::Trial { action, params, require_confirm: true }
  - L1_formal → 返回 ReflexAction::Instinct { action, params, notify_thinker: true }
  - 无匹配 → 返回 None（升级到 L2，转发 ThinkerNode）
- ReflexRouter::record_result(rule_id, success) 更新计数器
  - consecutive_fails >= 2 → 禁用规则，升级到 L2
  - success_rate > 95% && use_count >= 10 → L1→L0 升级
- 预置规则：缺分号/缺 import/文件变化重索引/心跳超时

**核心逻辑示意**：
```rust
pub enum ReflexAction {
    Direct { action: String, params: Value },      // L0
    Instinct { action: String, params: Value },     // L1_formal
    Trial { action: String, params: Value },        // L1_trial
}
impl ReflexRouter {
    pub fn route(&self, topic: &str, payload: &str) -> Option<ReflexAction>
    pub fn record_result(&mut self, rule_id: &str, success: bool)
}
```

---

### 任务 3：Kernel AutonomicNervousSystem 实现

**目标**：将 SelfHealingManager 重构为更完整的自主神经系统

**文件**：
- 新增：`ccore/src/kernel/autonomic.rs`
- 修改：`ccore/src/kernel/mod.rs`（替换 self_healing 为 autonomic）

**实现要点**：
- AutonomicNervousSystem 合并 SelfHealingManager + ConcurrencyController + MessagePool
- 心跳监控（原 SelfHealingManager）
- 并发控制（原 ConcurrencyController）：Agent 并发 10，Tool 并发 20
- 内存回收（原 MessagePool）：定期检查内存压力，触发 BufferGuard 归还
- 启动健康检查循环（每 10 秒）
- 与 ReflexRouter 联动：skin/memory_pressure → autonomic 自动回收

**核心逻辑示意**：
```rust
pub struct AutonomicNervousSystem {
    agents: Arc<Mutex<HashMap<String, AgentHealth>>>,
    agent_semaphore: Arc<Semaphore>,
    tool_semaphore: Arc<Semaphore>,
    memory_pool: Arc<MessagePool>,
    heartbeat_timeout: Duration,
}
```

---

### 任务 4：7 个器官 Node 骨架实现

**目标**：实现所有器官 Node 的基本结构、Topic 订阅、handle_message 框架

**文件**：
- 新增：`ccore/src/node/eye.rs`
- 新增：`ccore/src/node/ear.rs`
- 新增：`ccore/src/node/nose.rs`
- 新增：`ccore/src/node/skin.rs`
- 新增：`ccore/src/node/mouth.rs`
- 新增：`ccore/src/node/hand.rs`
- 新增：`ccore/src/node/limb.rs`
- 修改：`ccore/src/node/mod.rs`（注册新 Node 类型）

**实现要点**：

**EyeNode**：
- 订阅：eye/observe（外部触发观察）、eye/file_changed（notify 监听）、eye/terminal_output（终端输出流）
- 发布：eye/observe（观察结果，含文件内容/AST 信息/终端输出）
- 感官缓冲：最近 10 条观察结果的 LRU 缓存
- handle_message：收到观察请求 → 读取文件/终端 → 解析 → 发布结果
- A+ 融合：ConfigWatcher 的文件监听逻辑迁移到 EyeNode

**EarNode**：
- 订阅：ear/hear（用户输入）、ear/notification（系统通知）、ear/heartbeat（心跳事件）
- 发布：ear/hear（将用户指令转发给 ThinkerNode）
- handle_message：收到用户输入 → 转发到 cortex/{agent_id}/input

**NoseNode**：
- 订阅：nose/smell（外部触发嗅探）、skin/touch（工具执行结果，含编译输出）
- 发布：nose/compile_error、nose/test_failure、nose/performance_degradation
- 嗅探逻辑：解析编译输出，提取 error/warning，按严重程度分级
- 与 ReflexRouter 联动：发布到 nose/* topic，Kernel ReflexRouter 匹配后路由

**SkinNode**：
- 订阅：hand/*、limb/*（所有 motor 操作的结果）
- 发布：skin/touch、skin/process_exit、skin/memory_pressure
- 短期记忆：ShortTermMemory 迁移到 SkinNode，存储最近工具结果
- A+ 融合：persistence 模块迁移，工具结果持久化

**MouthNode**：
- 订阅：cortex/{agent_id}/speak（ThinkerNode 要求输出）、mouth/status（状态报告请求）
- 发布：agent/{agent_id}/output（兼容现有 TUI 订阅）
- A+ 融合：tracing/json_formatter 迁移，输出格式化

**HandNode**：
- 订阅：hand/edit、hand/search、hand/restructure + agent/{agent_id}/tool_call
- 发布：skin/touch（操作结果反馈）、agent/{agent_id}/tool_result
- 工具执行框架：复用现有 ToolNode 的 bridge + builtin 逻辑
- 与 ReflexRouter 联动：接收 L0/L1 反射动作指令
- A+ 融合：degradation 的 fallback 逻辑

**LimbNode**：
- 订阅：limb/execute、limb/build、limb/git + cortex/{agent_id}/tool_call（命令类工具）
- 发布：skin/touch（执行结果反馈）、agent/{agent_id}/tool_result
- 肌肉记忆：记录常用命令的执行模式（如 cargo check 完整路径、常用 git 操作）
- A+ 融合：retry/backoff 迁移，命令执行自动重试

每个 Node 的 subscriptions() 和 published_topics() 按上表实现。

---

### 任务 5：ThinkerNode 实现（从 AgentNode 重构）

**目标**：将 AgentNode 重构为 ThinkerNode，接收感官综合信号 + 发布运动指令

**文件**：
- 新增：`ccore/src/node/thinker.rs`
- 修改：`ccore/src/node/agent.rs`（标记 deprecated，保留兼容）

**实现要点**：
- ThinkerNode 继承 AgentNode 的核心循环（工作记忆 + 采样请求 + 流式处理 + Doom Loop）
- 新增感官输入处理：订阅 nose/*, skin/touch 等，将感官信号融入工作记忆
- 新增运动指令发布：工具调用不再直接发给 ToolNode，而是发布到 hand/* 或 limb/*
- L2 信号路由：未匹配 ReflexRouter 的感官信号转发到 ThinkerNode
- 订阅列表：
  - cortex/{agent_id}/input（替代 agent/{agent_id}/input）
  - cortex/{agent_id}/sensory（感官综合信号，由 Kernel 汇总）
  - sampler/*/stream（LLM 流式返回）
  - skin/touch（工具结果反馈）
  - nose/smell（嗅探结果，L2 级别）

**核心逻辑示意**：
```rust
// ThinkerNode 在 AgentNode 基础上新增：
// 1. 感官输入处理
fn handle_sensory(&mut self, signal: &SensorySignal) {
    // 将感官信号注入工作记忆，供 LLM 下一轮推理使用
    self.working_memory.push_hot("sensory", signal.summary(), tokens);
}
// 2. 运动指令发布（替代直接 tool_call）
fn dispatch_motor(&self, tool_call: &PendingToolCall, transport: &NodeTransportHandle) {
    let motor_topic = classify_motor_topic(&tool_call.tool_name);
    // hand/edit, limb/execute, etc.
    transport.publish_data(&motor_msg).await;
}
```

---

### 任务 6：ExperienceLog + 经历回放提规则

**目标**：实现务实的闭环学习

**文件**：
- 新增：`ccore/src/kernel/experience.rs`
- 修改：`ccore/src/kernel/reflex.rs`（新增 propose_rule 方法）

**实现要点**：
- ExperienceLog：记录每次 L2 处理的 {signal, action, result, context}
- 存储在 StateNode（海马体），跨会话持久化
- 经历回放：Session 结束时，扫描本次 L2 经历
  - GROUP BY (signal_pattern_prefix, action_taken)
  - HAVING COUNT(success) >= 3
  - 提议新 ReflexRule { level: L1_trial, source: learned }
- ReflexRouter::propose_rule：新增 trial 规则，前 3 次使用需 ThinkerNode 确认
- 规则演化统计：success_rate > 95% && use_count >= 10 → L1→L0 升级

**核心逻辑示意**：
```rust
pub struct ExperienceLog {
    entries: Vec<ExperienceEntry>,
}
pub struct ExperienceEntry {
    pub timestamp: DateTime<Utc>,
    pub signal: String,
    pub level: ReflexLevel,
    pub action: String,
    pub result: bool,  // success or not
}
impl ExperienceLog {
    pub fn extract_patterns(&self) -> Vec<ProposedRule> {
        // 按信号前缀+动作分组，成功>=3次的提议为 L1_trial
    }
}
```

---

### 任务 7：Kernel 集成 + launcher 更新 + 编译验证

**目标**：将所有新组件集成到 Kernel，更新启动流程

**文件**：
- 修改：`ccore/src/kernel/mod.rs`（集成 ReflexRouter + AutonomicNervousSystem）
- 修改：`ccore/src/kernel/launcher.rs`（spawn 器官 Node）
- 修改：`ccore/src/lib.rs`（新增模块导出）

**实现要点**：
- Kernel 新增字段：reflex_router: ReflexRouter, autonomic: AutonomicNervousSystem
- Kernel::handle_incoming 新增分支：nose/*, skin/* 等 sensory topic → ReflexRouter::route
  - 匹配到 L0/L1 → 直接构造 motor 消息发送到 hand/* 或 limb/*
  - 无匹配 → 转发到 cortex/{agent_id}/sensory
- ReflexRouter 匹配结果记录：收到 skin/touch 时调用 record_result
- launcher::spawn_initial_set 新增器官 Node 启动
- lib.rs 新增器官 Node 模块导出

**注意**：保持与现有 session 路径（ccode-shell）的兼容，双轨过渡

---

### 任务 8：A+ 模块融合清理

**目标**：将 A+ 模块能力迁移到器官后，清理重复代码

**文件**：
- 修改/删除：`ccore/src/degradation/`（融合到 ReflexRouter）
- 修改/删除：`ccore/src/retry/`（融合到 LimbNode）
- 修改/删除：`ccore/src/performance/`（融合到 AutonomicNervousSystem + HandNode）
- 修改/删除：`ccore/src/config/watcher.rs`（融合到 EyeNode）
- 修改/删除：`ccore/src/config/reloader.rs`（融合到 EyeNode）
- 保留：`ccore/src/persistence/`（作为 SkinNode + StateNode 的底层存储）
- 保留：`ccore/src/metrics/`（各器官使用 AgentMetrics 上报）
- 保留：`ccore/src/tracing/`（MouthNode 使用）

**实现要点**：
- 不直接删除文件，而是将 A+ 模块标记为 deprecated，内部调用转发到器官
- 新代码直接使用器官 Node 的 API
- 确保编译通过后再逐步清理 deprecated 模块
