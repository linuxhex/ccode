//! Proactive Loop — 主动循环（借鉴 Claude Code Proactive Loop 设计）
//!
//! Agent 闲置时主动扫描代码质量、测试覆盖、安全问题，
//! 发现问题后自主修复，无需用户触发。
//!
//! 工作流：
//! 1. Agent 空闲超过阈值时间 → 触发扫描
//! 2. 扫描范围：代码异味、测试覆盖、类型安全、安全漏洞
//! 3. 发现问题 → 自主修复（复用 Turn 循环）
//! 4. 修复完成 → 记录日志 → 回到空闲等待
//!
//! 与 Claude Code 的区别：
//! - Claude Code: 无独立 Proactive Loop（靠 /schedule 手动配置）
//! - ccode: 内置 Proactive Loop，闲置自动触发

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// 主动扫描状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProactiveState {
    /// 空闲等待
    Idle,
    /// 扫描中
    Scanning,
    /// 修复中
    Repairing,
    /// 暂停
    Paused,
    /// 停止
    Stopped,
}

/// 扫描类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScanType {
    /// 代码异味（unused imports, dead code, 复杂度）
    CodeSmell,
    /// 测试覆盖
    TestCoverage,
    /// 类型安全（unwrap, expect, panic）
    TypeSafety,
    /// 安全问题（硬编码密钥, SQL 注入风险）
    Security,
    /// 编译警告
    CompilerWarnings,
}

/// 扫描发现的问题
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProactiveIssue {
    /// 问题类型
    pub scan_type: ScanType,
    /// 问题描述
    pub description: String,
    /// 问题严重度
    pub severity: IssueSeverity,
    /// 建议修复方案
    pub suggested_fix: String,
    /// 关联文件
    pub file_path: Option<String>,
}

/// 问题严重度
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum IssueSeverity {
    /// 低（代码风格）
    Low,
    /// 中（潜在问题）
    Medium,
    /// 高（必须修复）
    High,
    /// 紧急（安全漏洞）
    Critical,
}

/// Proactive Loop 规格
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProactiveSpec {
    /// 空闲多久后触发首次扫描
    pub idle_timeout: Duration,
    /// 扫描间隔（两次扫描之间的最短间隔）
    pub scan_interval: Duration,
    /// 最大并发修复数
    pub max_concurrent_repairs: usize,
    /// 只自动修复 Medium 及以上严重度
    pub min_auto_fix_severity: IssueSeverity,
    /// 是否启用（默认关闭，需用户显式开启）
    pub enabled: bool,
}

impl Default for ProactiveSpec {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(120),  // 2 分钟空闲后触发
            scan_interval: Duration::from_secs(300), // 5 分钟最短间隔
            max_concurrent_repairs: 3,
            min_auto_fix_severity: IssueSeverity::Medium,
            enabled: false,
        }
    }
}

/// Proactive Loop 输出动作
#[derive(Debug, Clone)]
pub enum ProactiveAction {
    /// 继续等待（未到空闲时间）
    ContinueWaiting {
        remaining: Duration,
    },
    /// 开始扫描
    StartScan {
        scan_types: Vec<ScanType>,
    },
    /// 发现问题，开始修复
    RepairIssue {
        issue: ProactiveIssue,
    },
    /// 扫描完成，无问题
    ScanComplete {
        issues_found: usize,
        issues_fixed: usize,
    },
    /// 所有修复完成
    AllRepairsComplete {
        total_issues: usize,
        fixed: usize,
        failed: usize,
    },
}

/// Proactive Loop 状态机
pub struct ProactiveLoop {
    /// 规格
    spec: ProactiveSpec,
    /// 当前状态
    state: ProactiveState,
    /// 上次扫描完成时间
    last_scan_time: Option<Instant>,
    /// 上次活跃时间（收到用户输入时更新）
    last_active_time: Instant,
    /// 当前扫描发现的问题队列
    pending_issues: Vec<ProactiveIssue>,
    /// 正在修复的问题索引
    current_repair_idx: usize,
    /// 修复统计
    fixed_count: usize,
    failed_count: usize,
}

impl ProactiveLoop {
    /// 创建新的 ProactiveLoop
    pub fn new(spec: ProactiveSpec) -> Self {
        Self {
            state: ProactiveState::Idle,
            last_scan_time: None,
            last_active_time: Instant::now(),
            pending_issues: Vec::new(),
            current_repair_idx: 0,
            fixed_count: 0,
            failed_count: 0,
            spec,
        }
    }

    /// 获取当前状态
    pub fn state(&self) -> ProactiveState {
        self.state
    }

    /// 是否启用
    pub fn is_enabled(&self) -> bool {
        self.spec.enabled
    }

    /// 启用/禁用
    pub fn set_enabled(&mut self, enabled: bool) {
        self.spec.enabled = enabled;
        if !enabled {
            self.state = ProactiveState::Stopped;
        } else if self.state == ProactiveState::Stopped {
            self.state = ProactiveState::Idle;
            self.last_active_time = Instant::now();
        }
    }

    /// 用户活跃回调（收到输入时调用）
    pub fn on_user_active(&mut self) {
        self.last_active_time = Instant::now();
    }

    /// 检查是否应该开始扫描
    pub fn should_scan(&self) -> bool {
        if !self.spec.enabled || self.state != ProactiveState::Idle {
            return false;
        }

        // 空闲时间不足
        let idle_duration = self.last_active_time.elapsed();
        if idle_duration < self.spec.idle_timeout {
            return false;
        }

        // 距上次扫描间隔不足
        if let Some(last) = self.last_scan_time {
            if last.elapsed() < self.spec.scan_interval {
                return false;
            }
        }

        true
    }

    /// 开始扫描
    pub fn on_start_scan(&mut self) -> ProactiveAction {
        self.state = ProactiveState::Scanning;
        tracing::info!(
            target: "ccore::proactive",
            "开始主动扫描"
        );
        ProactiveAction::StartScan {
            scan_types: vec![
                ScanType::CodeSmell,
                ScanType::TypeSafety,
                ScanType::Security,
                ScanType::CompilerWarnings,
            ],
        }
    }

    /// 扫描完成回调（传入发现的问题列表）
    pub fn on_scan_complete(&mut self, issues: Vec<ProactiveIssue>) -> ProactiveAction {
        self.last_scan_time = Some(Instant::now());

        // 过滤出需要自动修复的问题
        let auto_fix_issues: Vec<ProactiveIssue> = issues
            .into_iter()
            .filter(|i| i.severity >= self.spec.min_auto_fix_severity)
            .collect();

        if auto_fix_issues.is_empty() {
            self.state = ProactiveState::Idle;
            tracing::info!(
                target: "ccore::proactive",
                "扫描完成，无需修复的问题"
            );
            return ProactiveAction::ScanComplete {
                issues_found: 0,
                issues_fixed: 0,
            };
        }

        tracing::info!(
            target: "ccore::proactive",
            count = auto_fix_issues.len(),
            "发现需要修复的问题"
        );

        self.pending_issues = auto_fix_issues;
        self.current_repair_idx = 0;
        self.fixed_count = 0;
        self.failed_count = 0;
        self.state = ProactiveState::Repairing;

        self.next_repair_action()
    }

    /// 修复完成回调
    pub fn on_repair_complete(&mut self, success: bool) -> ProactiveAction {
        if success {
            self.fixed_count += 1;
        } else {
            self.failed_count += 1;
        }
        self.current_repair_idx += 1;
        self.next_repair_action()
    }

    /// 暂停
    pub fn pause(&mut self) {
        if self.state == ProactiveState::Idle || self.state == ProactiveState::Scanning {
            self.state = ProactiveState::Paused;
        }
    }

    /// 恢复
    pub fn resume(&mut self) {
        if self.state == ProactiveState::Paused {
            self.state = ProactiveState::Idle;
            self.last_active_time = Instant::now();
        }
    }

    // ---- 内部方法 ----

    fn next_repair_action(&self) -> ProactiveAction {
        if self.current_repair_idx >= self.pending_issues.len() {
            return ProactiveAction::AllRepairsComplete {
                total_issues: self.pending_issues.len(),
                fixed: self.fixed_count,
                failed: self.failed_count,
            };
        }

        ProactiveAction::RepairIssue {
            issue: self.pending_issues[self.current_repair_idx].clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proactive_disabled_by_default() {
        let proactive = ProactiveLoop::new(ProactiveSpec::default());
        assert!(!proactive.is_enabled());
        assert!(!proactive.should_scan());
    }

    #[test]
    fn test_proactive_scan_after_idle() {
        let mut spec = ProactiveSpec::default();
        spec.enabled = true;
        spec.idle_timeout = Duration::from_secs(0); // 立即触发
        spec.scan_interval = Duration::from_secs(0);

        let mut proactive = ProactiveLoop::new(spec);
        assert!(proactive.should_scan());

        let action = proactive.on_start_scan();
        assert!(matches!(action, ProactiveAction::StartScan { .. }));

        // 扫描完成，无问题
        let action = proactive.on_scan_complete(vec![]);
        assert!(matches!(action, ProactiveAction::ScanComplete { .. }));
    }

    #[test]
    fn test_proactive_repair_flow() {
        let mut spec = ProactiveSpec::default();
        spec.enabled = true;
        spec.idle_timeout = Duration::from_secs(0);
        spec.scan_interval = Duration::from_secs(0);

        let mut proactive = ProactiveLoop::new(spec);
        proactive.on_start_scan();

        let issues = vec![
            ProactiveIssue {
                scan_type: ScanType::CodeSmell,
                description: "unused import".to_string(),
                severity: IssueSeverity::Medium,
                suggested_fix: "remove unused import".to_string(),
                file_path: Some("src/main.rs".to_string()),
            },
            ProactiveIssue {
                scan_type: ScanType::Security,
                description: "hardcoded API key".to_string(),
                severity: IssueSeverity::Critical,
                suggested_fix: "move to env var".to_string(),
                file_path: Some("src/config.rs".to_string()),
            },
        ];

        let action = proactive.on_scan_complete(issues);
        assert!(matches!(action, ProactiveAction::RepairIssue { .. }));

        // 第一个修复成功
        let action = proactive.on_repair_complete(true);
        assert!(matches!(action, ProactiveAction::RepairIssue { .. }));

        // 第二个修复成功
        let action = proactive.on_repair_complete(true);
        assert!(matches!(action, ProactiveAction::AllRepairsComplete {
            fixed: 2,
            failed: 0,
            ..
        }));
    }
}
