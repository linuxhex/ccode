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
            model_degrade_level: 0,
        }
    }

    /// 调整重复阈值（保留历史计数，不重建实例）
    pub fn set_repeat_threshold(&mut self, threshold: usize) {
        self.repeat_threshold = threshold;
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
}
