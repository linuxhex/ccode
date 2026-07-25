//! 记忆系统配置

use serde::{Deserialize, Serialize};

/// 记忆系统配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// L0 最大 token 数
    pub l0_max_tokens: u32,
    /// L1 向量库类型
    pub l1_backend: L1Backend,
    /// L2 持久化路径
    pub l2_path: String,
    /// 冷热评分权重
    pub heat_weights: HeatWeightsConfig,
    /// 冷热阈值
    pub heat_thresholds: HeatThresholdsConfig,
    /// 是否启用 Dream 整理
    pub dream_enabled: bool,
    /// Dream 整理间隔（秒）
    pub dream_interval_secs: u64,
}

/// L1 向量库后端
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum L1Backend {
    /// 内存级 hora HNSW
    Hora,
}

/// 冷热评分权重配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeatWeightsConfig {
    pub recency: f64,
    pub relevance: f64,
    pub activity: f64,
    pub tool_weight: f64,
    pub decay_lambda: f64,
}

/// 冷热阈值配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeatThresholdsConfig {
    pub warm: f64,
    pub cold: f64,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            l0_max_tokens: 128_000,
            l1_backend: L1Backend::Hora,
            l2_path: "~/.ccode/memory".into(),
            heat_weights: HeatWeightsConfig {
                recency: 0.4,
                relevance: 0.3,
                activity: 0.15,
                tool_weight: 0.15,
                decay_lambda: 0.1,
            },
            heat_thresholds: HeatThresholdsConfig {
                warm: 0.4,
                cold: 0.2,
            },
            dream_enabled: true,
            dream_interval_secs: 300,
        }
    }
}
