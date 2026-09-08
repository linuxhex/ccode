//! Doom Loop 检测 - 检测 Agent 是否陷入重复循环
//!
//! 检测到循环后，不仅给出判定，还会生成一组"逃脱动作"供 Agent 节点执行，
//! 帮助 Agent 跳出重复执行：注入换策略提示、临时禁用重复工具、降级模型推理强度。
//!
//! 与 MetaCognitiveController 的集成：当检测到 doom loop 时，
//! 推荐策略变更为 ReflectiveExecution，帮助 Agent 跳出循环。

use std::collections::{HashMap, VecDeque};
use serde::{Deserialize, Serialize};

/// 工具调用的签名，用于检测重复
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ToolCallSignature {
    pub tool_name: String,
    pub args_hash: u64,
}

/// Doom Loop 逃脱动作
///
/// 检测到循环后，由检测器生成的一组应对动作。
/// Agent 节点按顺序应用这些动作，帮助 Agent 跳出重复循环。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EscapeAction {
    /// 注入提示到 agent 上下文，引导其换一种方法
    InjectHint(String),
    /// 禁用重复工具（仅下一轮禁用，非永久）
    DisableTool(String),
    /// 降级模型（reasoning_effort 降一级）
    DegradeModel,
}

/// Doom Loop 检测器
pub struct DoomLoopDetector {
    /// 最近的工具调用签名历史
    history: VecDeque<ToolCallSignature>,
    /// 检测窗口大小
    window_size: usize,
    /// 重复阈值：在窗口内同一签名出现 N 次则判定为 doom loop
    repeat_threshold: usize,
    /// 构造时的原始阈值，供 `restore_repeat_threshold` 恢复
    ///
    /// 反射弧 motor 的 `slow_down` 会临时收紧阈值，若无此字段就无法还原，
    /// 检测灵敏度会永久停留在收紧状态（单向棘轮）。
    default_repeat_threshold: usize,
    /// 模型降级等级（0=原级，1=降一级，2=降两级，3=已最低不再降级）
    model_degrade_level: u32,
}

/// Doom Loop 检测结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoomLoopResult {
    pub detected: bool,
    pub repeated_tool: Option<String>,
    pub repeat_count: usize,
    /// 逃脱动作列表（检测到循环时非空）
    pub escape_actions: Vec<EscapeAction>,
}

impl DoomLoopDetector {
    pub fn new(window_size: usize, repeat_threshold: usize) -> Self {
        Self {
            history: VecDeque::new(),
            window_size,
            repeat_threshold,
            default_repeat_threshold: repeat_threshold,
            model_degrade_level: 0,
        }
    }

    /// 调整重复阈值（保留历史计数，不重建实例）
    pub fn set_repeat_threshold(&mut self, threshold: usize) {
        self.repeat_threshold = threshold;
    }

    /// 恢复构造时的原始阈值
    ///
    /// 反射弧 motor 的 `slow_down` 会收紧阈值以提高灵敏度，逃脱成功后必须调用
    /// 本方法还原，否则灵敏度永久停留在收紧状态、正常操作也会被判定为循环。
    pub fn restore_repeat_threshold(&mut self) {
        self.repeat_threshold = self.default_repeat_threshold;
    }

    /// 当前重复阈值
    pub fn repeat_threshold(&self) -> usize {
        self.repeat_threshold
    }

    /// 记录一次工具调用
    pub fn record(&mut self, tool_name: String, args_hash: u64) {
        self.history.push_back(ToolCallSignature { tool_name, args_hash });
        // 只保留窗口大小的历史
        if self.history.len() > self.window_size {
            self.history.pop_front();
        }
    }

    /// 检测是否存在 doom loop
    ///
    /// 检测到循环时，会同时递增模型降级等级（上限 3）并生成对应的逃脱动作列表。
    /// 注意：该方法会修改内部降级等级，因此需要 `&mut self`。
    pub fn detect(&mut self) -> DoomLoopResult {
        if self.history.len() < self.repeat_threshold {
            return DoomLoopResult {
                detected: false,
                repeated_tool: None,
                repeat_count: 0,
                escape_actions: Vec::new(),
            };
        }

        // 统计窗口内每个签名出现的次数
        let mut counts: HashMap<ToolCallSignature, usize> = HashMap::new();
        for sig in &self.history {
            *counts.entry(sig.clone()).or_insert(0) += 1;
        }

        // 找到重复最多的签名
        if let Some((sig, count)) = counts.iter().max_by_key(|(_, c)| *c) {
            if *count >= self.repeat_threshold {
                // 检测到 doom loop：递增降级等级，上限 3（3 表示已最低，不再降级）
                if self.model_degrade_level < 3 {
                    self.model_degrade_level += 1;
                }
                let tool_name = sig.tool_name.clone();
                let repeat_count = *count;

                // 构建逃脱动作列表
                let mut escape_actions = Vec::new();

                // 1. 注入提示：告知 Agent 已陷入循环，要求换一种方法
                escape_actions.push(EscapeAction::InjectHint(format!(
                    "检测到 Doom Loop：工具 {} 重复调用 {} 次。请换一种方法，避免重复相同的操作。",
                    tool_name, repeat_count
                )));

                // 2. 禁用重复工具：下一轮不再允许调用该工具
                escape_actions.push(EscapeAction::DisableTool(tool_name.clone()));

                // 3. 降级模型：仅在尚未达到降级上限时触发
                //    level 1/2 对应实际降级，level 3 表示已最低、不再降级
                if self.model_degrade_level < 3 {
                    escape_actions.push(EscapeAction::DegradeModel);
                }

                // 4. 元认知策略建议：推荐切换到反思式执行策略
                //    当检测到 doom loop 时，推荐使用 ReflectiveExecution 策略
                //    帮助 Agent 跳出循环
                use super::meta_cognitive::{DifficultyLevel, ExecutionStrategy};
                let current_difficulty = DifficultyLevel::Complex;
                let new_strategy = ExecutionStrategy::ReflectiveExecution;
                tracing::info!(
                    target: "ccore::loop",
                    strategy = ?new_strategy,
                    difficulty = ?current_difficulty,
                    "recommending strategy change to escape doom loop"
                );

                return DoomLoopResult {
                    detected: true,
                    repeated_tool: Some(tool_name),
                    repeat_count,
                    escape_actions,
                };
            }
        }

        DoomLoopResult {
            detected: false,
            repeated_tool: None,
            repeat_count: 0,
            escape_actions: Vec::new(),
        }
    }

    /// 当前模型降级等级
    pub fn model_degrade_level(&self) -> u32 {
        self.model_degrade_level
    }

    /// 根据当前降级等级推导 reasoning_effort
    ///
    /// 映射关系：0=原级（None，由模型默认决定）、1=Medium(0.5)、2=Low(0.2)、3=Low(0.2，已最低)。
    /// 返回 None 表示不主动设置推理强度，沿用默认（High）。
    pub fn current_reasoning_effort(&self) -> Option<f64> {
        match self.model_degrade_level {
            0 => None,
            1 => Some(0.5),
            _ => Some(0.2),
        }
    }

    /// 重置检测器
    pub fn reset(&mut self) {
        self.history.clear();
    }

    /// 重置模型降级等级（逃脱成功后调用，恢复原级推理强度）
    ///
    /// 与 `reset()` 区别：`reset()` 清空历史签名（用于全新会话），
    /// `reset_degrade_level()` 仅恢复降级等级，保留检测历史。
    pub fn reset_degrade_level(&mut self) {
        self.model_degrade_level = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detector() -> DoomLoopDetector {
        // 与 ThinkerNode 生产配置一致：窗口 10，阈值 3
        DoomLoopDetector::new(10, 3)
    }

    #[test]
    fn detects_repeated_identical_calls() {
        let mut d = detector();
        for _ in 0..3 {
            d.record("read_file".into(), 42);
        }
        let r = d.detect();
        assert!(r.detected, "同签名重复达阈值应判定为 doom loop");
        assert_eq!(r.repeated_tool.as_deref(), Some("read_file"));
        assert_eq!(r.repeat_count, 3);
        assert!(!r.escape_actions.is_empty(), "应生成逃脱动作");
    }

    #[test]
    fn does_not_detect_below_threshold() {
        let mut d = detector();
        d.record("grep".into(), 1);
        d.record("grep".into(), 1);
        assert!(!d.detect().detected, "未达阈值不应判定");
    }

    /// 逃脱成功后必须清空历史，否则旧签名留在窗口内会立刻重新触发判定，
    /// 且每次判定都递增降级等级，把推理强度一路压到最低（误报自我强化）。
    #[test]
    fn reset_clears_history_so_escape_is_not_immediately_retriggered() {
        let mut d = detector();
        for _ in 0..3 {
            d.record("bash".into(), 7);
        }
        assert!(d.detect().detected);

        // 模拟 ThinkerNode 逃脱成功后的清理
        d.reset_degrade_level();
        d.reset();

        // 逃脱后再调一次同名同参工具，不应立刻又被判定为循环
        d.record("bash".into(), 7);
        assert!(
            !d.detect().detected,
            "reset 后窗口内只剩 1 条签名，不应重新触发判定"
        );
        assert_eq!(d.model_degrade_level(), 0, "降级等级应已恢复原级");
    }

    /// 降级等级封顶在 3，且 current_reasoning_effort 的映射随之收敛。
    #[test]
    fn degrade_level_caps_at_three() {
        let mut d = detector();
        for _ in 0..3 {
            d.record("edit".into(), 9);
        }
        assert_eq!(d.model_degrade_level(), 0);

        d.detect();
        assert_eq!(d.model_degrade_level(), 1);
        assert_eq!(d.current_reasoning_effort(), Some(0.5));

        d.detect();
        assert_eq!(d.model_degrade_level(), 2);
        assert_eq!(d.current_reasoning_effort(), Some(0.2));

        for _ in 0..5 {
            d.detect();
        }
        assert_eq!(d.model_degrade_level(), 3, "降级等级应封顶在 3");
        assert_eq!(d.current_reasoning_effort(), Some(0.2));
    }

    #[test]
    fn reset_degrade_level_keeps_history() {
        let mut d = detector();
        for _ in 0..3 {
            d.record("grep".into(), 5);
        }
        d.detect();
        d.reset_degrade_level();

        assert_eq!(d.model_degrade_level(), 0);
        // reset_degrade_level 只恢复降级等级，不动历史，因此仍能判定
        assert!(d.detect().detected, "reset_degrade_level 不应清空历史");
    }

    #[test]
    fn window_evicts_oldest_signatures() {
        let mut d = DoomLoopDetector::new(3, 3);
        d.record("a".into(), 1);
        // 后续 3 次不同签名把 "a" 挤出窗口
        d.record("b".into(), 2);
        d.record("c".into(), 3);
        d.record("d".into(), 4);
        let r = d.detect();
        assert!(!r.detected, "窗口内各签名仅 1 次，不应判定");
    }

    #[test]
    fn lowered_threshold_takes_effect() {
        let mut d = detector();
        // 反射弧 motor 减速会把阈值降到 2
        d.set_repeat_threshold(2);
        d.record("bash".into(), 1);
        d.record("bash".into(), 1);
        assert!(d.detect().detected, "阈值降至 2 后两次重复即应判定");
    }

    /// `slow_down` 收紧阈值后必须能还原，否则是单向棘轮——
    /// 灵敏度永久停留在收紧状态，正常操作也会被判定为循环。
    #[test]
    fn threshold_can_be_restored_after_slow_down() {
        let mut d = detector();
        assert_eq!(d.repeat_threshold(), 3, "构造时阈值应为 3");

        // 模拟反射弧 motor 的 slow_down
        d.set_repeat_threshold(2);
        assert_eq!(d.repeat_threshold(), 2);

        // 逃脱成功后还原
        d.restore_repeat_threshold();
        assert_eq!(d.repeat_threshold(), 3, "应还原为构造时的阈值");

        // 还原后两次重复不应再触发判定
        d.record("bash".into(), 1);
        d.record("bash".into(), 1);
        assert!(
            !d.detect().detected,
            "阈值还原为 3 后，两次重复不应判定为循环"
        );
    }

    /// 已知盲区（当前实现不覆盖，记录为待办而非断言失败）：
    /// 1. 参数微调绕过——args_hash 全等比较，offset 差 1 即视为不同签名
    /// 2. 多工具轮转——A→B→A→B 这类环只按单签名计数，识别不到模式重复
    #[test]
    fn known_blind_spot_param_tweak_evades_detection() {
        let mut d = detector();
        // 同一工具、参数仅微调，签名各不相同
        for h in 0..6u64 {
            d.record("read_file".into(), h);
        }
        assert!(
            !d.detect().detected,
            "当前实现无法识别参数微调型循环——此断言记录已知盲区，实现增强后应反转"
        );
    }
}

