//! T22's negative proof: a public process description must not be executable.

#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use lgwks_bot::Runtime;
use lgwks_bot::rt::process::ProcessSpec;
use lgwks_bot::rt::supervise::{Supervisor, TaskOutcome};
use lgwks_bot::rt::time::{Instant, sleep};

type TestResult = Result<(), Box<dyn std::error::Error>>;

// The compile-probe harness, shared with `t02_compile_surface`: one definition
// of where a nested build puts its artifacts, so the two targets cannot drift.
#[path = "support/compile.rs"]
mod compile;

use compile::{assert_refused_for, compile_probe};

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
