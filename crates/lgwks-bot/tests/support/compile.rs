//! The compile-probe harness: type-check a one-file consumer of `lgwks_bot`
//! against the real workspace lockfile.
//!
//! One definition, shared by every target that type-checks a downstream
//! consumer (`t22_process_surface` and `t02_compile_surface`). The probe copies
//! the workspace `Cargo.lock` and shares the workspace target directory, so it
//! resolves the versions the workspace compiled rather than whatever the local
//! registry cache holds newest, and it does not compile every dependency cold.
//!
//! This module is included with `#[path = "support/compile.rs"] mod compile;`.
#![allow(
    dead_code,
    reason = "each including test target uses a different subset of the probe harness"
)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

#[path = "../../../lgwks-deps/tests/support/target_dir.rs"]
mod target_dir;

use target_dir::{workspace_root, workspace_target_dir};

/// Type-checks a one-file consumer of `lgwks_bot` and returns cargo's output.
///
/// The probe starts from the workspace lockfile, so it resolves the versions
/// the workspace build compiled rather than whatever the local registry cache
/// holds newest. Scratch is named by wall-clock nanos plus a sequence, not a
/// process or thread id (both are reused) and not `lgwks_std::random` (behind
/// features this test is built without).
///
/// # Errors
///
/// Whatever the filesystem or the child process reports.
pub fn compile_probe(
    name: &str,
    dependency: &str,
    main: &str,
) -> Result<Output, Box<dyn std::error::Error>> {
    probe("check", name, dependency, main)
}

/// [`compile_probe`] under `cargo clippy` with every warning denied: the
/// consumer's own lint pass, which is what enforces a lint an expansion
/// declares at `forbid` but rustc alone does not know.
///
/// # Errors
///
/// As [`compile_probe`].
pub fn clippy_probe(
    name: &str,
    dependency: &str,
    main: &str,
) -> Result<Output, Box<dyn std::error::Error>> {
    probe("clippy", name, dependency, main)
}

/// Write the one-file consumer and run `cargo <subcommand>` over it.
fn probe(
    subcommand: &str,
    name: &str,
    dependency: &str,
    main: &str,
) -> Result<Output, Box<dyn std::error::Error>> {
    static DIR_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or(0);
    let seq = DIR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("{name}-{nanos}-{seq}"));
    fs::create_dir_all(root.join("src"))?;
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::copy(
        workspace_root()?.join("Cargo.lock"),
        root.join("Cargo.lock"),
    )?;
    fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\nlgwks_bot = {{ path = \"{}\"{dependency} }}\n",
            manifest_dir.display()
        ),
    )?;
    fs::write(root.join("src/main.rs"), main)?;
    let output = Command::new(env!("CARGO"))
        .args([subcommand, "--offline", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", workspace_target_dir()?)
        .output();
    fs::remove_dir_all(&root)?;
    Ok(output?)
}

/// The probe compiled, so a neighbouring negative probe that failed for a
/// missing dependency cannot be mistaken for the intended refusal.
///
/// # Panics
///
/// When the probe did not compile, with the compiler's own output in the
/// message.
pub fn assert_compiles(output: &Output) {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "the positive control must compile:\n{text}"
    );
}

/// The probe was refused by the compiler for the reason named, not by cargo
/// for some other one.
///
/// A bare `!status.success()` also passed when the probe never reached rustc at
/// all: an offline resolution failure, a lock error, a missing toolchain. This
/// asserts the specific diagnostic code and the symbol it names.
///
/// # Panics
///
/// When the probe compiled, or failed with a different diagnostic.
pub fn assert_refused_for(output: &Output, code: &str, symbol: &str) {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.status.success(),
        "the probe unexpectedly compiled:\n{text}"
    );
    assert!(
        text.contains(&format!("error[{code}]")) && text.contains(symbol),
        "the probe failed, but not with {code} naming `{symbol}`:\n{text}"
    );
}

/// The probe was refused by the named lint, at error level, and not for some
/// other reason. A lint error carries no `E` code, so the lint's own name in
/// the diagnostic is what identifies it.
///
/// # Panics
///
/// When the probe compiled, or failed without naming `lint`.
pub fn assert_refused_by_lint(output: &Output, lint: &str) {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.status.success(),
        "the probe unexpectedly compiled:\n{text}"
    );
    assert!(
        text.contains("error") && text.contains(lint),
        "the probe failed, but not by `{lint}`:\n{text}"
    );
}
