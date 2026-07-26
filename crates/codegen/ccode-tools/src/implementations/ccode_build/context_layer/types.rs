use serde::{Deserialize, Serialize};

/// 上下文区域：冷热分层的三级划分
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextZone {
    /// 热区：最近 N 轮对话，完整保留
    Hot,
    /// 温区：压缩摘要 + 关键决策 + 错误纠正标注
    Warm,
    /// 冷区：长期记忆，按需检索
    Cold,
}

/// 语义价值等级，决定消息在滑动窗口中的去留
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SemanticValue {
    /// 低价值：闲聊、试错、重复
    Low = 0,
    /// 中价值：普通对话、代码读取
    Medium = 1,
    /// 高价值：决策、纠正、约束
    High = 2,
}

/// 错误纠正标注：用户纠正/否定过的内容标记为反向知识
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrectedKnowledge {
    /// 原始错误说法
    pub original: String,
    /// 正确说法
    pub corrected: String,
    /// 纠正原因
    pub reason: String,
    /// 纠正时间
    pub timestamp: String,
}

/// 带上下文元数据的消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMessage {
    /// 消息唯一标识
    pub uuid: String,
    /// 所属区域
    pub zone: ContextZone,
    /// 语义价值
    pub semantic_value: SemanticValue,
    /// 是否为反向知识（已纠正/已否定）
    pub is_negated: bool,
    /// 纠正标注（如果此消息纠正了之前的错误）
    pub correction: Option<CorrectedKnowledge>,
    /// 消息内容
    pub content: String,
    /// 创建时间
    pub created_at: String,
}

/// 滑动窗口配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlidingWindowConfig {
    /// 热区窗口大小（轮次）
    pub hot_window_size: usize,
    /// 温区最大摘要数
    pub warm_max_summaries: usize,
    /// 冷区检索时注入的最大条目数
    pub cold_inject_limit: usize,
}

impl Default for SlidingWindowConfig {
    fn default() -> Self {
        Self {
            hot_window_size: 10,
            warm_max_summaries: 20,
            cold_inject_limit: 5,
        }
    }
}
