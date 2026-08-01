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
//!    ```text
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

    /// 获取最近执行结果的摘要文本
    pub fn recent_outcome_summary(&self) -> String {
        self.entries.iter().rev().take(5)
            .filter_map(|e| if e.result { Some("success") } else { Some("failure") })
            .collect::<Vec<_>>()
            .join(",")
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

    // -----------------------------------------------------------------------
    // LLM 模式提取
    // -----------------------------------------------------------------------

    /// 构建 LLM 模式提取 prompt
    ///
    /// 将最近的经验条目发送给 LLM，让 LLM 识别深层模式，
    /// 超越简单的 (topic, action) 统计。
    pub fn build_pattern_extraction_prompt(&self, recent_n: usize) -> Option<String> {
        let entries = self.recent_entries(recent_n);
        if entries.len() < 3 {
            return None;
        }

        let entry_text = entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                format!(
                    "[{}] signal={}, topic={}, action={}, result={}",
                    i,
                    e.signal,
                    e.signal_topic,
                    e.action,
                    if e.result { "ok" } else { "fail" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        Some(format!(
            r#"Analyze the following experience entries and identify deeper patterns that simple statistics might miss.

Look for:
1. Sequential patterns (action A often leads to action B)
2. Conditional patterns (when signal contains X, action Y works better)
3. Contextual patterns (certain conditions correlate with success/failure)

Experience entries:
{}

Output patterns as JSON array:
```json
[
  {{"pattern": "description", "condition": "when/where", "action": "what to do", "confidence": 0.8}}
]
```"#,
            entry_text
        ))
    }

    /// 解析 LLM 模式提取响应
    pub fn parse_pattern_extraction_response(response: &str) -> Vec<ProposedRule> {
        let json_str = extract_json_array(response);

        #[derive(Deserialize)]
        struct PatternItem {
            pattern: String,
            condition: String,
            action: String,
            confidence: f64,
        }

        match serde_json::from_str::<Vec<PatternItem>>(&json_str) {
            Ok(items) => items
                .into_iter()
                .filter(|item| item.confidence >= 0.7)
                .map(|item| ProposedRule {
                    signal_topic: item.condition,
                    pattern_hint: item.pattern.chars().take(100).collect(),
                    action: item.action,
                    success_rate: item.confidence,
                    sample_count: 0, // LLM extracted, no direct sample count
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    // -----------------------------------------------------------------------
    // 与 auto_extract 的集成
    // -----------------------------------------------------------------------

    /// 将经验条目转换为可提取知识的消息列表
    ///
    /// 将最近的失败经历转换为消息，供 KnowledgeExtractor 提取。
    pub fn failed_experiences_as_messages(&self, n: usize) -> Vec<String> {
        self.entries
            .iter()
            .rev()
            .filter(|e| !e.result)
            .take(n)
            .map(|e| {
                format!(
                    "Signal: {} | Topic: {} | Action: {} | Result: FAILED",
                    e.signal, e.signal_topic, e.action
                )
            })
            .collect()
    }
}

impl Default for ExperienceLog {
    fn default() -> Self {
        Self::new()
    }
}

/// 从 LLM 响应中提取 JSON 数组
fn extract_json_array(response: &str) -> String {
    if let Some(start) = response.find('[') {
        if let Some(end) = response.rfind(']') {
            if end > start {
                return response[start..=end].to_string();
            }
        }
    }
    response.to_string()
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

    // =======================================================================
    // LLM 模式提取测试
    // =======================================================================

    #[test]
    fn test_build_pattern_extraction_prompt_too_few_entries() {
        let mut log = ExperienceLog::new();
        log.record(make_entry("s1", "t", "a", true));
        log.record(make_entry("s2", "t", "a", true));
        assert!(log.build_pattern_extraction_prompt(10).is_none());
    }

    #[test]
    fn test_build_pattern_extraction_prompt_enough_entries() {
        let mut log = ExperienceLog::new();
        for i in 0..5 {
            log.record(make_entry(&format!("sig_{}", i), "nose/compile_error", "hand/edit", i % 2 == 0));
        }
        let prompt = log.build_pattern_extraction_prompt(10);
        assert!(prompt.is_some());
        let prompt = prompt.unwrap();
        assert!(prompt.contains("Experience entries:"));
        assert!(prompt.contains("[0]"));
        assert!(prompt.contains("[4]"));
        assert!(prompt.contains("nose/compile_error"));
    }

    #[test]
    fn test_build_pattern_extraction_prompt_respects_recent_n() {
        let mut log = ExperienceLog::new();
        for i in 0..10 {
            log.record(make_entry(&format!("sig_{}", i), "t", "a", true));
        }
        let prompt = log.build_pattern_extraction_prompt(5).unwrap();
        // 应只包含最近 5 条（编号 [0]-[4]）
        assert!(prompt.contains("[4]"));
        assert!(!prompt.contains("[5]"));
    }

    #[test]
    fn test_parse_pattern_extraction_response_valid() {
        let response = r#"
Here are the patterns:

```json
[
  {"pattern": "compile errors after edit", "condition": "when signal contains expected", "action": "hand/fix_semicolon", "confidence": 0.85},
  {"pattern": "type errors need cast", "condition": "when signal contains mismatched types", "action": "hand/type_cast", "confidence": 0.75}
]
```
"#;
        let rules = ExperienceLog::parse_pattern_extraction_response(response);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].signal_topic, "when signal contains expected");
        assert_eq!(rules[0].action, "hand/fix_semicolon");
        assert!((rules[0].success_rate - 0.85).abs() < f64::EPSILON);
        assert_eq!(rules[0].sample_count, 0);
        assert_eq!(rules[1].signal_topic, "when signal contains mismatched types");
    }

    #[test]
    fn test_parse_pattern_extraction_response_filters_low_confidence() {
        let response = r#"[{"pattern": "weak pattern", "condition": "cond", "action": "act", "confidence": 0.5}]"#;
        let rules = ExperienceLog::parse_pattern_extraction_response(response);
        assert!(rules.is_empty(), "置信度低于 0.7 的模式应被过滤");
    }

    #[test]
    fn test_parse_pattern_extraction_response_invalid_json() {
        let rules = ExperienceLog::parse_pattern_extraction_response("not json at all");
        assert!(rules.is_empty());
    }

    #[test]
    fn test_parse_pattern_extraction_response_truncates_long_pattern() {
        let long_pattern: String = "x".repeat(200);
        let response = format!(
            r#"[{{"pattern": "{}", "condition": "cond", "action": "act", "confidence": 0.8}}]"#,
            long_pattern
        );
        let rules = ExperienceLog::parse_pattern_extraction_response(&response);
        assert_eq!(rules.len(), 1);
        assert!(rules[0].pattern_hint.len() <= 100);
    }

    // =======================================================================
    // failed_experiences_as_messages 测试
    // =======================================================================

    #[test]
    fn test_failed_experiences_as_messages_empty() {
        let log = ExperienceLog::new();
        let msgs = log.failed_experiences_as_messages(5);
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_failed_experiences_as_messages_filters_success() {
        let mut log = ExperienceLog::new();
        log.record(make_entry("err1", "nose/compile_error", "hand/edit", true));
        log.record(make_entry("err2", "nose/compile_error", "hand/edit", false));
        log.record(make_entry("err3", "nose/type_error", "hand/cast", false));
        log.record(make_entry("err4", "nose/test_fail", "hand/fix", true));

        let msgs = log.failed_experiences_as_messages(5);
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].contains("FAILED"));
        assert!(msgs[0].contains("err3") || msgs[0].contains("err2"));
    }

    #[test]
    fn test_failed_experiences_as_messages_respects_n() {
        let mut log = ExperienceLog::new();
        for i in 0..5 {
            log.record(make_entry(&format!("fail_{}", i), "nose/err", "hand/edit", false));
        }
        let msgs = log.failed_experiences_as_messages(2);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn test_failed_experiences_as_messages_format() {
        let mut log = ExperienceLog::new();
        log.record(make_entry("expected `;`", "nose/compile_error", "hand/edit", false));
        let msgs = log.failed_experiences_as_messages(1);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("Signal: expected `;`"));
        assert!(msgs[0].contains("Topic: nose/compile_error"));
        assert!(msgs[0].contains("Action: hand/edit"));
        assert!(msgs[0].contains("FAILED"));
    }

    // =======================================================================
    // extract_json_array 测试
    // =======================================================================

    #[test]
    fn test_extract_json_array_brackets() {
        let result = extract_json_array("text [1,2,3] end");
        assert_eq!(result, "[1,2,3]");
    }

    #[test]
    fn test_extract_json_array_no_brackets() {
        let result = extract_json_array("no json here");
        assert_eq!(result, "no json here");
    }
}
