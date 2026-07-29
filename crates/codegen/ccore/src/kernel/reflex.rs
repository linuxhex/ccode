//! 反射路由器 - 脊髓反射弧的实现
//!
//! 模拟人体脊髓反射弧：感官信号 → 模式匹配 → 运动指令
//! L0（反射）：确定性高，不经 LLM，直接触发动作
//! L1（本能）：需简单判断，不经 LLM 但通知 ThinkerNode
//! L2（思考）：不确定性高，必须经 LLM，ReflexRouter 不处理
//!
//! 规则演化：L1_trial（需确认）→ L1_formal（自动执行）→ L0（纯反射）
//! 失败回退：consecutive_fails >= 2 → 禁用规则，升级到 L2

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{info, warn};

/// 反射级别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReflexLevel {
    /// L0 反射：确定性高，不经 LLM，直接触发动作
    L0,
    /// L1 试验：从 L2 经历中提取的新规则，需 ThinkerNode 确认 3 次后升级
    L1Trial,
    /// L1 正式：确认后的本能规则，自动执行但通知 ThinkerNode
    L1Formal,
}

impl std::fmt::Display for ReflexLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::L0 => write!(f, "L0"),
            Self::L1Trial => write!(f, "L1_trial"),
            Self::L1Formal => write!(f, "L1_formal"),
        }
    }
}

/// 反射规则
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflexRule {
    /// 规则唯一 ID
    pub id: String,
    /// 信号匹配模式（正则表达式，匹配信号内容）
    pub pattern: String,
    /// 触发的信号 topic 前缀（如 "nose/compile_error"）
    pub signal_topic: String,
    /// 反射级别
    pub level: ReflexLevel,
    /// 触发的运动 topic（如 "hand/edit"）
    pub action: String,
    /// 运动参数
    pub params: serde_json::Value,
    /// 规则来源：manual（人工编写）/ learned（从经历中学习）/ evolved（从 L1 升级）
    #[serde(default = "default_source")]
    pub source: String,
    /// 使用次数
    #[serde(default)]
    pub use_count: u32,
    /// 成功次数
    #[serde(default)]
    pub success_count: u32,
    /// 连续失败次数
    #[serde(default)]
    pub consecutive_fails: u32,
    /// 是否已禁用
    #[serde(default)]
    pub disabled: bool,
}

fn default_source() -> String {
    "manual".to_string()
}

/// 反射动作：ReflexRouter 匹配信号后返回的动作
#[derive(Debug, Clone)]
pub enum ReflexAction {
    /// L0 直接反射：不经 LLM，直接发运动指令
    Direct {
        action: String,
        params: serde_json::Value,
    },
    /// L1 正式本能：发运动指令 + 通知 ThinkerNode
    Instinct {
        action: String,
        params: serde_json::Value,
    },
    /// L1 试验本能：发运动指令 + 需 ThinkerNode 确认
    Trial {
        action: String,
        params: serde_json::Value,
    },
}

/// 反射路由器
///
/// 维护反射规则库，匹配感官信号并返回对应的运动指令。
/// 规则按信号 topic 前缀索引，匹配时只检查对应 topic 下的规则。
pub struct ReflexRouter {
    /// 规则库：signal_topic_prefix → 规则列表
    rules: HashMap<String, Vec<ReflexRule>>,
    /// 已编译的正则：rule_id → regex::Regex
    #[allow(dead_code)]
    compiled_patterns: HashMap<String, regex::Regex>,
}

impl ReflexRouter {
    /// 创建空的反射路由器
    pub fn new() -> Self {
        Self {
            rules: HashMap::new(),
            compiled_patterns: HashMap::new(),
        }
    }

    /// 从预置规则列表创建反射路由器
    pub fn with_rules(rules: Vec<ReflexRule>) -> Self {
        let mut router = Self::new();
        for rule in rules {
            router.add_rule(rule);
        }
        router
    }

    /// 添加规则
    pub fn add_rule(&mut self, rule: ReflexRule) {
        // 编译正则
        if let Ok(re) = regex::Regex::new(&rule.pattern) {
            self.compiled_patterns.insert(rule.id.clone(), re);
        } else {
            warn!(rule_id = %rule.id, pattern = %rule.pattern, "反射规则正则编译失败，跳过");
            return;
        }

        let key = rule.signal_topic.clone();
        self.rules.entry(key).or_default().push(rule);
    }

    /// 匹配感官信号，返回对应的反射动作
    ///
    /// 匹配策略（优先级从高到低）：
    /// 1. 精确匹配：signal_topic == rule.signal_topic
    /// 2. 前缀匹配：signal_topic 以 rule.signal_topic + "/" 开头
    ///    （如 sensory/eye/read 匹配 sensory/eye 规则）
    /// 3. 通配符匹配：rule.signal_topic 以 * 结尾，前缀匹配
    pub fn route(&self, signal_topic: &str, signal_payload: &str) -> Option<ReflexAction> {
        // 先精确匹配 topic
        if let Some(rules) = self.rules.get(signal_topic) {
            for rule in rules {
                if rule.disabled {
                    continue;
                }
                if let Some(re) = self.compiled_patterns.get(&rule.id) {
                    if re.is_match(signal_payload) {
                        return Some(match rule.level {
                            ReflexLevel::L0 => ReflexAction::Direct {
                                action: rule.action.clone(),
                                params: rule.params.clone(),
                            },
                            ReflexLevel::L1Formal => ReflexAction::Instinct {
                                action: rule.action.clone(),
                                params: rule.params.clone(),
                            },
                            ReflexLevel::L1Trial => ReflexAction::Trial {
                                action: rule.action.clone(),
                                params: rule.params.clone(),
                            },
                        });
                    }
                }
            }
        }

        // 前缀匹配：sensory/eye/read 匹配 sensory/eye 规则
        for (topic_key, rules) in &self.rules {
            if signal_topic.starts_with(&format!("{}/", topic_key)) {
                for rule in rules {
                    if rule.disabled {
                        continue;
                    }
                    if let Some(re) = self.compiled_patterns.get(&rule.id) {
                        if re.is_match(signal_payload) {
                            return Some(match rule.level {
                                ReflexLevel::L0 => ReflexAction::Direct {
                                    action: rule.action.clone(),
                                    params: rule.params.clone(),
                                },
                                ReflexLevel::L1Formal => ReflexAction::Instinct {
                                    action: rule.action.clone(),
                                    params: rule.params.clone(),
                                },
                                ReflexLevel::L1Trial => ReflexAction::Trial {
                                    action: rule.action.clone(),
                                    params: rule.params.clone(),
                                },
                            });
                        }
                    }
                }
            }
        }

        // 回退：用通配符 "nose/*" 等前缀匹配
        for (topic_prefix, rules) in &self.rules {
            if topic_prefix.ends_with('*') && signal_topic.starts_with(&topic_prefix[..topic_prefix.len() - 1]) {
                for rule in rules {
                    if rule.disabled {
                        continue;
                    }
                    if let Some(re) = self.compiled_patterns.get(&rule.id) {
                        if re.is_match(signal_payload) {
                            return Some(match rule.level {
                                ReflexLevel::L0 => ReflexAction::Direct {
                                    action: rule.action.clone(),
                                    params: rule.params.clone(),
                                },
                                ReflexLevel::L1Formal => ReflexAction::Instinct {
                                    action: rule.action.clone(),
                                    params: rule.params.clone(),
                                },
                                ReflexLevel::L1Trial => ReflexAction::Trial {
                                    action: rule.action.clone(),
                                    params: rule.params.clone(),
                                },
                            });
                        }
                    }
                }
            }
        }

        None
    }

    /// 记录规则执行结果，更新计数器并执行规则演化
    ///
    /// 返回 true 表示规则发生了级别变更（升级或降级）
    pub fn record_result(&mut self, rule_id: &str, success: bool) -> bool {
        let mut evolved = false;
        for rules in self.rules.values_mut() {
            for rule in rules {
                if rule.id == rule_id {
                    rule.use_count += 1;
                    if success {
                        rule.success_count += 1;
                        rule.consecutive_fails = 0;
                    } else {
                        rule.consecutive_fails += 1;
                    }

                    // 降级：连续失败 >= 2 → 禁用规则，升级到 L2
                    if rule.consecutive_fails >= 2 && rule.level != ReflexLevel::L0 {
                        rule.disabled = true;
                        warn!(
                            rule_id = %rule.id,
                            consecutive_fails = rule.consecutive_fails,
                            "反射规则连续失败，已禁用并升级到 L2"
                        );
                        evolved = true;
                    }

                    // 升级：L1_formal → L0：必须 100% 成功率且使用 >= 20 次
                    // 设计原则：除非 100% 把握，否则都要经过 LLM
                    // 只有经过充分验证、从未失败的规则才能升级为 L0（不经 LLM 直接执行）
                    if rule.level == ReflexLevel::L1Formal
                        && rule.use_count >= 20
                        && rule.success_count == rule.use_count  // 100% 成功率
                    {
                        // 额外安全检查：只有非代码修改类规则才能升级到 L0
                        // 代码修改类规则（action 含 hand/edit、limb/execute 等）永远不应成为 L0
                        let is_code_modification = rule.action.starts_with("hand/")
                            || rule.action.starts_with("limb/")
                            || rule.action.starts_with("mouth/");
                        if !is_code_modification {
                            rule.level = ReflexLevel::L0;
                            rule.source = "evolved".to_string();
                            info!(rule_id = %rule.id, "L1_formal 规则 100% 成功率且使用 >= 20 次，升级为 L0 反射");
                            evolved = true;
                        } else {
                            info!(rule_id = %rule.id, "代码修改类规则不允许升级为 L0，保持 L1_formal");
                        }
                    }

                    // 升级：L1_trial → L1_formal：使用 >= 10 次且 100% 成功
                    // 设计原则：升级门槛必须极高，不能让不确定的规则自动执行
                    if rule.level == ReflexLevel::L1Trial
                        && rule.use_count >= 10
                        && rule.success_count == rule.use_count  // 100% 成功率
                    {
                        rule.level = ReflexLevel::L1Formal;
                        info!(rule_id = %rule.id, "L1_trial 规则 100% 成功率且使用 >= 10 次，升级为 L1_formal");
                        evolved = true;
                    }

                    return evolved;
                }
            }
        }
        warn!(rule_id = %rule_id, "记录规则结果时未找到规则");
        false
    }

    /// 提议新规则（从经历回放中提取）
    ///
    /// 新规则标记为 L1_trial，需要 ThinkerNode 确认 3 次后才能升级
    pub fn propose_rule(
        &mut self,
        id: String,
        pattern: String,
        signal_topic: String,
        action: String,
        params: serde_json::Value,
    ) {
        let rule = ReflexRule {
            id,
            pattern,
            signal_topic,
            level: ReflexLevel::L1Trial,
            action,
            params,
            source: "learned".to_string(),
            use_count: 0,
            success_count: 0,
            consecutive_fails: 0,
            disabled: false,
        };
        self.add_rule(rule);
        info!("从经历中学习到新规则，已添加为 L1_trial");
    }

    /// 获取所有规则（用于持久化和状态查询）
    pub fn all_rules(&self) -> Vec<&ReflexRule> {
        self.rules.values().flatten().collect()
    }

    /// 获取指定 topic 下的规则
    pub fn rules_for_topic(&self, topic: &str) -> Vec<&ReflexRule> {
        self.rules.get(topic).map(|r| r.iter().collect()).unwrap_or_default()
    }

    /// 统计信息
    pub fn stats(&self) -> ReflexRouterStats {
        let all: Vec<&ReflexRule> = self.all_rules();
        ReflexRouterStats {
            total_rules: all.len(),
            l0_count: all.iter().filter(|r| r.level == ReflexLevel::L0).count(),
            l1_formal_count: all.iter().filter(|r| r.level == ReflexLevel::L1Formal).count(),
            l1_trial_count: all.iter().filter(|r| r.level == ReflexLevel::L1Trial).count(),
            disabled_count: all.iter().filter(|r| r.disabled).count(),
        }
    }
}

/// 反射路由器统计信息
#[derive(Debug, Clone)]
pub struct ReflexRouterStats {
    pub total_rules: usize,
    pub l0_count: usize,
    pub l1_formal_count: usize,
    pub l1_trial_count: usize,
    pub disabled_count: usize,
}

/// 创建预置反射规则
///
/// 出厂自带的"肌肉记忆"：仅包含 100% 确定性的动作。
///
/// 设计原则：**除非 100% 把握，否则都要经过 LLM。**
/// - L0 反射：仅限纯通知/日志类动作（不改代码，100% 无副作用）
/// - 代码修改类操作：全部走 L2（经 ThinkerNode/LLM 判断）
/// - 从经历中学习的规则：初始为 L1_trial，需经 LLM 确认多次后才能升级
///
/// 路线 A 说明：感官处理（Eye/Ear/Nose/Skin）已内置到 ThinkerNode，
/// ReflexRouter 不再路由到独立器官 Node，而是：
/// - L0：直接记录/通知（Kernel 内部动作，不经 LLM）
/// - 无匹配：升级到 L2（ThinkerNode/LLM 处理）
/// - 经验学习：从 L2 成功经历中提取规则，逐步升级
pub fn builtin_reflex_rules() -> Vec<ReflexRule> {
    vec![
        // ── L0 反射：纯通知/日志（100% 确定，无副作用） ──

        // 文件变化 → 记录日志（仅通知，不改代码）
        // ThinkerNode.observe() 处理后通过 sensory/eye/* 通知 Kernel
        ReflexRule {
            id: "log_file_change".into(),
            pattern: r".*".into(),
            signal_topic: "sensory/eye".into(),
            level: ReflexLevel::L0,
            action: "kernel/log".into(),
            params: serde_json::json!({
                "action": "log_file_change",
                "level": "debug"
            }),
            source: "manual".into(),
            use_count: 0,
            success_count: 0,
            consecutive_fails: 0,
            disabled: false,
        },
        // 编译错误 → 记录日志（仅通知，不改代码）
        // ThinkerNode.sniff() 处理后通过 sensory/nose/compile_error 通知 Kernel
        ReflexRule {
            id: "log_compile_error".into(),
            pattern: r".*".into(),
            signal_topic: "sensory/nose/compile_error".into(),
            level: ReflexLevel::L0,
            action: "kernel/log".into(),
            params: serde_json::json!({
                "action": "log_compile_error",
                "level": "warn"
            }),
            source: "manual".into(),
            use_count: 0,
            success_count: 0,
            consecutive_fails: 0,
            disabled: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_rules_load() {
        let router = ReflexRouter::with_rules(builtin_reflex_rules());
        let stats = router.stats();
        assert!(stats.l0_count >= 2, "应有至少 2 条 L0 预置规则（sensory/eye + sensory/nose）");
        // L0 规则不应包含代码修改类动作
        for rule in router.all_rules() {
            if rule.level == ReflexLevel::L0 {
                let is_code_modification = rule.action.starts_with("hand/")
                    || rule.action.starts_with("limb/")
                    || rule.action.starts_with("mouth/");
                assert!(!is_code_modification, "L0 规则不应包含代码修改类动作：{}", rule.action);
            }
        }
    }

    #[test]
    fn test_route_sensory_eye() {
        let router = ReflexRouter::with_rules(builtin_reflex_rules());
        let action = router.route(
            "sensory/eye/read",
            r#"{"tool_name": "Read", "agent_id": "test"}"#,
        );
        assert!(action.is_some());
        match action {
            Some(ReflexAction::Direct { action, .. }) => {
                assert_eq!(action, "kernel/log"); // 纯日志通知
            }
            _ => panic!("L0 规则应返回 Direct"),
        }
    }

    #[test]
    fn test_route_sensory_nose() {
        let router = ReflexRouter::with_rules(builtin_reflex_rules());
        let action = router.route(
            "sensory/nose/compile_error",
            r#"{"error_count": 3, "errors": ["error[E0277]: mismatched types"]}"#,
        );
        assert!(action.is_some());
        match action {
            Some(ReflexAction::Direct { action, .. }) => {
                assert_eq!(action, "kernel/log"); // 纯日志通知
            }
            _ => panic!("L0 规则应返回 Direct"),
        }
    }

    #[test]
    fn test_code_modification_escalates_to_l2() {
        let router = ReflexRouter::with_rules(builtin_reflex_rules());
        // 编译错误中 "expected `;`" 不再匹配 L0 规则，应返回 None → 升级到 L2
        let action = router.route(
            "nose/compile_error",
            "error: expected `;`",
        );
        // 代码修改类信号无 L0/L1 匹配 → 必须走 L2（LLM）
        assert!(action.is_none(), "代码修改类信号必须走 L2（经 LLM），不应有 L0/L1 反射");
    }

    #[test]
    fn test_route_unknown_signal() {
        let router = ReflexRouter::with_rules(builtin_reflex_rules());
        let action = router.route(
            "nose/compile_error",
            "error: some complex error we don't have a rule for",
        );
        // 无匹配 → 返回 None，应升级到 L2
        assert!(action.is_none());
    }

    #[test]
    fn test_record_result_evolution_l1_trial_to_formal() {
        let mut router = ReflexRouter::with_rules(builtin_reflex_rules());
        // 学习一条纯观察类规则
        router.propose_rule(
            "learned_observation".into(),
            r"test_pattern".into(),
            "nose/compile_error".into(),
            "nose/smell".into(), // 纯观察动作
            serde_json::json!({"action": "test"}),
        );

        // L1_trial → L1_formal 需要 10 次且 100% 成功
        for _ in 0..9 {
            router.record_result("learned_observation", true);
        }
        let rule = router.all_rules().into_iter().find(|r| r.id == "learned_observation").expect("规则应存在");
        assert_eq!(rule.level, ReflexLevel::L1Trial, "9 次成功还不够升级");

        // 第 10 次
        router.record_result("learned_observation", true);
        let rule = router.all_rules().into_iter().find(|r| r.id == "learned_observation").expect("规则应存在");
        assert_eq!(rule.level, ReflexLevel::L1Formal, "10 次 100% 成功应升级为 L1_formal");
    }

    #[test]
    fn test_code_modification_rule_never_becomes_l0() {
        let mut router = ReflexRouter::with_rules(builtin_reflex_rules());
        // 学习一条代码修改类规则
        router.propose_rule(
            "learned_code_fix".into(),
            r"test_pattern".into(),
            "nose/compile_error".into(),
            "hand/edit".into(), // 代码修改动作
            serde_json::json!({"fix_type": "test"}),
        );

        // 先升级到 L1_formal（10 次成功）
        for _ in 0..10 {
            router.record_result("learned_code_fix", true);
        }
        let rule = router.all_rules().into_iter().find(|r| r.id == "learned_code_fix").expect("规则应存在");
        assert_eq!(rule.level, ReflexLevel::L1Formal);

        // 即使 20 次 100% 成功，代码修改类规则也不应升级到 L0
        for _ in 0..10 {
            router.record_result("learned_code_fix", true);
        }
        let rule = router.all_rules().into_iter().find(|r| r.id == "learned_code_fix").expect("规则应存在");
        assert_eq!(rule.level, ReflexLevel::L1Formal, "代码修改类规则永远不应升级为 L0");
    }

    #[test]
    fn test_record_result_disable_on_failure() {
        let mut router = ReflexRouter::with_rules(builtin_reflex_rules());
        // 添加一条测试规则
        router.propose_rule(
            "test_rule".into(),
            r"test".into(),
            "nose/compile_error".into(),
            "nose/smell".into(),
            serde_json::json!({}),
        );

        // 连续失败 2 次 → 禁用
        router.record_result("test_rule", false);
        router.record_result("test_rule", false);

        let rule = router.all_rules().into_iter().find(|r| r.id == "test_rule").expect("规则应存在");
        assert!(rule.disabled);
    }
}
