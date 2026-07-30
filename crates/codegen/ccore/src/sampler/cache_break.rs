//! Prompt 缓存断裂检测（借鉴 Claude Code promptCacheBreakDetection.ts）
//!
//! 核心模式：
//! 1. 追踪缓存键变化（prompt缓存何时失效）
//! 2. 检测断裂原因（工具schema变化/系统提示变化等）
//! 3. 日志记录缓存命中率
//! 4. 追踪缓存创建 vs 缓存读取 token 数

use std::collections::HashMap;
use std::sync::RwLock;

/// 缓存断裂原因
#[derive(Debug, Clone, PartialEq)]
pub enum CacheBreakReason {
    /// 工具定义变化
    ToolSchemaChanged,
    /// 系统提示变化
    SystemPromptChanged,
    /// 对话历史增长
    ConversationGrowth,
    /// 上下文注入变化（heuristics等）
    ContextInjectionChanged,
    /// TTL过期（服务端）
    TtlExpired,
    /// 未知原因
    Unknown,
}

/// 缓存事件
#[derive(Debug, Clone)]
pub struct CacheEvent {
    /// 请求ID
    pub request_id: u64,
    /// 缓存命中
    pub cache_hit: bool,
    /// 缓存创建token数
    pub cache_creation_tokens: usize,
    /// 缓存读取token数
    pub cache_read_tokens: usize,
    /// 断裂原因（如果缓存断裂）
    pub break_reason: Option<CacheBreakReason>,
    /// 请求时的系统提示hash
    pub system_prompt_hash: u64,
    /// 请求时的工具定义hash
    pub tool_schema_hash: u64,
}

/// Prompt 缓存追踪器
pub struct PromptCacheTracker {
    /// 上次系统提示hash
    last_system_prompt_hash: RwLock<Option<u64>>,
    /// 上次工具定义hash
    last_tool_schema_hash: RwLock<Option<u64>>,
    /// 缓存事件历史
    events: RwLock<Vec<CacheEvent>>,
    /// 累计统计
    total_hits: RwLock<u64>,
    total_misses: RwLock<u64>,
    total_cache_creation_tokens: RwLock<usize>,
    total_cache_read_tokens: RwLock<usize>,
    /// 断裂原因统计
    break_reasons: RwLock<HashMap<String, u64>>,
}

impl PromptCacheTracker {
    pub fn new() -> Self {
        Self {
            last_system_prompt_hash: RwLock::new(None),
            last_tool_schema_hash: RwLock::new(None),
            events: RwLock::new(Vec::new()),
            total_hits: RwLock::new(0),
            total_misses: RwLock::new(0),
            total_cache_creation_tokens: RwLock::new(0),
            total_cache_read_tokens: RwLock::new(0),
            break_reasons: RwLock::new(HashMap::new()),
        }
    }

    /// 记录缓存事件
    pub fn record(&self, event: CacheEvent) {
        // 检测断裂原因
        let mut reason = event.break_reason.clone();

        if reason.is_none() && !event.cache_hit {
            // 自动检测断裂原因
            let last_sys = self.last_system_prompt_hash.read().unwrap();
            let last_tool = self.last_tool_schema_hash.read().unwrap();

            if let Some(last) = *last_sys {
                if last != event.system_prompt_hash {
                    reason = Some(CacheBreakReason::SystemPromptChanged);
                }
            }
            if reason.is_none() {
                if let Some(last) = *last_tool {
                    if last != event.tool_schema_hash {
                        reason = Some(CacheBreakReason::ToolSchemaChanged);
                    }
                }
            }
            if reason.is_none() {
                reason = Some(CacheBreakReason::ConversationGrowth);
            }
        }

        // 更新hash
        {
            let mut last_sys = self.last_system_prompt_hash.write().unwrap();
            *last_sys = Some(event.system_prompt_hash);
        }
        {
            let mut last_tool = self.last_tool_schema_hash.write().unwrap();
            *last_tool = Some(event.tool_schema_hash);
        }

        // 更新统计
        if event.cache_hit {
            *self.total_hits.write().unwrap() += 1;
        } else {
            *self.total_misses.write().unwrap() += 1;
        }
        *self.total_cache_creation_tokens.write().unwrap() += event.cache_creation_tokens;
        *self.total_cache_read_tokens.write().unwrap() += event.cache_read_tokens;

        if let Some(ref r) = reason {
            let key = format!("{:?}", r);
            *self.break_reasons.write().unwrap().entry(key).or_insert(0) += 1;
        }

        // 日志
        if event.cache_hit {
            tracing::debug!(
                target: "ccore::cache",
                cache_read_tokens = event.cache_read_tokens,
                "prompt cache HIT"
            );
        } else {
            tracing::debug!(
                target: "ccore::cache",
                reason = ?reason,
                cache_creation_tokens = event.cache_creation_tokens,
                "prompt cache MISS"
            );
        }

        // 存储事件
        let mut stored = event;
        stored.break_reason = reason;
        self.events.write().unwrap().push(stored);

        // 限制事件历史大小
        let mut events = self.events.write().unwrap();
        if events.len() > 100 {
            events.drain(0..50);
        }
    }

    /// 获取缓存命中率
    pub fn hit_rate(&self) -> f64 {
        let hits = *self.total_hits.read().unwrap();
        let misses = *self.total_misses.read().unwrap();
        let total = hits + misses;
        if total == 0 { return 0.0; }
        hits as f64 / total as f64
    }

    /// 获取缓存节省的token数
    pub fn tokens_saved(&self) -> usize {
        // 缓存读取的token即节省的token
        *self.total_cache_read_tokens.read().unwrap()
    }

    /// 获取统计信息
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            total_hits: *self.total_hits.read().unwrap(),
            total_misses: *self.total_misses.read().unwrap(),
            hit_rate: self.hit_rate(),
            total_cache_creation_tokens: *self.total_cache_creation_tokens.read().unwrap(),
            total_cache_read_tokens: *self.total_cache_read_tokens.read().unwrap(),
            tokens_saved: self.tokens_saved(),
            break_reasons: self.break_reasons.read().unwrap().clone(),
        }
    }

    /// 简单hash函数
    pub fn simple_hash(s: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut hasher);
        hasher.finish()
    }
}

/// 缓存统计
#[derive(Debug, Clone)]
pub struct CacheStats {
    pub total_hits: u64,
    pub total_misses: u64,
    pub hit_rate: f64,
    pub total_cache_creation_tokens: usize,
    pub total_cache_read_tokens: usize,
    pub tokens_saved: usize,
    pub break_reasons: HashMap<String, u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_tracker_new() {
        let tracker = PromptCacheTracker::new();
        let stats = tracker.stats();
        assert_eq!(stats.total_hits, 0);
        assert_eq!(stats.total_misses, 0);
        assert_eq!(stats.hit_rate, 0.0);
    }

    #[test]
    fn test_cache_hit() {
        let tracker = PromptCacheTracker::new();
        tracker.record(CacheEvent {
            request_id: 1,
            cache_hit: true,
            cache_creation_tokens: 0,
            cache_read_tokens: 5000,
            break_reason: None,
            system_prompt_hash: 123,
            tool_schema_hash: 456,
        });
        let stats = tracker.stats();
        assert_eq!(stats.total_hits, 1);
        assert_eq!(stats.total_misses, 0);
        assert_eq!(stats.tokens_saved, 5000);
    }

    #[test]
    fn test_cache_miss_with_reason_detection() {
        let tracker = PromptCacheTracker::new();

        // First request sets baseline
        tracker.record(CacheEvent {
            request_id: 1,
            cache_hit: false,
            cache_creation_tokens: 10000,
            cache_read_tokens: 0,
            break_reason: None,
            system_prompt_hash: 100,
            tool_schema_hash: 200,
        });

        // Second request with different system prompt hash
        tracker.record(CacheEvent {
            request_id: 2,
            cache_hit: false,
            cache_creation_tokens: 8000,
            cache_read_tokens: 0,
            break_reason: None,
            system_prompt_hash: 999, // changed
            tool_schema_hash: 200,
        });

        let stats = tracker.stats();
        assert_eq!(stats.total_misses, 2);
        assert!(stats.break_reasons.contains_key("SystemPromptChanged"));
    }

    #[test]
    fn test_hit_rate() {
        let tracker = PromptCacheTracker::new();

        for i in 0..3 {
            tracker.record(CacheEvent {
                request_id: i,
                cache_hit: true,
                cache_creation_tokens: 0,
                cache_read_tokens: 100,
                break_reason: None,
                system_prompt_hash: 1,
                tool_schema_hash: 1,
            });
        }
        tracker.record(CacheEvent {
            request_id: 3,
            cache_hit: false,
            cache_creation_tokens: 5000,
            cache_read_tokens: 0,
            break_reason: None,
            system_prompt_hash: 1,
            tool_schema_hash: 1,
        });

        let stats = tracker.stats();
        assert!((stats.hit_rate - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_simple_hash() {
        let h1 = PromptCacheTracker::simple_hash("hello");
        let h2 = PromptCacheTracker::simple_hash("hello");
        let h3 = PromptCacheTracker::simple_hash("world");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_event_history_bounded() {
        let tracker = PromptCacheTracker::new();
        for i in 0..120 {
            tracker.record(CacheEvent {
                request_id: i,
                cache_hit: true,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                break_reason: None,
                system_prompt_hash: 1,
                tool_schema_hash: 1,
            });
        }
        let events = tracker.events.read().unwrap();
        assert!(events.len() <= 100);
    }
}
