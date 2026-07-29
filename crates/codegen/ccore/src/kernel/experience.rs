//! 经历记录与回放 - 闭环学习中的经验积累
//!
//! 经历回放（Experience Replay）是闭环学习的核心环节，扮演"记忆"的角色：
//!
//! 1. **记录**：每次反射弧执行后，记录 (信号, 动作, 结果) 三元组，
//!    相当于人体将每次反射的经历存入小脑记忆。
//!
//! 2. **提取**：定期扫描历史经历，按 (signal_topic, action) 分组统计成功率，
//!    当同一模式成功次数 ≥ 3 时，提议为 ProposedRule（提议规则），
//!    由 Kernel 调用 ReflexRouter::propose_rule 注入 L1_trial。
//!
//! 3. **闭环**：ProposedRule → L1_trial → L1_formal → L0，
//!    构成从"思考"到"反射"的完整演化路径：
//!    ```
//!    L2（思考，需 LLM）
//!      ↓ 经历回放提取模式
//!    ProposedRule（提议规则）
//!      ↓ propose_rule 注入
//!    L1_trial（试验本能，需确认）
//!      ↓ 连续成功 3 次
//!    L1_formal（正式本能）
//!      ↓ 成功率 > 95% 且使用 ≥ 10 次
//!    L0（纯反射，不经 LLM）
//!    ```
//!
//! 与 ReflexRouter 的关系：
//! - ExperienceLog 不直接调用 ReflexRouter，仅产出 ProposedRule
//! - Kernel 在集成时负责调用 propose_rule，将提议注入反射路由器
//! - 这样保持了模块间的松耦合

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::reflex::ReflexLevel;

/// 经历条目：记录一次反射弧执行的完整信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperienceEntry {
    /// 时间戳
    pub timestamp: DateTime<Utc>,
    /// 感官信号内容
    pub signal: String,
    /// 信号 topic（如 "nose/compile_error"）
    pub signal_topic: String,
    /// 反射级别（L0/L1_trial/L1_formal；L2 时记录当时的级别）
    pub level: ReflexLevel,
    /// 执行的动作
    pub action: String,
    /// 成功/失败
    pub result: bool,
    /// 上下文快照
    pub context: serde_json::Value,
}

/// 提议规则：从经历中提取的可学习模式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedRule {
    /// 信号 topic
    pub signal_topic: String,
    /// 从信号中提取的模式提示（如 "expected `;`" 的前 50 个字符）
    pub pattern_hint: String,
    /// 执行的动作
    pub action: String,
    /// 成功率
    pub success_rate: f64,
    /// 样本数
    pub sample_count: usize,
}

/// 经历日志：记录和回放反射弧的执行经历
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperienceLog {
    /// 经历条目列表
    entries: Vec<ExperienceEntry>,
    /// 最大条目数（超出时淘汰最旧的）
    max_entries: usize,
}

impl ExperienceLog {
    /// 创建空日志，默认最大 1000 条
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            max_entries: 1000,
        }
    }

    /// 记录一条经历，超出 max_entries 时移除最旧的
    pub fn record(&mut self, entry: ExperienceEntry) {
        if self.entries.len() >= self.max_entries {
            self.entries.remove(0);
        }
        self.entries.push(entry);
    }

    /// 扫描经历，提取可学习的模式
    ///
    /// 算法：
    /// 1. 按 (signal_topic, action) 分组
    /// 2. 每组中统计 success_count 和 total_count
    /// 3. success_count >= 3 且 total_count >= 3 → 提议为 ProposedRule
    /// 4. 按成功率降序排列
    pub fn extract_patterns(&self) -> Vec<ProposedRule> {
        // 按 (signal_topic, action) 分组
        let mut groups: HashMap<(String, String), Vec<&ExperienceEntry>> = HashMap::new();
        for entry in &self.entries {
            let key = (entry.signal_topic.clone(), entry.action.clone());
            groups.entry(key).or_default().push(entry);
        }

        let mut proposed: Vec<ProposedRule> = Vec::new();

        for ((signal_topic, action), entries) in groups {
            let total = entries.len();
            let success_count = entries.iter().filter(|e| e.result).count();

            // 成功次数 >= 3 且总次数 >= 3 → 提议
            if success_count >= 3 && total >= 3 {
                let success_rate = success_count as f64 / total as f64;

                // 从成功条目的信号中提取模式提示
                // 取第一条成功条目的信号前 50 个字符
                let pattern_hint = entries
                    .iter()
                    .filter(|e| e.result)
                    .next()
                    .map(|e| e.signal.chars().take(50).collect())
                    .unwrap_or_default();

                proposed.push(ProposedRule {
                    signal_topic,
                    pattern_hint,
                    action,
                    success_rate,
                    sample_count: total,
                });
            }
        }

        // 按成功率降序排列
        proposed.sort_by(|a, b| {
            b.success_rate
                .partial_cmp(&a.success_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        proposed
    }

    /// 最近 n 条经历
    pub fn recent_entries(&self, n: usize) -> Vec<&ExperienceEntry> {
        self.entries.iter().rev().take(n).collect()
    }

    /// 按 topic 过滤经历
    pub fn entries_for_topic(&self, topic: &str) -> Vec<&ExperienceEntry> {
        self.entries
            .iter()
            .filter(|e| e.signal_topic == topic)
            .collect()
    }

    /// 清空日志
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// 当前条目数
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 日志是否为空
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for ExperienceLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(signal: &str, topic: &str, action: &str, result: bool) -> ExperienceEntry {
        ExperienceEntry {
            timestamp: Utc::now(),
            signal: signal.to_string(),
            signal_topic: topic.to_string(),
            level: ReflexLevel::L0,
            action: action.to_string(),
            result,
            context: serde_json::json!({}),
        }
    }

    #[test]
    fn test_new_log_is_empty() {
        let log = ExperienceLog::new();
        assert!(log.is_empty());
        assert_eq!(log.len(), 0);
    }

    #[test]
    fn test_record_and_len() {
        let mut log = ExperienceLog::new();
        log.record(make_entry("err1", "nose/compile_error", "hand/edit", true));
        log.record(make_entry("err2", "nose/compile_error", "hand/edit", false));
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn test_max_entries_eviction() {
        let mut log = ExperienceLog::new();
        // max_entries 默认 1000，手动设小测试淘汰逻辑
        log.max_entries = 3;
        for i in 0..5 {
            log.record(make_entry(
                &format!("signal_{}", i),
                "nose/test",
                "hand/edit",
                true,
            ));
        }
        assert_eq!(log.len(), 3);
        // 最旧的 2 条应被淘汰
        assert_eq!(log.entries[0].signal, "signal_2");
    }

    #[test]
    fn test_extract_patterns() {
        let mut log = ExperienceLog::new();
        // 3 次成功，满足提取条件
        for _ in 0..3 {
            log.record(make_entry(
                "expected `;`",
                "nose/compile_error",
                "hand/edit",
                true,
            ));
        }
        // 1 次失败，不应单独成组
        log.record(make_entry(
            "mismatched types",
            "nose/compile_error",
            "hand/type_cast",
            false,
        ));

        let patterns = log.extract_patterns();
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].signal_topic, "nose/compile_error");
        assert_eq!(patterns[0].action, "hand/edit");
        assert!((patterns[0].success_rate - 1.0).abs() < f64::EPSILON);
        assert_eq!(patterns[0].sample_count, 3);
        assert_eq!(patterns[0].pattern_hint, "expected `;`");
    }

    #[test]
    fn test_extract_patterns_sorted_by_success_rate() {
        let mut log = ExperienceLog::new();
        // 组 A：3 成功 1 失败 → 75%
        for i in 0..4 {
            log.record(ExperienceEntry {
                timestamp: Utc::now(),
                signal: format!("sig_a_{}", i),
                signal_topic: "nose/a".to_string(),
                level: ReflexLevel::L0,
                action: "hand/a".to_string(),
                result: i < 3,
                context: serde_json::json!({}),
            });
        }
        // 组 B：3 成功 → 100%
        for _ in 0..3 {
            log.record(make_entry("sig_b", "nose/b", "hand/b", true));
        }

        let patterns = log.extract_patterns();
        assert_eq!(patterns.len(), 2);
        // 100% 应排在前面
        assert!(patterns[0].success_rate >= patterns[1].success_rate);
    }

    #[test]
    fn test_recent_entries() {
        let mut log = ExperienceLog::new();
        for i in 0..5 {
            log.record(make_entry(&format!("s{}", i), "t", "a", true));
        }
        let recent = log.recent_entries(3);
        assert_eq!(recent.len(), 3);
        // 最近 3 条应是倒序的 s4, s3, s2
        assert_eq!(recent[0].signal, "s4");
        assert_eq!(recent[1].signal, "s3");
        assert_eq!(recent[2].signal, "s2");
    }

    #[test]
    fn test_entries_for_topic() {
        let mut log = ExperienceLog::new();
        log.record(make_entry("s1", "nose/a", "hand/edit", true));
        log.record(make_entry("s2", "nose/b", "hand/edit", true));
        log.record(make_entry("s3", "nose/a", "hand/edit", false));

        let entries = log.entries_for_topic("nose/a");
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_clear() {
        let mut log = ExperienceLog::new();
        log.record(make_entry("s1", "t", "a", true));
        assert!(!log.is_empty());
        log.clear();
        assert!(log.is_empty());
    }

    #[test]
    fn test_extract_patterns_insufficient_success() {
        let mut log = ExperienceLog::new();
        // 只有 2 次成功，不满足 >= 3 的条件
        log.record(make_entry("s1", "nose/a", "hand/edit", true));
        log.record(make_entry("s2", "nose/a", "hand/edit", true));
        let patterns = log.extract_patterns();
        assert!(patterns.is_empty());
    }
}
