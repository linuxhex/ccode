//! rustfmt 检查钩子 - Write/Edit 工具执行后对 .rs 文件运行 rustfmt --check
//!
//! 作为 PostExecuteHook 注册到 ToolBridge：
//! - 仅对 write/edit 工具触发
//! - 仅对 .rs 文件检查
//! - 检查失败不阻塞工具执行，只把格式问题追加到工具输出作为警告

use std::future::Future;
use std::pin::Pin;

use super::bridge::PostExecuteHook;
use super::compile_feedback::run_rustfmt_check;

/// rustfmt 检查钩子
/// Write/Edit 工具执行后，对修改的文件运行 rustfmt --check
pub struct RustfmtHook;

impl PostExecuteHook for RustfmtHook {
    fn should_run(&self, tool_name: &str) -> bool {
        tool_name == "write" || tool_name == "edit"
    }

    fn run<'a>(
        &'a self,
        _tool_name: &str,
        args: &'a serde_json::Value,
        _result: &'a str,
    ) -> Pin<Box<dyn Future<Output = String> + Send + 'a>> {
        Box::pin(async move {
            // 从 args 提取文件路径（write/edit 的 path 参数，兼容 file_path）
            let filepath = args
                .get("file_path")
                .or_else(|| args.get("path"))
                .and_then(|v| v.as_str());

            if let Some(path) = filepath {
                let path = std::path::Path::new(path);
                // 只对 .rs 文件检查
                if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    match run_rustfmt_check(path).await {
                        Ok(Some(msg)) => {
                            // 格式不规范：把问题追加到工具结果，不阻断流程
                            return format!(
                                "\n[rustfmt 检查] 文件格式不符合 rustfmt 规范：\n{}",
                                msg
                            );
                        }
                        Ok(None) => {} // 检查通过
                        Err(e) => {
                            // 检查自身失败（如 rustfmt 未安装）：仅记录日志，不阻塞工具
                            tracing::warn!("rustfmt 检查失败：{}", e);
                        }
                    }
                }
            }
            String::new()
        })
    }
}
