//! GoalLoop 验证器 — 使用 LLM 评估模型判断子任务是否完成
//!
//! 借鉴 Claude Code 的评估模型（evaluation model）：
//! Claude Code 使用单独的快速 LLM 调用来判断 agent 的输出是否满足验证标准。
//! ccode 采用类似策略：将子任务描述和验证标准发送给 SamplerNode，
//! 由 LLM 判断是否通过验证。
//!
//! 验证策略（双路径）：
//! 1. 快速路径：经验日志关键词匹配（零延迟，适用于明显成功/失败的场景）
//! 2. LLM 评估：异步发送给 SamplerNode，由 LLM 判断（适用于模糊场景）
//!    —— SamplerNode 识别请求的 `goal_verify: true` 标志，流式收集完整响应后
//!       解析 JSON 并将结果发到 cortex/goal_verify_result，闭环验证流程。

use serde::{Deserialize, Serialize};

/// GoalLoop 验证请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalVerifyRequest {
    /// 子 Agent ID
    pub agent_id: String,
    /// 子任务描述
    pub subtask_description: String,
    /// 验证标准
    pub verification: String,
}

impl GoalVerifyRequest {
    /// 构建 LLM 验证 prompt
    pub fn to_verify_prompt(&self) -> String {
        format!(
            "你是一个任务验证评估器。请判断以下子任务是否已完成。\n\n\
             子任务：{}\n\
             验证标准：{}\n\n\
             请仅回答 JSON：{{\"passed\": true/false, \"reasoning\": \"评估理由\"}}",
            self.subtask_description, self.verification
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_prompt_format() {
        let req = GoalVerifyRequest {
            agent_id: "agent-1".to_string(),
            subtask_description: "创建 hello.rs 文件".to_string(),
            verification: "文件存在且可编译".to_string(),
        };
        let prompt = req.to_verify_prompt();
        assert!(prompt.contains("hello.rs"));
        assert!(prompt.contains("文件存在且可编译"));
        assert!(prompt.contains("passed"));
    }
}
