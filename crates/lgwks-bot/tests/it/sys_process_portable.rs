//! Portability: on a non-Unix target the supervised process surface refuses as
//! unsupported before it forks.
//!
//! This target is empty on Unix (`#![cfg(all(not(unix), feature = "process"))]`),
//! which is the point: the `not(unix)` branch is compiled and executed on the
//! Windows runner, and only there. On a Unix host the branch is not reachable,
//! so this file compiles to no tests rather than pretending to exercise it —
//! compiling a cfg branch on a Unix host is not runtime evidence for it.
#![cfg(all(not(unix), feature = "process"))]

use lgwks_bot::domain::sys::Process;
use lgwks_bot::rt::process::{ProcessRunError, ProcessSpec};
use lgwks_bot::rt::supervise::Supervisor;
use lgwks_bot::{Auth, BotError, Cap, DispatchCertainty, Execute, GrantSet};

/// An `Auth` covering `bot.sys`.
fn sys_auth() -> Result<Auth, BotError> {
    GrantSet::empty().grant(Cap::sys()).issue(&[Cap::sys()])
}

/// A non-Unix `run_process` is a typed pre-fork refusal, and nothing is spawned.
#[test]
fn a_non_unix_run_process_is_a_typed_pre_fork_refusal() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = lgwks_bot::Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        match supervisor.run_process(&ProcessSpec::new("unused")).await {
            Err(ProcessRunError::NotStarted { source }) => assert_eq!(
                source.kind(),
                std::io::ErrorKind::Unsupported,
                "the platform must be named as unsupported: {source}"
            ),
            other => {
                return Err(std::io::Error::other(format!(
                    "expected a typed pre-fork refusal, got {other:?}"
                ))
                .into());
            }
        }
        assert_eq!(
            supervisor.stats().spawned,
            0,
            "a refused run must start nothing"
        );
        Ok(())
    })
}

/// The `sys::Process` domain reports the same refusal with `Refused` certainty.
#[test]
fn the_sys_domain_refuses_without_a_runner() -> Result<(), Box<dyn std::error::Error>> {
    let process = Process::new("unused");
    let error = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })
        .err()
        .ok_or("a non-Unix process must be refused rather than run")?;
    assert_eq!(
        error.dispatch_certainty(),
        DispatchCertainty::Refused,
        "nothing ran, so the certainty is Refused: {error}"
    );
    Ok(())
}
