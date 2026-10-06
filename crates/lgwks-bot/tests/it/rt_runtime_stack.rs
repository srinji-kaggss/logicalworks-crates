//! A worker's stack size is a knob, and a bounded one.
//!
//! The migration of a real consumer found a bounded future chain that needs a
//! little more than the engine's 2 MiB default worker stack: the process aborts
//! with a stack overflow, and the sanctioned runtime surface had no setting for it.

#![cfg(all(feature = "rt", feature = "sync"))]

use std::io::ErrorKind;
use std::num::NonZeroUsize;

use lgwks_bot::rt::runtime::{Builder, MAX_THREAD_STACK_SIZE};
use lgwks_bot::rt::task::JoinSet;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Recurse `depth` frames, each holding a 256-byte array the optimizer may not drop.
///
/// The array's bytes come from a conversion that cannot fail rather than from a
/// narrowed one: a byte that stood in for a failed conversion would be a frame
/// the optimizer could reason about, which is the frame this measurement needs.
fn deep(depth: usize) -> usize {
    if depth == 0 {
        return 0;
    }
    let padding = [u8::from(depth.is_multiple_of(2)); 256];
    std::hint::black_box(&padding);
    deep(depth.saturating_sub(1)).saturating_add(usize::from(padding[0]))
}

#[test]
fn a_chain_deeper_than_the_default_stack_runs_on_a_larger_one() -> TestResult {
    // About 12 MiB of frames: far past the 2 MiB default, well inside 64 MiB.
    const DEPTH: usize = 40_000;
    let runtime = Builder::new()
        .worker_threads(NonZeroUsize::new(2))
        .thread_stack_size(NonZeroUsize::new(64 * 1024 * 1024))
        .build()?;
    let joined = runtime.block_on(async {
        let mut set = JoinSet::new();
        set.spawn(async { deep(DEPTH) });
        set.join_next().await
    });
    let counted = joined.ok_or("the task ended without a result")??;
    assert!(counted <= DEPTH, "the recursion completed: {counted}");
    Ok(())
}

#[test]
fn a_stack_above_the_ceiling_is_refused_at_build() -> TestResult {
    let over = NonZeroUsize::new(MAX_THREAD_STACK_SIZE + 1).ok_or("non-zero")?;
    match Builder::new().thread_stack_size(Some(over)).build() {
        Err(error) if error.kind() == ErrorKind::InvalidInput => Ok(()),
        Err(other) => Err(format!("expected InvalidInput, got {other}").into()),
        Ok(_) => Err("a stack past the ceiling was accepted".into()),
    }
}
