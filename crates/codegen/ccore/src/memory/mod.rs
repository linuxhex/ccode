//! 记忆系统 - 三层冷热分层 + Context Engine
//!
//! ## 三层记忆架构
//!
//! | 层级 | 模块 | 说明 |
//! |------|------|------|
//! | L0 | `working` | 工作记忆：当前 context window 内的消息，Hot→Warm→Cold 三级压缩 |
//! | L1 | `short_term` | 短期记忆：本能反射信号，驱动 Agent 快速响应 |
//! | L2 | `episodic` | 情景记忆：跨会话关键词索引，支持检索注入 |
//!
//! ## Context Engine（对标 Augment Code）
//!
//! | 组件 | 模块 | 对标 | 说明 |
//! |------|------|------|------|
//! | 代码块解析 | `function_embed` | TinyEmbed | 按函数/类粒度切分代码并嵌入 |
//! | 意图检索 | `intent_retriever` | Retriever-XL | LLM 意图展开 + 多维度查询 + 依赖链扩展 |
//! | 依赖图 | `repo_map` | - | 文件级 import 依赖图 + 影响分析 |
//! | 向量索引 | `embedding` | - | O(n log k) TopK 搜索 + MMR 多样性 |
//!
//! ## 上下文压缩（4 级策略，对标 Claude Code）
//!
//! | 级别 | 名称 | 触发条件 | 操作 |
//! |------|------|---------|------|
//! | 1 | Snip | 85%~90% | 截断过长工具输出 |
//! | 2 | MicroCompact | 90%~95% | 单条消息首尾截断 |
//! | 3 | AutoCompact | 95%~99% | 批量降级 Hot→Warm→Cold |
//! | 4 | ReactiveCompact | ≥99% | LLM 全量摘要替换 |
//!
//! Stable Prefix 保护：压缩不修改 system prompt 前缀，最大化 API Prompt Cache 命中率。

pub mod working;
pub mod short_term;
pub mod heat;
pub mod window;
pub mod embedding;
pub mod mmr;
pub mod storage;
pub mod episodic;
pub mod repo_map;
pub mod function_embed;
pub mod intent_retriever;
