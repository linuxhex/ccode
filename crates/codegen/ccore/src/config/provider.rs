//! Provider 配置 — 多模型适配器定义

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
    /// 适配器类型（openai / claude / gemini，其他别名自动映射到 openai）
    pub provider_type: String,
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
    /// 上下文窗口大小（tokens），用于自动压缩阈值计算
    #[serde(default)]
    pub context_window: Option<u64>,
    /// 是否支持思维链/推理模式（DeepSeek R1、GLM-5、Qwen thinking）
    #[serde(default)]
    pub supports_thinking: bool,
    /// 是否支持视觉/多模态输入
    #[serde(default)]
    pub supports_vision: bool,
    /// 额外的 HTTP 请求头（如 x-api-key、anthropic-version 等）
    #[serde(default)]
    pub extra_headers: std::collections::HashMap<String, String>,
}

impl ProviderConfig {
    // ==================== OpenAI 兼容类 ====================

    /// DeepSeek Provider 模板
    pub fn deepseek_template() -> Self {
        Self {
            name: "deepseek".into(),
            api_key: String::new(),
            base_url: "https://api.deepseek.com/v1".into(),
            provider_type: "openai".into(),
            models: vec![
                "deepseek-chat".into(),
                "deepseek-reasoner".into(),
                "deepseek-v3".into(),
                "deepseek-r1".into(),
            ],
            fallback: vec!["glm".into()],
            rate_limit: Some(60),
            api_version: None,
            context_window: Some(128_000), // DeepSeek V3/R1: 128K context
            supports_thinking: true, // deepseek-reasoner/r1 support thinking
            supports_vision: false,
            extra_headers: std::collections::HashMap::new(),
        }
    }

    /// GLM Provider 模板（智谱 GLM-4 / GLM-5.x）
    pub fn glm_template() -> Self {
        Self {
            name: "glm".into(),
            api_key: String::new(),
            base_url: "https://open.bigmodel.cn/api/paas/v4".into(),
            provider_type: "openai".into(),
            models: vec![
                "glm-4-plus".into(),
                "glm-4-flash".into(),
                "glm-4-air".into(),
                "glm-4-long".into(),
                "glm-5.0".into(),
                "glm-5.0-flash".into(),
            ],
            fallback: vec!["deepseek".into()],
            rate_limit: Some(60),
            api_version: None,
            context_window: Some(128_000), // GLM-4/5: 128K context
            supports_thinking: true, // GLM-5.0 supports thinking
            supports_vision: true,  // GLM-4V supports vision
            extra_headers: std::collections::HashMap::new(),
        }
    }

    /// Qwen Provider 模板（通义千问）
    pub fn qwen_template() -> Self {
        Self {
            name: "qwen".into(),
            api_key: String::new(),
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
            provider_type: "openai".into(),
            models: vec![
                "qwen-max".into(),
                "qwen-plus".into(),
                "qwen-turbo".into(),
                "qwen3-235b-a22b".into(),
                "qwen-coder-plus".into(),
            ],
            fallback: vec!["deepseek".into()],
            rate_limit: Some(60),
            api_version: None,
            context_window: Some(131_072), // Qwen: 128K context
            supports_thinking: true, // qwen3-235b-a22b supports thinking
            supports_vision: true,  // qwen-vl supports vision
            extra_headers: std::collections::HashMap::new(),
        }
    }

    /// Kimi Provider 模板（月之暗面）
    pub fn kimi_template() -> Self {
        Self {
            name: "kimi".into(),
            api_key: String::new(),
            base_url: "https://api.moonshot.cn/v1".into(),
            provider_type: "openai".into(),
            models: vec![
                "moonshot-v1-8k".into(),
                "moonshot-v1-32k".into(),
                "moonshot-v1-128k".into(),
                "kimi-latest".into(),
            ],
            fallback: vec![],
            rate_limit: Some(30),
            api_version: None,
            context_window: Some(128_000),
            supports_thinking: false,
            supports_vision: false,
            extra_headers: std::collections::HashMap::new(),
        }
    }

    /// Ollama Provider 模板（本地模型）
    pub fn ollama_template() -> Self {
        Self {
            name: "ollama".into(),
            api_key: "ollama".into(),
            base_url: "http://localhost:11434/v1".into(),
            provider_type: "openai".into(),
            models: vec![
                "llama3.3".into(),
                "qwen3".into(),
                "deepseek-r1".into(),
                "codestral".into(),
                "mistral".into(),
            ],
            fallback: vec![],
            rate_limit: None,
            api_version: None,
            context_window: None, // 本地模型上下文窗口因模型而异
            supports_thinking: false,
            supports_vision: false,
            extra_headers: std::collections::HashMap::new(),
        }
    }

    /// Qoder Provider 模板（Qoder ccode）
    pub fn qoder_template() -> Self {
        Self {
            name: "qoder".into(),
            api_key: String::new(),
            base_url: "https://api.qoder.dev/v1".into(),
            provider_type: "openai".into(),
            models: vec![
                "qoder-3".into(),
                "qoder-3-fast".into(),
            ],
            fallback: vec!["deepseek".into()],
            rate_limit: Some(60),
            api_version: None,
            context_window: Some(128_000),
            supports_thinking: false,
            supports_vision: false,
            extra_headers: std::collections::HashMap::new(),
        }
    }

    // ==================== Claude 类 ====================

    /// Claude Provider 模板（Anthropic）
    pub fn claude_template() -> Self {
        Self {
            name: "claude".into(),
            api_key: String::new(),
            base_url: "https://api.anthropic.com".into(),
            provider_type: "claude".into(),
            models: vec![
                "claude-sonnet-4-20250514".into(),
                "claude-opus-4-20250514".into(),
                "claude-haiku-3-5".into(),
            ],
            fallback: vec!["deepseek".into()],
            rate_limit: Some(50),
            api_version: Some("2023-06-01".into()),
            context_window: Some(200_000), // Claude: 200K context
            supports_thinking: true, // Claude extended thinking
            supports_vision: true,  // Claude supports vision
            extra_headers: {
                let mut headers = std::collections::HashMap::new();
                headers.insert("anthropic-version".into(), "2023-06-01".into());
                headers
            },
        }
    }

    // ==================== Gemini 类 ====================

    /// Gemini Provider 模板（Google）
    pub fn gemini_template() -> Self {
        Self {
            name: "gemini".into(),
            api_key: String::new(),
            base_url: "https://generativelanguage.googleapis.com".into(),
            provider_type: "gemini".into(),
            models: vec![
                "gemini-2.5-pro".into(),
                "gemini-2.5-flash".into(),
                "gemini-2.0-flash".into(),
            ],
            fallback: vec!["deepseek".into()],
            rate_limit: Some(30),
            api_version: None,
            context_window: Some(1_048_576), // Gemini 2.5: 1M context
            supports_thinking: true, // Gemini 2.5 supports thinking
            supports_vision: true,  // Gemini supports vision
            extra_headers: std::collections::HashMap::new(),
        }
    }
}