//! Shared utilities used by both `ccode-shell` and its downstream clients
//! (e.g. `ccode-pager-render`). This crate sits upstream of `ccode-shell`
//! so it must never depend on it.

pub mod clipboard;
pub mod placeholder_images;
pub mod session;
pub mod stderr;
pub mod ui_config;
