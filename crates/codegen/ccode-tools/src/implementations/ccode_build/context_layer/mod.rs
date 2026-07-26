//! 上下文冷热分层与滑动窗口机制
//!
//! 将对话上下文分为热区（最近 N 轮完整保留）、温区（压缩摘要 + 关键决策 + 错误纠正标注）、
//! 冷区（长期记忆按需检索）三个层级。滑动窗口根据语义价值决定消息的去留，
//! 高价值消息优先保留，低价值消息优先淘汰。

mod types;
pub use types::*;

use std::collections::VecDeque;

/// 决策类关键词：表示关键选择、确认、约束
const DECISION_KEYWORDS: &[&str] = &["决定", "选择", "确认", "必须", "不能"];

/// 纠正类关键词：表示对先前内容的否定/修正
const CORRECTION_KEYWORDS: &[&str] = &["不对", "错了", "应该是", "不是这样"];

/// 闲聊类关键词：低信息量的日常对话
const CHITCHAT_KEYWORDS: &[&str] = &["嗯", "好", "谢谢"];

/// 上下文管理器：实现冷热分层 + 滑动窗口
pub struct ContextManager {
    /// 热区：最近 N 轮完整消息
    hot: VecDeque<ContextMessage>,
    /// 温区：压缩摘要
    warm: Vec<ContextMessage>,
    /// 滑动窗口配置
    config: SlidingWindowConfig,
}

impl ContextManager {
    /// 创建新的上下文管理器
    pub fn new(config: SlidingWindowConfig) -> Self {
        Self {
            hot: VecDeque::with_capacity(config.hot_window_size),
            warm: Vec::with_capacity(config.warm_max_summaries),
            config,
        }
    }

    /// 添加消息到上下文
    ///
    /// 新消息进入热区；如果热区超过 hot_window_size，
    /// 最老的消息按语义价值分流：
    /// - 高价值 → 温区（压缩为摘要）
    /// - 低价值 → 丢弃
    /// - 中价值 → 温区（简单摘要）
    pub fn add_message(&mut self, mut msg: ContextMessage) {
        // 新消息进入热区
        msg.zone = ContextZone::Hot;
        self.hot.push_back(msg);

        // 热区溢出时，将最老消息按语义价值分流
        while self.hot.len() > self.config.hot_window_size {
            if let Some(old) = self.hot.pop_front() {
                match old.semantic_value {
                    // 高价值和中价值消息进入温区（压缩为摘要）
                    SemanticValue::High | SemanticValue::Medium => {
                        self.push_to_warm(old);
                    }
                    // 低价值消息直接丢弃
                    SemanticValue::Low => {}
                }
            }
        }
    }

    /// 将消息压缩后推入温区；温区溢出时淘汰最老的摘要
    fn push_to_warm(&mut self, msg: ContextMessage) {
        // 温区满时淘汰最老的摘要
        if self.warm.len() >= self.config.warm_max_summaries {
            self.warm.remove(0);
        }

        // 将消息压缩为摘要形式存入温区
        let summary = self.compress_to_summary(&msg);
        self.warm.push(summary);
    }

    /// 将消息压缩为温区摘要
    ///
    /// 高价值消息保留完整内容，中价值消息截断为简短摘要
    fn compress_to_summary(&self, msg: &ContextMessage) -> ContextMessage {
        let content = match msg.semantic_value {
            // 高价值消息保留完整内容
            SemanticValue::High => msg.content.clone(),
            // 中价值消息截断为摘要（最多 200 字符）
            SemanticValue::Medium => {
                if msg.content.len() > 200 {
                    format!("{}…", &msg.content[..200.min(msg.content.len())])
                } else {
                    msg.content.clone()
                }
            }
            // 低价值消息不会进入温区，此处为防御性处理
            SemanticValue::Low => msg.content.clone(),
        };

        ContextMessage {
            uuid: msg.uuid.clone(),
            zone: ContextZone::Warm,
            semantic_value: msg.semantic_value,
            is_negated: msg.is_negated,
            correction: msg.correction.clone(),
            content,
            created_at: msg.created_at.clone(),
        }
    }

    /// 标记纠正：将原始消息标记为反向知识，并添加纠正消息到热区
    ///
    /// - 在热区和温区中查找原始消息，标记为 is_negated = true
    /// - 创建 CorrectedKnowledge 标注
    /// - 添加纠正消息到热区
    pub fn mark_correction(
        &mut self,
        original_uuid: &str,
        corrected: &str,
        reason: &str,
        timestamp: &str,
    ) {
        // 在热区中查找并标记原始消息
        let original_content = self.find_and_mark_negated(original_uuid);

        // 如果热区没找到，在温区中查找并标记
        let original_content = original_content.or_else(|| self.find_and_mark_negated_in_warm(original_uuid));

        // 创建纠正消息并添加到热区
        let correction = CorrectedKnowledge {
            original: original_content.unwrap_or_default(),
            corrected: corrected.to_string(),
            reason: reason.to_string(),
            timestamp: timestamp.to_string(),
        };

        let correction_msg = ContextMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            zone: ContextZone::Hot,
            semantic_value: SemanticValue::High,
            is_negated: false,
            correction: Some(correction),
            content: format!("纠正：{}", corrected),
            created_at: timestamp.to_string(),
        };

        // 直接推入热区（不走 add_message，避免重复分流）
        self.hot.push_back(correction_msg);
    }

    /// 在热区中查找消息并标记为反向知识，返回原始内容
    fn find_and_mark_negated(&mut self, uuid: &str) -> Option<String> {
        for msg in &mut self.hot {
            if msg.uuid == uuid {
                msg.is_negated = true;
                return Some(msg.content.clone());
            }
        }
        None
    }

    /// 在温区中查找消息并标记为反向知识，返回原始内容
    fn find_and_mark_negated_in_warm(&mut self, uuid: &str) -> Option<String> {
        for msg in &mut self.warm {
            if msg.uuid == uuid {
                msg.is_negated = true;
                return Some(msg.content.clone());
            }
        }
        None
    }

    /// 分类消息的语义价值
    ///
    /// 基于关键词匹配判断消息的语义价值等级：
    /// - 决策关键词（"决定"、"选择"、"确认"、"必须"、"不能"）→ High
    /// - 纠正关键词（"不对"、"错了"、"应该是"、"不是这样"）→ High
    /// - 闲聊关键词（"嗯"、"好"、"谢谢"）→ Low
    /// - 其他 → Medium
    pub fn classify_message(content: &str) -> SemanticValue {
        // 纠正类关键词优先级最高：即使短消息也标记为高价值
        for keyword in CORRECTION_KEYWORDS {
            if content.contains(keyword) {
                return SemanticValue::High;
            }
        }

        // 决策类关键词：表示关键选择和约束
        for keyword in DECISION_KEYWORDS {
            if content.contains(keyword) {
                return SemanticValue::High;
            }
        }

        // 闲聊类关键词：仅当内容很短时才判为低价值
        // 避免将包含"好"字的正常技术讨论误判为低价值
        for keyword in CHITCHAT_KEYWORDS {
            if content.contains(keyword) && content.len() <= 10 {
                return SemanticValue::Low;
            }
        }

        // 其他内容默认为中价值
        SemanticValue::Medium
    }

    /// 注入冷区记忆：按语义相关性选择最相关的条目注入热区
    ///
    /// 从提供的冷区记忆中选择最多 cold_inject_limit 条注入热区，
    /// 优先选择高语义价值的记忆。注入后用完回收释放空间。
    pub fn inject_cold_memories(&mut self, memories: Vec<ContextMessage>) {
        if memories.is_empty() {
            return;
        }

        // 按语义价值降序排序，优先注入高价值记忆
        let mut sorted = memories;
        sorted.sort_by(|a, b| b.semantic_value.cmp(&a.semantic_value));

        // 选择最多 cold_inject_limit 条注入
        let limit = self.config.cold_inject_limit.min(sorted.len());
        for mem in sorted.into_iter().take(limit) {
            let mut cold_msg = mem;
            cold_msg.zone = ContextZone::Hot;
            self.hot.push_back(cold_msg);
        }

        // 热区可能溢出，执行分流
        while self.hot.len() > self.config.hot_window_size {
            if let Some(old) = self.hot.pop_front() {
                match old.semantic_value {
                    SemanticValue::High | SemanticValue::Medium => {
                        self.push_to_warm(old);
                    }
                    SemanticValue::Low => {}
                }
            }
        }
    }

    /// 获取当前上下文窗口：热区 + 温区的完整上下文
    ///
    /// 返回热区消息（zone=Hot）和温区消息（zone=Warm，可能 is_negated=true），
    /// 供 LLM 使用。冷区不直接暴露，需通过 inject_cold_memories 按需注入。
    pub fn get_context_window(&self) -> Vec<&ContextMessage> {
        let mut result = Vec::with_capacity(self.hot.len() + self.warm.len());

        // 先温区再热区，保证热区（最新对话）在上下文末尾
        for msg in &self.warm {
            result.push(msg);
        }
        for msg in &self.hot {
            result.push(msg);
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 辅助函数：创建测试用消息
    fn make_msg(uuid: &str, content: &str, zone: ContextZone, value: SemanticValue) -> ContextMessage {
        ContextMessage {
            uuid: uuid.to_string(),
            zone,
            semantic_value: value,
            is_negated: false,
            correction: None,
            content: content.to_string(),
            created_at: "2025-01-01T00:00:00".to_string(),
        }
    }

    #[test]
    fn 新消息进入热区() {
        let config = SlidingWindowConfig::default();
        let mut mgr = ContextManager::new(config);

        let msg = make_msg("1", "测试消息", ContextZone::Hot, SemanticValue::Medium);
        mgr.add_message(msg);

        let window = mgr.get_context_window();
        assert_eq!(window.len(), 1);
        assert_eq!(window[0].uuid, "1");
        assert_eq!(window[0].zone, ContextZone::Hot);
    }

    #[test]
    fn 热区溢出低价值消息丢弃() {
        let config = SlidingWindowConfig {
            hot_window_size: 2,
            ..Default::default()
        };
        let mut mgr = ContextManager::new(config);

        // 添加 3 条低价值消息，热区容量为 2
        mgr.add_message(make_msg("1", "嗯", ContextZone::Hot, SemanticValue::Low));
        mgr.add_message(make_msg("2", "好", ContextZone::Hot, SemanticValue::Low));
        mgr.add_message(make_msg("3", "谢谢", ContextZone::Hot, SemanticValue::Low));

        // 热区保留最新 2 条，第 1 条低价值被丢弃
        let window = mgr.get_context_window();
        assert_eq!(window.len(), 2);
        assert_eq!(window[0].uuid, "2");
        assert_eq!(window[1].uuid, "3");
    }

    #[test]
    fn 热区溢出高价值消息进入温区() {
        let config = SlidingWindowConfig {
            hot_window_size: 2,
            warm_max_summaries: 10,
            ..Default::default()
        };
        let mut mgr = ContextManager::new(config);

        // 添加 3 条高价值消息
        mgr.add_message(make_msg("1", "决定使用 Rust", ContextZone::Hot, SemanticValue::High));
        mgr.add_message(make_msg("2", "确认方案 A", ContextZone::Hot, SemanticValue::High));
        mgr.add_message(make_msg("3", "必须使用异步", ContextZone::Hot, SemanticValue::High));

        let window = mgr.get_context_window();
        // 温区 1 条 + 热区 2 条
        assert_eq!(window.len(), 3);
        assert_eq!(window[0].zone, ContextZone::Warm);
        assert_eq!(window[0].uuid, "1");
        assert_eq!(window[1].zone, ContextZone::Hot);
        assert_eq!(window[2].zone, ContextZone::Hot);
    }

    #[test]
    fn 语义分类决策为高价值() {
        assert_eq!(ContextManager::classify_message("我们决定使用 PostgreSQL"), SemanticValue::High);
        assert_eq!(ContextManager::classify_message("选择方案 B"), SemanticValue::High);
        assert_eq!(ContextManager::classify_message("确认无误"), SemanticValue::High);
        assert_eq!(ContextManager::classify_message("必须完成"), SemanticValue::High);
        assert_eq!(ContextManager::classify_message("不能删除"), SemanticValue::High);
    }

    #[test]
    fn 语义分类纠正为高价值() {
        assert_eq!(ContextManager::classify_message("不对，应该是 A"), SemanticValue::High);
        assert_eq!(ContextManager::classify_message("你错了"), SemanticValue::High);
        assert_eq!(ContextManager::classify_message("应该是这样"), SemanticValue::High);
        assert_eq!(ContextManager::classify_message("不是这样做的"), SemanticValue::High);
    }

    #[test]
    fn 语义分类闲聊为低价值() {
        assert_eq!(ContextManager::classify_message("嗯"), SemanticValue::Low);
        assert_eq!(ContextManager::classify_message("好"), SemanticValue::Low);
        assert_eq!(ContextManager::classify_message("谢谢"), SemanticValue::Low);
    }

    #[test]
    fn 语义分类长消息不为低价值() {
        // 包含"好"但较长的消息不应被判为低价值
        assert_eq!(ContextManager::classify_message("好的，我来检查一下代码逻辑"), SemanticValue::Medium);
    }

    #[test]
    fn 语义分类普通为中价值() {
        assert_eq!(ContextManager::classify_message("请读取文件内容"), SemanticValue::Medium);
        assert_eq!(ContextManager::classify_message("查看当前状态"), SemanticValue::Medium);
    }

    #[test]
    fn 标记纠正() {
        let config = SlidingWindowConfig::default();
        let mut mgr = ContextManager::new(config);

        mgr.add_message(make_msg("1", "使用 MySQL", ContextZone::Hot, SemanticValue::High));
        mgr.mark_correction("1", "使用 PostgreSQL", "MySQL 不支持需要的特性", "2025-01-01T01:00:00");

        // 原始消息应被标记为反向知识
        let original = mgr.hot.front().expect("原始消息应在热区");
        assert!(original.is_negated);
        assert_eq!(original.uuid, "1");

        // 应有纠正消息添加到热区
        assert_eq!(mgr.hot.len(), 2);
        let correction_msg = mgr.hot.back().expect("纠正消息在热区末尾");
        assert!(correction_msg.correction.is_some());
        assert_eq!(correction_msg.semantic_value, SemanticValue::High);
    }

    #[test]
    fn 冷区记忆注入() {
        let config = SlidingWindowConfig {
            hot_window_size: 10,
            cold_inject_limit: 2,
            ..Default::default()
        };
        let mut mgr = ContextManager::new(config);

        let cold_memories = vec![
            make_msg("c1", "冷区低价值", ContextZone::Cold, SemanticValue::Low),
            make_msg("c2", "冷区高价值", ContextZone::Cold, SemanticValue::High),
            make_msg("c3", "冷区中价值", ContextZone::Cold, SemanticValue::Medium),
        ];

        mgr.inject_cold_memories(cold_memories);

        // 按价值排序后只注入 2 条（High + Medium）
        let hot_uuids: Vec<&str> = mgr.hot.iter().map(|m| m.uuid.as_str()).collect();
        assert!(hot_uuids.contains(&"c2"));
        assert!(hot_uuids.contains(&"c3"));
        assert!(!hot_uuids.contains(&"c1"));
    }

    #[test]
    fn 温区溢出淘汰最老摘要() {
        let config = SlidingWindowConfig {
            hot_window_size: 1,
            warm_max_summaries: 2,
            ..Default::default()
        };
        let mut mgr = ContextManager::new(config);

        // 添加 4 条高价值消息，热区容量 1，温区容量 2
        mgr.add_message(make_msg("1", "决策一", ContextZone::Hot, SemanticValue::High));
        mgr.add_message(make_msg("2", "决策二", ContextZone::Hot, SemanticValue::High));
        mgr.add_message(make_msg("3", "决策三", ContextZone::Hot, SemanticValue::High));
        mgr.add_message(make_msg("4", "决策四", ContextZone::Hot, SemanticValue::High));

        // 温区最多 2 条，应保留 msg2 和 msg3（msg1 被淘汰）
        assert_eq!(mgr.warm.len(), 2);
        assert_eq!(mgr.warm[0].uuid, "2");
        assert_eq!(mgr.warm[1].uuid, "3");
    }

    #[test]
    fn 默认配置值() {
        let config = SlidingWindowConfig::default();
        assert_eq!(config.hot_window_size, 10);
        assert_eq!(config.warm_max_summaries, 20);
        assert_eq!(config.cold_inject_limit, 5);
    }
}
