# Agent Loop 融合实现计划

## 目标

融合两套 Agent Loop 为 ThinkerNode 唯一 Agent Loop，集成 ccore 高级特性。

## 架构概述

```
融合后：
ccode -p "fix bug"
  → main.rs (kernel.run() 非阻塞 + prompt 发送)
  → ThinkerNode (唯一 Agent Loop，集成全部 ccore 特性)
  → SamplerNode / ToolNode / StateNode (ZMQ 协作)
```

## 文件清单

| 文件 | 操作 | 职责 |
|------|------|------|
| ccrates/codegen/ccore/src/node/thinker/mod.rs | 修改 | 集成 ccore 高级特性 |
| ccrates/codegen/ccode-cli/src/main.rs | 修改 | 修复 prompt 传递 |
| ccrates/codegen/ccode-pager-bin/ | 删除 | 合并到 ccode-cli |

---

## 任务拆分

### 任务 1：ThinkerNode 集成 ccore 高级特性

**目标**：在 ThinkerNode 的 Agent Loop 中嵌入 CircuitBreaker/ReadTracker/TokenBudget/EpisodicMemory/MetaCognitive/PromptCache

**文件**：
- 修改：`ccrates/codegen/ccore/src/node/thinker/mod.rs`

**实现要点**：
- 添加 `ccore_session_state: CcoreSessionState` 字段
- `send_sample_request` 前：CircuitBreaker 门控
- `handle_stream_chunk` Ok：TokenBudget 记录 + PromptCache 记录
- `handle_tool_result`：ReadTracker 集成
- `handle_message` 结束：EpisodicMemory 编码 + MetaCognitive 冲突检测

### 任务 2：修复 main.rs prompt 传递

**目标**：`ccode -p "fix bug"` 的 prompt 真正发送到 ThinkerNode

**文件**：
- 修改：`crates/codegen/ccode-cli/src/main.rs`

**实现要点**：
- `kernel.run()` 改为非阻塞 spawn
- 等待 ThinkerNode 注册完成
- 通过 ZMQ `agent/{thinker_id}/input` 发送 prompt

### 任务 3：删除 pager-bin，合并到 ccode-cli

**目标**：ccode-cli 成为唯一入口，pager 功能合并

**文件**：
- 删除：`crates/codegen/ccode-pager-bin/`
- 修改：`crates/codegen/ccode-cli/src/main.rs`

**实现要点**：
- 移除 pager-bin 依赖
- SessionActor 的 Agent Loop 标记为 `#[deprecated]`，保留数据结构供 ThinkerNode 引用