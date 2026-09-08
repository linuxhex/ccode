//! 字节稳定工具 schema — 确保工具定义跨请求字节级一致
//!
//! 借鉴 Claude Code 的 byte-stable tool schema 策略。工具定义的序列化必须
//! 在跨轮次间保持字节级一致，以最大化 prompt cache 命中率。
//!
//! 保证：
//! 1. 工具按名称字母序排列
//! 2. JSON 对象键按字母序排列（ccore 默认 BTreeMap）
//! 3. 无动态内容（时间戳、随机 ID 等）
//!
//! 提供：
//! - `canonical_tool_bytes()` — 生成字节稳定的工具定义序列化
//! - `tool_schema_hash()` — 计算工具 schema 指纹，用于检测变化
//! - `ToolStabilityTracker` — 追踪跨轮次的 schema 变化

use super::provider::ToolDefinition;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// 生成字节稳定的工具定义序列化。
///
/// 保证：
/// - 工具按 `name` 字母序排列
/// - 每个工具的字段按固定顺序：`name`, `description`, `input_schema`
/// - `parameters` 内的 JSON 键由 BTreeMap 保证字母序
pub fn canonical_tool_bytes(tools: &[ToolDefinition]) -> Vec<u8> {
    let mut sorted = tools.to_vec();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));

    // 手动构建确保字段顺序稳定
    let canonical: Vec<serde_json::Value> = sorted
        .iter()
        .map(|t| {
            serde_json::json!({
                "description": t.description,
                "input_schema": t.parameters,
                "name": t.name,
            })
        })
        .collect();

    serde_json::to_vec(&canonical).unwrap_or_default()
}

/// 计算工具 schema 的指纹（hash）。
///
/// 用于检测工具定义是否跨轮次变化。相同输入保证相同输出。
pub fn tool_schema_hash(tools: &[ToolDefinition]) -> u64 {
    let bytes = canonical_tool_bytes(tools);
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// 工具 schema 稳定性追踪器
///
/// 记录每次请求的工具 schema hash，检测变化并记录原因。
pub struct ToolStabilityTracker {
    last_hash: Option<u64>,
    last_tool_count: usize,
    change_count: u64,
    stable_count: u64,
}

impl Default for ToolStabilityTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolStabilityTracker {
    pub fn new() -> Self {
        Self {
            last_hash: None,
            last_tool_count: 0,
            change_count: 0,
            stable_count: 0,
        }
    }

    /// 记录一次请求的工具 schema。返回 `true` 如果 schema 与上次相同（稳定）。
    pub fn record(&mut self, tools: &[ToolDefinition]) -> bool {
        let hash = tool_schema_hash(tools);
        let count = tools.len();
        let is_stable = self.last_hash.is_none() || self.last_hash == Some(hash);

        if is_stable {
            self.stable_count += 1;
        } else {
            if self.last_hash.is_some() {
                self.change_count += 1;
                let reason = if count != self.last_tool_count {
                    format!("tool count changed ({} → {})", self.last_tool_count, count)
                } else {
                    "schema content changed (same count)".to_string()
                };
                tracing::debug!(
                    hash = %hash,
                    prev_hash = ?self.last_hash,
                    reason = %reason,
                    "tool schema changed — prompt cache will miss"
                );
            }
        }

        self.last_hash = Some(hash);
        self.last_tool_count = count;
        is_stable
    }

    /// 稳定性比率（0.0 ~ 1.0）
    pub fn stability_rate(&self) -> f64 {
        let total = self.stable_count + self.change_count;
        if total == 0 {
            return 1.0;
        }
        self.stable_count as f64 / total as f64
    }

    pub fn change_count(&self) -> u64 {
        self.change_count
    }

    pub fn stable_count(&self) -> u64 {
        self.stable_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, desc: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: desc.to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "input": {"type": "string"}
                }
            }),
        }
    }

    #[test]
    fn canonical_bytes_sorted_by_name() {
        let tools = vec![tool("zebra", "z"), tool("alpha", "a"), tool("middle", "m")];
        let bytes = canonical_tool_bytes(&tools);
        let s = String::from_utf8(bytes).unwrap();

        // "alpha" should appear before "middle" which appears before "zebra"
        let pos_alpha = s.find("\"alpha\"").unwrap();
        let pos_middle = s.find("\"middle\"").unwrap();
        let pos_zebra = s.find("\"zebra\"").unwrap();
        assert!(pos_alpha < pos_middle);
        assert!(pos_middle < pos_zebra);
    }

    #[test]
    fn canonical_bytes_deterministic() {
        let tools = vec![tool("bash", "run commands"), tool("read", "read files")];
        let b1 = canonical_tool_bytes(&tools);
        let b2 = canonical_tool_bytes(&tools);
        assert_eq!(b1, b2);
    }

    #[test]
    fn canonical_bytes_order_independent() {
        let tools_a = vec![tool("bash", "run"), tool("read", "read")];
        let tools_b = vec![tool("read", "read"), tool("bash", "run")];
        assert_eq!(canonical_tool_bytes(&tools_a), canonical_tool_bytes(&tools_b));
    }

    #[test]
    fn hash_stable_across_calls() {
        let tools = vec![tool("bash", "run"), tool("read", "read")];
        assert_eq!(tool_schema_hash(&tools), tool_schema_hash(&tools));
    }

    #[test]
    fn hash_changes_with_content() {
        let tools_a = vec![tool("bash", "run commands")];
        let tools_b = vec![tool("bash", "execute shell commands")];
        assert_ne!(tool_schema_hash(&tools_a), tool_schema_hash(&tools_b));
    }

    #[test]
    fn hash_changes_with_count() {
        let tools_a = vec![tool("bash", "run")];
        let tools_b = vec![tool("bash", "run"), tool("read", "read")];
        assert_ne!(tool_schema_hash(&tools_a), tool_schema_hash(&tools_b));
    }

    #[test]
    fn tracker_detects_stability() {
        let mut tracker = ToolStabilityTracker::new();
        let tools = vec![tool("bash", "run")];

        // First call: no previous, returns true (stable by default)
        assert!(tracker.record(&tools));
        assert!(tracker.record(&tools));
        assert!(tracker.record(&tools));

        assert_eq!(tracker.stable_count(), 3);
        assert_eq!(tracker.change_count(), 0);
        assert!((tracker.stability_rate() - 1.0).abs() < 0.01);
    }

    #[test]
    fn tracker_detects_changes() {
        let mut tracker = ToolStabilityTracker::new();
        let tools_a = vec![tool("bash", "run")];
        let tools_b = vec![tool("bash", "execute")];

        tracker.record(&tools_a); // first call, baseline
        assert!(!tracker.record(&tools_b)); // changed

        assert_eq!(tracker.stable_count(), 1);
        assert_eq!(tracker.change_count(), 1);
        assert!((tracker.stability_rate() - 0.5).abs() < 0.01);
    }

    #[test]
    fn tracker_detects_count_change() {
        let mut tracker = ToolStabilityTracker::new();
        let tools_a = vec![tool("bash", "run")];
        let tools_b = vec![tool("bash", "run"), tool("read", "read")];

        tracker.record(&tools_a);
        assert!(!tracker.record(&tools_b));
    }

    #[test]
    fn empty_tools_stable() {
        let tools: Vec<ToolDefinition> = vec![];
        let b1 = canonical_tool_bytes(&tools);
        let b2 = canonical_tool_bytes(&tools);
        assert_eq!(b1, b2);
        assert_eq!(String::from_utf8(b1).unwrap(), "[]");
    }
}
