//! Deliberately wrong solution — `capture`, unbounded retain.
//!
//! This is a mutant input for the harness, not an example: it reads the
//! child's whole output into one buffer and reports it all as the head with
//! `truncated` always false, so the oracle's flood clause must fail it. It is
//! never built by the estate workspace (this directory is not a member), so
//! it is not held to the estate lint contract.

use std::io::Read;
use std::time::Duration;

pub use ai_task_support::capture::CaptureError as CaptureError;
pub use ai_task_support::capture::CaptureResult as CaptureResult;

pub async fn solve(
    argv: Vec<String>,
    head_limit: usize,
    deadline: Duration,
) -> Result<CaptureResult, CaptureError> {
    let _ = head_limit;
    let mut words = argv.into_iter();
    let program = words.next().ok_or_else(|| CaptureError::Spawn("empty argv".to_owned()))?;
    let mut child = std::process::Command::new(program)
        .args(words.collect::<Vec<String>>())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|source| CaptureError::Spawn(source.to_string()))?;
    let started = std::time::Instant::now();
    let mut kept = Vec::new();
    let mut pipe = child.stdout.take().ok_or_else(|| {
        ai_task_support::diagnostic(format_args!("capture mutant: no stdout pipe"));
        CaptureError::Spawn("no stdout pipe".to_owned())
    })?;
    pipe.read_to_end(&mut kept)
        .map_err(|source| CaptureError::Spawn(source.to_string()))?;
    let _ = child.wait();
    if started.elapsed() <= deadline {
        let total = kept.len();
        Ok(CaptureResult::new(kept, total as u64, false))
    } else {
        ai_task_support::diagnostic(format_args!("capture mutant: the run outlived its deadline"));
        Err(CaptureError::Deadline)
    }
}
