//! 目标驱动循环 — GoalLoop 集成（/goal 命令触发）

use super::*;

impl ThinkerNode {
    /// 启动目标驱动循环
    ///
    /// 从用户描述创建 GoalLoop，自动拆解子任务、执行、验证。
    pub fn start_goal(&mut self, description: String) {
        let goal_loop = GoalLoop::from_description(description);
        tracing::info!(
            target: "ccore::goal",
            description = %goal_loop.description(),
            "GoalLoop 已启动"
        );
        self.goal_loop = Some(goal_loop);
    }

    /// 启动目标驱动循环并注入规划提示
    ///
    /// `/goal` 命令与 `cortex/goal_start` 消息的统一入口——两条路径此前各自
    /// 复制了「建 GoalLoop + 注入拆解提示」的逻辑，改一处漏一处，故收敛到此。
    ///
    /// 注入后由调用方决定如何驱动采样：`/goal` 走 handler 末尾的统一采样出口，
    /// `cortex/goal_start` 因不经该出口需自行开启 agentic session。
    pub(crate) fn start_goal_with_planning(&mut self, description: String) {
        self.start_goal(description);
        if let Some(ref gl) = self.goal_loop {
            let desc = gl.description().to_string();
            let prompt = format!(
                "请将以下目标拆解为可执行的子任务列表（JSON 数组格式，每项包含 description 和 verification 字段）：\n{}",
                desc
            );
            // 按提示全文估算，而非只算目标描述——此前两处都只传了 desc 的
            // token 数，低估了实际注入量（提示模板本身约 40 token）
            let tokens = Self::estimate_tokens(&prompt);
            self.working_memory.push_user(prompt, tokens);
        }
    }

    /// 处理 GoalLoop 动作（由 on_turn_complete / on_verification_result 产生）
    ///
    /// 根据动作类型：注入工作记忆、发送验证请求、或清除 goal_loop。
    pub(crate) async fn process_goal_action(&mut self, action: GoalAction, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        match action {
            GoalAction::ExecuteSubTask { description } => {
                // 将子任务描述注入工作记忆，驱动下一轮对话
                self.working_memory.push_user(
                    description.clone(),
                    Self::estimate_tokens(&description),
                );
                tracing::info!(target: "ccore::goal", description = %description, "GoalLoop：执行子任务");
            }
            GoalAction::VerifySubTask { verification } => {
                // 通过 bus 发送验证请求
                let verify_msg = FrameCodec::new_message(
                    Topic::new("cortex/goal_verify"),
                    self.id.as_str(),
                    &serde_json::json!({
                        "agent_id": self.id.to_string(),
                        "verification": verification,
                    }),
                )?;
                if let Err(e) = transport.send_message(&verify_msg).await {
                    tracing::debug!("发送 GoalLoop 验证请求失败：{}", e);
                }
                tracing::info!(target: "ccore::goal", "GoalLoop：请求验证子任务");
            }
            GoalAction::GoalComplete { reason } => {
                tracing::info!(target: "ccore::goal", reason = ?reason, "GoalLoop：目标完成");
                self.goal_loop = None;
            }
            GoalAction::SubTaskFailed { subtask_idx, will_retry } => {
                tracing::warn!(
                    target: "ccore::goal",
                    subtask_idx,
                    will_retry,
                    "GoalLoop：子任务失败"
                );
                if !will_retry {
                    // 不再重试时，继续循环让 GoalLoop 前进到下一个子任务
                }
            }
        }
        Ok(())
    }

    /// 处理目标验证结果回调
    pub async fn on_goal_verification_result(&mut self, passed: bool, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        if let Some(ref mut gl) = self.goal_loop {
            let action = gl.on_verification_result(passed);
            self.process_goal_action(action, transport).await?;
        }
        Ok(())
    }

    /// 从 LLM 响应中解析子任务列表
    ///
    /// 支持两种格式：
    /// 1. JSON 数组：[{"description":"...","verification":"..."}]
    /// 2. 编号列表：1. 任务描述
    pub(crate) fn parse_subtasks_from_llm_response(&self, text: &str) -> Vec<crate::agent::goal_loop::SubTask> {
        use crate::agent::goal_loop::{SubTask, SubTaskState};

        // 尝试 JSON 数组格式
        if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(text) {
            return arr.iter().filter_map(|item| {
                let desc = item["description"].as_str()?.to_string();
                let verification = item["verification"].as_str().unwrap_or("完成").to_string();
                Some(SubTask {
                    description: desc,
                    state: SubTaskState::Pending,
                    verification,
                    attempts: 0,
                    max_retries: 2,
                })
            }).collect();
        }

        // 尝试在文本中查找 JSON 数组（可能被包裹在 markdown code block 中）
        if let Some(start) = text.find('[') {
            if let Some(end) = text.rfind(']') {
                if end > start {
                    let json_str = &text[start..=end];
                    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(json_str) {
                        return arr.iter().filter_map(|item| {
                            let desc = item["description"].as_str()?.to_string();
                            let verification = item["verification"].as_str().unwrap_or("完成").to_string();
                            Some(SubTask {
                                description: desc,
                                state: SubTaskState::Pending,
                                verification,
                                attempts: 0,
                                max_retries: 2,
                            })
                        }).collect();
                    }
                }
            }
        }

        // 尝试编号列表格式（1. xxx 或 - xxx）
        let subtasks: Vec<SubTask> = text.lines()
            .filter(|line| {
                let trimmed = line.trim();
                trimmed.starts_with(|c: char| c.is_ascii_digit()) && trimmed.contains('.')
                    || trimmed.starts_with("- ")
            })
            .filter_map(|line| {
                let desc = line.trim()
                    .trim_start_matches(|c: char| c.is_ascii_digit())
                    .trim_start_matches('.')
                    .trim_start_matches('-')
                    .trim()
                    .to_string();
                if desc.len() > 3 {
                    Some(SubTask {
                        description: desc,
                        state: SubTaskState::Pending,
                        verification: "完成".to_string(),
                        attempts: 0,
                        max_retries: 2,
                    })
                } else {
                    None
                }
            })
            .collect();

        subtasks
    }
}
