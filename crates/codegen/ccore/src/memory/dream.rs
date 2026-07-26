//! Dream 整理 - Agent 空闲时自动整理 L1→L2 记忆
//!
//! 从 L1 短期记忆中提取冷条目，总结为知识条目存入 L2 长期记忆，
//! 整理后从 L1 中移除原条目，释放短期记忆空间

use std::collections::HashSet;

use crate::memory::long_term::{KnowledgeCategory, KnowledgeEntry, LongTermMemory};
use crate::memory::short_term::ShortTermMemory;

/// Dream 整理器配置
#[derive(Debug, Clone)]
pub struct DreamConfig {
    /// 冷条目热度阈值，低于此值的条目被视为冷条目
    pub heat_threshold: f64,
    /// 触发整理的最小冷条目数量
    pub min_cold_entries: usize,
    /// 时间衰减系数 λ，用于简化热度计算
    pub decay_lambda: f64,
}

impl Default for DreamConfig {
    fn default() -> Self {
        Self {
            heat_threshold: 0.3,
            min_cold_entries: 5,
            decay_lambda: 0.1,
        }
    }
}

/// Dream 整理器
pub struct DreamOrganizer {
    /// 整理配置
    config: DreamConfig,
}

impl DreamOrganizer {
    /// 使用默认配置创建
    pub fn new() -> Self {
        Self {
            config: DreamConfig::default(),
        }
    }

    /// 使用自定义配置创建
    pub fn with_config(config: DreamConfig) -> Self {
        Self { config }
    }

    /// 执行一轮 Dream 整理
    ///
    /// 1. 从 L1 短期记忆中提取冷条目（heat_score < 阈值）
    /// 2. 若冷条目数 ≥ min_cold_entries，触发整理
    /// 3. 将冷条目总结为 KnowledgeEntry 并存入 L2
    /// 4. 从 L1 中移除已整理的条目
    pub async fn run(
        &self,
        short_term: &mut ShortTermMemory,
        long_term: &LongTermMemory,
    ) -> anyhow::Result<DreamResult> {
        let current_turn = short_term.current_turn();
        let all_entries = short_term.all_entries();

        // 计算每条条目的简化热度，筛选冷条目
        let cold_ids: Vec<String> = all_entries
            .iter()
            .filter(|entry| {
                let heat = self.compute_simple_heat(entry, current_turn);
                heat < self.config.heat_threshold
            })
            .map(|entry| entry.id.clone())
            .collect();

        // 冷条目数量不足，不触发整理
        if cold_ids.len() < self.config.min_cold_entries {
            return Ok(DreamResult {
                consolidated: 0,
                removed_from_l1: 0,
            });
        }

        // 构建 HashSet 用于 O(1) 查找
        let cold_id_set: HashSet<String> = cold_ids.iter().cloned().collect();

        // 将冷条目转化为知识条目存入 L2
        let mut consolidated = 0usize;
        let now = chrono::Utc::now().to_rfc3339();

        for entry in all_entries.iter() {
            if !cold_id_set.contains(&entry.id) {
                continue;
            }

            let knowledge = KnowledgeEntry {
                id: uuid::Uuid::new_v4().to_string(),
                category: Self::infer_category(&entry.role, &entry.content),
                content: entry.content.clone(),
                embedding: entry.embedding.clone(),
                metadata: serde_json::json!({
                    "source_id": entry.id,
                    "source_turn": entry.turn,
                    "source_role": entry.role,
                    "recall_count": entry.recall_count,
                }),
                created_at: now.clone(),
                updated_at: now.clone(),
            };

            long_term.store(knowledge).await?;
            consolidated += 1;
        }

        // 从 L1 中移除已整理的冷条目
        let removed = short_term.remove_entries(&cold_ids);

        Ok(DreamResult {
            consolidated,
            removed_from_l1: removed,
        })
    }

    /// 计算简化热度（不依赖完整 HeatInput，基于 recency + activity）
    ///
    /// heat = 0.6 * e^(-λ * elapsed_turns) + 0.4 * min(1.0, recall_count / 5.0)
    fn compute_simple_heat(
        &self,
        entry: &crate::memory::short_term::ShortTermEntry,
        current_turn: u32,
    ) -> f64 {
        let elapsed = current_turn.saturating_sub(entry.turn);
        let recency = (-self.config.decay_lambda * elapsed as f64).exp();
        let activity = (entry.recall_count as f64 / 5.0).min(1.0);
        0.6 * recency + 0.4 * activity
    }

    /// 根据条目的角色和内容推断知识分类
    fn infer_category(_role: &str, content: &str) -> KnowledgeCategory {
        let lower = content.to_lowercase();
        if lower.contains("架构") || lower.contains("architecture") || lower.contains("模块") {
            KnowledgeCategory::Architecture
        } else if lower.contains("决定") || lower.contains("决策") || lower.contains("选择")
            || lower.contains("decision")
        {
            KnowledgeCategory::Decision
        } else if lower.contains("偏好") || lower.contains("习惯") || lower.contains("preference") {
            KnowledgeCategory::UserPreference
        } else if lower.contains("修复") || lower.contains("bug") || lower.contains("错误")
            || lower.contains("error")
        {
            KnowledgeCategory::ErrorFix
        } else {
            KnowledgeCategory::CodePattern
        }
    }
}

impl Default for DreamOrganizer {
    fn default() -> Self {
        Self::new()
    }
}

/// Dream 整理结果
#[derive(Debug)]
pub struct DreamResult {
    /// 整理并存入 L2 的条目数
    pub consolidated: usize,
    /// 从 L1 中移除的条目数
    pub removed_from_l1: usize,
}
