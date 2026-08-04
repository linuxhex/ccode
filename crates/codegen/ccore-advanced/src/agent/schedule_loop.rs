//! Schedule Loop — 定时循环（借鉴 Claude Code /schedule 设计）
//!
//! 用户指定定时任务（如"每 5 分钟检查 CI"），ScheduleLoop 自动：
//! 1. 等待指定间隔
//! 2. 唤醒执行任务（复用 Turn 循环）
//! 3. 执行完毕后重新进入等待
//! 4. 达到最大执行次数或用户取消时退出
//!
//! 实现：基于 tokio::time 定时器，不阻塞主循环。

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// 定时任务状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScheduleState {
    /// 等待下一次执行
    Waiting,
    /// 执行中
    Executing,
    /// 暂停
    Paused,
    /// 完成
    Completed,
    /// 失败
    Failed,
}

/// 定时任务规格
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleSpec {
    /// 任务描述
    pub description: String,
    /// 执行间隔
    pub interval: Duration,
    /// 最大执行次数（None = 无限）
    pub max_executions: Option<u32>,
    /// 单次执行最大轮次
    pub max_turns_per_execution: u32,
    /// 连续失败多少次后停止
    pub max_consecutive_failures: u32,
}

/// 定时任务完成原因
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ScheduleDoneReason {
    /// 达到最大执行次数
    MaxExecutionsReached,
    /// 连续失败次数过多
    ConsecutiveFailures { count: u32 },
    /// 用户取消
    UserCancelled,
}

/// Schedule Loop 输出动作
#[derive(Debug, Clone)]
pub enum ScheduleAction {
    /// 等待下次执行（返回需要等待的时长）
    Wait {
        until_next: Duration,
    },
    /// 执行任务
    Execute {
        description: String,
    },
    /// 任务完成
    ScheduleComplete {
        reason: ScheduleDoneReason,
        total_executions: u32,
        total_successes: u32,
    },
}

/// Schedule Loop 状态机
pub struct ScheduleLoop {
    /// 任务规格
    spec: ScheduleSpec,
    /// 当前状态
    state: ScheduleState,
    /// 已执行次数
    execution_count: u32,
    /// 成功次数
    success_count: u32,
    /// 连续失败次数
    consecutive_failures: u32,
    /// 上次执行完成时间
    last_execution_end: Option<Instant>,
    /// 完成原因
    done_reason: Option<ScheduleDoneReason>,
}

impl ScheduleLoop {
    /// 创建新的 ScheduleLoop
    pub fn new(spec: ScheduleSpec) -> Self {
        Self {
            state: ScheduleState::Waiting,
            spec,
            execution_count: 0,
            success_count: 0,
            consecutive_failures: 0,
            last_execution_end: None,
            done_reason: None,
        }
    }

    /// 从自然语言描述和间隔创建
    pub fn from_description(description: String, interval_secs: u64) -> Self {
        Self::new(ScheduleSpec {
            description,
            interval: Duration::from_secs(interval_secs),
            max_executions: None,
            max_turns_per_execution: 10,
            max_consecutive_failures: 3,
        })
    }

    /// 获取当前状态
    pub fn state(&self) -> ScheduleState {
        self.state
    }

    /// 获取任务描述
    pub fn description(&self) -> &str {
        &self.spec.description
    }

    /// 获取执行统计
    pub fn stats(&self) -> (u32, u32) {
        (self.execution_count, self.success_count)
    }

    /// 计算下次执行需要等待的时长
    pub fn time_until_next(&self) -> Duration {
        match self.last_execution_end {
            Some(last_end) => {
                let elapsed = last_end.elapsed();
                if elapsed >= self.spec.interval {
                    Duration::ZERO
                } else {
                    self.spec.interval - elapsed
                }
            }
            None => Duration::ZERO, // 首次立即执行
        }
    }

    /// 检查是否应该执行（定时器到期）
    pub fn should_execute_now(&self) -> bool {
        self.state == ScheduleState::Waiting && self.time_until_next() == Duration::ZERO
    }

    /// 定时器到期回调——开始执行
    pub fn on_timer_fired(&mut self) -> ScheduleAction {
        if self.state != ScheduleState::Waiting {
            return ScheduleAction::Wait {
                until_next: self.spec.interval,
            };
        }

        // 检查执行次数上限
        if let Some(max) = self.spec.max_executions {
            if self.execution_count >= max {
                self.state = ScheduleState::Completed;
                self.done_reason = Some(ScheduleDoneReason::MaxExecutionsReached);
                return ScheduleAction::ScheduleComplete {
                    reason: ScheduleDoneReason::MaxExecutionsReached,
                    total_executions: self.execution_count,
                    total_successes: self.success_count,
                };
            }
        }

        self.state = ScheduleState::Executing;
        self.execution_count += 1;

        tracing::info!(
            target: "ccore::schedule",
            execution = self.execution_count,
            description = %self.spec.description,
            "定时任务执行"
        );

        ScheduleAction::Execute {
            description: self.spec.description.clone(),
        }
    }

    /// 执行完成回调
    pub fn on_execution_complete(&mut self, success: bool) -> ScheduleAction {
        self.last_execution_end = Some(Instant::now());

        if success {
            self.success_count += 1;
            self.consecutive_failures = 0;
        } else {
            self.consecutive_failures += 1;
            tracing::warn!(
                target: "ccore::schedule",
                consecutive_failures = self.consecutive_failures,
                "定时任务执行失败"
            );
        }

        // 检查连续失败上限
        if self.consecutive_failures >= self.spec.max_consecutive_failures {
            self.state = ScheduleState::Failed;
            self.done_reason = Some(ScheduleDoneReason::ConsecutiveFailures {
                count: self.consecutive_failures,
            });
            return ScheduleAction::ScheduleComplete {
                reason: ScheduleDoneReason::ConsecutiveFailures {
                    count: self.consecutive_failures,
                },
                total_executions: self.execution_count,
                total_successes: self.success_count,
            };
        }

        // 回到等待状态
        self.state = ScheduleState::Waiting;
        let until_next = self.spec.interval;

        tracing::info!(
            target: "ccore::schedule",
            until_next_secs = until_next.as_secs(),
            "等待下次执行"
        );

        ScheduleAction::Wait { until_next }
    }

    /// 暂停
    pub fn pause(&mut self) {
        if self.state == ScheduleState::Waiting {
            self.state = ScheduleState::Paused;
        }
    }

    /// 恢复
    pub fn resume(&mut self) {
        if self.state == ScheduleState::Paused {
            self.state = ScheduleState::Waiting;
        }
    }

    /// 取消
    pub fn cancel(&mut self) -> ScheduleAction {
        self.state = ScheduleState::Completed;
        self.done_reason = Some(ScheduleDoneReason::UserCancelled);
        ScheduleAction::ScheduleComplete {
            reason: ScheduleDoneReason::UserCancelled,
            total_executions: self.execution_count,
            total_successes: self.success_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schedule_basic() {
        let mut schedule = ScheduleLoop::from_description("检查 CI".to_string(), 300);

        // 首次立即执行
        assert!(schedule.should_execute_now());
        let action = schedule.on_timer_fired();
        assert!(matches!(action, ScheduleAction::Execute { .. }));

        // 执行成功，进入等待
        let action = schedule.on_execution_complete(true);
        assert!(matches!(action, ScheduleAction::Wait { .. }));
        assert!(!schedule.should_execute_now()); // 还没到时间
    }

    #[test]
    fn test_schedule_max_executions() {
        let mut schedule = ScheduleLoop::new(ScheduleSpec {
            description: "测试".to_string(),
            interval: Duration::from_secs(0),
            max_executions: Some(2),
            max_turns_per_execution: 5,
            max_consecutive_failures: 3,
        });

        // 执行 2 次
        schedule.on_timer_fired();
        schedule.on_execution_complete(true);
        schedule.on_timer_fired();
        schedule.on_execution_complete(true);

        // 第 3 次应该触发上限
        let action = schedule.on_timer_fired();
        assert!(matches!(action, ScheduleAction::ScheduleComplete {
            reason: ScheduleDoneReason::MaxExecutionsReached,
            ..
        }));
    }

    #[test]
    fn test_schedule_consecutive_failures() {
        let mut schedule = ScheduleLoop::from_description("测试".to_string(), 0);
        schedule.spec.max_consecutive_failures = 2;

        schedule.on_timer_fired();
        schedule.on_execution_complete(false);
        schedule.on_timer_fired();
        let action = schedule.on_execution_complete(false);

        assert!(matches!(action, ScheduleAction::ScheduleComplete {
            reason: ScheduleDoneReason::ConsecutiveFailures { count: 2 },
            ..
        }));
    }
}
