//! 降级策略实现
//!
//! 工具执行失败时，根据配置尝试降级到备选工具或返回简化结果。

use std::collections::HashMap;
use tracing::warn;

/// 降级配置
#[derive(Debug, Clone)]
pub struct DegradationConfig {
    /// 是否启用降级
    pub enabled: bool,
    /// 工具降级映射：原工具名 → 备选工具名
    pub fallback_tools: HashMap<String, String>,
    /// 简化响应模板：工具名 → 简化响应内容
    pub simplified_responses: HashMap<String, String>,
}

impl Default for DegradationConfig {
    fn default() -> Self {
        let mut fallback_tools = HashMap::new();
        // 文件读取失败时降级到安全读取（只读前 100 行）
        fallback_tools.insert("read_file".to_string(), "read_file_safe".to_string());
        fallback_tools.insert("Read".to_string(), "read_file_safe".to_string());

        let mut simplified_responses = HashMap::new();
        // Bash 执行失败时返回提示
        simplified_responses.insert(
            "bash".to_string(),
            "命令执行失败，建议检查命令语法或权限。".to_string(),
        );
        simplified_responses.insert(
            "Bash".to_string(),
            "命令执行失败，建议检查命令语法或权限。".to_string(),
        );

        Self {
            enabled: true,
            fallback_tools,
            simplified_responses,
        }
    }
}

/// 降级策略
pub struct DegradationStrategy {
    config: DegradationConfig,
}

impl DegradationStrategy {
    /// 创建降级策略
    pub fn new(config: DegradationConfig) -> Self {
        Self { config }
    }

    /// 从默认配置创建
    pub fn default_strategy() -> Self {
        Self::new(DegradationConfig::default())
    }

    /// 检查降级是否启用
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// 获取工具的降级备选工具
    ///
    /// # 参数
    /// - tool_name: 原工具名
    ///
    /// # 返回
    /// - Some(fallback) 存在降级工具
    /// - None 无降级工具
    pub fn get_fallback_tool(&self, tool_name: &str) -> Option<&str> {
        if !self.config.enabled {
            return None;
        }
        self.config
            .fallback_tools
            .get(tool_name)
            .map(|s| s.as_str())
    }

    /// 获取工具的简化响应
    ///
    /// # 参数
    /// - tool_name: 工具名
    ///
    /// # 返回
    /// - Some(response) 存在简化响应
    /// - None 无简化响应
    pub fn get_simplified_response(&self, tool_name: &str) -> Option<&str> {
        if !self.config.enabled {
            return None;
        }
        self.config
            .simplified_responses
            .get(tool_name)
            .map(|s| s.as_str())
    }

    /// 尝试降级处理工具失败
    ///
    /// # 参数
    /// - tool_name: 失败的工具名
    /// - error: 错误信息
    ///
    /// # 返回
    /// - Some(response) 降级成功，返回简化响应
    /// - None 无法降级
    pub fn handle_failure(&self, tool_name: &str, error: &str) -> Option<String> {
        if !self.config.enabled {
            return None;
        }

        warn!(
            target: "ccore::degradation",
            tool = tool_name,
            error = error,
            from = "normal",
            to = "degraded",
            reason = "tool_execution_failed",
            "degradation level changed"
        );

        // 优先返回简化响应
        if let Some(response) = self.get_simplified_response(tool_name) {
            tracing::info!(
                target: "ccore::fallback",
                component = tool_name,
                strategy = "simplified_response",
                "fallback activated"
            );
            tracing::info!(
                target: "ccore::degradation",
                level = "degraded",
                component = tool_name,
                "service recovered with simplified response"
            );
            return Some(response.to_string());
        }

        // 如果有降级工具，返回提示（实际降级工具调用由调用方处理）
        if let Some(fallback) = self.get_fallback_tool(tool_name) {
            tracing::warn!(
                target: "ccore::fallback",
                component = tool_name,
                strategy = %format!("fallback_to_{}", fallback),
                "fallback activated"
            );
            tracing::info!(
                target: "ccore::degradation",
                level = "degraded",
                component = tool_name,
                "service recovered with fallback tool"
            );
            return Some(format!(
                "工具 {} 失败，建议使用 {} 重试。错误：{}",
                tool_name, fallback, error
            ));
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fallback_tool() {
        let strategy = DegradationStrategy::default_strategy();
        assert_eq!(
            strategy.get_fallback_tool("read_file"),
            Some("read_file_safe")
        );
        assert_eq!(strategy.get_fallback_tool("unknown_tool"), None);
    }

    #[test]
    fn test_simplified_response() {
        let strategy = DegradationStrategy::default_strategy();
        assert!(strategy.get_simplified_response("bash").is_some());
        assert!(strategy.get_simplified_response("unknown").is_none());
    }

    #[test]
    fn test_handle_failure() {
        let strategy = DegradationStrategy::default_strategy();
        assert!(strategy.handle_failure("bash", "timeout").is_some());
        assert!(strategy.handle_failure("unknown", "error").is_none());
    }
}