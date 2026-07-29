//! 工具级沙箱（借鉴 Claude Code ccode-sandbox 设计）
//!
//! Claude Code 使用 OS 级沙箱（Landlock/Seatbelt/bwrap）隔离文件系统访问。
//! ccore 采用轻量级工具级沙箱，在 ToolBridge 层面控制工具行为：
//! - 路径白名单/黑名单
//! - 网络访问控制
//! - 命令执行限制
//!
//! 沙箱在工具执行前检查，而非 OS 层面拦截。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 沙箱配置文件名
pub const SANDBOX_CONFIG_FILE: &str = "sandbox.toml";

/// 沙箱档案名（对应 Claude Code 的 ProfileName）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxProfileName {
    /// 关闭沙箱
    Off,
    /// 工作区只读（默认）：允许读取所有文件，只允许写入工作区内
    WorkspaceReadOnly,
    /// 工作区读写：允许读写工作区内所有文件
    Workspace,
    /// 严格模式：只能读写明确允许的路径
    Strict,
    /// 自定义档案
    Custom(String),
}

impl Default for SandboxProfileName {
    fn default() -> Self {
        Self::WorkspaceReadOnly
    }
}

/// 沙箱档案（对应 Claude Code 的 SandboxProfile）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxProfile {
    /// 档案名
    pub name: SandboxProfileName,
    /// 允许写入的路径前缀列表（空 = 不限制）
    pub allow_write_prefixes: Vec<String>,
    /// 允许读取的路径前缀列表（空 = 不限制）
    pub allow_read_prefixes: Vec<String>,
    /// 拒绝访问的路径前缀列表（优先级最高）
    pub deny_prefixes: Vec<String>,
    /// 是否允许执行 shell 命令
    pub allow_shell: bool,
    /// 是否允许网络访问
    pub allow_network: bool,
    /// shell 命令黑名单（禁止执行的命令前缀）
    pub shell_deny_prefixes: Vec<String>,
}

impl Default for SandboxProfile {
    fn default() -> Self {
        Self {
            name: SandboxProfileName::WorkspaceReadOnly,
            allow_write_prefixes: Vec::new(),  // 空表示由 ToolBridge 动态设置工作区
            allow_read_prefixes: Vec::new(),    // 空表示不限制
            deny_prefixes: vec![
                "/etc/shadow".into(),
                "/etc/passwd".into(),
                "/root/.ssh".into(),
                "/home/*/.ssh".into(),
            ],
            allow_shell: true,
            allow_network: true,
            shell_deny_prefixes: vec![
                "rm -rf /".into(),
                "mkfs".into(),
                "dd if=".into(),
            ],
        }
    }
}

/// 沙箱检查结果
#[derive(Debug, Clone)]
pub enum SandboxCheckResult {
    /// 允许执行
    Allowed,
    /// 拒绝执行，附带原因
    Denied(String),
    /// 允许但需用户确认（对敏感操作）
    RequiresConfirmation(String),
}

/// 工具级沙箱管理器
pub struct ToolSandbox {
    /// 当前档案
    profile: SandboxProfile,
    /// 工作区路径（动态设置）
    workspace_path: Option<PathBuf>,
}

impl ToolSandbox {
    pub fn new(profile: SandboxProfile) -> Self {
        Self {
            profile,
            workspace_path: None,
        }
    }

    /// 设置工作区路径
    pub fn set_workspace(&mut self, path: PathBuf) {
        self.workspace_path = Some(path);
    }

    /// 获取当前档案
    pub fn profile(&self) -> &SandboxProfile {
        &self.profile
    }

    /// 检查文件路径访问权限
    pub fn check_file_access(
        &self,
        path: &str,
        operation: FileAccessOperation,
    ) -> SandboxCheckResult {
        let _resolved = Path::new(path);

        // 1. 检查拒绝列表（最高优先级）
        for deny in &self.profile.deny_prefixes {
            if path.starts_with(deny) || Self::glob_matches(path, deny) {
                return SandboxCheckResult::Denied(
                    format!("沙箱拒绝：路径 {} 匹配拒绝规则 {}", path, deny)
                );
            }
        }

        // 2. 根据操作类型检查
        match operation {
            FileAccessOperation::Read => {
                // 读取：检查 allow_read_prefixes
                if !self.profile.allow_read_prefixes.is_empty() {
                    let allowed = self.profile.allow_read_prefixes.iter()
                        .any(|prefix| path.starts_with(prefix));
                    if !allowed {
                        return SandboxCheckResult::Denied(
                            format!("沙箱拒绝：路径 {} 不在读取白名单中", path)
                        );
                    }
                }
            }
            FileAccessOperation::Write => {
                // 写入：检查是否在工作区内
                match self.profile.name {
                    SandboxProfileName::Off => {}
                    SandboxProfileName::WorkspaceReadOnly => {
                        // 只允许写入工作区内
                        if let Some(ws) = &self.workspace_path {
                            if !path.starts_with(ws.to_string_lossy().as_ref()) {
                                return SandboxCheckResult::Denied(
                                    format!("沙箱拒绝：WorkspaceReadOnly 模式下不允许写入工作区外路径 {}", path)
                                );
                            }
                        }
                    }
                    SandboxProfileName::Workspace => {
                        if let Some(ws) = &self.workspace_path {
                            if !path.starts_with(ws.to_string_lossy().as_ref()) {
                                return SandboxCheckResult::RequiresConfirmation(
                                    format!("沙箱提示：写入路径 {} 在工作区外，建议确认", path)
                                );
                            }
                        }
                    }
                    SandboxProfileName::Strict => {
                        let allowed = self.profile.allow_write_prefixes.iter()
                            .any(|prefix| path.starts_with(prefix));
                        if !allowed {
                            return SandboxCheckResult::Denied(
                                format!("沙箱拒绝：Strict 模式下路径 {} 不在写入白名单中", path)
                            );
                        }
                    }
                    SandboxProfileName::Custom(_) => {
                        if !self.profile.allow_write_prefixes.is_empty() {
                            let allowed = self.profile.allow_write_prefixes.iter()
                                .any(|prefix| path.starts_with(prefix));
                            if !allowed {
                                return SandboxCheckResult::Denied(
                                    format!("沙箱拒绝：路径 {} 不在写入白名单中", path)
                                );
                            }
                        }
                    }
                }
            }
        }

        SandboxCheckResult::Allowed
    }

    /// 检查 shell 命令权限
    pub fn check_shell_command(&self, command: &str) -> SandboxCheckResult {
        if !self.profile.allow_shell {
            return SandboxCheckResult::Denied("沙箱拒绝：当前档案禁止执行 shell 命令".into());
        }

        // 检查命令黑名单
        for deny in &self.profile.shell_deny_prefixes {
            if command.starts_with(deny) || command.contains(deny) {
                return SandboxCheckResult::Denied(
                    format!("沙箱拒绝：命令匹配黑名单规则 '{}'", deny)
                );
            }
        }

        SandboxCheckResult::Allowed
    }

    /// 检查网络访问权限
    pub fn check_network_access(&self) -> SandboxCheckResult {
        if !self.profile.allow_network {
            return SandboxCheckResult::Denied("沙箱拒绝：当前档案禁止网络访问".into());
        }
        SandboxCheckResult::Allowed
    }

    /// 简单的 glob 匹配（仅支持 * 通配符）
    fn glob_matches(path: &str, pattern: &str) -> bool {
        if !pattern.contains('*') {
            return false;
        }
        // 简单实现：检查去掉 * 后的前缀/后缀是否在路径中出现
        path.contains(&pattern.replace('*', ""))
    }
}

/// 文件访问操作类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAccessOperation {
    Read,
    Write,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workspace_read_only_denies_write_outside() {
        let mut sandbox = ToolSandbox::new(SandboxProfile::default());
        sandbox.set_workspace(PathBuf::from("/home/user/project"));

        let result = sandbox.check_file_access("/etc/config", FileAccessOperation::Write);
        assert!(matches!(result, SandboxCheckResult::Denied(_)));
    }

    #[test]
    fn test_workspace_read_only_allows_write_inside() {
        let mut sandbox = ToolSandbox::new(SandboxProfile::default());
        sandbox.set_workspace(PathBuf::from("/home/user/project"));

        let result = sandbox.check_file_access("/home/user/project/src/main.rs", FileAccessOperation::Write);
        assert!(matches!(result, SandboxCheckResult::Allowed));
    }

    #[test]
    fn test_deny_list_takes_priority() {
        let sandbox = ToolSandbox::new(SandboxProfile::default());

        let result = sandbox.check_file_access("/etc/shadow", FileAccessOperation::Read);
        assert!(matches!(result, SandboxCheckResult::Denied(_)));
    }

    #[test]
    fn test_shell_deny_list() {
        let sandbox = ToolSandbox::new(SandboxProfile::default());

        let result = sandbox.check_shell_command("rm -rf /");
        assert!(matches!(result, SandboxCheckResult::Denied(_)));
    }

    #[test]
    fn test_off_profile_allows_everything() {
        let sandbox = ToolSandbox::new(SandboxProfile {
            name: SandboxProfileName::Off,
            allow_write_prefixes: vec![],
            allow_read_prefixes: vec![],
            deny_prefixes: vec![],
            allow_shell: true,
            allow_network: true,
            shell_deny_prefixes: vec![],
        });

        let result = sandbox.check_file_access("/etc/shadow", FileAccessOperation::Write);
        assert!(matches!(result, SandboxCheckResult::Allowed));
    }

    #[test]
    fn test_strict_profile_denies_unlisted() {
        let sandbox = ToolSandbox::new(SandboxProfile {
            name: SandboxProfileName::Strict,
            allow_write_prefixes: vec!["/tmp/sandbox".into()],
            allow_read_prefixes: vec![],
            deny_prefixes: vec![],
            allow_shell: false,
            allow_network: false,
            shell_deny_prefixes: vec![],
        });

        let result = sandbox.check_file_access("/tmp/sandbox/test.rs", FileAccessOperation::Write);
        assert!(matches!(result, SandboxCheckResult::Allowed));

        let result = sandbox.check_file_access("/tmp/other", FileAccessOperation::Write);
        assert!(matches!(result, SandboxCheckResult::Denied(_)));
    }

    #[test]
    fn test_network_denied() {
        let sandbox = ToolSandbox::new(SandboxProfile {
            name: SandboxProfileName::Strict,
            allow_write_prefixes: vec![],
            allow_read_prefixes: vec![],
            deny_prefixes: vec![],
            allow_shell: false,
            allow_network: false,
            shell_deny_prefixes: vec![],
        });

        let result = sandbox.check_network_access();
        assert!(matches!(result, SandboxCheckResult::Denied(_)));
    }
}
