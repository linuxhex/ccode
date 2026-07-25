//! Agent prompt 模板管理

/// Agent 的系统 prompt
pub struct AgentPrompt;

impl AgentPrompt {
    /// 构建主 Agent 的系统 prompt
    pub fn primary_system_prompt() -> String {
        // 默认系统 prompt，实际实现中可从 ~/.ccode/prompts/ 加载自定义模板
        "你是一个终端 AI 编程助手 ccode。你可以读取文件、编辑代码、执行命令来帮助用户完成编程任务。\n\
         使用工具时请确保参数正确。编辑文件前请先读取确认内容。\n\
         如果任务复杂，可以使用 task 工具创建子 agent 并行处理。".into()
    }

    /// 构建子 Agent 的系统 prompt
    pub fn subagent_prompt(agent_type: super::AgentType) -> String {
        match agent_type {
            super::AgentType::Explore => Self::explore_prompt(),
            super::AgentType::Plan => Self::plan_prompt(),
            super::AgentType::GeneralPurpose => Self::general_purpose_prompt(),
            super::AgentType::Codex => Self::codex_prompt(),
            _ => Self::primary_system_prompt(),
        }
    }

    fn explore_prompt() -> String {
        "你是一个只读代码探索 Agent。你的任务是搜索和分析代码，但不能修改任何文件。\
         你可以使用 grep、read、list_dir 等只读工具。\
         将发现总结后返回给主 Agent。".into()
    }

    fn plan_prompt() -> String {
        "你是一个架构规划 Agent。你的任务是分析需求并制定实现计划。\
         你可以使用只读工具理解代码库，然后生成详细的实现计划。\
         不要执行任何修改操作。".into()
    }

    fn general_purpose_prompt() -> String {
        "你是一个通用子 Agent。你可以使用所有工具完成任务。\
         完成后将结果返回给主 Agent。".into()
    }

    fn codex_prompt() -> String {
        "你是一个 Codex 兼容 Agent，使用 patch 式编辑工具。".into()
    }
}
