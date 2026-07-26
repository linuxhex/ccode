//! 配置系统 - ~/.ccode/config.toml

pub mod provider;
pub mod memory;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::node::PermissionMode;

/// ccode 全局配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CcodeConfig {
    /// Provider 配置列表
    pub providers: Vec<provider::ProviderConfig>,
    /// 默认模型
    pub default_model: String,
    /// 默认 Agent 类型
    pub default_agent_type: String,
    /// 记忆系统配置
    pub memory: memory::MemoryConfig,
    /// 权限模式
    pub permission_mode: PermissionMode,
    /// 最大子 Agent 数量
    pub max_subagents: usize,
}

impl Default for CcodeConfig {
    fn default() -> Self {
        Self {
            providers: vec![provider::ProviderConfig::default_ccode()],
            default_model: "ccode-3-fast".into(),
            default_agent_type: "primary".into(),
            memory: memory::MemoryConfig::default(),
            permission_mode: PermissionMode::Trust,
            max_subagents: 10,
        }
    }
}

impl CcodeConfig {
    /// 从文件加载配置
    pub fn load_from_file(path: &PathBuf) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: CcodeConfig = toml::from_str(&content)?;
        Ok(config)
    }

    /// 保存配置到文件
    pub fn save_to_file(&self, path: &PathBuf) -> anyhow::Result<()> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// 获取默认配置路径
    pub fn default_config_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".ccode").join("config.toml")
    }
}
