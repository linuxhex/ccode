//! Shared hook source path discovery.

use std::path::{Path, PathBuf};

use ccode_config::resolve_global_hook_sources;
use ccode_hooks::discovery::HookSource;
use ccode_hooks::error::HookError;

/// Owned paths for hook sources. Callers borrow via `as_sources()`.
pub struct HookSourcePaths {
    pub global: Vec<PathBuf>,
    pub project: Vec<PathBuf>,
}

impl HookSourcePaths {
    /// Borrow as `HookSource` refs. Project sources are excluded when untrusted.
    pub fn as_sources(&self, include_project: bool) -> (Vec<HookSource<'_>>, Vec<HookSource<'_>>) {
        let global = self.global.iter().map(|p| path_to_source(p)).collect();
        let project = if include_project {
            self.project.iter().map(|p| path_to_source(p)).collect()
        } else {
            vec![]
        };
        (global, project)
    }
}

fn path_to_source(p: &Path) -> HookSource<'_> {
    if p.is_dir() {
        HookSource::Directory(p)
    } else {
        HookSource::SettingsFile(p)
    }
}

fn include_claude_hooks(compat: &ccode_tools::types::compat::CompatConfig) -> bool {
    compat.claude.hooks
        && !crate::claude_import::is_claude_import_marked_with_log("discover_hook_source_paths")
}

fn include_cursor_hooks(compat: &ccode_tools::types::compat::CompatConfig) -> bool {
    compat.cursor.hooks
}

/// Global + project hook source paths. Registry file is never a discovery
/// source; Claude/Cursor globals are appended when gates are on.
pub fn discover_hook_source_paths(
    git_root: Option<&Path>,
    compat: &ccode_tools::types::compat::CompatConfig,
) -> HookSourcePaths {
    let ccode = ccode_config::user_ccode_home();
    let home = dirs::home_dir();
    let include_claude = include_claude_hooks(compat);
    let include_cursor = include_cursor_hooks(compat);

    // Soft hooks-paths I/O keeps fixed slots; hard resolve omits Ccode globals.
    let mut global: Vec<PathBuf> =
        match resolve_global_hook_sources(ccode.as_deref(), /* reject_symlinks */ false) {
            Ok(resolved) => {
                if let Some(e) = &resolved.configured_error {
                    tracing::warn!(
                        error = %e,
                        "hooks-paths unreadable; retaining fixed Ccode hook discovery sources only"
                    );
                }
                resolved
                    .discovery_sources()
                    .map(|s| s.path.clone())
                    .collect()
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "global hook source resolve hard-failed; omitting Ccode global sources"
                );
                Vec::new()
            }
        };

    if let Some(h) = home.as_deref() {
        if include_claude {
            global.push(h.join(".claude").join("settings.json"));
            global.push(h.join(".claude").join("settings.local.json"));
        }
        if include_cursor {
            global.push(h.join(".cursor").join("hooks.json"));
        }
    }

    let mut project = Vec::new();
    if let Some(root) = git_root {
        if include_claude {
            project.push(root.join(".claude").join("settings.json"));
            project.push(root.join(".claude").join("settings.local.json"));
        }
        project.push(root.join(".ccode").join("hooks"));
        if include_cursor {
            project.push(root.join(".cursor").join("hooks.json"));
        }
    }

    HookSourcePaths { global, project }
}

/// Single load entry point: build compat-aware sources, gate project sources on
/// trust, then load. Every session-startup and mid-session reload site routes
/// through here so the source policy stays in one place.
pub fn discover_hooks(
    git_root: Option<&Path>,
    compat: &ccode_tools::types::compat::CompatConfig,
    trusted: bool,
) -> (ccode_hooks::discovery::HookRegistry, Vec<HookError>) {
    let source_paths = discover_hook_source_paths(git_root, compat);
    let (global_sources, project_sources) = source_paths.as_sources(trusted);
    ccode_hooks::discovery::load_hooks_from_sources(&global_sources, &project_sources)
}
