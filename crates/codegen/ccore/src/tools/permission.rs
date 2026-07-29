//! 权限审批流（借鉴 Claude Code PermissionMode）
//!
//! 提供三种模式：
//! - AllowAll: 所有工具自动允许
//! - AskUser: 危险操作需用户确认
//! - DenyAll: 所有工具拒绝
//!
//! 危险操作定义：
//! - write: 写文件
//! - edit: 编辑文件
//! - bash: 执行命令

use serde::{Deserialize, Serialize};

/// 权限模式
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PermissionMode {
    /// 所有工具自动允许
    AllowAll,
    /// 危险操作需用户确认
    AskUser,
    /// 所有工具拒绝
    DenyAll,
}

/// 工具权限级别
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum PermissionLevel {
    /// 安全（只读）
    Safe,
    /// 危险（写入/执行）
    Dangerous,
}

/// 权限决策
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PermissionDecision {
    /// 允许
    Allow,
    /// 拒绝
    Deny,
    /// 需要用户确认
    AskUser,
}

/// 工具权限检查器
pub struct PermissionChecker {
    mode: PermissionMode,
    /// 已批准的操作（会话级缓存）
    approved: std::collections::HashSet<String>,
}

impl PermissionChecker {
    pub fn new(mode: PermissionMode) -> Self {
        Self {
            mode,
            approved: std::collections::HashSet::new(),
        }
    }

    /// 检查工具权限
    pub fn check(&self, tool_name: &str, args: &serde_json::Value) -> PermissionDecision {
        match self.mode {
            PermissionMode::AllowAll => PermissionDecision::Allow,
            PermissionMode::DenyAll => PermissionDecision::Deny,
            PermissionMode::AskUser => {
                let level = self.get_permission_level(tool_name);
                match level {
                    PermissionLevel::Safe => PermissionDecision::Allow,
                    PermissionLevel::Dangerous => {
                        // 检查是否已经批准过类似操作
                        let key = self.approval_key(tool_name, args);
                        if self.approved.contains(&key) {
                            PermissionDecision::Allow
                        } else {
                            PermissionDecision::AskUser
                        }
                    }
                }
            }
        }
    }

    /// 批准操作（用户确认后调用）
    pub fn approve(&mut self, tool_name: &str, args: &serde_json::Value) {
        let key = self.approval_key(tool_name, args);
        self.approved.insert(key);
    }

    /// 拒绝操作
    pub fn deny(&mut self, _tool_name: &str, _args: &serde_json::Value) {
        // 不做任何事，未批准的操作自然会被拒绝
    }

    /// 获取工具权限级别
    fn get_permission_level(&self, tool_name: &str) -> PermissionLevel {
        match tool_name {
            "read" | "read_file" | "grep" | "glob" | "list_dir" => PermissionLevel::Safe,
            "write" | "write_file" | "edit" | "search_replace" | "bash" => {
                PermissionLevel::Dangerous
            }
            _ => PermissionLevel::Dangerous, // 未知工具默认危险
        }
    }

    /// 生成批准缓存键
    fn approval_key(&self, tool_name: &str, args: &serde_json::Value) -> String {
        // 对 bash：按命令前缀缓存（同一命令不同参数需重新确认）
        if tool_name == "bash" {
            if let Some(cmd) = args["command"].as_str() {
                // 取命令的第一个词作为键
                let first_word = cmd.split_whitespace().next().unwrap_or(cmd);
                return format!("bash:{}", first_word);
            }
        }
        // 对 write/edit：按文件路径缓存
        if tool_name == "write" || tool_name == "write_file" || tool_name == "edit" || tool_name == "search_replace" {
            if let Some(path) = args["path"].as_str() {
                return format!("{}:{}", tool_name, path);
            }
        }
        format!("{}:default", tool_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allow_all() {
        let checker = PermissionChecker::new(PermissionMode::AllowAll);
        assert_eq!(
            checker.check("bash", &serde_json::json!({})),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn test_deny_all() {
        let checker = PermissionChecker::new(PermissionMode::DenyAll);
        assert_eq!(
            checker.check("read", &serde_json::json!({})),
            PermissionDecision::Deny
        );
    }

    #[test]
    fn test_ask_user_safe_tool() {
        let checker = PermissionChecker::new(PermissionMode::AskUser);
        assert_eq!(
            checker.check("read", &serde_json::json!({})),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn test_ask_user_dangerous_tool() {
        let checker = PermissionChecker::new(PermissionMode::AskUser);
        assert_eq!(
            checker.check("bash", &serde_json::json!({})),
            PermissionDecision::AskUser
        );
    }

    #[test]
    fn test_approve_caches() {
        let mut checker = PermissionChecker::new(PermissionMode::AskUser);
        let args = serde_json::json!({"command": "ls -la"});
        assert_eq!(checker.check("bash", &args), PermissionDecision::AskUser);
        checker.approve("bash", &args);
        assert_eq!(checker.check("bash", &args), PermissionDecision::Allow);
    }

    #[test]
    fn test_permission_level() {
        let checker = PermissionChecker::new(PermissionMode::AllowAll);
        assert_eq!(
            checker.get_permission_level("read"),
            PermissionLevel::Safe
        );
        assert_eq!(
            checker.get_permission_level("bash"),
            PermissionLevel::Dangerous
        );
        assert_eq!(
            checker.get_permission_level("write"),
            PermissionLevel::Dangerous
        );
    }
}
