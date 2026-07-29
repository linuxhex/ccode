//! 批量工具执行器
//!
//! 批量发送工具调用到消息总线，减少消息往返次数。

use anyhow::Result;
use std::future::Future;
use std::sync::Arc;
use tracing::{info, warn};

/// 批量工具调用请求
#[derive(Debug, Clone)]
pub struct BatchToolCall {
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// 批量工具执行结果
#[derive(Debug, Clone)]
pub struct BatchToolResult {
    pub tool_call_id: String,
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

/// 批量执行器配置
#[derive(Debug, Clone)]
pub struct BatchExecutorConfig {
    /// 批量大小
    pub batch_size: usize,
    /// 是否启用并行执行
    pub parallel: bool,
}

impl Default for BatchExecutorConfig {
    fn default() -> Self {
        Self {
            batch_size: 10,
            parallel: true,
        }
    }
}

/// 批量工具执行器
///
/// 收集多个工具调用，批量发送执行，减少消息总线往返。
pub struct BatchExecutor {
    config: BatchExecutorConfig,
}

impl BatchExecutor {
    /// 创建批量执行器
    pub fn new(config: BatchExecutorConfig) -> Self {
        Self { config }
    }

    /// 从默认配置创建
    pub fn default_executor() -> Self {
        Self::new(BatchExecutorConfig::default())
    }

    /// 批量执行工具调用
    ///
    /// # 参数
    /// - calls: 工具调用列表
    /// - executor: 单个工具执行函数（异步）
    ///
    /// # 返回
    /// - Ok(results) 所有工具执行完成
    /// - Err(e) 执行过程中发生错误
    pub async fn execute_batch<F, Fut>(
        &self,
        calls: Vec<BatchToolCall>,
        executor: F,
    ) -> Result<Vec<BatchToolResult>>
    where
        F: Fn(BatchToolCall) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<BatchToolResult>> + Send,
    {
        if calls.is_empty() {
            return Ok(Vec::new());
        }

        let executor = Arc::new(executor);
        let mut results = Vec::with_capacity(calls.len());

        if self.config.parallel {
            // 并行执行
            let mut futures = Vec::with_capacity(calls.len());
            for call in calls {
                let exec = executor.clone();
                futures.push(tokio::spawn(async move { exec(call).await }));
            }

            for f in futures {
                match f.await {
                    Ok(Ok(result)) => results.push(result),
                    Ok(Err(e)) => {
                        warn!("批量执行中工具失败：{}", e);
                        results.push(BatchToolResult {
                            tool_call_id: String::new(),
                            success: false,
                            output: String::new(),
                            error: Some(e.to_string()),
                        });
                    }
                    Err(e) => {
                        warn!("批量执行任务 panic：{}", e);
                    }
                }
            }
        } else {
            // 顺序执行
            for call in calls {
                match executor(call).await {
                    Ok(result) => results.push(result),
                    Err(e) => {
                        warn!("工具执行失败：{}", e);
                    }
                }
            }
        }

        info!(
            total = results.len(),
            success = results.iter().filter(|r| r.success).count(),
            "批量工具执行完成"
        );

        Ok(results)
    }

    /// 获取配置
    pub fn config(&self) -> &BatchExecutorConfig {
        &self.config
    }
}