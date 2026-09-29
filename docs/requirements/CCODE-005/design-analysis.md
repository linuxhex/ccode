# ccore 设计层分析与优化建议

> 分析日期：2026-08-27
> 定位：前九轮修的是「实现与声明不符」，本文档分析**架构设计本身**的问题
> 方法：追运行时构造点与调用链，不停在声明层（第三轮曾因只比对声明而误判）

---

## 一、与已有对比文档的关系

`docs/requirements/source-comparison/` 下已有 5 份对比（沙箱、记忆、工具、Hook、TUI），本文档**不重复**它们的横向对比，而是做两件它们没做的事：

1. **核实已有结论在九轮修复后是否仍成立**
2. **找出代码里可直接优化的设计问题**（而非与竞品的能力差距）

### 核实结果：一条已有结论不准确

`phase-d-comparison.md` 称 ccore L1 短期记忆用「伪向量嵌入（64 维字节特征）」。

**实测不符**：`memory/embedding.rs:45` 的默认维度是 **1536**（OpenAI ada-002），且该文件**不生成向量**——它只是索引容器，零 HTTP 调用。向量由调用方传入。

这个差异重要，因为它改变了问题性质：不是「嵌入质量差」，而是**根本没有向量来源**。

---

## 二、核心发现：语义检索链路是双重死锁

### 2.1 证据链

| 环节 | 实测状态 |
|------|---------|
| `EmbeddingIndex` / `mmr_select` | 776 行完整实现 + 29 个测试 |
| `WorkingMemory::add_embedding` | **零生产调用** |
| `search_similar` / `search_diverse` | **零生产调用** |
| `IntentRetriever::search_by_intents` | ✅ 真被调用（`sensory.rs:62`） |
| `IntentRetriever::register_block(s)` | **零生产调用** → 索引恒空 |
| 查询向量来源（`sensory.rs:61`） | **`vec![0.0f32; 1536]` 全零** |

### 2.2 两处独立失效，互相掩盖

**失效一：查询向量是全零**

```rust
// node/thinker/sensory.rs:61
let query_embedding = vec![0.0f32; 1536];
let results = self.intent_retriever.search_by_intents(&intents, &query_embedding, 5);
```

`cosine_similarity`（`embedding.rs:109-111`）在 `norm_a == 0.0` 时返回 `0.0`。所以**所有候选相似度恒为 0**，排序无意义。

**失效二：索引恒空**

`IntentRetriever::new(EmbeddingIndex::new())` 建空索引，而唯一的填充入口 `register_block(s)` 零调用。

### 2.3 为什么这比崩溃更糟

`search_by_intents` **无相似度阈值过滤**（`intent_retriever.rs:102-109` 直接 push）。若索引非空，`score=0` 的无关结果会被当作「代码上下文」注入工作记忆：

```rust
format!("[代码上下文] {} ({}:{}, 相关度:{:.2})", ...)
```

即**静默产出垃圾并消耗 token 预算**。当前因索引恒空而侥幸返回空——两个 bug 互相掩盖，修好任意一个都会暴露另一个。

> ⚠️ 这一点决定了修复顺序：**不能只修查询向量**。先接通向量来源，再修查询，否则第一步就会开始注入垃圾。

---

## 三、架构级重复：ccore 自造了用不了的轮子

| | ccore `memory/` | `ccode-memory` crate |
|---|---|---|
| 规模 | 776 行（embedding + mmr） | 9918 行 |
| 真实 embedding API | ❌ 无（零 HTTP） | ✅ 有（reqwest + 认证 + 批量） |
| 谁在用 | 无（零生产调用） | `ccode-shell`（真产品路径） |
| ccore 是否依赖它 | — | ❌ 未依赖 |

**判断**：ccore 侧的索引层不是「差一点就能用」，而是缺了最关键的一环（向量生成），而这一环在同 workspace 的另一个 crate 里**已经实现且在被使用**。

---

## 四、优化建议（按性价比排序）

### 建议 1：语义检索——复用 `ccode-memory` 而非补全自造（推荐）

**理由**：`ccode-memory` 的 `EmbeddingProvider::embed_batch` 是异步真 API，正是 ccore 缺的那一环；且它已被 `ccode-shell` 验证。

**接线路径**（与本次已验证的 L4 摘要范式同构）：
- 向量生成是**异步网络调用**，不能在同步的 `observe()` 里 await
- 应走消息总线往返：`memory/embed_request` → 专用处理 → `memory/embed_result`
- 或在会话启动时批量索引一次代码库（`register_blocks`），避免每轮实时嵌入

**必须同时修**：
1. `sensory.rs:61` 的全零向量（否则接通索引后立刻开始注入垃圾）
2. `search_by_intents` 加相似度阈值过滤（`score < 0.3` 丢弃）

### 建议 2：加相似度阈值（低成本，可独立做）

即使暂不接向量来源，`search_by_intents` 也应过滤低分结果。当前设计「无阈值 + 全零查询」的组合是危险的，加阈值能让失效变成**可见的空结果**而非**不可见的垃圾注入**。

### 建议 3：删除或标注 ccore 自造的索引层

若采纳建议 1，`memory/embedding.rs` + `mmr.rs`（776 行）应删除或明确标注为「仅供 IntentRetriever 内部使用，向量由 ccode-memory 提供」。保留 29 个测试的价值取决于是否继续用这套索引。

---

## 五、本轮未改代码的原因

以上三项都属于**架构决策**而非缺陷修复：

- 建议 1 涉及跨 crate 依赖（ccore → ccode-memory）与异步往返设计
- 建议 3 涉及 776 行代码与 29 个测试的存废

前九轮我改的都是「证据确凿 + 改动可控 + 测试能锁」的实现缺陷。这三项不满足第二条，需要先定方向。

**唯一例外**：建议 2（加阈值）可独立实施且零风险，若确认要做可立即执行。

---

## 六、方法论备忘

本轮再次验证第三轮的教训：**判定死代码必须追到运行时构造点**。

本次分析中我的第一版 grep 提取模式有 bug（`memory/episodic` 只抓到 1 个 pub fn，实际 8 个），若据此下结论会得出「episodic 是死簇」的错误判断——实测 `encode` 有 3 处、`stats` 有 6 处生产调用，它是**活的**。

同样，`function_embed` 初看零调用，但它被 `intent_retriever` 引用，而后者被 `sensory.rs` 真调用——**死活判断必须追完整条链**。
