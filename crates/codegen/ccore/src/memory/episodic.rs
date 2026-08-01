//! 情景记忆模块（借鉴 AdMem/E-mem/A-MEM 2025-2026 论文）
//!
//! 核心创新：
//! 1. Zettelkasten双向链接（A-MEM）：记忆不是扁平存储，而是动态知识网络
//! 2. 情景上下文重建（E-mem）：保留完整上下文而非压缩嵌入
//! 3. 三种记忆统一（AdMem）：语义+情景+程序记忆双向转换
//!
//! 参考：
//! - AdMem: Advanced Memory for Task-solving Agents (Princeton, 2026)
//! - E-mem: Multi-Agent Based Episodic Context Reconstruction (2026)
//! - A-MEM: Agentic Memory for LLM Agents (NeurIPS 2025)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

/// 记忆节点ID
pub type MemoryId = String;

/// 记忆类型（借鉴 CoALA 框架 + AdMem）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum MemoryType {
    /// 语义记忆：世界知识和事实
    /// 如 "Rust的HashMap是线程不安全的"
    Semantic,
    /// 情景记忆：特定事件的完整记录
    /// 如 "上次尝试用HashMap做缓存导致了数据竞争"
    Episodic,
    /// 程序记忆：可复用的技能和规则
    /// 如 "并发场景使用Arc<RwLock<HashMap>>而非HashMap"
    Procedural,
}

/// 记忆来源追踪
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySource {
    /// 来源会话ID
    pub session_id: String,
    /// 来源时间戳
    pub timestamp: i64,
    /// 来源消息索引
    pub message_index: Option<usize>,
    /// 置信度
    pub confidence: f64,
}

/// 记忆节点（Zettelkasten风格）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryNode {
    /// 唯一ID
    pub id: MemoryId,
    /// 记忆类型
    pub memory_type: MemoryType,
    /// 核心内容
    pub content: String,
    /// 上下文（E-mem: 保留完整上下文，不只是压缩嵌入）
    pub context: String,
    /// 关键词标签
    pub keywords: Vec<String>,
    /// 前向链接：此记忆引用的其他记忆
    pub forward_links: Vec<MemoryId>,
    /// 后向链接：引用此记忆的其他记忆
    pub back_links: Vec<MemoryId>,
    /// 来源追踪
    pub source: MemorySource,
    /// 访问次数
    pub access_count: u64,
    /// 最后访问时间
    pub last_accessed: i64,
    /// 奖励值（AdMem: reward-based evaluation）
    pub reward: f64,
}

/// 情景记忆存储（Zettelkasten知识网络）
pub struct EpisodicMemoryStore {
    /// 记忆节点图
    nodes: RwLock<HashMap<MemoryId, MemoryNode>>,
    /// 关键词索引
    keyword_index: RwLock<HashMap<String, Vec<MemoryId>>>,
    /// 类型索引
    type_index: RwLock<HashMap<MemoryType, Vec<MemoryId>>>,
    /// 计数器（确保ID唯一）
    counter: AtomicU64,
    /// 最大记忆容量（FIFO淘汰）
    max_memories: usize,
    /// 插入顺序（用于FIFO淘汰）
    insertion_order: RwLock<Vec<MemoryId>>,
}

impl EpisodicMemoryStore {
    /// 默认最大记忆容量
    const DEFAULT_MAX_MEMORIES: usize = 1000;

    pub fn new() -> Self {
        Self {
            nodes: RwLock::new(HashMap::new()),
            keyword_index: RwLock::new(HashMap::new()),
            type_index: RwLock::new(HashMap::new()),
            counter: AtomicU64::new(0),
            max_memories: Self::DEFAULT_MAX_MEMORIES,
            insertion_order: RwLock::new(Vec::new()),
        }
    }

    /// 创建指定容量的情景记忆存储
    pub fn with_capacity(max: usize) -> Self {
        Self {
            nodes: RwLock::new(HashMap::new()),
            keyword_index: RwLock::new(HashMap::new()),
            type_index: RwLock::new(HashMap::new()),
            counter: AtomicU64::new(0),
            max_memories: max,
            insertion_order: RwLock::new(Vec::new()),
        }
    }

    /// 编码新记忆（A-MEM三步写入路径）
    ///
    /// Step 1: 生成带元数据的笔记
    /// Step 2: 搜索历史记忆寻找语义连接
    /// Step 3: 双向更新链接的记忆
    pub fn encode(
        &self,
        memory_type: MemoryType,
        content: &str,
        context: &str,
        keywords: Vec<String>,
        source: MemorySource,
    ) -> MemoryId {
        let id = format!("mem_{}_{}", chrono::Utc::now().timestamp_millis(), self.counter.fetch_add(1, Ordering::Relaxed));

        // Step 2: 搜索相关记忆
        let related = self.find_related(content, &keywords);

        let node = MemoryNode {
            id: id.clone(),
            memory_type: memory_type.clone(),
            content: content.to_string(),
            context: context.to_string(),
            keywords: keywords.clone(),
            forward_links: related.clone(),
            back_links: Vec::new(),
            source,
            access_count: 0,
            last_accessed: chrono::Utc::now().timestamp(),
            reward: 0.5,
        };

        // Step 3: 双向更新链接（Zettelkasten核心）
        {
            let mut nodes = self.nodes.write().unwrap();
            for related_id in &related {
                if let Some(existing) = nodes.get_mut(related_id) {
                    existing.back_links.push(id.clone());
                    // 追溯性重新评估（A-MEM: retroactive linking）
                    existing.reward = (existing.reward + 0.1).min(1.0);
                }
            }
            nodes.insert(id.clone(), node);
            self.insertion_order.write().unwrap().push(id.clone());
        }

        // FIFO 淘汰：超出容量时移除最早插入的记忆
        {
            let mut insertion_order = self.insertion_order.write().unwrap();
            while insertion_order.len() > self.max_memories {
                let oldest_id = insertion_order.remove(0);
                self.evict_memory(&oldest_id);
            }
        }

        // 更新索引
        for keyword in &keywords {
            self.keyword_index
                .write()
                .unwrap()
                .entry(keyword.clone())
                .or_default()
                .push(id.clone());
        }

        self.type_index
            .write()
            .unwrap()
            .entry(memory_type)
            .or_default()
            .push(id.clone());

        tracing::debug!(
            target: "ccore::episodic",
            id = %id,
            links = related.len(),
            "memory encoded with bidirectional links"
        );

        id
    }

    /// 淘汰指定记忆：从节点图和所有索引中移除
    fn evict_memory(&self, id: &MemoryId) {
        // 从节点图中移除，并清理其他节点中指向该记忆的链接
        if let Some(removed) = self.nodes.write().unwrap().remove(id) {
            // 清理前向链接引用
            for forward_id in &removed.forward_links {
                if let Some(node) = self.nodes.write().unwrap().get_mut(forward_id) {
                    node.back_links.retain(|l| l != id);
                }
            }
            // 清理后向链接引用
            for back_id in &removed.back_links {
                if let Some(node) = self.nodes.write().unwrap().get_mut(back_id) {
                    node.forward_links.retain(|l| l != id);
                }
            }
            // 清理关键词索引
            for kw in &removed.keywords {
                if let Some(vec) = self.keyword_index.write().unwrap().get_mut(kw) {
                    vec.retain(|l| l != id);
                }
            }
            // 清理类型索引
            if let Some(vec) = self.type_index.write().unwrap().get_mut(&removed.memory_type) {
                vec.retain(|l| l != id);
            }
        }
        tracing::debug!(target: "ccore::episodic", id = %id, "memory evicted (FIFO)");
    }

    /// 情景上下文重建（E-mem核心思想）
    ///
    /// 不是被动检索，而是在激活的记忆片段内本地推理，
    /// 提取上下文感知的证据后聚合
    pub fn reconstruct_context(&self, query: &str, max_depth: usize) -> String {
        let nodes = self.nodes.read().unwrap();
        let mut visited = std::collections::HashSet::new();
        let mut result = Vec::new();

        // 找到初始匹配
        let seeds = self.find_related(query, &[]);

        // 沿着链接图遍历（BFS）
        let mut queue = std::collections::VecDeque::new();
        for seed in seeds {
            queue.push_back((seed, 0));
        }

        while let Some((node_id, depth)) = queue.pop_front() {
            if visited.contains(&node_id) || depth > max_depth {
                continue;
            }
            visited.insert(node_id.clone());

            if let Some(node) = nodes.get(&node_id) {
                // E-mem: 保留完整上下文
                result.push(format!(
                    "[{}] {} (上下文: {})",
                    match node.memory_type {
                        MemoryType::Semantic => "语义",
                        MemoryType::Episodic => "情景",
                        MemoryType::Procedural => "程序",
                    },
                    node.content,
                    node.context
                ));

                // 继续沿链接遍历
                for link in &node.forward_links {
                    if !visited.contains(link) {
                        queue.push_back((link.clone(), depth + 1));
                    }
                }
                for link in &node.back_links {
                    if !visited.contains(link) {
                        queue.push_back((link.clone(), depth + 1));
                    }
                }
            }
        }

        result.join("\n")
    }

    /// 奖励标注（AdMem: reward-based evaluation）
    ///
    /// 根据记忆的实际有用性调整奖励值
    pub fn annotate_reward(&self, memory_id: &str, was_useful: bool) {
        if let Some(node) = self.nodes.write().unwrap().get_mut(memory_id) {
            let delta = if was_useful { 0.1 } else { -0.2 };
            node.reward = (node.reward + delta).clamp(0.0, 1.0);
            node.access_count += 1;
            node.last_accessed = chrono::Utc::now().timestamp();
        }
    }

    /// 记忆合并（AdMem: merging for scalability）
    ///
    /// 合并语义相似的低奖励记忆
    pub fn merge_similar(&self, similarity_threshold: f64) -> usize {
        let nodes = self.nodes.read().unwrap();
        let mut to_merge: Vec<(MemoryId, MemoryId)> = Vec::new();
        let node_ids: Vec<_> = nodes.keys().cloned().collect();

        // 找出低奖励记忆对
        for i in 0..node_ids.len() {
            for j in (i + 1)..node_ids.len() {
                if let (Some(a), Some(b)) = (nodes.get(&node_ids[i]), nodes.get(&node_ids[j])) {
                    if a.memory_type == b.memory_type && a.reward < 0.3 && b.reward < 0.3 {
                        let sim = self.trigram_similarity(&a.content, &b.content);
                        if sim > similarity_threshold {
                            to_merge.push((a.id.clone(), b.id.clone()));
                        }
                    }
                }
            }
        }

        drop(nodes);

        let mut merged = 0;
        let mut nodes = self.nodes.write().unwrap();
        for (id_a, id_b) in to_merge {
            if let (Some(a), Some(b)) = (nodes.remove(&id_a), nodes.remove(&id_b)) {
                // 合并：保留高奖励的，吸收低奖励的链接
                let (keeper, absorbed) = if a.reward >= b.reward {
                    (a, b)
                } else {
                    (b, a)
                };
                let mut merged_node = keeper;
                merged_node.content = format!("{}\n---\n{}", merged_node.content, absorbed.content);
                merged_node
                    .forward_links
                    .extend(absorbed.forward_links);
                merged_node.back_links.extend(absorbed.back_links);
                merged_node.keywords.extend(absorbed.keywords);
                merged_node.keywords.sort_unstable();
                merged_node.keywords.dedup();
                nodes.insert(merged_node.id.clone(), merged_node);
                merged += 1;
            }
        }

        tracing::info!(target: "ccore::episodic", merged, "memory merge completed");
        merged
    }

    /// 记忆修剪（AdMem: pruning for scalability）
    pub fn prune(&self, min_reward: f64, max_age_days: i64) -> usize {
        let now = chrono::Utc::now().timestamp();
        let day_secs = 86400i64;

        let mut nodes = self.nodes.write().unwrap();
        let before = nodes.len();

        nodes.retain(|_, node| {
            let age_days = (now - node.last_accessed) / day_secs;
            node.reward >= min_reward || age_days <= max_age_days
        });

        let pruned = before - nodes.len();
        if pruned > 0 {
            tracing::info!(target: "ccore::episodic", pruned, "memories pruned");
        }
        pruned
    }

    /// 查找相关记忆
    fn find_related(&self, content: &str, keywords: &[String]) -> Vec<MemoryId> {
        let nodes = self.nodes.read().unwrap();
        let mut scored: Vec<(MemoryId, f64)> = Vec::new();

        for (id, node) in nodes.iter() {
            let mut score = 0.0;

            // 关键词匹配
            for kw in keywords {
                if node.keywords.contains(kw) || node.content.contains(kw) {
                    score += 0.3;
                }
            }

            // trigram 相似度
            score += self.trigram_similarity(content, &node.content) * 0.7;

            // 奖励加权
            score *= node.reward;

            if score > 0.1 {
                scored.push((id.clone(), score));
            }
        }

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(5).map(|(id, _)| id).collect()
    }

    /// trigram 相似度（委托到共享工具函数）
    fn trigram_similarity(&self, a: &str, b: &str) -> f64 {
        crate::utils::trigram_similarity(a, b)
    }

    /// 获取统计信息
    pub fn stats(&self) -> (usize, usize, usize) {
        let nodes = self.nodes.read().unwrap();
        let semantic = nodes
            .values()
            .filter(|n| n.memory_type == MemoryType::Semantic)
            .count();
        let episodic = nodes
            .values()
            .filter(|n| n.memory_type == MemoryType::Episodic)
            .count();
        let procedural = nodes
            .values()
            .filter(|n| n.memory_type == MemoryType::Procedural)
            .count();
        (semantic, episodic, procedural)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_source() -> MemorySource {
        MemorySource {
            session_id: "test_session".to_string(),
            timestamp: chrono::Utc::now().timestamp(),
            message_index: Some(0),
            confidence: 0.9,
        }
    }

    #[test]
    fn test_encode_memory() {
        let store = EpisodicMemoryStore::new();
        let id = store.encode(
            MemoryType::Semantic,
            "Rust的HashMap是线程不安全的",
            "在并发编程讨论中提到",
            vec!["Rust".to_string(), "HashMap".to_string(), "并发".to_string()],
            make_source(),
        );
        assert!(id.starts_with("mem_"));
        let (s, e, p) = store.stats();
        assert_eq!(s, 1);
        assert_eq!(e, 0);
        assert_eq!(p, 0);
    }

    #[test]
    fn test_bidirectional_links() {
        let store = EpisodicMemoryStore::new();

        let id1 = store.encode(
            MemoryType::Semantic,
            "Rust的HashMap是线程不安全的",
            "在并发编程讨论中提到",
            vec!["Rust".to_string(), "HashMap".to_string()],
            make_source(),
        );

        let id2 = store.encode(
            MemoryType::Procedural,
            "并发场景使用Arc<RwLock<HashMap>>而非HashMap",
            "从HashMap数据竞争事故中学到的",
            vec!["Rust".to_string(), "HashMap".to_string(), "并发".to_string()],
            make_source(),
        );

        // id2应该链接到id1（因为关键词重叠）
        let nodes = store.nodes.read().unwrap();
        let node2 = nodes.get(&id2).unwrap();
        assert!(node2.forward_links.contains(&id1));

        // id1应该有id2的后向链接
        let node1 = nodes.get(&id1).unwrap();
        assert!(node1.back_links.contains(&id2));
    }

    #[test]
    fn test_reconstruct_context() {
        let store = EpisodicMemoryStore::new();

        store.encode(
            MemoryType::Semantic,
            "Rust的HashMap是线程不安全的",
            "在并发编程讨论中提到",
            vec!["Rust".to_string(), "HashMap".to_string()],
            make_source(),
        );

        store.encode(
            MemoryType::Episodic,
            "上次尝试用HashMap做缓存导致了数据竞争",
            "代码审查中发现了这个问题",
            vec!["HashMap".to_string(), "数据竞争".to_string()],
            make_source(),
        );

        let context = store.reconstruct_context("HashMap", 2);
        assert!(!context.is_empty());
        assert!(context.contains("HashMap"));
    }

    #[test]
    fn test_annotate_reward() {
        let store = EpisodicMemoryStore::new();

        let id = store.encode(
            MemoryType::Semantic,
            "测试记忆",
            "测试上下文",
            vec!["测试".to_string()],
            make_source(),
        );

        store.annotate_reward(&id, true);
        store.annotate_reward(&id, true);

        let nodes = store.nodes.read().unwrap();
        let node = nodes.get(&id).unwrap();
        assert!(node.reward > 0.5);
        assert_eq!(node.access_count, 2);
    }

    #[test]
    fn test_trigram_similarity() {
        let store = EpisodicMemoryStore::new();

        let sim = store.trigram_similarity("Rust的HashMap", "Rust的HashMap是线程不安全的");
        assert!(sim > 0.0);

        let sim2 = store.trigram_similarity("完全不相关的内容", "另一个无关文本");
        assert!(sim2 < sim);
    }

    #[test]
    fn test_merge_similar() {
        let store = EpisodicMemoryStore::new();

        // 创建两个低奖励的相似记忆
        let id1 = store.encode(
            MemoryType::Semantic,
            "Rust的Vec是线程不安全的",
            "讨论1",
            vec!["Rust".to_string()],
            make_source(),
        );
        let id2 = store.encode(
            MemoryType::Semantic,
            "Rust的Vec是线程不安全的",
            "讨论2",
            vec!["Rust".to_string()],
            make_source(),
        );

        // 降低奖励
        store.annotate_reward(&id1, false);
        store.annotate_reward(&id1, false);
        store.annotate_reward(&id2, false);
        store.annotate_reward(&id2, false);

        let merged = store.merge_similar(0.3);
        // 由于内容完全相同，应该被合并
        assert!(merged >= 1);
    }

    #[test]
    fn test_prune() {
        let store = EpisodicMemoryStore::new();

        let id = store.encode(
            MemoryType::Semantic,
            "待修剪的记忆",
            "旧上下文",
            vec!["旧".to_string()],
            make_source(),
        );

        // 降低奖励使其可被修剪
        store.annotate_reward(&id, false);
        store.annotate_reward(&id, false);
        store.annotate_reward(&id, false);

        // 修改last_accessed使其变"老"（模拟30天前访问）
        {
            let mut nodes = store.nodes.write().unwrap();
            if let Some(node) = nodes.get_mut(&id) {
                node.last_accessed = chrono::Utc::now().timestamp() - 31 * 86400;
            }
        }

        // min_reward=0.5 且 max_age_days=30 → 低奖励+超过30天未访问 = 被修剪
        let pruned = store.prune(0.5, 30);
        assert!(pruned >= 1);
    }

    #[test]
    fn test_three_memory_types() {
        let store = EpisodicMemoryStore::new();

        store.encode(
            MemoryType::Semantic,
            "Rust的HashMap是线程不安全的",
            "知识库",
            vec!["Rust".to_string()],
            make_source(),
        );
        store.encode(
            MemoryType::Episodic,
            "上次HashMap导致了数据竞争",
            "事故报告",
            vec!["HashMap".to_string()],
            make_source(),
        );
        store.encode(
            MemoryType::Procedural,
            "使用Arc<RwLock<HashMap>>",
            "最佳实践",
            vec!["并发".to_string()],
            make_source(),
        );

        let (s, e, p) = store.stats();
        assert_eq!(s, 1);
        assert_eq!(e, 1);
        assert_eq!(p, 1);
    }
}
