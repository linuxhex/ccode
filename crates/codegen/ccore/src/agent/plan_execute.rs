//! Plan-Execute 循环 - Plan Mode → 用户审批 → 执行 → 验证

use serde::{Deserialize, Serialize};

use crate::tools::bridge::ToolBridge;
use crate::tools::ToolCallRequest;

/// Plan 步骤
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub step_id: String,
    pub description: String,
    pub tool_name: String,
    pub tool_args: serde_json::Value,
    pub status: PlanStepStatus,
}

/// Plan 步骤状态
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum PlanStepStatus {
    Pending,
    Approved,
    Executing,
    Completed,
    Failed,
    Skipped,
}

/// 单步执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    /// 步骤 ID
    pub step_id: String,
    /// 执行是否成功
    pub success: bool,
    /// 工具输出内容
    pub output: String,
    /// 执行耗时（毫秒）
    pub duration_ms: u64,
    /// 是否需要重新规划
    pub needs_replan: bool,
}

/// Plan 执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanResult {
    /// Plan ID
    pub plan_id: String,
    /// 各步骤执行结果
    pub step_results: Vec<StepResult>,
    /// 最终状态
    pub final_status: PlanStatus,
    /// 是否被暂停
    pub paused: bool,
}

/// 重新规划原因
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReplanReason {
    /// 步骤执行失败
    StepFailed { step_id: String, error: String },
    /// 依赖的步骤结果不符合预期
    UnexpectedResult { step_id: String, output: String },
    /// 外部条件变化
    ConditionChanged(String),
}

/// Plan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub plan_id: String,
    pub steps: Vec<PlanStep>,
    pub status: PlanStatus,
    /// 暂停点：执行到指定步骤后暂停（步骤 ID）
    pub pause_after: Option<String>,
}

impl Plan {
    /// 执行单步计划，通过 ToolBridge 调用对应工具
    pub async fn execute_step(
        &mut self,
        step_id: &str,
        bridge: &ToolBridge,
        agent_id: &str,
    ) -> anyhow::Result<StepResult> {
        // 查找目标步骤
        let step = self.steps.iter_mut().find(|s| s.step_id == step_id)
            .ok_or_else(|| anyhow::anyhow!("步骤不存在：{}", step_id))?;

        // 校验步骤状态：只有 Approved 的步骤可以执行
        if step.status != PlanStepStatus::Approved {
            anyhow::bail!("步骤 {} 状态为 {:?}，无法执行（需要 Approved）", step_id, step.status);
        }

        // 标记为执行中
        step.status = PlanStepStatus::Executing;
        let tool_name = step.tool_name.clone();
        let tool_args = step.tool_args.clone();

        // 构建工具调用请求
        let request = ToolCallRequest {
            tool_call_id: uuid::Uuid::new_v4().to_string(),
            tool_name: tool_name.clone(),
            arguments: tool_args,
            agent_id: agent_id.to_string(),
        };

        // 通过 ToolBridge 执行工具
        let tool_result = bridge.execute(&request).await;

        // 更新步骤状态
        let step = match self.steps.iter_mut().find(|s| s.step_id == step_id) {
            Some(s) => s,
            None => {
                return Err(anyhow::anyhow!("Plan 步骤不存在：{}", step_id));
            }
        };
        let success = tool_result.success;
        if success {
            step.status = PlanStepStatus::Completed;
        } else {
            step.status = PlanStepStatus::Failed;
        }

        // 判断是否需要重新规划：失败或输出包含关键错误模式时需要
        let needs_replan = !success || tool_result.output.contains("REPLAN_REQUIRED");

        Ok(StepResult {
            step_id: step_id.to_string(),
            success,
            output: tool_result.output,
            duration_ms: tool_result.duration_ms,
            needs_replan,
        })
    }

    /// 获取下一个待执行（Approved）步骤的 ID
    pub fn next_step_id(&self) -> Option<&str> {
        self.steps
            .iter()
            .find(|s| s.status == PlanStepStatus::Approved)
            .map(|s| s.step_id.as_str())
    }

    /// 检查计划是否所有步骤都已完成
    pub fn is_all_done(&self) -> bool {
        self.steps.iter().all(|s| matches!(
            s.status,
            PlanStepStatus::Completed | PlanStepStatus::Skipped
        ))
    }
}

/// Plan 状态
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum PlanStatus {
    /// 正在生成计划
    Drafting,
    /// 等待用户审批
    AwaitingApproval,
    /// 执行中
    Executing,
    /// 已暂停（等待恢复）
    Paused,
    /// 已完成
    Completed,
    /// 需要重新规划
    Replanning,
    /// 被用户拒绝
    Rejected,
}

/// Plan 执行器 - 逐步执行计划中的步骤，支持暂停/恢复和重新规划
pub struct PlanExecutor {
    /// 工具桥接器
    bridge: ToolBridge,
    /// 发起执行的 Agent ID
    agent_id: String,
    /// 最大连续重新规划次数，防止无限循环
    max_replan_count: u32,
}

impl PlanExecutor {
    pub fn new(bridge: ToolBridge, agent_id: String, max_replan_count: u32) -> Self {
        Self {
            bridge,
            agent_id,
            max_replan_count,
        }
    }

    /// 逐步执行计划中的步骤，支持暂停/恢复
    ///
    /// 执行流程：
    /// 1. 依次执行 Approved 状态的步骤
    /// 2. 每步执行后检查结果，判断是否需要 replan
    /// 3. 如果设置了暂停点，到达后暂停并返回
    /// 4. 所有步骤完成后更新 Plan 状态为 Completed
    pub async fn execute(&self, plan: &mut Plan) -> PlanResult {
        let mut step_results = Vec::new();
        let mut _replan_count = 0u32;

        while let Some(id) = plan.next_step_id() {
            // 获取下一个待执行步骤
            let next_step_id = id.to_string();

            // 执行单步
            let result = plan
                .execute_step(&next_step_id, &self.bridge, &self.agent_id)
                .await
                .unwrap_or_else(|e| StepResult {
                    step_id: next_step_id.clone(),
                    success: false,
                    output: format!("步骤执行异常：{}", e),
                    duration_ms: 0,
                    needs_replan: true,
                });

            let needs_replan = result.needs_replan;
            step_results.push(result);

            // 检查暂停点
            if let Some(pause_after) = &plan.pause_after {
                if &next_step_id == pause_after {
                    plan.status = PlanStatus::Paused;
                    return PlanResult {
                        plan_id: plan.plan_id.clone(),
                        step_results,
                        final_status: PlanStatus::Paused,
                        paused: true,
                    };
                }
            }

            // 检查是否需要重新规划
            if needs_replan && _replan_count < self.max_replan_count {
                _replan_count += 1;
                plan.status = PlanStatus::Replanning;
                // 返回结果，由调用方决定是否调用 replan 生成新步骤
                return PlanResult {
                    plan_id: plan.plan_id.clone(),
                    step_results,
                    final_status: PlanStatus::Replanning,
                    paused: false,
                };
            }

            // 如果不需要 replan 或已达到上限，继续执行下一步
        }

        // 所有步骤完成
        plan.status = PlanStatus::Completed;
        PlanResult {
            plan_id: plan.plan_id.clone(),
            step_results,
            final_status: PlanStatus::Completed,
            paused: false,
        }
    }

    /// 根据执行结果重新规划，在失败的步骤之后插入新的步骤
    ///
    /// replan 策略：
    /// - 失败步骤标记为 Skipped
    /// - 在其之后插入新的修复步骤
    /// - 新步骤状态为 Approved，可立即执行
    pub fn replan(
        &self,
        plan: &mut Plan,
        reason: &ReplanReason,
        new_steps: Vec<PlanStep>,
    ) -> anyhow::Result<()> {
        // 确定插入位置：根据 replan 原因找到对应步骤
        let insert_after = match reason {
            ReplanReason::StepFailed { step_id, .. } => step_id.clone(),
            ReplanReason::UnexpectedResult { step_id, .. } => step_id.clone(),
            ReplanReason::ConditionChanged(_) => {
                // 外部条件变化时，在最后一个失败/执行中步骤之后插入
                plan.steps
                    .iter()
                    .rfind(|s| matches!(s.status, PlanStepStatus::Failed | PlanStepStatus::Executing))
                    .map(|s| s.step_id.clone())
                    .ok_or_else(|| anyhow::anyhow!("没有可重新规划的步骤"))?
            }
        };

        // 找到步骤索引
        let pos = plan
            .steps
            .iter()
            .position(|s| s.step_id == insert_after)
            .ok_or_else(|| anyhow::anyhow!("重新规划目标步骤不存在：{}", insert_after))?;

        // 将失败步骤标记为 Skipped
        if matches!(plan.steps[pos].status, PlanStepStatus::Failed) {
            plan.steps[pos].status = PlanStepStatus::Skipped;
        }

        // 在目标步骤之后插入新步骤（状态设为 Approved 以便立即执行）
        let mut new_steps = new_steps;
        for step in &mut new_steps {
            if step.status == PlanStepStatus::Pending {
                step.status = PlanStepStatus::Approved;
            }
        }
        for (i, step) in new_steps.into_iter().enumerate() {
            plan.steps.insert(pos + 1 + i, step);
        }

        // 恢复为执行状态
        plan.status = PlanStatus::Executing;
        Ok(())
    }
}

/// Plan-Execute 控制器
pub struct PlanExecuteController {
    current_plan: Option<Plan>,
}

impl Default for PlanExecuteController {
    fn default() -> Self {
        Self::new()
    }
}

impl PlanExecuteController {
    pub fn new() -> Self {
        Self { current_plan: None }
    }

    /// 创建新 Plan
    pub fn create_plan(&mut self, steps: Vec<PlanStep>) -> &Plan {
        let plan = Plan {
            plan_id: uuid::Uuid::new_v4().to_string(),
            steps,
            status: PlanStatus::AwaitingApproval,
            pause_after: None,
        };
        self.current_plan = Some(plan);
        self.current_plan.as_ref().expect("create_plan: plan just set")
    }

    /// 审批 Plan
    pub fn approve(&mut self) -> Option<&Plan> {
        if let Some(plan) = &mut self.current_plan {
            plan.status = PlanStatus::Executing;
            for step in &mut plan.steps {
                if step.status == PlanStepStatus::Pending {
                    step.status = PlanStepStatus::Approved;
                }
            }
        }
        self.current_plan.as_ref()
    }

    /// 拒绝 Plan
    pub fn reject(&mut self) {
        if let Some(plan) = &mut self.current_plan {
            plan.status = PlanStatus::Rejected;
        }
    }

    /// 获取下一个待执行步骤
    pub fn next_step(&self) -> Option<&PlanStep> {
        self.current_plan
            .as_ref()?
            .steps
            .iter()
            .find(|s| s.status == PlanStepStatus::Approved)
    }

    /// 标记步骤完成
    pub fn step_completed(&mut self, step_id: &str) {
        if let Some(plan) = &mut self.current_plan {
            if let Some(step) = plan.steps.iter_mut().find(|s| s.step_id == step_id) {
                step.status = PlanStepStatus::Completed;
            }
            // 检查是否所有步骤都完成
            if plan.is_all_done() {
                plan.status = PlanStatus::Completed;
            }
        }
    }

    /// 设置暂停点：执行到指定步骤后暂停
    pub fn set_pause_after(&mut self, step_id: &str) {
        if let Some(plan) = &mut self.current_plan {
            plan.pause_after = Some(step_id.to_string());
        }
    }
}
