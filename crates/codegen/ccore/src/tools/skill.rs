//! Skill 工具执行器（对标 Claude Code Skill 系统）
//!
//! 技能是一组预定义的任务模板，可自动化常见工作流：
//! - commit: 生成提交信息并提交
//! - test: 运行测试并分析结果
//! - review: 代码审查
//! - fix: 自动修复 lint 错误
//! - refactor: 重构建议

use async_trait::async_trait;
use crate::tools::bridge::ToolExecutor;

/// Skill 工具执行器
pub struct SkillExecutor;

/// 预定义技能
#[derive(Debug, Clone)]
pub enum Skill {
    /// 提交代码
    Commit,
    /// 运行测试
    Test,
    /// 代码审查
    Review,
    /// 修复 lint 错误
    Fix,
    /// 重构建议
    Refactor,
}

impl Skill {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "commit" => Some(Self::Commit),
            "test" => Some(Self::Test),
            "review" => Some(Self::Review),
            "fix" => Some(Self::Fix),
            "refactor" => Some(Self::Refactor),
            _ => None,
        }
    }
    
    /// 获取技能的提示模板
    fn prompt_template(&self) -> &str {
        match self {
            Self::Commit => r#"分析当前 git 变更并生成提交信息：
1. 运行 `git diff --cached` 查看暂存变更
2. 运行 `git diff` 查看未暂存变更
3. 生成简洁的提交信息（关注 why 而非 what）
4. 不要提交包含敏感文件的变更（.env, credentials 等）"#,
            Self::Test => r#"运行测试并分析结果：
1. 识别项目类型（cargo/npm/pip）
2. 运行适当的测试命令
3. 分析失败测试的根因
4. 提出修复建议"#,
            Self::Review => r#"对当前变更进行代码审查：
1. 运行 `git diff` 查看变更
2. 评估代码质量、安全性、性能
3. 提出改进建议
4. 标注严重程度（Critical/Warning/Suggestion）"#,
            Self::Fix => r#"自动修复代码问题：
1. 运行 lint/check 命令
2. 分析错误和警告
3. 自动修复可修复的问题
4. 验证修复后代码可通过检查"#,
            Self::Refactor => r#"分析代码并提供重构建议：
1. 识别代码异味（重复代码、过长函数、深层嵌套）
2. 提出重构方案
3. 评估重构风险
4. 生成重构步骤"#,
        }
    }
}

#[async_trait]
impl ToolExecutor for SkillExecutor {
    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let skill_name = args["skill"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("skill: 缺少 skill 参数"))?;
        
        let skill = Skill::from_str(skill_name)
            .ok_or_else(|| anyhow::anyhow!("skill: 未知技能 '{}'。可用技能: commit, test, review, fix, refactor", skill_name))?;
        
        tracing::info!(target: "ccore::skill", skill = skill_name, "executing skill");
        
        // 返回技能的提示模板
        // 实际执行由 Agent 循环中的 LLM 根据 prompt 完成
        Ok(format!("技能: {}\n\n{}", skill_name, skill.prompt_template()))
    }
    
    fn name(&self) -> &str { "skill" }
}
