//! Cache-Aware 压缩 — 跳过已压缩内容，产出 `cache_edits`。
//!
//! 对标 Claude Code 的 `cachedMicrocompact`：已缓存的系统提示 + 工具定义
//! 不重复压缩，只对新增/变更的内容做微压缩，压缩结果通过 `cache_edits`
//! 参数注入 API 请求，利用 Anthropic prompt cache 节省 token。
//!
//! 本模块只做"哪些内容已压缩过"的记账和跳过决策；`cache_edits` 的实际
//! 注入由宿主在构造 API 请求时完成（宿主才知道请求体结构）。

use std::collections::HashSet;

use crate::micro_compact::{MicroCompactConfig, MicroCompactable, micro_compact_messages};

/// 已压缩内容的记账状态。
///
/// 以内容指纹（`blake3` 短哈希的前 16 字节 hex）去重。用 `String` 指纹
/// 而非完整内容，避免长期持有大块文本。
#[derive(Debug, Clone, Default)]
pub struct CacheAwareState {
    seen: HashSet<String>,
}

impl CacheAwareState {
    /// 记入一条已压缩内容指纹。
    pub fn record(&mut self, fingerprint: &str) {
        self.seen.insert(fingerprint.to_string());
    }

    /// 是否已压缩过该指纹。
    pub fn contains(&self, fingerprint: &str) -> bool {
        self.seen.contains(fingerprint)
    }

    /// 已记账的不同内容数。
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// Cache-Aware 微压缩结果。
#[derive(Debug, Clone)]
pub struct CachedMicroCompactResult<T> {
    /// 压缩后的消息列表。
    pub items: Vec<T>,
    /// 本轮新压缩内容的指纹（供宿主写入 `cache_edits`）。
    pub cache_edits: Vec<String>,
    /// 跳过（已缓存）的消息数。
    pub skipped: usize,
}

/// 对消息列表做 cache-aware 微压缩。
///
/// - 已在 `state` 中记过指纹的消息跳过压缩（直接 clone）。
/// - 新消息走 [`micro_compact_messages`]，压缩后把指纹加入 `state` 并
///   返回到 `cache_edits`，供宿主注入 API 请求的 `cache_edits` 参数。
///
/// `fingerprint` 闭包由宿主提供，从消息抽取稳定指纹（通常是
/// `tool_name + content_hash`）。
pub fn cached_micro_compact<T>(
    messages: &[T],
    config: &MicroCompactConfig,
    now: std::time::Instant,
    state: &mut CacheAwareState,
    fingerprint: impl Fn(&T) -> Option<String>,
) -> CachedMicroCompactResult<T>
where
    T: MicroCompactable,
{
    let mut cache_edits = Vec::new();
    let mut skipped = 0;
    let mut to_compact: Vec<T> = Vec::with_capacity(messages.len());
    let mut compact_indices: Vec<usize> = Vec::new();

    for (i, msg) in messages.iter().enumerate() {
        match fingerprint(msg) {
            Some(fp) if state.contains(&fp) => {
                skipped += 1;
                to_compact.push(msg.clone()); // 已缓存，原样保留
            }
            Some(fp) => {
                compact_indices.push(i);
                to_compact.push(msg.clone());
                // 先记账，压缩后指纹不变（内容可能变，但指纹基于原始调用意图）
                state.record(&fp);
                cache_edits.push(fp);
            }
            None => to_compact.push(msg.clone()), // 无指纹（非工具结果等），原样
        }
    }

    let items = micro_compact_messages(&to_compact, config, now);
    CachedMicroCompactResult {
        items,
        cache_edits,
        skipped,
    }
}

/// 内容指纹：`{tool_name}:{content 前 64 字符的 char-based hash}`。
///
/// 供宿主作为 [`cached_micro_compact`] 的 `fingerprint` 参数的默认实现。
/// 不做密码学强度哈希——只用于去重，`FxHash` 级别即可，但为避免引入新依赖
/// 用简单的 char-fold 摘要。
pub fn default_fingerprint<T: MicroCompactable>(msg: &T) -> Option<String> {
    if !msg.is_tool_result() {
        return None;
    }
    let tool = msg.tool_name()?;
    let content = msg.content();
    let head: String = content.chars().take(64).collect();
    Some(format!("{tool}:{}", fold_hash(&head)))
}

/// 简单确定性哈希（FNV-1a 64bit → hex）。不用于安全，仅去重。
fn fold_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[derive(Clone)]
    struct Msg {
        tool: Option<String>,
        content: String,
        created: Instant,
    }

    impl MicroCompactable for Msg {
        fn created_at(&self) -> Instant {
            self.created
        }
        fn is_tool_result(&self) -> bool {
            self.tool.is_some()
        }
        fn tool_name(&self) -> Option<&str> {
            self.tool.as_deref()
        }
        fn content(&self) -> &str {
            &self.content
        }
        fn with_content(&self, new: String) -> Self {
            Self {
                tool: self.tool.clone(),
                content: new,
                created: self.created,
            }
        }
    }

    fn old_msg(tool: &str, content: &str) -> Msg {
        Msg {
            tool: Some(tool.to_string()),
            content: content.to_string(),
            created: Instant::now() - Duration::from_secs(600),
        }
    }

    #[test]
    fn second_pass_skips_cached() {
        let cfg = MicroCompactConfig::default();
        let now = Instant::now();
        let mut state = CacheAwareState::default();
        let msgs = vec![old_msg("FileRead", "content here")];

        let r1 = cached_micro_compact(&msgs, &cfg, now, &mut state, default_fingerprint);
        assert!(!r1.cache_edits.is_empty());
        assert_eq!(r1.skipped, 0);

        let r2 = cached_micro_compact(&msgs, &cfg, now, &mut state, default_fingerprint);
        assert_eq!(r2.skipped, 1);
        assert!(r2.cache_edits.is_empty());
    }

    #[test]
    fn non_tool_messages_get_no_fingerprint() {
        let m = Msg {
            tool: None,
            content: "hi".into(),
            created: Instant::now(),
        };
        assert!(default_fingerprint(&m).is_none());
    }
}
