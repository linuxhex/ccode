//! 多模型路由与 Provider 池
//!
//! 职责：
//! 1. 管理多个 Provider 实例（OpenAI 兼容、Claude 等）
//! 2. 根据模型名自动路由到对应 Provider
//! 3. 支持 fallback 链：主 Provider 失败时自动切换
//! 4. 速率限制与重试

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

use super::provider::{Provider, SampleRequest, SampleResponse, StreamChunk};
use super::openai_compat::OpenAICompatProvider;
use super::claude_compat::ClaudeCompatProvider;

use crate::config::provider::ProviderConfig;

/// Provider 实例包装
struct ProviderEntry {
    provider: Box<dyn Provider>,
    /// 该 Provider 支持的模型名列表
    models: Vec<String>,
    /// fallback 链：当此 Provider 失败时，按顺序尝试的 Provider 名称
    fallback_chain: Vec<String>,
    /// 速率限制状态
    rate_limit: RateLimitState,
}

/// 简单的令牌桶速率限制
struct RateLimitState {
    /// 每分钟最大请求数
    max_requests_per_min: u32,
    /// 当前窗口内的请求数
    request_count: u32,
    /// 窗口起始时间
    window_start: Instant,
}

impl RateLimitState {
    fn new(max_requests_per_min: u32) -> Self {
        Self {
            max_requests_per_min,
            request_count: 0,
            window_start: Instant::now(),
        }
    }

    /// 检查是否可以发送请求，若可以则递增计数
    fn try_acquire(&mut self) -> bool {
        let now = Instant::now();
        // 如果超过 1 分钟，重置窗口
        if now.duration_since(self.window_start) > Duration::from_secs(60) {
            self.request_count = 0;
            self.window_start = now;
        }

        if self.request_count < self.max_requests_per_min {
            self.request_count += 1;
            true
        } else {
            false
        }
    }
}

/// 多模型 Provider 路由器
pub struct ProviderRouter {
    /// Provider 名称 → Provider 实例
    providers: HashMap<String, ProviderEntry>,
    /// 模型名 → Provider 名称的映射
    model_to_provider: HashMap<String, String>,
}

impl ProviderRouter {
    /// 从配置列表创建路由器
    pub fn from_configs(configs: &[ProviderConfig]) -> Self {
        let mut router = Self {
            providers: HashMap::new(),
            model_to_provider: HashMap::new(),
        };

        for config in configs {
            let provider: Box<dyn Provider> = match config.provider_type.as_str() {
                "openai" | "openai-compat" | "xai" | "deepseek" => {
                    Box::new(OpenAICompatProvider::new(super::openai_compat::OpenAICompatConfig {
                        name: config.name.clone(),
                        api_key: config.api_key.clone(),
                        base_url: config.base_url.clone(),
                        models: config.models.clone(),
                    }))
                }
                "claude" | "anthropic" => {
                    Box::new(ClaudeCompatProvider::new(super::claude_compat::ClaudeCompatConfig {
                        name: config.name.clone(),
                        api_key: config.api_key.clone(),
                        base_url: config.base_url.clone(),
                        models: config.models.clone(),
                        api_version: config.api_version.clone().unwrap_or_else(|| "2023-06-01".into()),
                    }))
                }
                _ => {
                    tracing::warn!("未知 Provider 类型：{}，使用 OpenAI 兼容模式", config.provider_type);
                    Box::new(OpenAICompatProvider::new(super::openai_compat::OpenAICompatConfig {
                        name: config.name.clone(),
                        api_key: config.api_key.clone(),
                        base_url: config.base_url.clone(),
                        models: config.models.clone(),
                    }))
                }
            };

            let models = config.models.clone();
            let name = config.name.clone();
            let fallback_chain = config.fallback.clone();

            // 注册模型→Provider 映射
            for model in &models {
                router.model_to_provider.insert(model.clone(), name.clone());
            }

            router.providers.insert(name, ProviderEntry {
                provider,
                models,
                fallback_chain,
                rate_limit: RateLimitState::new(config.rate_limit.unwrap_or(60)),
            });
        }

        tracing::info!("Provider 路由器初始化完成：{} 个 Provider", router.providers.len());
        router
    }

    /// 根据模型名查找 Provider
    pub fn find_provider(&mut self, model: &str) -> Option<&mut dyn Provider> {
        // 先精确匹配模型名
        if let Some(provider_name) = self.model_to_provider.get(model) {
            if let Some(entry) = self.providers.get_mut(provider_name) {
                if entry.rate_limit.try_acquire() {
                    return Some(entry.provider.as_mut());
                }
                tracing::warn!("Provider {} 速率限制，尝试 fallback", provider_name);
                // 尝试 fallback
                return self.find_fallback(provider_name, model);
            }
        }

        // 模糊匹配：检查模型前缀
        for (provider_name, entry) in &mut self.providers {
            if entry.models.iter().any(|m| model.starts_with(m.split(':').next().unwrap_or(m))) {
                if entry.rate_limit.try_acquire() {
                    return Some(entry.provider.as_mut());
                }
            }
        }

        None
    }

    /// 查找 fallback Provider
    fn find_fallback(&mut self, failed_provider: &str, model: &str) -> Option<&mut dyn Provider> {
        let fallback_chain = self.providers.get(failed_provider)
            .map(|e| e.fallback_chain.clone())
            .unwrap_or_default();

        for fb_name in &fallback_chain {
            if let Some(entry) = self.providers.get_mut(fb_name) {
                if entry.rate_limit.try_acquire() {
                    tracing::info!("Fallback 到 Provider：{}", fb_name);
                    return Some(entry.provider.as_mut());
                }
            }
        }

        // 最后尝试任意支持该模型的 Provider
        for (name, entry) in &mut self.providers {
            if name == failed_provider {
                continue;
            }
            if entry.models.iter().any(|m| m == model) && entry.rate_limit.try_acquire() {
                tracing::info!("Fallback 到任意 Provider：{}", name);
                return Some(entry.provider.as_mut());
            }
        }

        None
    }

    /// 获取所有可用模型列表
    pub fn available_models(&self) -> Vec<String> {
        let mut models: Vec<String> = self.providers.values()
            .flat_map(|e| e.models.clone())
            .collect();
        models.sort();
        models.dedup();
        models
    }

    /// 获取 Provider 数量
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }
}
