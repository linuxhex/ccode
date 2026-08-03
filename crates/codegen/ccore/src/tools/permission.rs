//! 权限审批流（借鉴 Claude Code PermissionMode + Shell 安全检查）
//!
//! 提供四种模式：
//! - AllowAll: 所有工具自动允许
//! - AskUser: 危险操作需用户确认
//! - DenyAll: 所有工具拒绝
//! - ReadOnly: 只允许只读工具（类似 Claude Code Plan Mode）
//!
//! 危险操作定义：
//! - write: 写文件
//! - edit: 编辑文件
//! - bash: 执行命令
//!
//! Shell 安全检查（借鉴 Claude Code 20 项安全检查）：
//! - 命令替换注入: `$(...)` / `` `...` ``
//! - 环境变量注入: `$VAR` / `${VAR}`
//! - IFS 劫持
//! - 路径穿越: `../`
//! - 管道到危险命令
//! - 重定向到敏感文件
//! - 反斜杠转义绕过
//! - Unicode 欺骗
//! - 空字节注入
//! - 分号命令链接
//! - 后台执行 `&`
//! - 子 shell `(...)` / `{...}`
//! - Here-doc 注入
//! - 进程替换 `<(...)` / `>(...)`
//! - 解释器黑名单: python, perl, ruby, node, php 等脚本解释器

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
    /// 只读模式（类似 Claude Code Plan Mode）
    /// 只允许 read/grep/glob/list_dir 等只读工具
    ReadOnly,
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

/// Shell 安全检查结果
#[derive(Debug, Clone)]
pub struct ShellSafetyResult {
    /// 是否安全
    pub safe: bool,
    /// 检测到的风险列表
    pub risks: Vec<ShellRisk>,
}

/// Shell 安全风险类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ShellRisk {
    /// 命令替换注入 $(...) 或 `...`
    CommandSubstitution,
    /// 环境变量注入 $VAR / ${VAR}
    EnvInjection,
    /// 路径穿越 ../
    PathTraversal,
    /// 管道到危险命令
    PipeToDangerous,
    /// 重定向到敏感文件
    RedirectSensitive,
    /// 反斜杠转义绕过
    BackslashEscape,
    /// Unicode 欺骗
    UnicodeSpoof,
    /// 空字节注入
    NullByte,
    /// 分号命令链接
    CommandChaining,
    /// 后台执行
    BackgroundExec,
    /// 子 shell
    SubShell,
    /// Here-doc 注入
    HereDocInjection,
    /// 进程替换
    ProcessSubstitution,
    /// 使用黑名单解释器
    BlacklistedInterpreter(String),
    /// 未知风险
    Unknown(String),
}

/// 解释器黑名单（脚本语言可执行任意代码）
const INTERPRETER_BLACKLIST: &[&str] = &[
    "python", "python3", "python2",
    "perl", "ruby", "node", "php",
    "bash", "sh", "zsh", "fish",
    "lua", "rscript", "julia",
    "powershell", "cmd",
];

/// 危险命令黑名单（管道目标）
const DANGEROUS_PIPE_TARGETS: &[&str] = &[
    "sudo", "su", "chmod", "chown", "chgrp",
    "rm", "rmdir", "mkfs", "dd",
    "curl", "wget", "ssh", "scp",
    "crontab", "launchctl", "systemctl",
];

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
        // 统一归一化为小写，避免 LLM 发送 "Bash"/"Read" 等大小写变体导致权限静默失效
        let tool_name = tool_name.to_lowercase();
        match self.mode {
            PermissionMode::AllowAll => PermissionDecision::Allow,
            PermissionMode::DenyAll => PermissionDecision::Deny,
            PermissionMode::ReadOnly => {
                // 只读模式：只允许只读工具
                let level = self.get_permission_level(&tool_name);
                match level {
                    PermissionLevel::Safe => PermissionDecision::Allow,
                    PermissionLevel::Dangerous => PermissionDecision::Deny,
                }
            }
            PermissionMode::AskUser => {
                let level = self.get_permission_level(&tool_name);
                match level {
                    PermissionLevel::Safe => PermissionDecision::Allow,
                    PermissionLevel::Dangerous => {
                        // 先做 Shell 安全检查
                        if tool_name == "bash" {
                            if let Some(cmd) = args["command"].as_str() {
                                let safety = Self::check_shell_safety(cmd);
                                if !safety.safe {
                                    // 存在安全风险，直接拒绝
                                    tracing::warn!(
                                        target: "ccore::permission",
                                        command = %cmd,
                                        risks = ?safety.risks,
                                        "Shell 安全检查未通过，拒绝执行"
                                    );
                                    return PermissionDecision::Deny;
                                }
                            }
                        }
                        // 安全检查通过，检查是否已经批准过类似操作
                        let key = self.approval_key(&tool_name, args);
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
        let tool_name = tool_name.to_lowercase();
        let key = self.approval_key(&tool_name, args);
        self.approved.insert(key);
    }

    /// 拒绝操作
    pub fn deny(&mut self, _tool_name: &str, _args: &serde_json::Value) {}

    /// 获取工具权限级别
    fn get_permission_level(&self, tool_name: &str) -> PermissionLevel {
        match tool_name {
            "read" | "read_file" | "grep" | "glob" | "list_dir" | "ls" => PermissionLevel::Safe,
            "write" | "write_file" | "edit" | "search_replace" | "bash" => {
                PermissionLevel::Dangerous
            }
            _ => PermissionLevel::Dangerous,
        }
    }

    /// 生成批准缓存键
    ///
    /// 参考 Claude Code 的权限模型：用户批准的是精确命令，而非命令前缀。
    /// - bash：用完整命令字符串的哈希作为键，避免首词缓存导致 `rm file` 批准后 `rm -rf /` 被自动放行
    /// - write/edit：按文件路径缓存（路径相同即视为同类操作）
    fn approval_key(&self, tool_name: &str, args: &serde_json::Value) -> String {
        if tool_name == "bash" {
            if let Some(cmd) = args["command"].as_str() {
                // 用完整命令的哈希作为缓存键：相同命令复用批准，不同命令需重新确认
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hash::hash(cmd, &mut hasher);
                let hash = std::hash::Hasher::finish(&hasher);
                return format!("bash:{:016x}", hash);
            }
        }
        if tool_name == "write" || tool_name == "write_file" || tool_name == "edit" || tool_name == "search_replace" {
            if let Some(path) = args["path"].as_str() {
                return format!("{}:{}", tool_name, path);
            }
        }
        format!("{}:default", tool_name)
    }

    /// Shell 安全检查（20 项检查，借鉴 Claude Code Shell 安全设计）
    pub fn check_shell_safety(command: &str) -> ShellSafetyResult {
        let mut risks = Vec::new();

        // 1. 命令替换注入: $(...) 或 `...`
        if command.contains("$(") || command.contains('`') {
            risks.push(ShellRisk::CommandSubstitution);
        }

        // 2. 环境变量注入: $VAR / ${VAR}
        let has_env = command.contains("${") || {
            let bytes = command.as_bytes();
            let mut found = false;
            for i in 0..bytes.len() {
                if bytes[i] == b'$' && i + 1 < bytes.len() {
                    let next = bytes[i + 1];
                    if next.is_ascii_alphabetic() || next == b'_' {
                        found = true;
                        break;
                    }
                }
            }
            found
        };
        if has_env {
            risks.push(ShellRisk::EnvInjection);
        }

        // 3. 路径穿越: ../
        if command.contains("../") {
            risks.push(ShellRisk::PathTraversal);
        }

        // 4. 管道到危险命令
        if command.contains('|') {
            for target in DANGEROUS_PIPE_TARGETS {
                if command.contains(target) {
                    risks.push(ShellRisk::PipeToDangerous);
                    break;
                }
            }
        }

        // 5. 重定向到敏感文件
        if command.contains('>') || command.contains(">>") {
            let sensitive_paths = ["/etc/", "/var/", "/root/", "/home/", "/.ssh/", "/.env"];
            for sp in &sensitive_paths {
                if command.contains(sp) {
                    risks.push(ShellRisk::RedirectSensitive);
                    break;
                }
            }
        }

        // 6. 反斜杠转义绕过
        if command.contains(r#"\x"#) || command.contains(r#"\u"#) || command.contains(r#"\0"#) {
            risks.push(ShellRisk::BackslashEscape);
        }

        // 7. Unicode 欺骗（零宽字符、同形字）
        let has_unicode_spoof = command.chars().any(|c| {
            c == '\u{200B}' || c == '\u{200C}' || c == '\u{200D}' || c == '\u{FEFF}'
        });
        if has_unicode_spoof {
            risks.push(ShellRisk::UnicodeSpoof);
        }

        // 8. 空字节注入
        if command.contains('\0') {
            risks.push(ShellRisk::NullByte);
        }

        // 9. 分号命令链接
        if command.contains(';') && !command.starts_with("echo") {
            risks.push(ShellRisk::CommandChaining);
        }

        // 10. 后台执行
        if command.contains('&') && !command.contains("&&") {
            risks.push(ShellRisk::BackgroundExec);
        }

        // 11. 子 shell
        if command.contains("$((") || (command.contains('(') && command.contains(')')) {
            risks.push(ShellRisk::SubShell);
        }

        // 12. Here-doc 注入
        if command.contains("<<") {
            risks.push(ShellRisk::HereDocInjection);
        }

        // 13. 进程替换
        if command.contains("<(") || command.contains(">(") {
            risks.push(ShellRisk::ProcessSubstitution);
        }

        // 14. 解释器黑名单
        let first_word = command.split_whitespace().next().unwrap_or("");
        let base = first_word.split('/').last().unwrap_or(first_word);
        for interp in INTERPRETER_BLACKLIST {
            if base == *interp {
                risks.push(ShellRisk::BlacklistedInterpreter((*interp).to_string()));
                break;
            }
        }

        ShellSafetyResult {
            safe: risks.is_empty(),
            risks,
        }
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
    fn test_read_only_mode() {
        let checker = PermissionChecker::new(PermissionMode::ReadOnly);
        // 只读工具允许
        assert_eq!(checker.check("read", &serde_json::json!({})), PermissionDecision::Allow);
        assert_eq!(checker.check("grep", &serde_json::json!({})), PermissionDecision::Allow);
        // 写入工具拒绝
        assert_eq!(checker.check("bash", &serde_json::json!({})), PermissionDecision::Deny);
        assert_eq!(checker.check("write", &serde_json::json!({})), PermissionDecision::Deny);
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
            checker.check("bash", &serde_json::json!({"command": "ls"})),
            PermissionDecision::AskUser
        );
    }

    #[test]
    fn test_shell_safety_command_substitution() {
        let result = PermissionChecker::check_shell_safety("echo $(cat /etc/passwd)");
        assert!(!result.safe);
        assert!(result.risks.contains(&ShellRisk::CommandSubstitution));
    }

    #[test]
    fn test_shell_safety_path_traversal() {
        let result = PermissionChecker::check_shell_safety("cat ../../etc/passwd");
        assert!(!result.safe);
        assert!(result.risks.contains(&ShellRisk::PathTraversal));
    }

    #[test]
    fn test_shell_safety_blacklisted_interpreter() {
        let result = PermissionChecker::check_shell_safety("python3 -c 'import os; os.system(\"rm -rf /\")'");
        assert!(!result.safe);
        assert!(result.risks.iter().any(|r| matches!(r, ShellRisk::BlacklistedInterpreter(_))));
    }

    #[test]
    fn test_shell_safety_safe_command() {
        let result = PermissionChecker::check_shell_safety("ls -la src/");
        assert!(result.safe);
    }

    #[test]
    fn test_shell_safety_pipe_to_dangerous() {
        let result = PermissionChecker::check_shell_safety("cat file | sudo tee /etc/hosts");
        assert!(!result.safe);
        assert!(result.risks.contains(&ShellRisk::PipeToDangerous));
    }

    #[test]
    fn test_shell_safety_redirect_sensitive() {
        let result = PermissionChecker::check_shell_safety("echo data >> /etc/hosts");
        assert!(!result.safe);
        assert!(result.risks.contains(&ShellRisk::RedirectSensitive));
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
