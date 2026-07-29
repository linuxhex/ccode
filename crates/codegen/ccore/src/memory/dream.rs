//! Dream 整理 - Agent 空闲时自动整理 L1→L2 记忆
//!
//! 从 L1 短期记忆中提取冷条目，总结为知识条目存入 L2 长期记忆，
//! 整理后从 L1 中移除原条目，释放短期记忆空间
//!
//! 增强功能：
//! - PID 文件锁：防止多实例并发整理
//! - LLM 整合摘要：将多个冷条目整合为精简摘要
//! - 自动触发检测：检查是否应自动触发整理

use std::collections::HashSet;
use std::path::Path;

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

/// 整理锁（RAII，drop 时自动释放）
pub struct DreamLock {
    path: std::path::PathBuf,
}

impl Drop for DreamLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
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

    /// 尝试获取整理锁（PID 文件，防止多实例并发整理）
    ///
    /// 如果锁文件存在且持有进程仍活跃，返回 None（锁被占用）。
    /// 否则写入当前进程 PID 并返回 DreamLock（RAII 释放）。
    pub async fn try_acquire_lock(&self, lock_dir: &Path) -> anyhow::Result<Option<DreamLock>> {
        let lock_path = lock_dir.join("dream.lock");

        if lock_path.exists() {
            // 检查持有锁的进程是否仍活跃
            let content = tokio::fs::read_to_string(&lock_path).await?;
            if let Ok(pid) = content.trim().parse::<u32>() {
                // Unix: 使用 kill(pid, 0) 检查进程是否存活
                #[cfg(unix)]
                {
                    // SAFETY: kill(pid, 0) 不发送信号，仅检查进程存在性
                    if unsafe { libc::kill(pid as i32, 0) } == 0 {
                        return Ok(None); // 锁被活跃进程持有
                    }
                }
                #[cfg(not(unix))]
                {
                    // 非 Unix 平台：简单忽略，总是允许获取锁
                    let _ = pid;
                }
            }
        }

        // 写入当前进程 PID
        let pid = std::process::id();
        tokio::fs::create_dir_all(lock_dir).await?;
        tokio::fs::write(&lock_path, pid.to_string()).await?;
        Ok(Some(DreamLock {
            path: lock_path,
        }))
    }

    /// 使用 LLM 整合摘要（构建 prompt）
    ///
    /// 将多个冷条目构建为 LLM 整合 prompt，用于后续调用 LLM 生成精简摘要。
    pub fn build_consolidation_prompt(&self, entries: &[&str]) -> String {
        format!(
            r#"Consolidate the following memory entries into a concise summary.
Remove redundancy, keep unique information, and maintain the most important points.

Entries:
{}

Output a single consolidated markdown section:"#,
            entries
                .iter()
                .enumerate()
                .map(|(i, e)| format!("[{}] {}", i, e))
                .collect::<Vec<_>>()
                .join("\n\n")
        )
    }

    /// 解析 LLM 整合响应
    pub fn parse_consolidation_response(response: &str) -> String {
        response.trim().to_string()
    }

    /// 检查是否应该自动触发整理
    pub fn should_auto_trigger(&self, short_term: &ShortTermMemory) -> bool {
        let current_turn = short_term.current_turn();
        let all_entries = short_term.all_entries();

        let cold_count = all_entries
            .iter()
            .filter(|entry| {
                let heat = self.compute_simple_heat(entry, current_turn);
                heat < self.config.heat_threshold
            })
            .count();

        cold_count >= self.config.min_cold_entries
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_build_consolidation_prompt() {
        let organizer = DreamOrganizer::new();
        let entries = vec!["Entry one about architecture", "Entry two about decisions"];
        let prompt = organizer.build_consolidation_prompt(&entries);

        assert!(prompt.contains("Consolidate"));
        assert!(prompt.contains("[0] Entry one"));
        assert!(prompt.contains("[1] Entry two"));
        assert!(prompt.contains("consolidated markdown section"));
    }

    #[test]
    fn test_build_consolidation_prompt_empty() {
        let organizer = DreamOrganizer::new();
        let entries: Vec<&str> = vec![];
        let prompt = organizer.build_consolidation_prompt(&entries);

        assert!(prompt.contains("Consolidate"));
        assert!(prompt.contains("Entries:"));
    }

    #[test]
    fn test_parse_consolidation_response() {
        let response = "  ## Summary\n\nKey points here.  \n";
        let parsed = DreamOrganizer::parse_consolidation_response(response);
        assert_eq!(parsed, "## Summary\n\nKey points here.");
    }

    #[test]
    fn test_should_auto_trigger_below_threshold() {
        let mut short_term = ShortTermMemory::new();
        // 添加少量条目（不够触发阈值）
        for _ in 0..3 {
            short_term.store("user".into(), "some content".into(), 10, false);
        }

        let organizer = DreamOrganizer::with_config(DreamConfig {
            min_cold_entries: 5,
            heat_threshold: 0.3,
            decay_lambda: 0.1,
        });

        // 条目数不足，不应触发
        // 注意：这些条目的 turn 很近，热度可能不低
        assert!(!organizer.should_auto_trigger(&short_term));
    }

    #[test]
    fn test_should_auto_trigger_sufficient_cold() {
        let mut short_term = ShortTermMemory::new();
        // 添加大量条目
        for _ in 0..10 {
            short_term.store("user".into(), "some content".into(), 10, false);
        }

        let organizer = DreamOrganizer::with_config(DreamConfig {
            min_cold_entries: 3,
            heat_threshold: 0.99, // 极高阈值，几乎所有条目都被视为冷条目
            decay_lambda: 0.1,
        });

        assert!(organizer.should_auto_trigger(&short_term));
    }

    #[tokio::test]
    async fn test_try_acquire_lock() {
        let tmp = TempDir::new().unwrap();
        let organizer = DreamOrganizer::new();

        // 首次获取应成功
        let lock = organizer.try_acquire_lock(tmp.path()).await.unwrap();
        assert!(lock.is_some());

        // 锁在 RAII drop 后应释放
        drop(lock);

        // 再次获取应成功
        let lock2 = organizer.try_acquire_lock(tmp.path()).await.unwrap();
        assert!(lock2.is_some());
    }

    #[tokio::test]
    async fn test_try_acquire_lock_existing_stale() {
        let tmp = TempDir::new().unwrap();
        let lock_dir = tmp.path().join("locks");
        tokio::fs::create_dir_all(&lock_dir).await.unwrap();

        // 写入一个不存在的 PID（过期锁）
        let lock_path = lock_dir.join("dream.lock");
        tokio::fs::write(&lock_path, "999999999").await.unwrap(); // 不存在的 PID

        let organizer = DreamOrganizer::new();
        let lock = organizer.try_acquire_lock(&lock_dir).await.unwrap();

        #[cfg(unix)]
        {
            // Unix 下应能获取锁（PID 不存在）
            assert!(lock.is_some());
        }
        #[cfg(not(unix))]
        {
            // 非 Unix 平台总是允许
            assert!(lock.is_some());
        }
    }

    #[test]
    fn test_infer_category() {
        assert!(matches!(
            DreamOrganizer::infer_category("", "系统架构设计"),
            KnowledgeCategory::Architecture
        ));
        assert!(matches!(
            DreamOrganizer::infer_category("", "决定使用 Rust"),
            KnowledgeCategory::Decision
        ));
        assert!(matches!(
            DreamOrganizer::infer_category("", "用户偏好暗色主题"),
            KnowledgeCategory::UserPreference
        ));
        assert!(matches!(
            DreamOrganizer::infer_category("", "修复了空指针 bug"),
            KnowledgeCategory::ErrorFix
        ));
        assert!(matches!(
            DreamOrganizer::infer_category("", "使用 Builder 模式"),
            KnowledgeCategory::CodePattern
        ));
    }
}
