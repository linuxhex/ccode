//! Plugin local settings — per-project, user-managed configuration.
//!
//! Each plugin can store user-configurable settings at
//! `.ccode/<plugin-name>.local.md` (YAML frontmatter + markdown body).
//! These files are per-project and should be in `.gitignore`.
//!
//! Hooks, commands, and agents can read these settings at runtime to
//! adapt their behavior per project.

use std::path::{Path, PathBuf};

/// Parsed local settings for a plugin.
#[derive(Debug, Clone)]
pub struct PluginLocalSettings {
    /// The plugin name these settings belong to.
    pub plugin_name: String,
    /// Path to the settings file.
    pub path: PathBuf,
    /// Raw YAML frontmatter as a string (parsed by consumer).
    pub frontmatter: String,
    /// Markdown body content (prompts, instructions, etc.).
    pub body: String,
}

/// Find the local settings file for a plugin in the given project directory.
///
/// Searches for `.ccode/<plugin_name>.local.md` relative to `project_dir`.
/// Returns `None` if the file doesn't exist.
pub fn find_local_settings(project_dir: &Path, plugin_name: &str) -> Option<PluginLocalSettings> {
    let path = project_dir
        .join(".ccode")
        .join(format!("{plugin_name}.local.md"));

    if !path.is_file() {
        return None;
    }

    let content = std::fs::read_to_string(&path).ok()?;
    let (frontmatter, body) = split_frontmatter(&content);

    Some(PluginLocalSettings {
        plugin_name: plugin_name.to_string(),
        path,
        frontmatter,
        body,
    })
}

/// Split a markdown file into YAML frontmatter and body.
///
/// Frontmatter is delimited by `---` at the start and end. If no frontmatter
/// is found, returns an empty string for frontmatter and the full content as body.
fn split_frontmatter(content: &str) -> (String, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (String::new(), content.to_string());
    }

    let after_first = &trimmed[3..];
    let Some(end_idx) = after_first.find("\n---") else {
        return (String::new(), content.to_string());
    };

    let frontmatter = after_first[..end_idx].trim().to_string();
    let after_second = &after_first[end_idx + 4..];
    let body = after_second.trim_start_matches('\n').to_string();

    (frontmatter, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_frontmatter_with_yaml() {
        let content = "---\nenabled: true\nmax_retries: 3\n---\n\n# Instructions\nDo stuff.";
        let (fm, body) = split_frontmatter(content);
        assert_eq!(fm, "enabled: true\nmax_retries: 3");
        assert!(body.contains("# Instructions"));
    }

    #[test]
    fn split_frontmatter_without_yaml() {
        let content = "# Just markdown\nNo frontmatter here.";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.is_empty());
        assert_eq!(body, content);
    }

    #[test]
    fn find_local_settings_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(find_local_settings(tmp.path(), "nonexistent").is_none());
    }

    #[test]
    fn find_local_settings_present() {
        let tmp = tempfile::tempdir().unwrap();
        let ccode_dir = tmp.path().join(".ccode");
        std::fs::create_dir_all(&ccode_dir).unwrap();
        std::fs::write(
            ccode_dir.join("my-plugin.local.md"),
            "---\nenabled: true\n---\n\nProject instructions.",
        )
        .unwrap();

        let settings = find_local_settings(tmp.path(), "my-plugin").unwrap();
        assert_eq!(settings.plugin_name, "my-plugin");
        assert_eq!(settings.frontmatter, "enabled: true");
        assert!(settings.body.contains("Project instructions."));
    }
}
