//! Token 预算管理器（借鉴 Claude Code tokenBudget.ts + autoCompact.ts）
//!
//! 核心模式：
//! 1. 基于模型上下文窗口的动态token预算
//! 2. 自动压缩触发：使用超过80%时触发
//! 3. 缓冲区：10%预留给系统提示+工具
//! 4. 字符估算回退：token计数不可用时的粗略估算

use std::collections::HashMap;

/// 模型上下文窗口大小
pub const MODEL_CONTEXT_WINDOWS: &[(&str, usize)] = &[
    ("claude-3.5-sonnet", 200_000),
    ("claude-3-opus", 200_000),
    ("claude-3-haiku", 200_000),
    ("gpt-4o", 128_000),
    ("gpt-4-turbo", 128_000),
    ("gpt-4", 8_192),
    ("gpt-3.5-turbo", 16_385),
    ("deepseek-chat", 64_000),
    ("deepseek-coder", 16_384),
];

/// 默认上下文窗口
const DEFAULT_CONTEXT_WINDOW: usize = 128_000;

/// 自动压缩阈值百分比（Claude Code: 0.8）
const AUTOCOMPACT_THRESHOLD: f64 = 0.8;

/// 系统缓冲区百分比（Claude Code: 0.1）
const SYSTEM_BUFFER: f64 = 0.1;

/// Token 估算比率（英文约4字符/token，中文约1.5字符/token）
const CHARS_PER_TOKEN_EN: f64 = 4.0;
const CHARS_PER_TOKEN_CJK: f64 = 1.5;

/// Token 预算管理器
pub struct TokenBudgetManager {
    /// 当前模型
    model: String,
    /// 上下文窗口大小
    context_window: usize,
    /// 已使用的token数
    tokens_used: usize,
    /// 系统提示占用的token数
    system_prompt_tokens: usize,
    /// 工具定义占用的token数
    tool_definition_tokens: usize,
    /// 模型上下文窗口缓存
    model_windows: HashMap<String, usize>,
}

/// Token 预算状态
#[derive(Debug, Clone)]
pub struct BudgetStatus {
    /// 上下文窗口大小
    pub context_window: usize,
    /// 已使用token
    pub tokens_used: usize,
    /// 可用token（扣除缓冲区）
    pub tokens_available: usize,
    /// 使用率
    pub usage_ratio: f64,
    /// 是否应触发自动压缩
    pub should_compact: bool,
    /// 距离压缩还有多少token
    pub tokens_until_compact: i64,
}

impl TokenBudgetManager {
    pub fn new(model: &str) -> Self {
        let mut mgr = Self {
            model: model.to_string(),
            context_window: DEFAULT_CONTEXT_WINDOW,
            tokens_used: 0,
            system_prompt_tokens: 0,
            tool_definition_tokens: 0,
            model_windows: HashMap::new(),
        };

        // 初始化模型窗口表
        for &(name, window) in MODEL_CONTEXT_WINDOWS {
            mgr.model_windows.insert(name.to_string(), window);
        }

        mgr.context_window = mgr.resolve_context_window(model);
        mgr
    }

    /// 解析模型上下文窗口大小
    fn resolve_context_window(&self, model: &str) -> usize {
        // 精确匹配
        if let Some(&window) = self.model_windows.get(model) {
            return window;
        }

        // 模糊匹配（如 "claude-3.5-sonnet-20241022" → "claude-3.5-sonnet"）
        for (name, &window) in &self.model_windows {
            if model.starts_with(name) {
                return window;
            }
        }

        DEFAULT_CONTEXT_WINDOW
    }

    /// 切换模型
    pub fn switch_model(&mut self, model: &str) {
        self.model = model.to_string();
        self.context_window = self.resolve_context_window(model);
        tracing::info!(target: "ccore::budget", model = %model, context_window = self.context_window, "model switched");
    }

    /// 获取当前预算状态
    pub fn status(&self) -> BudgetStatus {
        let reserved = (self.context_window as f64 * SYSTEM_BUFFER) as usize;
        let available = self.context_window.saturating_sub(reserved);
        let usage_ratio = if available > 0 {
            self.tokens_used as f64 / available as f64
        } else {
            1.0
        };

        let compact_threshold = (available as f64 * AUTOCOMPACT_THRESHOLD) as usize;
        let should_compact = self.tokens_used >= compact_threshold;
        let tokens_until_compact = compact_threshold as i64 - self.tokens_used as i64;

        BudgetStatus {
            context_window: self.context_window,
            tokens_used: self.tokens_used,
            tokens_available: available.saturating_sub(self.tokens_used),
            usage_ratio,
            should_compact,
            tokens_until_compact,
        }
    }

    /// 记录token使用量
    pub fn record_usage(&mut self, input_tokens: usize, output_tokens: usize) {
        let total = input_tokens + output_tokens;
        self.tokens_used += total;

        let status = self.status();
        tracing::debug!(
            target: "ccore::budget",
            input_tokens,
            output_tokens,
            total_used = self.tokens_used,
            usage_ratio = format!("{:.1}%", status.usage_ratio * 100.0),
            until_compact = status.tokens_until_compact,
            "token usage recorded"
        );

        if status.should_compact {
            tracing::warn!(
                target: "ccore::budget",
                usage_ratio = format!("{:.1}%", status.usage_ratio * 100.0),
                "approaching token budget limit, auto-compact recommended"
            );
        }
    }

    /// 压缩后重置token使用量
    pub fn reset_after_compact(&mut self, new_usage: usize) {
        let old = self.tokens_used;
        self.tokens_used = new_usage;
        tracing::info!(
            target: "ccore::budget",
            old_tokens = old,
            new_tokens = new_usage,
            saved = old.saturating_sub(new_usage),
            "token budget reset after compaction"
        );
    }

    /// 粗略token估算（Claude Code: roughTokenCountEstimation）
    ///
    /// 当精确token计数不可用时，基于字符数估算
    pub fn estimate_tokens(text: &str) -> usize {
        let cjk_count = text.chars().filter(|c| {
            ('\u{4E00}'..='\u{9FFF}').contains(c) ||  // CJK Unified
            ('\u{3400}'..='\u{4DBF}').contains(c) ||  // CJK Extension A
            ('\u{F900}'..='\u{FAFF}').contains(c)      // CJK Compat
        }).count();

        let total_chars = text.chars().count();
        let ascii_count = total_chars.saturating_sub(cjk_count);

        let cjk_tokens = (cjk_count as f64 / CHARS_PER_TOKEN_CJK).ceil() as usize;
        let ascii_tokens = (ascii_count as f64 / CHARS_PER_TOKEN_EN).ceil() as usize;

        cjk_tokens + ascii_tokens
    }

    /// 检查是否有足够空间添加新内容
    pub fn can_fit(&self, additional_tokens: usize) -> bool {
        let status = self.status();
        status.tokens_available >= additional_tokens
    }

    /// 计算可添加的最大token数
    pub fn remaining_capacity(&self) -> usize {
        self.status().tokens_available
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_budget_new() {
        let mgr = TokenBudgetManager::new("claude-3.5-sonnet");
        assert_eq!(mgr.context_window, 200_000);
    }

    #[test]
    fn test_token_budget_fuzzy_match() {
        let mgr = TokenBudgetManager::new("claude-3.5-sonnet-20241022");
        assert_eq!(mgr.context_window, 200_000);
    }

    #[test]
    fn test_token_budget_default() {
        let mgr = TokenBudgetManager::new("unknown-model");
        assert_eq!(mgr.context_window, DEFAULT_CONTEXT_WINDOW);
    }

    #[test]
    fn test_switch_model() {
        let mut mgr = TokenBudgetManager::new("gpt-4o");
        assert_eq!(mgr.context_window, 128_000);
        mgr.switch_model("gpt-4");
        assert_eq!(mgr.context_window, 8_192);
    }

    #[test]
    fn test_status_no_usage() {
        let mgr = TokenBudgetManager::new("gpt-4o");
        let status = mgr.status();
        assert_eq!(status.tokens_used, 0);
        assert!(!status.should_compact);
        assert!(status.usage_ratio < 0.01);
    }

    #[test]
    fn test_record_usage() {
        let mut mgr = TokenBudgetManager::new("gpt-4o");
        mgr.record_usage(50_000, 10_000);
        assert_eq!(mgr.tokens_used, 60_000);
        let status = mgr.status();
        assert!(!status.should_compact);
    }

    #[test]
    fn test_auto_compact_trigger() {
        let mut mgr = TokenBudgetManager::new("gpt-4");
        // gpt-4: 8192 window, reserved 10% = 819, available = 7373
        // compact threshold = 7373 * 0.8 = 5898
        mgr.record_usage(5000, 1000);
        let status = mgr.status();
        assert!(status.should_compact);
    }

    #[test]
    fn test_reset_after_compact() {
        let mut mgr = TokenBudgetManager::new("gpt-4o");
        mgr.record_usage(50_000, 10_000);
        mgr.reset_after_compact(20_000);
        assert_eq!(mgr.tokens_used, 20_000);
    }

    #[test]
    fn test_estimate_tokens_english() {
        let tokens = TokenBudgetManager::estimate_tokens("Hello world, this is a test.");
        // ~30 chars / 4 = ~8 tokens
        assert!(tokens >= 7 && tokens <= 9);
    }

    #[test]
    fn test_estimate_tokens_cjk() {
        let tokens = TokenBudgetManager::estimate_tokens("你好世界测试");
        // 5 CJK chars / 1.5 = ~4 tokens
        assert!(tokens >= 3 && tokens <= 5);
    }

    #[test]
    fn test_can_fit() {
        let mut mgr = TokenBudgetManager::new("gpt-4o");
        mgr.record_usage(50_000, 10_000);
        assert!(mgr.can_fit(10_000));
        assert!(!mgr.can_fit(1_000_000));
    }

    #[test]
    fn test_remaining_capacity() {
        let mgr = TokenBudgetManager::new("gpt-4o");
        // available = 128000 - 12800 = 115200
        assert_eq!(mgr.remaining_capacity(), 115_200);
    }
}
