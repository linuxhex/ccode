//! Provider 配置

use serde::{Deserialize, Serialize};

/// 单个 Provider 的配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Provider 名称
    pub name: String,
    /// API Key
    pub api_key: String,
    /// API Base URL
    pub base_url: String,
    /// 适配器类型（openai / claude / glm / kimi / qianwen / ccode / deepseek）
    pub provider_type: String,
    /// 适配器枚举（兼容旧字段）
    pub adapter: ProviderAdapter,
    /// 支持的模型列表
    pub models: Vec<String>,
    /// Fallback 链：当此 Provider 失败时，按顺序尝试的 Provider 名称
    #[serde(default)]
    pub fallback: Vec<String>,
    /// 每分钟最大请求数（速率限制）
    #[serde(default)]
    pub rate_limit: Option<u32>,
    /// API 版本（如 Anthropic 的 "2023-06-01"）
    #[serde(default)]
    pub api_version: Option<String>,
}

/// Provider 适配器类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderAdapter {
    /// OpenAI 原生兼容
    OpenAI,
    /// Claude 适配器
    Claude,
    /// GLM 适配器
    GLM,
    /// Kimi 适配器
    Kimi,
    /// 千问适配器
    Qianwen,
    /// ccode Ccode 原生
    Ccode,
}

impl ProviderConfig {
    /// 默认 Ccode Provider
    pub fn default_ccode() -> Self {
        Self {
            name: "ccode".into(),
            api_key: String::new(),
            base_url: "https://api.ccode.dev/v1".into(),
            provider_type: "openai".into(),
            adapter: ProviderAdapter::Ccode,
            models: vec![
                "ccode-3".into(),
                "ccode-3-fast".into(),
                "ccode-3-mini".into(),
            ],
            fallback: vec!["deepseek".into()],
            rate_limit: Some(60),
            api_version: None,
        }
    }

    /// Claude Provider 模板
    pub fn claude_template() -> Self {
        Self {
            name: "claude".into(),
            api_key: String::new(),
            base_url: "https://api.anthropic.com".into(),
            provider_type: "claude".into(),
            adapter: ProviderAdapter::Claude,
            models: vec![
                "claude-sonnet-4-20250514".into(),
                "claude-opus-4-20250514".into(),
            ],
            fallback: vec!["ccode".into()],
            rate_limit: Some(50),
            api_version: Some("2023-06-01".into()),
        }
    }

    /// DeepSeek Provider 模板
    pub fn deepseek_template() -> Self {
        Self {
            name: "deepseek".into(),
            api_key: String::new(),
            base_url: "https://api.deepseek.com/v1".into(),
            provider_type: "openai".into(),
            adapter: ProviderAdapter::OpenAI,
            models: vec![
                "deepseek-chat".into(),
                "deepseek-reasoner".into(),
            ],
            fallback: vec![],
            rate_limit: Some(60),
            api_version: None,
        }
    }

    /// GLM Provider 模板
    pub fn glm_template() -> Self {
        Self {
            name: "glm".into(),
            api_key: String::new(),
            base_url: "https://open.bigmodel.cn/api/paas/v4".into(),
            provider_type: "openai".into(),
            adapter: ProviderAdapter::GLM,
            models: vec![
                "glm-4".into(),
                "glm-4-flash".into(),
            ],
            fallback: vec!["deepseek".into()],
            rate_limit: Some(60),
            api_version: None,
        }
    }

    /// Kimi Provider 模板
    pub fn kimi_template() -> Self {
        Self {
            name: "kimi".into(),
            api_key: String::new(),
            base_url: "https://api.moonshot.cn/v1".into(),
            provider_type: "openai".into(),
            adapter: ProviderAdapter::Kimi,
            models: vec![
                "moonshot-v1-8k".into(),
                "moonshot-v1-32k".into(),
            ],
            fallback: vec![],
            rate_limit: Some(30),
            api_version: None,
        }
    }

    /// 千问 Provider 模板
    pub fn qianwen_template() -> Self {
        Self {
            name: "qianwen".into(),
            api_key: String::new(),
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
            provider_type: "openai".into(),
            adapter: ProviderAdapter::Qianwen,
            models: vec![
                "qwen-max".into(),
                "qwen-plus".into(),
                "qwen-turbo".into(),
            ],
            fallback: vec![],
            rate_limit: Some(60),
            api_version: None,
        }
    }
}
