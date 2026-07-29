//! JSON 日志格式化器
//!
//! 提供自定义 tracing 格式化器，输出 JSON 结构化日志。
//! 字段包含：level、timestamp、target、message、span 信息。

use chrono::Utc;
use serde_json::{json, Value};
use tracing_subscriber;

/// JSON 日志格式化器配置
pub struct JsonFormatter {
    /// 是否启用紧凑模式（单行 JSON）
    pub compact: bool,
}

impl Default for JsonFormatter {
    fn default() -> Self {
        Self { compact: true }
    }
}

impl JsonFormatter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }
}

/// 简单的 JSON 格式化器
///
/// 提供手动格式化日志为 JSON 的辅助方法。
/// 实际日志初始化使用 init_json_logging。
pub struct SimpleJsonFormatter;

impl SimpleJsonFormatter {
    /// 格式化单个事件为 JSON 字符串
    pub fn format_event_to_json(
        level: &str,
        target: &str,
        message: &str,
        fields: Option<Value>,
    ) -> String {
        let mut entry = json!({
            "level": level,
            "timestamp": Utc::now().to_rfc3339(),
            "target": target,
            "message": message,
        });

        if let Some(f) = fields {
            if let Some(obj) = entry.as_object_mut() {
                if let Some(fields_obj) = f.as_object() {
                    for (k, v) in fields_obj {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        entry.to_string()
    }
}

/// 初始化 JSON 日志
///
/// 使用 tracing_subscriber 的 JSON 格式器初始化全局日志。
pub fn init_json_logging() {
    use tracing_subscriber::{fmt, EnvFilter};

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    fmt()
        .with_env_filter(env_filter)
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .with_file(true)
        .with_line_number(true)
        .with_target(true)
        .init();
}