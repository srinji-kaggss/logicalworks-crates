//! T22's negative proof: a public process description must not be executable.

#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::rt::supervise::{Supervisor, TaskOutcome};
use lgwks_bot::rt::time::{Instant, sleep};

type TestResult = Result<(), Box<dyn std::error::Error>>;

// Shared with the storefront consumer probes: one definition of where a
// nested build puts its artifacts. A probe used to build into its own scratch
// directory, compiling `lgwks_bot` and every dependency cold: two probes,
// ~41 s each, the longest tests in the suite.
#[path = "../../lgwks-deps/tests/support/target_dir.rs"]
mod target_dir;

use target_dir::{workspace_root, workspace_target_dir};

/// Type-checks a one-file consumer of `lgwks_bot` and returns cargo's output.
///
/// The probe starts from the workspace lockfile, so it resolves the versions
/// the workspace build compiled rather than whatever the local registry cache
/// holds newest. Scratch is named by wall-clock nanos plus a sequence, not a
/// process or thread id (both are reused) and not `lgwks_std::random` (behind
/// features this test is built without).
fn compile_probe(
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
        .args(["check", "--offline", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", workspace_target_dir()?)
        .output();
    fs::remove_dir_all(&root)?;
    Ok(output?)
}

/// The probe was refused by the compiler for the reason named, not by cargo
/// for some other one. A bare `!status.success()` also passed when the probe
/// never reached rustc at all: an offline resolution failure, a lock error, a
/// missing toolchain.
fn assert_refused_for(output: &Output, code: &str, symbol: &str) {
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

#[test]
fn public_process_description_rejects_direct_execution() -> TestResult {
    let output = compile_probe(
        "t22-process-probe",
        ", features = [\"process\"]",
        "use lgwks_bot::rt::process::Command;\n\nfn main() {\n    let mut command = Command::new(\"true\");\n    let _ = command.spawn();\n}\n",
    )?;
    assert_refused_for(&output, "E0603", "Command");
    Ok(())
}

#[test]
fn the_guaranteed_task_set_does_not_expose_detach_all() -> TestResult {
    // A public consumer must not be able to detach work from the facade that
    // promises to own it. This is a real downstream compile probe, not a
    // source-text check: the method must be absent from the exported type.
    let output = compile_probe(
        "lgwks-bot-taskset-probe",
        "",
        "use lgwks_bot::rt::task::JoinSet;\nfn main() { let mut tasks = JoinSet::<()>::new(); tasks.detach_all(); }\n",
    )?;
    assert_refused_for(&output, "E0599", "detach_all");
    Ok(())
}

#[test]
fn supervisor_is_the_sanctioned_process_runner() -> TestResult {
    let runtime = Runtime::new()?;
    let outcome = runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let mut command = ProcessSpec::new("sh");
        command.arg("-c").arg("exit 0");
        supervisor.spawn_process(&command).await?;
        let deadline = Instant::now()
            .checked_add(std::time::Duration::from_secs(5))
            .ok_or_else(|| std::io::Error::other("clock deadline overflowed"))?;
        loop {
            supervisor.reap();
            if let Some(outcome) = supervisor.next_report() {
                break Ok::<_, std::io::Error>(outcome);
            }
            if Instant::now() >= deadline {
                break Err(std::io::Error::other(
                    "supervisor did not report the process",
                ));
            }
            sleep(std::time::Duration::from_millis(5)).await;
        }
    })?;
    assert!(matches!(outcome, TaskOutcome::Completed { .. }));
    Ok(())
}
