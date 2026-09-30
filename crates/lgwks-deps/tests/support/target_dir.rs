//! Where a nested consumer build puts its artifacts, shared by every test that
//! compiles a downstream probe against the workspace.

use std::path::{Path, PathBuf};

/// The caller's `CARGO_TARGET_DIR` when one is set, otherwise the workspace's
/// `target/`.
///
/// Sharing the outer build's directory is what keeps the heavy dependencies
/// compiled once: a probe that built into its own scratch directory compiled
/// every dependency cold on every run, and forcing the repository-local path
/// gave a developer (or a gate lane) with its own target directory a second,
/// cold copy. `CARGO_MANIFEST_DIR` is the including crate's, and every
/// workspace crate sits two levels below the workspace root.
pub fn workspace_target_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(configured) = std::env::var_os("CARGO_TARGET_DIR") {
        return Ok(PathBuf::from(configured));
    }
    Ok(workspace_root()?.join("target"))
}

/// The workspace root: two levels above the including crate's manifest.
pub fn workspace_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "crate manifest is not two levels below the workspace".into())
}
