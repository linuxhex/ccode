//! Cross-platform child-process lifecycle helpers for `tokio::process::Command`.
//!
//! All implementations now live in the lightweight [`ccode_tty`] crate
//! so that every crate in the workspace can use them without pulling in the
//! heavyweight `ccode-tools` dependency. This module re-exports the public
//! API for backward compatibility.

pub use ccode_tty::{
    ProcessGroup, ProcessScope, detach_command, global_process_scope, new_process_group,
};
