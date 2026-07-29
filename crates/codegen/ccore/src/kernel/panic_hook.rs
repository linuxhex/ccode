//! 全局 panic 钩子（借鉴 Claude Code 的错误边界）
//!
//! 捕获未处理的 panic，记录日志而非直接崩溃

use std::backtrace::Backtrace;
use std::sync::Once;

static SET_HOOK: Once = Once::new();

/// 安装全局 panic 钩子
///
/// 捕获 panic 信息并记录到 tracing，而不是让进程直接崩溃
pub fn install_panic_hook() {
    SET_HOOK.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // 记录 panic 位置
            let location = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "unknown".to_string());

            // 记录 panic 消息
            let message = if let Some(s) = info.payload().downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = info.payload().downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic payload".to_string()
            };

            // 捕获完整 backtrace
            let backtrace = Backtrace::capture();
            let backtrace_str = match backtrace.status() {
                std::backtrace::BacktraceStatus::Captured => format!("{:?}", backtrace),
                _ => "backtrace unavailable (set RUST_BACKTRACE=1 to enable)".to_string(),
            };

            // 尝试获取当前 tokio task 信息（如果处于异步上下文）
            let task_info = std::thread::current()
                .name()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown task".to_string());

            // 结构化 panic 日志
            tracing::error!(
                target: "ccore::panic",
                location = %location,
                message = %message,
                task = %task_info,
                "🚨 PANIC caught - attempting graceful recovery"
            );

            // 记录完整 backtrace（单独一行，便于日志过滤）
            tracing::error!(
                target: "ccore::panic",
                backtrace = %backtrace_str,
                "panic backtrace"
            );

            // 调用默认钩子（打印到 stderr）
            default_hook(info);
        }));
    });
}
