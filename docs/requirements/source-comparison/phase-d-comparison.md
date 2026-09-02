# Phase D 深度对比：记忆系统 / 工具执行 / 上下文压缩

> 基于 Qoder (ccore + ccode-shell)、Codex、Claude Code 三维度源码对比

---

## 一、记忆系统对比

### 1.1 架构总览

| 维度 | Qoder (ccore) | Codex | Claude Code |
|------|---------------|-------|-------------|
| 记忆层数 | **3 层** (L0/L1/L2) | 无正式分层 | 无正式分层 |
| L0 工作记忆 | Hot/Warm/Cold 温度分类 + 滑动窗口 | Fragment 系统（仅变化部分重发） | 4-breakpoint cache 布局 |
| L1 短期记忆 | 500 条上限 + 伪向量嵌入（64 维字节特征） | 服务端 rollout trace | 会话内完整历史 |
| L2 情景记忆 | Zettelkasten 双向链接网络（1000 节点） | SQLite thread store | 无跨会话记忆 |
| 热量评分 | `heat = w1*recency + w2*relevance + w3*activity + w4*tool_weight` | N/A | N/A |
| 多样性选择 | MMR (Maximal Marginal Relevance) | N/A | N/A |
| 意图检索 | `intent_retriever.rs` LLM 意图扩展 + 多维检索 | N/A | N/A |

**结论**：Qoder 的记忆架构复杂度远超竞品。但 L1 伪向量嵌入质量差（代码注释承认"quality too poor, better to skip"），L2 情景记忆在生产中零调用。

### 1.2 关键差距

| 差距 | 严重度 | 说明 |
|------|--------|------|
| L1 伪向量嵌入质量差 | 中 | 64 维字节特征哈希，非真实嵌入，语义检索不可靠 |
| L2 情景记忆零生产调用 | 高 | 构建了完整的 Zettelkasten 网络但主循环未接入 |
| 无服务端精确 token 计数 | 中 | 依赖字符启发式估算（ASCII/4, CJK/1），Codex 从 usage 字段获取精确值 |
| 无 TokenBudgetReminder 注入 | 中 | 模型不知道剩余上下文空间，Codex 通过 Fragment 注入提醒 |
| 稳定前缀锁未接入主循环 | 高 | `lock_stable_prefix()` 已定义但 Thinker turn 循环中未调用 |

---

## 二、上下文压缩对比

### 2.1 压缩层级

| 层级 | Qoder (ccode-compaction) | Claude Code | Codex |
|------|--------------------------|-------------|-------|
| L0 预算削减 | ✅ `budget.rs` — 按工具类型 token 上限 | ✅ 参考来源 | 服务端 token 预算 |
| L1 Snip | ✅ `snip.rs` — 结构截断（行/字符/匹配数） | ✅ 参考来源 | N/A |
| L2 MicroCompact | ✅ `micro_compact.rs` — 基于年龄的工具结果清理 | ✅ `cachedMicrocompact` | N/A |
| L3 AutoCompact | ✅ `working.rs` — Hot→Warm→Cold 温度降级 | ✅ 自动压缩 | N/A |
| L4 ReactiveCompact | ⚠️ 退化为 L3 截断，非真正 LLM 摘要 | ✅ Context Collapse (LLM) | ✅ `compact_remote_v2` (1191 行) |
| Cache-Aware | ✅ `cache_aware.rs` — FNV-1a 指纹去重 | ✅ 参考来源 | 服务端 prompt_cache_key |

### 2.2 触发条件

| 阈值 | Qoder ccore | Qoder shell |
|------|-------------|-------------|
| 85-90% | Snip | FullReplace LLM 摘要 |
| 90-95% | MicroCompact | N/A |
| 95-99% | AutoCompact | N/A |
| ≥99% | ReactiveCompact（退化为 L3） | N/A |

### 2.3 关键差距

| 差距 | 严重度 | 说明 |
|------|--------|------|
| ccore ReactiveCompact 无真正 LLM 摘要 | 高 | `LlmSummarizer` trait 存在但未接入 Level 4，退化为 Hot→Warm→Cold 截断 |
| ccore 和 shell 双引擎并行 | 高 | 共享 `ccode-compaction` 库但触发逻辑、层级选择、摘要策略各自独立 |
| MicroCompact 年龄阈值静态 | 中 | 固定 300 秒，不随对话节奏自适应 |
| ccore 压缩不区分用户消息 | 中 | 用户消息和工具结果同为 `role: User`，可能被一起降级 |
| 指纹去重过于简单 | 中 | `tool_name + 前 64 字符`，相似开头会碰撞 |
| 无压缩质量评分 | 中 | 仅检查 < 20 字符退化，无语义完整性验证 |

---

## 三、工具执行管线对比

### 3.1 架构

| 维度 | Qoder 生产管线 (ccode-shell) | Qoder ccore ThinkerNode |
|------|------------------------------|------------------------|
| 并发模型 | `FuturesUnordered` + 文件级 Mutex | `JoinSet` + 信号量(20) |
| 推测执行 | ✅ 只读工具流式预执行 | ❌ 无 |
| 全局并发上限 | ❌ 无（仅文件级锁） | ✅ Semaphore(20) |
| 后台任务 | ✅ `TerminalBackend::run_background` | ❌ 无 |
| 重试策略 | 仅 auth 重试 | 指数退避（1s~30s, 最多 3 次） |
| 循环检测 | 5 次相同 (tool, hash) | DoomLoopDetector |
| 失败熔断 | 3 次连续失败 | 5 次连续失败（CircuitBreaker） |
| 工具预算 | 50 次/轮 | 无显式预算 |

### 3.2 独特优化

**推测预执行**（仅生产管线）：LLM 流式输出期间，当只读工具调用参数完整时立即预执行，结果缓存。最终参数匹配时直接使用缓存结果，节省执行延迟。

### 3.3 关键差距

| 差距 | 严重度 | 说明 |
|------|--------|------|
| 生产管线无全局并发上限 | 中 | 大量并发 bash 命令可能耗尽资源 |
| 生产管线无瞬态错误重试 | 中 | 仅 auth 重试，不重试 timeout/429/502 |
| ccore 无后台任务支持 | 低 | 长时间运行命令无法后台化 |
| 工具超时不统一 | 低 | 60s (ThinkerNode) / 120s (ToolBridge) / 1hr (bash mode) |

---

## 四、优化方案（按优先级排序）

### Phase D1: 高优先级快速修复

#### D1.1 稳定前缀锁接入主循环
- **文件**: `ccore/src/node/thinker/mod.rs`
- **改动**: 在 `build_sample_request` 完成后调用 `working_memory.lock_stable_prefix()`
- **收益**: 确保系统提示 + 工具定义不被压缩，最大化 prompt cache 命中

#### D1.2 ccore ReactiveCompact 接入真正 LLM 摘要
- **文件**: `ccore/src/memory/working.rs`
- **改动**: Level 4 调用 `LlmSummarizer` 而非退化为 Level 3
- **收益**: 极端上下文压力下保留更多信息

#### D1.3 生产管线全局并发上限
- **文件**: `ccode-shell/src/session/acp_session_impl/tool_calls.rs`
- **改动**: 添加 `Semaphore(20)` 包裹 `FuturesUnordered`
- **收益**: 防止大量并发 bash 命令耗尽系统资源

### Phase D2: 中优先级改进

#### D2.1 TokenBudgetReminder 注入
- **文件**: `ccore/src/node/thinker/mod.rs` + `ccore/src/sampler/provider.rs`
- **改动**: 在 system prompt 末尾注入剩余 token 预算提示
- **收益**: 模型感知上下文压力，主动精简输出

#### D2.2 MicroCompact 自适应年龄阈值
- **文件**: `ccode-compaction/src/micro_compact.rs`
- **改动**: `max_age = base_age * (1.0 - token_pressure_ratio)`，压力越大清理越激进
- **收益**: 快速对话时积极清理，慢速思考时保留更多上下文

#### D2.3 生产管线瞬态错误重试
- **文件**: `ccode-shell/src/session/acp_session_impl/tool_calls.rs`
- **改动**: 接入 ccore `ToolRetryConfig` 的指数退避逻辑
- **收益**: timeout/429/502 自动重试，提升鲁棒性

#### D2.4 压缩质量评分
- **文件**: `ccode-compaction/src/context_collapse.rs`
- **改动**: 验证摘要中包含原始文件路径、错误信息、任务状态的比例
- **收益**: 防止压缩丢失关键上下文

### Phase D3: 长期演进

#### D3.1 L1 真实嵌入替换
- **改动**: 将 64 维字节特征哈希替换为真实嵌入模型（如 `all-MiniLM-L6-v2`）
- **收益**: 语义检索质量大幅提升

#### D3.2 统一 ccore/shell 压缩引擎
- **改动**: 将 shell 的 FullReplace LLM 摘要能力下沉到 `ccode-compaction`，ccore 直接调用
- **收益**: 消除双引擎维护成本

#### D3.3 增量摘要
- **改动**: 维护滚动摘要，每次仅摘要新增 turn + 前次摘要
- **收益**: 更 token 高效，摘要更稳定

#### D3.4 World State diff
- **改动**: 借鉴 Codex，仅重发变化的 context fragment
- **收益**: 最大化 prompt cache 命中，减少 token 消耗

---

## 五、实施矩阵

| 编号 | 改动 | 涉及文件 | 预估工作量 | 收益 |
|------|------|----------|-----------|------|
| D1.1 | 稳定前缀锁接入 | thinker/mod.rs | 小 | 高 |
| D1.2 | ReactiveCompact LLM 摘要 | working.rs | 中 | 高 |
| D1.3 | 全局并发上限 | tool_calls.rs | 小 | 中 |
| D2.1 | TokenBudgetReminder | thinker/mod.rs, provider.rs | 中 | 中 |
| D2.2 | 自适应 MicroCompact | micro_compact.rs | 小 | 中 |
| D2.3 | 瞬态错误重试 | tool_calls.rs | 中 | 中 |
| D2.4 | 压缩质量评分 | context_collapse.rs | 中 | 中 |
| D3.1 | 真实嵌入 | short_term.rs | 大 | 高 |
| D3.2 | 统一压缩引擎 | working.rs, compaction.rs | 大 | 高 |
| D3.3 | 增量摘要 | compaction 多处 | 大 | 中 |
| D3.4 | World State diff | 新模块 | 大 | 高 |
