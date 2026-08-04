//! 并发控制
//!
//! 使用 Semaphore 限制并发 Agent 数量，防止资源耗尽。

use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{debug, warn};

/// 并发控制配置
#[derive(Debug, Clone)]
pub struct ConcurrencyConfig {
    /// 最大并发 Agent 数
    pub max_concurrent_agents: usize,
    /// 最大并发工具调用数
    pub max_concurrent_tools: usize,
}

impl Default for ConcurrencyConfig {
    fn default() -> Self {
        Self {
            max_concurrent_agents: 10,
            max_concurrent_tools: 20,
        }
    }
}

/// 并发控制器
///
/// 使用 Semaphore 控制 Agent 和工具调用的并发数量。
pub struct ConcurrencyController {
    /// Agent 并发信号量
    agent_semaphore: Arc<Semaphore>,
    /// 工具并发信号量
    tool_semaphore: Arc<Semaphore>,
    /// 配置
    config: ConcurrencyConfig,
}

impl ConcurrencyController {
    /// 创建并发控制器
    pub fn new(config: ConcurrencyConfig) -> Self {
        Self {
            agent_semaphore: Arc::new(Semaphore::new(config.max_concurrent_agents)),
            tool_semaphore: Arc::new(Semaphore::new(config.max_concurrent_tools)),
            config,
        }
    }

    /// 从默认配置创建
    pub fn default_controller() -> Self {
        Self::new(ConcurrencyConfig::default())
    }

    /// 获取 Agent 并发许可
    ///
    /// 超过最大并发数时等待
    pub async fn acquire_agent(&self) -> Result<tokio::sync::SemaphorePermit<'_>, ()> {
        match self.agent_semaphore.acquire().await {
            Ok(permit) => {
                debug!(
                    available = self.agent_semaphore.available_permits(),
                    "获取 Agent 并发许可"
                );
                Ok(permit)
            }
            Err(e) => {
                warn!("获取 Agent 并发许可失败：{}", e);
                Err(())
            }
        }
    }

    /// 获取工具并发许可
    ///
    /// 超过最大并发数时等待
    pub async fn acquire_tool(&self) -> Result<tokio::sync::SemaphorePermit<'_>, ()> {
        match self.tool_semaphore.acquire().await {
            Ok(permit) => {
                debug!(
                    available = self.tool_semaphore.available_permits(),
                    "获取工具并发许可"
                );
                Ok(permit)
            }
            Err(e) => {
                warn!("获取工具并发许可失败：{}", e);
                Err(())
            }
        }
    }

    /// 尝试获取 Agent 并发许可（非阻塞）
    ///
    /// 无可用许可时立即返回错误
    pub fn try_acquire_agent(&self) -> Option<tokio::sync::SemaphorePermit<'_>> {
        self.agent_semaphore.try_acquire().ok()
    }

    /// 尝试获取工具并发许可（非阻塞）
    pub fn try_acquire_tool(&self) -> Option<tokio::sync::SemaphorePermit<'_>> {
        self.tool_semaphore.try_acquire().ok()
    }

    /// 获取 Agent 可用许可数
    pub fn available_agent_permits(&self) -> usize {
        self.agent_semaphore.available_permits()
    }

    /// 获取工具可用许可数
    pub fn available_tool_permits(&self) -> usize {
        self.tool_semaphore.available_permits()
    }

    /// 获取配置
    pub fn config(&self) -> &ConcurrencyConfig {
        &self.config
    }
}