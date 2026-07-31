# 运行时融合设计：生产并入 ccore（总线协作）

**日期：** 2026-07-31  
**状态：** 已确认（brainstorming）  
**范围：** 将 Grok Build 风格生产运行时能力并入实验版 ccore 微内核，删除旧生产路径，最终只保留一套由 ROS 风格消息总线拉起的协作运行时。

---

## 1. 目标与约束

### 1.1 目标

- 最终**只有一套**运行时。
- 总体协作由 **ROS 风格消息总线**拉起（topic / pub-sub；控制面 DEALER/ROUTER，数据面 PUB/SUB）。
- 生产能力（采样、工具/权限/MCP/hooks、会话状态、compaction、ACP、TUI、doom-loop 等）**迁入 ccore**。
- 迁齐后**删除** SessionActor 主循环、MessageBusBridge、local fallback、双入口分裂。
- 最终版本**功能完整、运行丝滑**。

### 1.2 非目标（本设计不包含）

- 保留双轨长期共存。
- 「总线挂了就走本地 Actor」的回退路径。
- 用假 ZMQ / 假 LLM 冒充可运行系统。

### 1.3 落地约束（实现阶段）

- 代码接**真实**模块边界、真实 topic 契约、真实总线 API 与真实能力实现。
- 「模拟」仅指：**逻辑层先写全，现阶段不编译、不运行**。
- 编译与集成验证放到能力迁入与旧路径删除完成之后。

---

## 2. 目标架构

```
                    ┌──────────── Kernel ────────────┐
                    │  Broker / Registry / Reflex / ANS │
                    │  ROS-style：控制面 ROUTER/DEALER   │
                    │            数据面 PUB/SUB topic    │
                    └───────────────┬────────────────┘
                                    │ 拉起 & 路由
     ┌──────────┬──────────┬────────┼──────────┬──────────┐
     ▼          ▼          ▼        ▼          ▼          ▼
 ThinkerNode SamplerNode ToolNode StateNode TUINode  AcpNode
 （仿生决策     （LLM）    （工具）  （状态）  （人机）  （IDE/ACP）
  + loop）
     │          │          │        │          │          │
     └──────────┴──────────┴──── bus ┴──────────┴──────────┘
              协作靠 topic，不靠直接函数调用跨 Node
```

### 2.1 硬规则

1. **总线是协作中枢**：Node 之间只通过 topic 协作；Kernel 负责拉起、注册、路由、反射、健康，不跑业务 loop。
2. **ThinkerNode 是决策 Node，不是上帝**：负责感知→决策→发起采样/工具请求；不内嵌工具执行、不直接碰持久化实现。
3. **每个能力一个 Node（或明确 topic 契约）**：原生产 Sampler/Tool/ChatState/ACP/TUI 能力迁成 Node 或挂到对应 Node 后面。
4. **唯一一套**：迁完后删除 SessionActor 主循环、本地 fallback、双入口分裂；产品入口只启动 Kernel。
5. **跨 Node 禁止抄近路**：共享库代码可复用，跨 Node 边界必须走 topic。

---

## 3. 能力映射与删除清单

### 3.1 迁入（生产 → ccore Node / 库）

| 生产能力 | 迁到 | 协作方式（topic 契约） |
|---------|------|----------------------|
| `SamplerActor` + providers | `SamplerNode` | `sampler/request` → `sampler/{id}/stream` |
| `ccode-tools` / 权限 / MCP / hooks | `ToolNode`（吸收生产实现，替换薄 `ccore::tools`） | `agent/*/tool_call` → `agent/*/tool_result`；权限走 `agent/*/permission` |
| `ChatStateActor` + JSONL 会话 | `StateNode`（唯一会话真相源） | `state/persist` / `state/query` / `state/event` |
| `ccode-compaction` | 优先挂 `StateNode`；若膨胀再拆 `CompactNode` | `state/compact` |
| `SessionActor` turn / goal / doom-loop | **逻辑迁入** `ThinkerNode`（不保留 SessionActor） | 经总线调 Sampler/Tool/State |
| `ccore_integration`（熔断、token budget、episodic、meta） | 沉入对应 Node / `ccore` 库，去掉 shell 桥 | 进程内库调用或 topic |
| ACP / leader 多 client | `AcpNode`（或 Gateway Node） | `agent/*/input\|output\|permission\|cancel` |
| TUI | 已有 `TUINode`，对齐同一 topic | 同上 |
| Reflex / 快捷路径 | `ReflexRouter`（Kernel） | 匹配则短路，不经 Thinker LLM |

### 3.2 删除（迁齐并验证后）

- `SessionActor` 主循环与 `acp_session_impl/{turn,sampler_turn,tool_calls}` 双路径
- `MessageBusBridge` 与 local fallback
- shell 内嵌的「假连 Kernel / 连不上就回退」逻辑
- 被生产工具栈替换后的薄 `ccore::tools`（若完全被吸收）
- 废弃器官 Node 独立 spawn 残留（Eye/Ear/Nose/Skin/Mouth/Hand/Limb 等 deprecated 路径）
- 双入口分裂：`ccode-pager-bin` 与 `ccode-cli` 收敛为**一个**启动 Kernel 的产品入口

### 3.3 Canonical 真相源

- **会话/记忆持久化**：`StateNode`（吸收原 shell JSONL / ChatState 语义）
- **工具执行与权限**：`ToolNode`（以迁入后的生产工具栈为准，挂在 ccore 下）
- **采样**：`SamplerNode`
- **编排决策**：`ThinkerNode`（经总线协作，非进程内直接调齐所有子系统）

---

## 4. 主路径时序与错误处理

### 4.1 正常一轮

```
User/IDE ─pub─► agent/{id}/input
                    │
              Kernel/Reflex？──yes──► 直接 tool/state/output（不经 LLM）
                    │ no
                    ▼
              ThinkerNode（感知 + 决策）
                    │
         ┌──────────┼──────────┐
         ▼          ▼          ▼
   sampler/request  state/query  （必要时）
         │
         ▼
   SamplerNode ─pub─► sampler/{req}/stream
         │
         ▼
   ThinkerNode 组装：文本结束 → end_turn
                 或 tool_call ─pub─► agent/{id}/tool_call
                                         │
                                    ToolNode（权限/MCP/hooks）
                                         │
                    need permission? ─pub─► agent/{id}/permission
                         │ ACK/deny from Acp/TUI
                         ▼
                    tool_result ─pub─► agent/{id}/tool_result
                         │
                         ▼
                    ThinkerNode 继续 loop（直到 EndTurn / max_turns / cancel）
                         │
                    state/persist + agent/{id}/output
```

### 4.2 错误与取消

| 场景 | 行为 |
|------|------|
| Sampler 失败 / 超时 | SamplerNode 发 error 事件；Thinker 按策略重试或 fallback provider；耗尽则 `output` 带可恢复错误，不崩 Kernel |
| Tool 失败 | `tool_result` 带错误正文回 Thinker（模型可见），不中断总线 |
| 权限拒绝 | ToolNode 不执行，结果标 denied；Thinker 继续或向用户说明 |
| Cancel | `agent/{id}/cancel`：Sampler/Tool 协作取消；Thinker 结束本轮并 persist 部分状态 |
| Node 挂掉 | Kernel 健康检查 + 重启策略；未 ACK 的请求对 client 显式失败，禁止静默丢消息 |
| 背压 | 总线背压与序列号；Thinker 不得在无背压下狂发 `tool_call` |
| Doom loop / max_turns | Thinker 内检测（迁入原 session 逻辑），触发 escape 或 EndTurn |

### 4.3 明确禁止

- Node 之间跨边界直接调用内部 API（库可共享，边界必须 topic）
- 保留「总线不可用 → local SamplerActor/ToolBridge fallback」

---

## 5. 落地节奏

### 阶段 1：契约与骨架

- 固定 topic、消息类型、Node 职责
- Kernel launcher 拉起 Thinker / Sampler / Tool / State / TUI / Acp
- 按真实类型与真实总线 API 写结构（不编译运行）

### 阶段 2：生产能力迁入 ccore

- 按第 3 节映射表搬迁 Sampler、Tools+权限+MCP+hooks、ChatState+JSONL、compaction、turn/doom-loop、ACP
- Thinker 只经 topic 协作

### 阶段 3：删除旧生产路径

- 去掉 SessionActor 主循环、MessageBusBridge、local fallback、双入口
- 产品入口只启动 Kernel

### 阶段 4：编译运行验证

- 全量构建
- 契约测 / Node 测 / 总线协作测 / 原生产关键场景回归（权限、MCP、compaction、ACP cancel）

### 「逻辑层模拟」含义

| 做 | 不做（现阶段） |
|----|----------------|
| 真实模块边界、真实 topic、真实调用关系 | `cargo build` / 跑二进制 |
| 完整控制流与错误路径写进代码 | 用假 LLM/假 ZMQ 冒充「能跑」 |
| 迁移注释与删除点标清 | 留双轨 fallback「以后再说」 |

---

## 6. 成功标准

1. 仅一套运行时：Kernel + ROS 风格总线 + 上述 Node 协作。
2. 原生产关键能力在 ccore 侧完整（无功能回退作为终态）。
3. 无 SessionActor 双路径、无 MessageBusBridge、无 local fallback。
4. 单一产品入口启动 Kernel。
5. 编译启用后：主路径稳定；取消、权限、工具/采样失败可恢复（丝滑）。

---

## 7. 关键现有代码锚点（迁移参考）

| 区域 | 路径 |
|------|------|
| 生产入口 | `crates/codegen/ccode-pager-bin/` |
| SessionActor / turn | `crates/codegen/ccode-shell/src/session/` |
| MessageBusBridge | `crates/codegen/ccode-shell/src/session/message_bus_bridge.rs` |
| ccore 集成桥 | `crates/codegen/ccode-shell/src/session/ccore_integration.rs` |
| Kernel / launcher | `crates/codegen/ccore/src/kernel/` |
| ThinkerNode | `crates/codegen/ccore/src/node/thinker.rs` |
| ccore CLI 入口 | `crates/codegen/ccode-cli/` |
| 既有架构文档 | `docs/ARCHITECTURE.md`、`docs/MESSAGE_BUS_ANALYSIS.md`、`docs/requirements/CCODE-002/` |

---

## 8. 后续

设计批准并落地后，用 writing-plans 产出分步实现计划；实现按本文件阶段 1–3 先写逻辑，阶段 4 再编译验证。
