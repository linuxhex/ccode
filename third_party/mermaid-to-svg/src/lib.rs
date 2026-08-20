//! Vendored dagre-based Mermaid layout engine (stub).
//!
//! The full implementation was removed; this crate now provides the minimum
//! type surface that downstream crates (`ccode-mermaid`) compile against.
//! All render calls return an error so the caller's code-block fallback activates.

use std::fmt;

/// Theme controlling diagram surface/text colours.
#[derive(Debug, Clone)]
pub struct MermaidTheme {
    pub background: String,
    pub text: String,
    pub line: String,
}

impl MermaidTheme {
    pub fn light() -> Self {
        Self {
            background: "#ffffff".into(),
            text: "#333333".into(),
            line: "#666666".into(),
        }
    }

    pub fn dark() -> Self {
        Self {
            background: "#1e1e1e".into(),
            text: "#cccccc".into(),
            line: "#999999".into(),
        }
    }
}

/// Error taxonomy produced by the layout engine.
#[derive(Debug, Clone)]
pub enum MermaidError {
    ParseError { line: usize, message: String },
    InvalidDirection(String),
    InvalidNodeShape(String),
    DotGenerationError(String),
    RenderError(String),
    UnsupportedDiagramType(String),
}

impl fmt::Display for MermaidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ParseError { line, message } => write!(f, "parse error at line {line}: {message}"),
            Self::InvalidDirection(d) => write!(f, "invalid direction: {d}"),
            Self::InvalidNodeShape(s) => write!(f, "invalid node shape: {s}"),
            Self::DotGenerationError(e) => write!(f, "dot generation error: {e}"),
            Self::RenderError(e) => write!(f, "render error: {e}"),
            Self::UnsupportedDiagramType(t) => write!(f, "unsupported diagram type: {t}"),
        }
    }
}

impl std::error::Error for MermaidError {}

/// Render Mermaid source to SVG. Stub: always returns `UnsupportedDiagramType`.
pub fn render_mermaid_to_svg(
    _source: &str,
    _theme: Option<&MermaidTheme>,
) -> Result<String, MermaidError> {
    Err(MermaidError::UnsupportedDiagramType(
        "mermaid-to-svg engine stub: full implementation not available".into(),
    ))
}
