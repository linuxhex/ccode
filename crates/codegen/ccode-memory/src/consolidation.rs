//! 记忆整合：跨会话去重、提炼摘要
//!
//! 参考 Claude Code 的 autoDream 模式，对散落在各会话中的知识文件
//! 进行跨会话整合：去重、排序、提炼摘要。
//!
//! V2 支持语义去重（trigram 相似度）和 LLM 整合摘要。
//!
//! 整合过程通过 PID 文件锁防止多实例并发，锁超时为 10 分钟。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::auto_extract::{ExtractedKnowledge, KnowledgeKind};

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// 整合配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationConfig {
    /// 启用语义去重（使用 trigram 相似度）
    pub semantic_dedup: bool,
    /// 语义去重阈值（相似度超过此值视为重复）
    pub semantic_threshold: f64,
    /// 启用 LLM 摘要
    pub llm_summarize: bool,
    /// 最大整合条目数
    pub max_entries: usize,
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            semantic_dedup: true,
            semantic_threshold: 0.7,
            llm_summarize: false,
            max_entries: 100,
        }
    }
}

// ---------------------------------------------------------------------------
// 整合结果类型
// ---------------------------------------------------------------------------

/// 整合后的知识
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidatedKnowledge {
    /// 整合后的知识条目
    pub entries: Vec<ConsolidatedEntry>,
    /// 整合统计
    pub stats: ConsolidationStats,
}

/// 整合后的单条知识
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidatedEntry {
    /// 知识类型
    pub kind: KnowledgeKind,
    /// 整合后的内容
    pub content: String,
    /// 来源数量
    pub source_count: usize,
    /// 平均置信度
    pub avg_confidence: f64,
    /// 标签
    pub tags: Vec<String>,
    /// 是否经过 LLM 整合
    pub llm_consolidated: bool,
}

/// 整合统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationStats {
    pub total_input: usize,
    pub duplicates_removed: usize,
    pub entries_consolidated: usize,
    pub llm_calls: usize,
}

// ---------------------------------------------------------------------------
// Trigram 相似度
// ---------------------------------------------------------------------------

/// 计算 trigram 相似度（无需 embedding）
///
/// 两个文本的相似度 = 共同 trigram 数 / 总 trigram 数
pub fn trigram_similarity(a: &str, b: &str) -> f64 {
    let trigrams_a: HashSet<String> = trigrams(a);
    let trigrams_b: HashSet<String> = trigrams(b);

    if trigrams_a.is_empty() && trigrams_b.is_empty() {
        return 1.0;
    }

    let intersection = trigrams_a.intersection(&trigrams_b).count();
    let union = trigrams_a.union(&trigrams_b).count();

    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

fn trigrams(text: &str) -> HashSet<String> {
    let normalized: String = text
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    normalized
        .as_bytes()
        .windows(3)
        .map(|w| String::from_utf8_lossy(w).to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// LLM 整合 prompt
// ---------------------------------------------------------------------------

/// 构建 LLM 整合 prompt
///
/// 将多条相关知识发送给 LLM，让其整合为一条更精炼的知识。
pub fn build_consolidation_prompt(entries: &[ExtractedKnowledge]) -> String {
    format!(
        r#"Consolidate the following related knowledge items into a single, concise entry.
Remove redundancy while preserving all unique information.

Items:
{}

Output a single consolidated statement:"#,
        entries
            .iter()
            .enumerate()
            .map(|(i, e)| format!("[{}] ({:?}, conf={:.2}) {}", i, e.kind, e.confidence, e.content))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// 获取自动记忆目录
///
/// 返回路径：<project_dir>/.ccode/memory/
/// 所有知识文件和锁文件均存放在此目录下。
pub fn get_auto_mem_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".ccode").join("memory")
}

/// 尝试获取整合锁（基于 PID 文件，防止多实例并发）
///
/// 返回锁文件路径和修改时间，失败返回 None。
/// 锁机制：
/// 1. 检查锁文件是否存在
/// 2. 若存在，检查持有锁的进程是否仍在运行
/// 3. 若锁超过 10 分钟视为过期
/// 4. 获取锁后写入当前 PID，并验证（防止竞态）
pub fn try_acquire_consolidation_lock(memory_dir: &Path) -> Option<(PathBuf, u64)> {
    let lock_path = memory_dir.join(".consolidation.lock");

    if lock_path.exists() {
        // 检查锁是否过期（超过 10 分钟视为过期）
        if let Ok(metadata) = fs::metadata(&lock_path) {
            if let Ok(modified) = metadata.modified() {
                let elapsed = modified.elapsed().unwrap_or_default();
                if elapsed.as_secs() < 600 {
                    // 锁仍有效，检查持有者是否存活
                    if let Ok(content) = fs::read_to_string(&lock_path) {
                        if let Ok(pid) = content.trim().parse::<u32>() {
                            // 检查进程是否仍在运行
                            if is_pid_alive(pid) {
                                return None; // 锁被其他进程持有
                            }
                        }
                    }
                }
            }
        }
    }

    // 获取锁：写入当前 PID
    let pid = std::process::id();
    fs::create_dir_all(memory_dir).ok()?;
    fs::write(&lock_path, pid.to_string()).ok()?;

    // 验证锁（防止竞态：另一个进程可能同时抢锁）
    let content = fs::read_to_string(&lock_path).ok()?;
    if content.trim() != pid.to_string() {
        return None; // 另一个进程抢到了锁
    }

    // 返回锁文件路径和修改时间
    let mtime = fs::metadata(&lock_path)
        .and_then(|m| m.modified())
        .map(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        })
        .unwrap_or(0);

    Some((lock_path, mtime))
}

/// 释放整合锁
///
/// 删除锁文件，允许其他实例获取锁。
pub fn release_consolidation_lock(lock_path: &Path) {
    let _ = fs::remove_file(lock_path);
}

/// 检查 PID 是否仍在运行
///
/// 在 Unix 系统上使用 libc::kill(pid, 0) 检测进程是否存在，
/// 信号 0 不会实际发送信号，仅检查进程存在性。
/// 在非 Unix 系统上始终返回 false（保守策略：假设进程已退出）。
#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn is_pid_alive(_pid: u32) -> bool {
    false
}

/// 整合记忆：读取所有知识文件，去重，提炼摘要
///
/// 扫描 memory_dir 下的所有 .md 文件，读取内容后进行简单去重
/// （完全匹配去重 + 排序），返回去重后的知识列表。
pub fn consolidate_memories(memory_dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut all_knowledge = Vec::new();

    // 读取所有 .md 文件
    for entry in fs::read_dir(memory_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "md") {
            let content = fs::read_to_string(&path)?;
            all_knowledge.push(content);
        }
    }

    // 去重（简单的完全匹配去重）
    all_knowledge.sort();
    all_knowledge.dedup();

    Ok(all_knowledge)
}

/// 整合记忆 V2：支持语义去重 + LLM 摘要
///
/// 对 `ExtractedKnowledge` 列表执行：
/// 1. 语义去重：使用 trigram 相似度合并近似重复条目
/// 2. LLM 整合：将语义相似的条目组构建 prompt，供外部 LLM 调用整合
/// 3. 生成 `ConsolidatedKnowledge` 结果
///
/// 注意：LLM 实际调用由调用者完成，此函数仅构建 prompt 并标记
/// 哪些条目组需要 LLM 整合。调用者可通过 `build_consolidation_prompt`
/// 获取 prompt，调用 LLM 后自行替换对应条目的内容。
pub fn consolidate_memories_v2(
    entries: Vec<ExtractedKnowledge>,
    config: &ConsolidationConfig,
) -> ConsolidatedKnowledge {
    let total_input = entries.len();
    let mut duplicates_removed = 0usize;

    // 按 (kind, content 前50字符) 分组，同一组视为候选拼接
    let mut groups: Vec<Vec<ExtractedKnowledge>> = Vec::new();

    for entry in entries {
        let mut matched_group: Option<usize> = None;

        if config.semantic_dedup {
            // 语义去重：检查与已有组中第一条的相似度
            for (gi, group) in groups.iter().enumerate() {
                if let Some(first) = group.first() {
                    if first.kind == entry.kind {
                        let sim = trigram_similarity(&first.content, &entry.content);
                        if sim >= config.semantic_threshold {
                            matched_group = Some(gi);
                            break;
                        }
                    }
                }
            }
        } else {
            // 简单去重：同 kind + 前50字符相同即视为重复
            for (gi, group) in groups.iter().enumerate() {
                if let Some(first) = group.first() {
                    if first.kind == entry.kind
                        && first.content.chars().take(50).collect::<String>()
                            == entry.content.chars().take(50).collect::<String>()
                    {
                        matched_group = Some(gi);
                        break;
                    }
                }
            }
        }

        match matched_group {
            Some(gi) => {
                groups[gi].push(entry);
                duplicates_removed += 1;
            }
            None => {
                groups.push(vec![entry]);
            }
        }
    }

    let llm_calls = if config.llm_summarize {
        groups.iter().filter(|g| g.len() > 1).count()
    } else {
        0
    };

    // 将每组整合为 ConsolidatedEntry
    let mut consolidated: Vec<ConsolidatedEntry> = groups
        .into_iter()
        .map(|group| {
            let source_count = group.len();
            let avg_confidence = group.iter().map(|e| e.confidence).sum::<f64>() / source_count as f64;
            // 合并所有标签
            let mut tags: Vec<String> = group
                .iter()
                .flat_map(|e| e.tags.clone())
                .collect();
            tags.sort();
            tags.dedup();

            // 取置信度最高的条目的内容作为初始内容
            // 如果启用 LLM 整合且有多个来源，则标记需要 LLM 整合
            let best = group
                .iter()
                .max_by(|a, b| a.confidence.partial_cmp(&b.confidence).unwrap_or(std::cmp::Ordering::Equal))
                .expect("组不应为空");

            let llm_consolidated = config.llm_summarize && source_count > 1;

            ConsolidatedEntry {
                kind: best.kind.clone(),
                content: best.content.clone(),
                source_count,
                avg_confidence,
                tags,
                llm_consolidated,
            }
        })
        .collect();

    // 按 max_entries 截断
    consolidated.truncate(config.max_entries);

    ConsolidatedKnowledge {
        entries: consolidated,
        stats: ConsolidationStats {
            total_input,
            duplicates_removed,
            entries_consolidated: llm_calls,
            llm_calls,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_extract::{ExtractionStrategy, KnowledgeKind};

    fn make_entry(kind: KnowledgeKind, content: &str, confidence: f64, tags: Vec<&str>) -> ExtractedKnowledge {
        ExtractedKnowledge {
            kind,
            content: content.to_string(),
            source_session: "test-session".to_string(),
            extracted_at: "2025-01-01T00:00:00Z".to_string(),
            confidence,
            strategy: ExtractionStrategy::KeywordMatch,
            context_summary: None,
            tags: tags.into_iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn test_get_auto_mem_path() {
        let path = get_auto_mem_path(Path::new("/tmp/project"));
        assert_eq!(path, PathBuf::from("/tmp/project/.ccode/memory"));
    }

    #[test]
    fn test_consolidate_memories_empty_dir() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let result = consolidate_memories(dir.path()).expect("整合记忆失败");
        assert!(result.is_empty());
    }

    #[test]
    fn test_consolidate_memories_dedup() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        fs::write(dir.path().join("a.md"), "知识A").expect("写入失败");
        fs::write(dir.path().join("b.md"), "知识B").expect("写入失败");
        fs::write(dir.path().join("c.md"), "知识A").expect("写入失败"); // 重复
        let result = consolidate_memories(dir.path()).expect("整合记忆失败");
        assert_eq!(result.len(), 2); // 去重后应为 2
    }

    #[test]
    fn test_release_consolidation_lock() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let lock_path = dir.path().join(".consolidation.lock");
        fs::write(&lock_path, "12345").expect("写入锁文件失败");
        release_consolidation_lock(&lock_path);
        assert!(!lock_path.exists());
    }

    // =======================================================================
    // Trigram 相似度测试
    // =======================================================================

    #[test]
    fn test_trigram_similarity_identical() {
        let sim = trigram_similarity("hello world", "hello world");
        assert!((sim - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_trigram_similarity_completely_different() {
        let sim = trigram_similarity("abc", "xyz");
        assert!(sim < 0.1);
    }

    #[test]
    fn test_trigram_similarity_similar() {
        let sim = trigram_similarity(
            "must use UTF-8 encoding",
            "must use UTF-8 encoding for all files",
        );
        assert!(sim > 0.5, "相似文本应有较高相似度，实际: {}", sim);
    }

    #[test]
    fn test_trigram_similarity_empty() {
        let sim = trigram_similarity("", "");
        assert!((sim - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_trigram_similarity_case_insensitive() {
        let sim = trigram_similarity("Hello World", "hello world");
        assert!((sim - 1.0).abs() < f64::EPSILON);
    }

    // =======================================================================
    // ConsolidationConfig 默认值测试
    // =======================================================================

    #[test]
    fn test_consolidation_config_default() {
        let config = ConsolidationConfig::default();
        assert!(config.semantic_dedup);
        assert!((config.semantic_threshold - 0.7).abs() < f64::EPSILON);
        assert!(!config.llm_summarize);
        assert_eq!(config.max_entries, 100);
    }

    // =======================================================================
    // consolidate_memories_v2 测试
    // =======================================================================

    #[test]
    fn test_consolidate_v2_empty_input() {
        let config = ConsolidationConfig::default();
        let result = consolidate_memories_v2(vec![], &config);
        assert!(result.entries.is_empty());
        assert_eq!(result.stats.total_input, 0);
        assert_eq!(result.stats.duplicates_removed, 0);
    }

    #[test]
    fn test_consolidate_v2_no_duplicates() {
        let config = ConsolidationConfig::default();
        let entries = vec![
            make_entry(KnowledgeKind::Decision, "decided to use Rust", 0.8, vec!["decision"]),
            make_entry(KnowledgeKind::Constraint, "must use HTTPS", 0.9, vec!["constraint"]),
        ];
        let result = consolidate_memories_v2(entries, &config);
        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.stats.duplicates_removed, 0);
    }

    #[test]
    fn test_consolidate_v2_semantic_dedup() {
        let config = ConsolidationConfig {
            semantic_dedup: true,
            semantic_threshold: 0.5,
            ..ConsolidationConfig::default()
        };
        let entries = vec![
            make_entry(KnowledgeKind::Decision, "decided to use Rust for the backend service", 0.8, vec!["decision"]),
            make_entry(KnowledgeKind::Decision, "decided to use Rust for the backend service layer", 0.9, vec!["decision"]),
        ];
        let result = consolidate_memories_v2(entries, &config);
        assert_eq!(result.entries.len(), 1, "语义相似条目应被合并");
        assert_eq!(result.stats.duplicates_removed, 1);
        assert_eq!(result.entries[0].source_count, 2);
    }

    #[test]
    fn test_consolidate_v2_simple_dedup() {
        let config = ConsolidationConfig {
            semantic_dedup: false,
            ..ConsolidationConfig::default()
        };
        // 两条内容完全相同的条目
        let entries = vec![
            make_entry(KnowledgeKind::Constraint, "must use HTTPS for all endpoints", 0.8, vec!["security"]),
            make_entry(KnowledgeKind::Constraint, "must use HTTPS for all endpoints", 0.9, vec!["security"]),
        ];
        let result = consolidate_memories_v2(entries, &config);
        assert_eq!(result.entries.len(), 1, "简单去重应合并内容完全相同的条目");
    }

    #[test]
    fn test_consolidate_v2_llm_flag() {
        let config = ConsolidationConfig {
            llm_summarize: true,
            semantic_dedup: true,
            semantic_threshold: 0.5,
            ..ConsolidationConfig::default()
        };
        let entries = vec![
            make_entry(KnowledgeKind::Decision, "decided to use Rust for backend", 0.8, vec!["decision"]),
            make_entry(KnowledgeKind::Decision, "decided to use Rust for backend services", 0.9, vec!["decision"]),
            make_entry(KnowledgeKind::Constraint, "must use UTF-8", 0.7, vec!["constraint"]),
        ];
        let result = consolidate_memories_v2(entries, &config);
        // 只有1组（两个 Decision 被合并）需要 LLM 调用
        assert_eq!(result.stats.llm_calls, 1);
        assert_eq!(result.stats.entries_consolidated, 1);
        // 被合并的条目应标记为需要 LLM 整合
        let consolidated_entry = result.entries.iter().find(|e| e.source_count > 1).unwrap();
        assert!(consolidated_entry.llm_consolidated);
    }

    #[test]
    fn test_consolidate_v2_max_entries() {
        let config = ConsolidationConfig {
            max_entries: 2,
            ..ConsolidationConfig::default()
        };
        let entries = vec![
            make_entry(KnowledgeKind::Decision, "decision A", 0.8, vec![]),
            make_entry(KnowledgeKind::Constraint, "constraint B", 0.9, vec![]),
            make_entry(KnowledgeKind::Correction, "correction C", 0.7, vec![]),
        ];
        let result = consolidate_memories_v2(entries, &config);
        assert!(result.entries.len() <= 2);
    }

    #[test]
    fn test_consolidate_v2_avg_confidence() {
        let config = ConsolidationConfig {
            semantic_dedup: true,
            semantic_threshold: 0.5,
            ..ConsolidationConfig::default()
        };
        let entries = vec![
            make_entry(KnowledgeKind::Decision, "decided to use Rust for backend", 0.6, vec![]),
            make_entry(KnowledgeKind::Decision, "decided to use Rust for backend service", 0.8, vec![]),
        ];
        let result = consolidate_memories_v2(entries, &config);
        assert_eq!(result.entries.len(), 1);
        let avg = (0.6 + 0.8) / 2.0;
        assert!((result.entries[0].avg_confidence - avg).abs() < f64::EPSILON);
    }

    // =======================================================================
    // build_consolidation_prompt 测试
    // =======================================================================

    #[test]
    fn test_build_consolidation_prompt() {
        let entries = vec![
            make_entry(KnowledgeKind::Decision, "decided to use Rust", 0.8, vec![]),
            make_entry(KnowledgeKind::Decision, "decided to use Rust too", 0.9, vec![]),
        ];
        let prompt = build_consolidation_prompt(&entries);
        assert!(prompt.contains("[0]"));
        assert!(prompt.contains("[1]"));
        assert!(prompt.contains("Decision"));
        assert!(prompt.contains("decided to use Rust"));
    }
}
