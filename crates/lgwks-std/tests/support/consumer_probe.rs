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
        let refusal = Err(format!(
            "probe lock generation failed: {}",
            String::from_utf8_lossy(&lock.stderr)
        )
        .into());
        #[cfg(feature = "trace")]
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build_and_run: returning an error to the caller");
        return refusal;
    }
    let run = cargo(&["run", "--locked", "--offline", "--quiet"])?;
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    if !run.status.success() {
        let refusal = Err(format!(
            "probe failed: {stdout}{}",
            String::from_utf8_lossy(&run.stderr)
        )
        .into());
        #[cfg(feature = "trace")]
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "build_and_run: returning an error to the caller");
        return refusal;
    }
    Ok(stdout)
}

/// Render a probe manifest named `package_name` that depends on this crate with
/// default features off and exactly `features` on.
pub fn manifest(package_name: &str, features: &[&str]) -> String {
    let features = if features.is_empty() {
        String::new()
    } else {
        let quoted: Vec<String> = features
            .iter()
            .map(|feature| format!("{feature:?}"))
            .collect();
        format!(", features = [{}]", quoted.join(", "))
    };
    format!(
        "[package]\nname = {package_name:?}\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
         [dependencies]\nlgwks_std = {{ path = {:?}, default-features = false{features} }}\n",
        Path::new(env!("CARGO_MANIFEST_DIR"))
    )
}

/// The value of the `<label> <integer>` line named `label` in probe output, or
/// `u64::MAX` when it is absent, so a missing measurement fails every bound
/// that uses it instead of silently reading as zero.
pub fn measurement(stdout: &str, label: &str) -> u64 {
    stdout
        .lines()
        .filter_map(|line| {
            let (name, value) = line.rsplit_once(' ')?;
            Some((name.trim(), value.trim().parse::<u64>().ok()?))
        })
        .find_map(|(name, value)| (name == label).then_some(value))
        .unwrap_or(u64::MAX)
}
