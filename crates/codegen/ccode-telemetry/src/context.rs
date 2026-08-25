//! Git context collection for telemetry events.

pub struct GitContext {
    pub is_git_repo: bool,
}

pub fn collect_git_context(cwd: &str) -> GitContext {
    use git2::Repository;
    use std::path::Path;

    GitContext {
        is_git_repo: Repository::discover(Path::new(cwd)).is_ok(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostingAgent {
    ClaudeCode,
    Cursor,
    GeminiCli,
    CodexCli,
    Augment,
    Cline,
    OpenCode,
    Trae,
    Devin,
    Unknown,
}

impl HostingAgent {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
            Self::GeminiCli => "gemini-cli",
            Self::CodexCli => "codex-cli",
            Self::Augment => "augment",
            Self::Cline => "cline",
            Self::OpenCode => "opencode",
            Self::Trae => "trae",
            Self::Devin => "devin",
            Self::Unknown => "unknown",
        }
    }
}

pub fn detect_hosting_agent() -> HostingAgent {
    if std::env::var("CLAUDECODE").as_deref() == Ok("1") {
        return HostingAgent::ClaudeCode;
    }
    if std::env::var("CURSOR_AGENT").as_deref() == Ok("1") {
        return HostingAgent::Cursor;
    }
    if std::env::var("GEMINI_CLI").as_deref() == Ok("1") {
        return HostingAgent::GeminiCli;
    }
    if matches!(std::env::var("CODEX_SANDBOX"), Ok(v) if !v.is_empty()) {
        return HostingAgent::CodexCli;
    }
    if std::env::var("AUGMENT_AGENT").as_deref() == Ok("1") {
        return HostingAgent::Augment;
    }
    if std::env::var("CLINE_ACTIVE").as_deref() == Ok("true") {
        return HostingAgent::Cline;
    }
    if std::env::var("OPENCODE_CLIENT").as_deref() == Ok("1") {
        return HostingAgent::OpenCode;
    }
    if matches!(std::env::var("TRAE_AI_SHELL_ID"), Ok(v) if !v.is_empty()) {
        return HostingAgent::Trae;
    }
    if std::path::Path::new("/opt/.devin").exists() {
        return HostingAgent::Devin;
    }
    HostingAgent::Unknown
}
