//! 自动验证循环 - 编辑后自动编译/测试验证

use tokio::process::Command;

use serde::{Deserialize, Serialize};

/// 验证请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyRequest {
    /// 验证类型
    pub verify_type: VerifyType,
    /// 工作目录
    pub working_dir: String,
    /// 最大重试次数
    pub max_retries: u32,
    /// 自定义验证命令（仅 VerifyType::Custom 时使用）
    pub custom_command: Option<String>,
    /// 自定义命令参数
    pub custom_args: Option<Vec<String>>,
}

/// 验证类型
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum VerifyType {
    /// cargo check
    RustCheck,
    /// cargo test
    RustTest,
    /// cargo clippy
    RustClippy,
    /// npm run build
    NpmBuild,
    /// npm run test
    NpmTest,
    /// 自定义命令
    Custom,
}

/// 验证结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyResult {
    /// 是否成功
    pub success: bool,
    /// 验证输出（包含 stdout + stderr）
    pub output: String,
    /// 已使用重试次数
    pub retries_used: u32,
    /// 各次验证的详细结果
    pub attempts: Vec<VerifyAttempt>,
}

/// 单次验证尝试结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyAttempt {
    /// 第几次尝试（从 1 开始）
    pub attempt: u32,
    /// 是否成功
    pub success: bool,
    /// 命令输出
    pub output: String,
    /// 执行耗时（毫秒）
    pub duration_ms: u64,
}

/// 自动验证控制器
pub struct VerifyController {
    #[allow(dead_code)]
    max_retries: u32,
}

impl VerifyController {
    pub fn new(max_retries: u32) -> Self {
        Self { max_retries }
    }

    /// 执行验证循环：验证 → 失败 → 修 → 再验证（最多 max_retries 次）
    ///
    /// 每次验证执行对应的构建/测试命令，收集 stdout 和 stderr。
    /// 失败时记录错误输出，调用方可将错误反馈给 Agent 进行修复后再次验证。
    pub async fn verify(&self, request: &VerifyRequest) -> anyhow::Result<VerifyResult> {
        let mut attempts = Vec::new();
        let mut retries_used = 0u32;
        let mut last_success = false;
        let mut last_output = String::new();

        for attempt in 1..=request.max_retries.max(1) {
            let result = self.run_verify_command(request).await?;
            last_success = result.success;
            last_output = result.output.clone();

            attempts.push(VerifyAttempt {
                attempt,
                success: result.success,
                output: result.output.clone(),
                duration_ms: result.duration_ms,
            });

            if result.success {
                // 验证通过，无需重试
                retries_used = attempt - 1;
                break;
            }

            retries_used = attempt;

            // 首次失败后直接返回，由调用方（Agent）决定是否修复后重新验证
            // 如果 max_retries > 1，调用方可多次调用 verify 实现重试
            if attempt >= request.max_retries {
                break;
            }
        }

        Ok(VerifyResult {
            success: last_success,
            output: last_output,
            retries_used,
            attempts,
        })
    }

    /// 执行单次验证命令
    async fn run_verify_command(&self, request: &VerifyRequest) -> anyhow::Result<VerifyAttempt> {
        let start = std::time::Instant::now();

        let (program, args) = match request.verify_type {
            VerifyType::RustCheck => ("cargo", vec!["check".to_string()]),
            VerifyType::RustTest => ("cargo", vec!["test".to_string()]),
            VerifyType::RustClippy => ("cargo", vec!["clippy".to_string(), "--".to_string(), "-D".to_string(), "warnings".to_string()]),
            VerifyType::NpmBuild => ("npm", vec!["run".to_string(), "build".to_string()]),
            VerifyType::NpmTest => ("npm", vec!["run".to_string(), "test".to_string()]),
            VerifyType::Custom => {
                let cmd = request.custom_command.as_deref()
                    .ok_or_else(|| anyhow::anyhow!("Custom 验证类型需要提供 custom_command"))?;
                let cmd_args = request.custom_args.as_deref().unwrap_or(&[]);
                (cmd, cmd_args.to_vec())
            }
        };

        // 执行验证命令
        let output = Command::new(program)
            .args(&args)
            .current_dir(&request.working_dir)
            .env("TERM", "dumb") // 避免终端颜色码干扰输出解析
            .output()
            .await?;

        let duration_ms = start.elapsed().as_millis() as u64;
        let success = output.status.success();

        // 合并 stdout 和 stderr
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined_output = if stderr.is_empty() {
            stdout.into_owned()
        } else if stdout.is_empty() {
            stderr.into_owned()
        } else {
            format!("{}\n{}", stdout, stderr)
        };

        Ok(VerifyAttempt {
            attempt: 0, // 由调用方设置
            success,
            output: combined_output,
            duration_ms,
        })
    }
}
