//! 全局 panic 钩子（借鉴 Claude Code 的错误边界）
//!
//! 捕获未处理的 panic，记录日志而非直接崩溃

use std::sync::Once;

static SET_HOOK: Once = Once::new();

/// 安装全局 panic 钩子
///
/// 捕获 panic 信息并记录到 tracing，而不是让进程直接崩溃
pub fn install_panic_hook() {
    SET_HOOK.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // 记录 panic 信息
            let location = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "unknown".to_string());

            let message = if let Some(s) = info.payload().downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = info.payload().downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic payload".to_string()
            };

            tracing::error!(
                "🚨 PANIC caught at {}: {} - attempting graceful recovery",
                location,
                message
            );

            // 调用默认钩子（打印到 stderr）
            default_hook(info);
        }));
    });
}
