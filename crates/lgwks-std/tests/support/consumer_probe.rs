//! Shared harness for building a throwaway downstream consumer crate.
//!
//! Both `codec_contract.rs` and `wire_feature_unification.rs` need to compile
//! and run a minimal external consumer against this crate, so the setup lives
//! here once instead of being copied into each test target.
#![allow(dead_code, reason = "each test target uses a subset of these items")]

use std::path::Path;

/// Write `main_rs` into a fresh crate whose manifest is `manifest`, resolve its
/// lockfile offline, run it, and return its stdout.
///
/// # Errors
/// Returns the refusal when lock generation or the run fails, with the child's
/// stderr attached.
pub fn build_and_run(manifest: &str, main_rs: &str) -> Result<String, Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    std::fs::create_dir(directory.path().join("src"))?;
    let manifest_path = directory.path().join("Cargo.toml");
    std::fs::write(&manifest_path, manifest)?;
    std::fs::write(directory.path().join("src/main.rs"), main_rs)?;

    let cargo = |args: &[&str]| {
        std::process::Command::new(env!("CARGO"))
            .args(args)
            .arg("--manifest-path")
            .arg(&manifest_path)
            .env(
                "CARGO_TARGET_DIR",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
            )
            .output()
    };
    let lock = cargo(&["generate-lockfile", "--offline"])?;
    if !lock.status.success() {
        return Err(format!(
            "probe lock generation failed: {}",
            String::from_utf8_lossy(&lock.stderr)
        )
        .into());
    }
    let run = cargo(&["run", "--locked", "--offline", "--quiet"])?;
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    if !run.status.success() {
        return Err(format!(
            "probe failed: {stdout}{}",
            String::from_utf8_lossy(&run.stderr)
        )
        .into());
    }
    Ok(stdout)
}
