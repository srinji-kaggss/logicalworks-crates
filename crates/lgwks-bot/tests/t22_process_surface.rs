//! T22's negative proof: a public process description must not be executable.

#![cfg(all(
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::rt::supervise::{Supervisor, TaskOutcome};
use lgwks_bot::rt::time::{Instant, sleep};

#[test]
fn public_process_description_rejects_direct_execution() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "lgwks-bot-t22-{}-{}",
        std::process::id(),
        std::thread::current()
            .name()
            .map_or_else(|| String::from("probe"), String::from)
    ));
    fs::create_dir_all(root.join("src"))?;
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\nname = \"t22-process-probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\nlgwks_bot = {{ path = \"{}\", features = [\"process\"] }}\n",
            manifest_dir.display()
        ),
    )?;
    fs::write(
        root.join("src/main.rs"),
        "use lgwks_bot::rt::process::Command;\n\nfn main() {\n    let mut command = Command::new(\"true\");\n    let _ = command.spawn();\n}\n",
    )?;

    let status = Command::new("cargo")
        .args(["check", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .arg("--offline")
        .env("CARGO_TARGET_DIR", root.join("target"))
        .status()?;
    let cleanup = fs::remove_dir_all(&root);
    cleanup?;

    assert!(
        !status.success(),
        "direct execution of the public process description unexpectedly compiled"
    );
    Ok(())
}

#[test]
fn supervisor_is_the_sanctioned_process_runner() -> Result<(), Box<dyn std::error::Error>> {
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
