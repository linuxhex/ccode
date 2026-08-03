//! # 权限规则引擎
//!
//! 提供 allow/deny/ask 三级权限决策，基于工具名和参数模式匹配。
//! 规则按顺序求值，第一个匹配的先生效（deny 优先级最高），
//! 无规则匹配时默认返回 Ask（需用户确认）。

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

// ── 核心类型 ──────────────────────────────────────────────────────────────

/// 权限决策结果
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionDecision {
    /// 允许执行
    Allow,
    /// 拒绝执行
    Deny { reason: String },
    /// 需要用户确认
    Ask { reason: String },
}

impl fmt::Display for PermissionDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allow => write!(f, "allow"),
            Self::Deny { reason } => write!(f, "deny: {reason}"),
            Self::Ask { reason } => write!(f, "ask: {reason}"),
        }
    }
}

/// 工具匹配模式，支持通配符
///
/// 语法：`ToolName(pattern)`，如：
/// - `Bash(git *)` — 匹配参数以 "git " 开头的 Bash 调用
/// - `Write(src/**)` — 匹配写入 src 目录下的文件
/// - `Read(*)` — 匹配所有 Read 调用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPattern {
    /// 工具名称，支持通配符 * 和 **
    pub tool: String,
    /// 参数匹配模式（可选），支持 glob 风格通配符
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arg_pattern: Option<String>,
}

impl ToolPattern {
    /// 判断给定的工具名和输入是否匹配此模式
    ///
    /// - 工具名使用 glob 风格匹配：`*` 匹配单层，`**` 匹配多层
    /// - 参数模式同样支持 glob 风格：`*` 匹配不含路径分隔符的任意字符，
    ///   `**` 匹配任意路径层级
    /// - 无参数模式时只匹配工具名
    pub fn matches(&self, tool_name: &str, tool_input: &serde_json::Value) -> bool {
        // 先匹配工具名
        if !glob_match(&self.tool, tool_name) {
            return false;
        }

        // 无参数模式则只要工具名匹配即可
        let Some(ref arg_pat) = self.arg_pattern else {
            return true;
        };

        // 从工具输入中提取待匹配的文本
        let arg_text = extract_arg_text(tool_input);
        glob_match(arg_pat, &arg_text)
    }
}

/// 权限规则
///
/// 每条规则指定一个匹配模式和一个动作：
/// - Allow：直接放行
/// - Deny：拒绝执行并给出原因
/// - Ask：需要用户确认
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PermissionRule {
    Allow {
        pattern: ToolPattern,
    },
    Deny {
        pattern: ToolPattern,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Ask {
        pattern: ToolPattern,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl PermissionRule {
    /// 获取规则的模式引用
    pub fn pattern(&self) -> &ToolPattern {
        match self {
            Self::Allow { pattern } => pattern,
            Self::Deny { pattern, .. } => pattern,
            Self::Ask { pattern, .. } => pattern,
        }
    }

    /// 判断此规则是否匹配给定的工具调用
    pub fn matches(&self, tool_name: &str, tool_input: &serde_json::Value) -> bool {
        self.pattern().matches(tool_name, tool_input)
    }

    /// 将匹配结果转为决策
    fn to_decision(&self) -> PermissionDecision {
        match self {
            Self::Allow { .. } => PermissionDecision::Allow,
            Self::Deny { reason, .. } => PermissionDecision::Deny {
                reason: reason.clone().unwrap_or_else(|| "被规则拒绝".to_string()),
            },
            Self::Ask { reason, .. } => PermissionDecision::Ask {
                reason: reason.clone().unwrap_or_else(|| "需要用户确认".to_string()),
            },
        }
    }
}

/// 规则集
///
/// 规则按列表顺序求值，第一个匹配的先生效。
/// deny 规则优先级最高：即使后面有 allow 规则匹配，deny 仍然生效。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRuleSet {
    /// 规则列表，按优先级排序（先匹配的先生效）
    pub rules: Vec<PermissionRule>,
}

impl PermissionRuleSet {
    /// 对给定的工具调用求值，返回权限决策
    ///
    /// 求值逻辑：
    /// 1. 先扫描所有 deny 规则，若有匹配则直接拒绝（deny 优先级最高）
    /// 2. 按顺序扫描所有规则，第一个非 deny 匹配的先生效
    /// 3. 无规则匹配时默认返回 Ask
    pub fn evaluate(&self, tool_name: &str, tool_input: &serde_json::Value) -> PermissionDecision {
        // 第一遍：deny 优先扫描，确保安全基线不被后续规则覆盖
        for rule in &self.rules {
            if let PermissionRule::Deny { .. } = rule {
                if rule.matches(tool_name, tool_input) {
                    return rule.to_decision();
                }
            }
        }

        // 第二遍：按顺序匹配 allow / ask 规则
        for rule in &self.rules {
            match rule {
                PermissionRule::Allow { .. } | PermissionRule::Ask { .. } => {
                    if rule.matches(tool_name, tool_input) {
                        return rule.to_decision();
                    }
                }
                PermissionRule::Deny { .. } => {
                    // deny 已在第一遍处理，跳过
                }
            }
        }

        // 无规则匹配，默认需要用户确认
        PermissionDecision::Ask {
            reason: format!("工具 '{tool_name}' 无匹配规则，需要用户确认"),
        }
    }

    /// 从 JSON 文件加载规则集
    pub fn load_from_file(path: &Path) -> Result<Self, RuleLoadError> {
        let content = std::fs::read_to_string(path).map_err(|source| RuleLoadError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        let rule_set: PermissionRuleSet =
            serde_json::from_str(&content).map_err(|source| RuleLoadError::ParseFile {
                path: path.to_path_buf(),
                detail: source.to_string(),
            })?;
        Ok(rule_set)
    }

    /// 从用户主目录和项目目录加载规则集
    ///
    /// 加载路径：
    /// 1. `~/.ccode/rules.json`（全局规则）
    /// 2. `.ccode/rules/` 目录下所有 `.json` 文件（项目级规则）
    ///
    /// 项目级规则追加在全局规则之后，优先级更低。
    /// 任一文件加载失败不会阻止其余文件的加载。
    pub fn load_from_dirs(project_root: &Path) -> Self {
        let mut rules = Vec::new();

        // 加载全局规则：~/.ccode/rules.json
        if let Some(home) = ccode_config::user_ccode_home() {
            let global_path = home.join("rules.json");
            if global_path.exists() {
                match Self::load_from_file(&global_path) {
                    Ok(rule_set) => rules.extend(rule_set.rules),
                    Err(e) => {
                        tracing::warn!(
                            path = %global_path.display(),
                            error = %e,
                            "权限规则：加载全局规则文件失败"
                        );
                    }
                }
            }
        }

        // 加载项目级规则：.ccode/rules/*.json
        let project_rules_dir = project_root.join(".ccode").join("rules");
        if project_rules_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&project_rules_dir) {
                let mut json_files: Vec<_> = entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                    .map(|entry| entry.path())
                    .collect();
                // 按文件名排序以保证加载顺序稳定
                json_files.sort();

                for path in json_files {
                    match Self::load_from_file(&path) {
                        Ok(rule_set) => rules.extend(rule_set.rules),
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "权限规则：加载项目规则文件失败"
                            );
                        }
                    }
                }
            }
        }

        Self { rules }
    }

    /// 返回默认规则集（安全基线）
    ///
    /// 包含以下默认策略：
    /// - 拒绝危险操作（递归删除根目录、写入 .env / 凭证文件）
    /// - 允许安全的只读操作（Read、Grep、Glob）
    /// - 允许常见开发命令（git、cargo、npm）
    /// - 其他 Bash 命令需用户确认
    pub fn default_rules() -> Self {
        Self {
            rules: vec![
                // ── 安全基线：拒绝危险操作 ──
                PermissionRule::Deny {
                    pattern: ToolPattern {
                        tool: "Bash".into(),
                        arg_pattern: Some("rm -rf /*".into()),
                    },
                    reason: Some("禁止递归删除根目录".into()),
                },
                PermissionRule::Deny {
                    pattern: ToolPattern {
                        tool: "Bash".into(),
                        arg_pattern: Some("rm -rf /".into()),
                    },
                    reason: Some("禁止删除根目录".into()),
                },
                PermissionRule::Deny {
                    pattern: ToolPattern {
                        tool: "Write".into(),
                        arg_pattern: Some(".env".into()),
                    },
                    reason: Some("禁止写入环境变量文件".into()),
                },
                PermissionRule::Deny {
                    pattern: ToolPattern {
                        tool: "Write".into(),
                        arg_pattern: Some("**/credentials*".into()),
                    },
                    reason: Some("禁止写入凭证文件".into()),
                },
                // ── 允许安全的只读操作 ──
                PermissionRule::Allow {
                    pattern: ToolPattern {
                        tool: "Read".into(),
                        arg_pattern: Some("*".into()),
                    },
                },
                // ── 允许常见开发命令 ──
                PermissionRule::Allow {
                    pattern: ToolPattern {
                        tool: "Bash".into(),
                        arg_pattern: Some("git *".into()),
                    },
                },
                PermissionRule::Allow {
                    pattern: ToolPattern {
                        tool: "Bash".into(),
                        arg_pattern: Some("cargo *".into()),
                    },
                },
                PermissionRule::Allow {
                    pattern: ToolPattern {
                        tool: "Bash".into(),
                        arg_pattern: Some("npm *".into()),
                    },
                },
                PermissionRule::Allow {
                    pattern: ToolPattern {
                        tool: "Grep".into(),
                        arg_pattern: Some("*".into()),
                    },
                },
                PermissionRule::Allow {
                    pattern: ToolPattern {
                        tool: "Glob".into(),
                        arg_pattern: Some("*".into()),
                    },
                },
                // ── 其他 Bash 命令需用户确认 ──
                PermissionRule::Ask {
                    pattern: ToolPattern {
                        tool: "Bash".into(),
                        arg_pattern: Some("*".into()),
                    },
                    reason: Some("其他命令需要确认".into()),
                },
            ],
        }
    }
}

// ── 错误类型 ──────────────────────────────────────────────────────────────

/// 规则加载错误
#[derive(Debug, thiserror::Error)]
pub enum RuleLoadError {
    #[error("读取规则文件失败 {path}: {source}")]
    ReadFile {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("解析规则文件失败 {path}: {detail}")]
    ParseFile {
        path: std::path::PathBuf,
        detail: String,
    },
}

// ── 内部工具函数 ──────────────────────────────────────────────────────────

/// 从工具输入 JSON 中提取用于模式匹配的文本
///
/// 对于 Bash 类工具，提取 "command" 字段；
/// 对于 Write/Edit 类工具，提取 "file_path" 或 "path" 字段；
/// 兜底情况将整个输入序列化为字符串。
fn extract_arg_text(tool_input: &serde_json::Value) -> String {
    match tool_input {
        serde_json::Value::Object(map) => {
            // Bash 命令：取 command 字段
            if let Some(cmd) = map.get("command").and_then(|v| v.as_str()) {
                return cmd.to_string();
            }
            // 文件操作：取 file_path 或 path 字段
            if let Some(path) = map.get("file_path").and_then(|v| v.as_str()) {
                return path.to_string();
            }
            if let Some(path) = map.get("path").and_then(|v| v.as_str()) {
                return path.to_string();
            }
            // 兜底：将 JSON 值转为紧凑字符串
            let compact = serde_json::to_string(tool_input).unwrap_or_else(|_| "{}".to_string());
            compact
        }
        serde_json::Value::String(s) => s.clone(),
        other => {
            let compact = serde_json::to_string(other).unwrap_or_else(|_| "".to_string());
            compact
        }
    }
}

/// 简化的 glob 风格模式匹配
///
/// 支持：
/// - `*` 匹配不含 `/` 的任意字符（单层路径）
/// - `**` 匹配任意路径层级（含 `/`）
/// - 普通字符精确匹配
/// - `?` 匹配单个字符
///
/// 空模式匹配空字符串；`*` 匹配任意不含 `/` 的字符串；`**` 匹配所有。
fn glob_match(pattern: &str, text: &str) -> bool {
    glob_match_impl(pattern, text)
}

/// glob 匹配的递归实现
///
/// 将模式逐字符与文本对比：
/// - `**` 贪心匹配任意层级（含零层），`**/` 可匹配零个路径段
/// - `*` 匹配任意字符序列
/// - `?` 匹配单个字符
/// - 其他字符精确匹配
fn glob_match_impl(pattern: &str, text: &str) -> bool {
    let pchars: Vec<char> = pattern.chars().collect();
    let tchars: Vec<char> = text.chars().collect();

    // 双指针动态规划：pi 为模式指针，ti 为文本指针
    fn dp(pchars: &[char], tchars: &[char], pi: usize, ti: usize) -> bool {
        let plen = pchars.len();
        let tlen = tchars.len();

        // 模式已消耗完，文本也必须消耗完
        if pi == plen {
            return ti == tlen;
        }

        // 处理 ** （跨层通配）
        if pi + 1 < plen && pchars[pi] == '*' && pchars[pi + 1] == '*' {
            // 跳过连续的 *，只保留一个 ** 的语义
            let mut next_pi = pi + 2;
            while next_pi < plen && pchars[next_pi] == '*' {
                next_pi += 1;
            }
            // **/ 可匹配零个路径段：跳过 **/ 直接从下一个路径段开始
            // （如 **/credentials* 匹配 credentials.json）
            if next_pi < plen && pchars[next_pi] == '/' {
                if dp(pchars, tchars, next_pi + 1, ti) {
                    return true;
                }
            }
            // ** 匹配零到多个字符
            for end in ti..=tlen {
                if dp(pchars, tchars, next_pi, end) {
                    return true;
                }
            }
            return false;
        }

        // 处理 * （通配，匹配任意字符序列）
        if pchars[pi] == '*' {
            // 跳过连续的 *（** 已在上方处理）
            let mut next_pi = pi + 1;
            while next_pi < plen && pchars[next_pi] == '*' {
                next_pi += 1;
            }
            // * 匹配零到多个任意字符
            for end in ti..=tlen {
                if dp(pchars, tchars, next_pi, end) {
                    return true;
                }
            }
            return false;
        }

        // 文本已消耗完但模式未消耗完
        if ti == tlen {
            return false;
        }

        // 处理 ? （匹配单个字符，不匹配 /）
        if pchars[pi] == '?' {
            if tchars[ti] != '/' {
                return dp(pchars, tchars, pi + 1, ti + 1);
            }
            return false;
        }

        // 精确字符匹配
        if pchars[pi] == tchars[ti] {
            return dp(pchars, tchars, pi + 1, ti + 1);
        }

        false
    }

    dp(&pchars, &tchars, 0, 0)
}

// ── 测试 ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── glob_match 测试 ──

    #[test]
    fn glob_精确匹配() {
        assert!(glob_match("Bash", "Bash"));
        assert!(!glob_match("Bash", "Read"));
        assert!(!glob_match("Bash", "BashExtra"));
    }

    #[test]
    fn glob_星号匹配任意字符串() {
        assert!(glob_match("*", "Bash"));
        assert!(glob_match("*", "Read"));
        assert!(glob_match("*", "path/to/file"));
    }

    #[test]
    fn glob_双层星号匹配任意路径() {
        assert!(glob_match("**", "src/main.rs"));
        assert!(glob_match("**", "a/b/c/d"));
        assert!(glob_match("**", "file"));
    }

    #[test]
    fn glob_前缀加通配符() {
        assert!(glob_match("git *", "git commit -m 'test'"));
        assert!(glob_match("git *", "git status"));
        assert!(!glob_match("git *", "svn status"));
    }

    #[test]
    fn glob_路径通配() {
        assert!(glob_match("src/**", "src/main.rs"));
        assert!(glob_match("src/**", "src/sub/deep.rs"));
        assert!(!glob_match("src/**", "lib/main.rs"));
    }

    #[test]
    fn glob_问号匹配单字符() {
        assert!(glob_match("file?.rs", "file1.rs"));
        assert!(!glob_match("file?.rs", "file12.rs"));
    }

    #[test]
    fn glob_空模式匹配空文本() {
        assert!(glob_match("", ""));
        assert!(!glob_match("", "notempty"));
    }

    // ── ToolPattern 测试 ──

    #[test]
    fn tool_pattern_匹配工具名() {
        let pat = ToolPattern {
            tool: "Bash".into(),
            arg_pattern: None,
        };
        assert!(pat.matches("Bash", &serde_json::json!({"command": "ls"})));
        assert!(!pat.matches("Read", &serde_json::json!({})));
    }

    #[test]
    fn tool_pattern_通配符匹配所有工具() {
        let pat = ToolPattern {
            tool: "*".into(),
            arg_pattern: None,
        };
        assert!(pat.matches("Bash", &serde_json::json!({})));
        assert!(pat.matches("Read", &serde_json::json!({})));
    }

    #[test]
    fn tool_pattern_带参数模式匹配() {
        let pat = ToolPattern {
            tool: "Bash".into(),
            arg_pattern: Some("git *".into()),
        };
        // 匹配：工具名 Bash，参数以 "git " 开头
        assert!(pat.matches("Bash", &serde_json::json!({"command": "git status"})));
        assert!(pat.matches(
            "Bash",
            &serde_json::json!({"command": "git commit -m 'test'"})
        ));
        // 不匹配：参数不以 "git " 开头
        assert!(!pat.matches("Bash", &serde_json::json!({"command": "ls"})));
        // 不匹配：工具名不同
        assert!(!pat.matches("Read", &serde_json::json!({"command": "git status"})));
    }

    #[test]
    fn tool_pattern_文件路径匹配() {
        let pat = ToolPattern {
            tool: "Write".into(),
            arg_pattern: Some("src/**".into()),
        };
        assert!(pat.matches("Write", &serde_json::json!({"file_path": "src/main.rs"})));
        assert!(pat.matches(
            "Write",
            &serde_json::json!({"file_path": "src/sub/deep.rs"})
        ));
        assert!(!pat.matches("Write", &serde_json::json!({"file_path": "lib/main.rs"})));
    }

    #[test]
    fn tool_pattern_星号匹配所有参数() {
        let pat = ToolPattern {
            tool: "Read".into(),
            arg_pattern: Some("*".into()),
        };
        assert!(pat.matches("Read", &serde_json::json!({"file_path": "any_file.rs"})));
    }

    #[test]
    fn tool_pattern_凭证文件路径匹配() {
        let pat = ToolPattern {
            tool: "Write".into(),
            arg_pattern: Some("**/credentials*".into()),
        };
        assert!(pat.matches(
            "Write",
            &serde_json::json!({"file_path": "credentials.json"})
        ));
        assert!(pat.matches(
            "Write",
            &serde_json::json!({"file_path": ".aws/credentials"})
        ));
        assert!(pat.matches(
            "Write",
            &serde_json::json!({"file_path": "home/.aws/credentials_backup"})
        ));
    }

    // ── PermissionDecision 测试 ──

    #[test]
    fn decision_display_format() {
        assert_eq!(PermissionDecision::Allow.to_string(), "allow");
        assert_eq!(
            PermissionDecision::Deny {
                reason: "危险操作".into()
            }
            .to_string(),
            "deny: 危险操作"
        );
        assert_eq!(
            PermissionDecision::Ask {
                reason: "请确认".into()
            }
            .to_string(),
            "ask: 请确认"
        );
    }

    // ── PermissionRuleSet 测试 ──

    #[test]
    fn evaluate_默认规则_拒绝危险操作() {
        let rules = PermissionRuleSet::default_rules();

        // rm -rf /* 应被拒绝
        let decision = rules.evaluate("Bash", &serde_json::json!({"command": "rm -rf /*"}));
        assert!(matches!(decision, PermissionDecision::Deny { .. }));

        // rm -rf / 应被拒绝
        let decision = rules.evaluate("Bash", &serde_json::json!({"command": "rm -rf /"}));
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn evaluate_默认规则_拒绝写入敏感文件() {
        let rules = PermissionRuleSet::default_rules();

        // 写入 .env 应被拒绝
        let decision = rules.evaluate("Write", &serde_json::json!({"file_path": ".env"}));
        assert!(matches!(decision, PermissionDecision::Deny { .. }));

        // 写入凭证文件应被拒绝
        let decision = rules.evaluate(
            "Write",
            &serde_json::json!({"file_path": ".aws/credentials"}),
        );
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn evaluate_默认规则_允许安全操作() {
        let rules = PermissionRuleSet::default_rules();

        // Read 应被允许
        let decision = rules.evaluate("Read", &serde_json::json!({"file_path": "src/main.rs"}));
        assert_eq!(decision, PermissionDecision::Allow);

        // Grep 应被允许
        let decision = rules.evaluate("Grep", &serde_json::json!({"pattern": "fn main"}));
        assert_eq!(decision, PermissionDecision::Allow);

        // Glob 应被允许
        let decision = rules.evaluate("Glob", &serde_json::json!({"pattern": "**/*.rs"}));
        assert_eq!(decision, PermissionDecision::Allow);
    }

    #[test]
    fn evaluate_默认规则_允许开发命令() {
        let rules = PermissionRuleSet::default_rules();

        // git 命令应被允许
        let decision = rules.evaluate("Bash", &serde_json::json!({"command": "git status"}));
        assert_eq!(decision, PermissionDecision::Allow);

        // cargo 命令应被允许
        let decision = rules.evaluate("Bash", &serde_json::json!({"command": "cargo build"}));
        assert_eq!(decision, PermissionDecision::Allow);

        // npm 命令应被允许
        let decision = rules.evaluate("Bash", &serde_json::json!({"command": "npm install"}));
        assert_eq!(decision, PermissionDecision::Allow);
    }

    #[test]
    fn evaluate_默认规则_其他bash需确认() {
        let rules = PermissionRuleSet::default_rules();

        // 未知 Bash 命令需要确认
        let decision = rules.evaluate(
            "Bash",
            &serde_json::json!({"command": "curl http://example.com"}),
        );
        assert!(matches!(decision, PermissionDecision::Ask { .. }));
    }

    #[test]
    fn evaluate_默认规则_未知工具需确认() {
        let rules = PermissionRuleSet::default_rules();

        let decision = rules.evaluate("CustomTool", &serde_json::json!({}));
        assert!(matches!(decision, PermissionDecision::Ask { .. }));
    }

    #[test]
    fn evaluate_deny优先级最高() {
        // 即使 deny 规则在 allow 规则之后，deny 仍然优先
        let rules = PermissionRuleSet {
            rules: vec![
                PermissionRule::Allow {
                    pattern: ToolPattern {
                        tool: "Bash".into(),
                        arg_pattern: Some("*".into()),
                    },
                },
                PermissionRule::Deny {
                    pattern: ToolPattern {
                        tool: "Bash".into(),
                        arg_pattern: Some("rm *".into()),
                    },
                    reason: Some("禁止删除操作".into()),
                },
            ],
        };

        // rm 命令应被拒绝，即使前面有 allow *
        let decision = rules.evaluate("Bash", &serde_json::json!({"command": "rm -rf /tmp"}));
        assert!(matches!(decision, PermissionDecision::Deny { .. }));

        // 其他命令应被允许
        let decision = rules.evaluate("Bash", &serde_json::json!({"command": "ls"}));
        assert_eq!(decision, PermissionDecision::Allow);
    }

    #[test]
    fn evaluate_先匹配先生效() {
        let rules = PermissionRuleSet {
            rules: vec![
                PermissionRule::Ask {
                    pattern: ToolPattern {
                        tool: "Bash".into(),
                        arg_pattern: Some("git *".into()),
                    },
                    reason: Some("请确认 git 操作".into()),
                },
                PermissionRule::Allow {
                    pattern: ToolPattern {
                        tool: "Bash".into(),
                        arg_pattern: Some("git *".into()),
                    },
                },
            ],
        };

        // 第一条 ask 匹配先生效
        let decision = rules.evaluate("Bash", &serde_json::json!({"command": "git status"}));
        assert!(matches!(decision, PermissionDecision::Ask { .. }));
    }

    #[test]
    fn evaluate_无匹配规则默认ask() {
        let rules = PermissionRuleSet { rules: vec![] };
        let decision = rules.evaluate("AnyTool", &serde_json::json!({}));
        assert!(matches!(decision, PermissionDecision::Ask { .. }));
    }

    // ── 序列化 / 反序列化测试 ──

    #[test]
    fn rule_set_序列化反序列化() {
        let rules = PermissionRuleSet::default_rules();
        let json = serde_json::to_string(&rules).expect("序列化失败");
        let parsed: PermissionRuleSet = serde_json::from_str(&json).expect("反序列化失败");

        // 验证反序列化后对同一条目的求值结果一致
        let original_decision =
            rules.evaluate("Bash", &serde_json::json!({"command": "git status"}));
        let parsed_decision =
            parsed.evaluate("Bash", &serde_json::json!({"command": "git status"}));
        assert_eq!(original_decision, parsed_decision);
    }

    #[test]
    fn rule_set_从json值加载() {
        let json = r#"{
            "rules": [
                { "action": "deny", "pattern": { "tool": "Bash", "arg_pattern": "rm -rf /*" }, "reason": "禁止递归删除根目录" },
                { "action": "allow", "pattern": { "tool": "Read", "arg_pattern": "*" } }
            ]
        }"#;
        let rules: PermissionRuleSet = serde_json::from_str(json).expect("解析 JSON 失败");

        // deny 规则生效
        let decision = rules.evaluate("Bash", &serde_json::json!({"command": "rm -rf /*"}));
        assert!(matches!(decision, PermissionDecision::Deny { .. }));

        // allow 规则生效
        let decision = rules.evaluate("Read", &serde_json::json!({"file_path": "any.rs"}));
        assert_eq!(decision, PermissionDecision::Allow);
    }

    #[test]
    fn load_from_file_文件不存在() {
        let result = PermissionRuleSet::load_from_file(Path::new("/nonexistent/rules.json"));
        assert!(result.is_err());
    }

    #[test]
    fn load_from_file_无效json() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let file_path = dir.path().join("rules.json");
        std::fs::write(&file_path, "not valid json {{{").expect("写入文件失败");

        let result = PermissionRuleSet::load_from_file(&file_path);
        assert!(result.is_err());
    }

    #[test]
    fn load_from_file_正常加载() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let file_path = dir.path().join("rules.json");
        let json = r#"{
            "rules": [
                { "action": "allow", "pattern": { "tool": "Read", "arg_pattern": "*" } }
            ]
        }"#;
        std::fs::write(&file_path, json).expect("写入文件失败");

        let rules = PermissionRuleSet::load_from_file(&file_path).expect("加载规则失败");
        let decision = rules.evaluate("Read", &serde_json::json!({"file_path": "any.rs"}));
        assert_eq!(decision, PermissionDecision::Allow);
    }

    // ── extract_arg_text 测试 ──

    #[test]
    fn extract_arg_text_从command字段提取() {
        let input = serde_json::json!({"command": "git status", "timeout": 10});
        assert_eq!(extract_arg_text(&input), "git status");
    }

    #[test]
    fn extract_arg_text_从file_path字段提取() {
        let input = serde_json::json!({"file_path": "/src/main.rs", "content": "fn main() {}"});
        assert_eq!(extract_arg_text(&input), "/src/main.rs");
    }

    #[test]
    fn extract_arg_text_从path字段提取() {
        let input = serde_json::json!({"path": "/src/lib.rs"});
        assert_eq!(extract_arg_text(&input), "/src/lib.rs");
    }

    #[test]
    fn extract_arg_text_字符串值直接返回() {
        let input = serde_json::Value::String("直接字符串".into());
        assert_eq!(extract_arg_text(&input), "直接字符串");
    }

    #[test]
    fn extract_arg_text_无已知字段兜底() {
        let input = serde_json::json!({"unknown_key": "value"});
        let text = extract_arg_text(&input);
        // 兜底会序列化整个 JSON
        assert!(text.contains("unknown_key"));
    }
}
