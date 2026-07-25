//! 冷热评分算法
//!
//! heat = w1 * recency + w2 * relevance + w3 * activity + w4 * tool_weight

use serde::{Deserialize, Serialize};

/// 冷热评分权重配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeatWeights {
    /// 时间衰减权重
    pub recency: f64,
    /// 语义相似度权重
    pub relevance: f64,
    /// 活跃度（被召回次数）权重
    pub activity: f64,
    /// 工具调用结果权重
    pub tool_weight: f64,
    /// 时间衰减系数 λ
    pub decay_lambda: f64,
}

impl Default for HeatWeights {
    fn default() -> Self {
        Self {
            recency: 0.4,
            relevance: 0.3,
            activity: 0.15,
            tool_weight: 0.15,
            decay_lambda: 0.1,
        }
    }
}

/// 单条消息的冷热评分输入
#[derive(Debug, Clone, PartialEq)]
pub struct HeatInput {
    /// 距当前轮次的距离
    pub elapsed_turns: u32,
    /// 与当前任务的语义相似度 (0.0 - 1.0)
    pub relevance: f64,
    /// 被召回次数
    pub recall_count: u32,
    /// 是否为工具调用结果
    pub is_tool_result: bool,
    /// 工具结果权重系数（文件内容高，bash 输出中，纯对话低）
    pub tool_importance: f64,
}

/// 计算冷热评分
pub fn compute_heat(input: &HeatInput, weights: &HeatWeights) -> f64 {
    // 时间衰减：越近的消息越热
    let recency_score = (-weights.decay_lambda * input.elapsed_turns as f64).exp();

    // 语义相似度
    let relevance_score = input.relevance;

    // 活跃度：被召回次数越多越热，使用对数平滑
    let activity_score = (1.0 + input.recall_count as f64).ln();

    // 工具权重：工具调用结果通常比纯对话更重要
    let tool_score = if input.is_tool_result {
        input.tool_importance
    } else {
        0.3 // 纯对话的基础权重
    };

    weights.recency * recency_score
        + weights.relevance * relevance_score
        + weights.activity * activity_score
        + weights.tool_weight * tool_score
}

/// 冷热阈值
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeatThresholds {
    /// 低于此值为冷消息
    pub cold: f64,
    /// 低于此值为温消息
    pub warm: f64,
}

impl Default for HeatThresholds {
    fn default() -> Self {
        Self {
            warm: 0.4,
            cold: 0.2,
        }
    }
}

/// 消息温度等级
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Temperature {
    Hot,
    Warm,
    Cold,
}

/// 根据热度评分判断温度等级
pub fn classify(heat: f64, thresholds: &HeatThresholds) -> Temperature {
    if heat >= thresholds.warm {
        Temperature::Hot
    } else if heat >= thresholds.cold {
        Temperature::Warm
    } else {
        Temperature::Cold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recent_message_is_hot() {
        let input = HeatInput {
            elapsed_turns: 0,
            relevance: 0.8,
            recall_count: 0,
            is_tool_result: false,
            tool_importance: 0.0,
        };
        let heat = compute_heat(&input, &HeatWeights::default());
        assert!(heat > 0.5, "最近的消息应该是热的，实际: {}", heat);
    }

    #[test]
    fn test_old_message_is_cold() {
        let input = HeatInput {
            elapsed_turns: 50,
            relevance: 0.1,
            recall_count: 0,
            is_tool_result: false,
            tool_importance: 0.0,
        };
        let heat = compute_heat(&input, &HeatWeights::default());
        assert!(heat < 0.3, "很旧的消息应该是冷的，实际: {}", heat);
    }

    #[test]
    fn test_tool_result_is_hotter() {
        let weights = HeatWeights::default();
        let base = HeatInput {
            elapsed_turns: 10,
            relevance: 0.5,
            recall_count: 0,
            is_tool_result: false,
            tool_importance: 0.0,
        };
        let tool = HeatInput {
            is_tool_result: true,
            tool_importance: 0.8,
            ..base.clone()
        };
        assert!(compute_heat(&tool, &weights) > compute_heat(&base, &weights));
    }
}
