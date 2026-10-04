//! Deliberately wrong solution — `recovery`, the store that is never reopened.
//!
//! This is a mutant input for the harness, not an example. It is the reference
//! solution with exactly one mutation, and the mutation is the plausible first
//! mistake this task exists to catch: **giving each attempt its own store
//! directory instead of the one the caller passed in**.
//!
//! Everything else is the reference — the same derived run identity, the same
//! task helper, the same store installation, the same read-before-apply on the
//! ledger. So the run still *completes* (clause 1), the effect is still applied
//! exactly once (clause 3, because the ledger lives in the world and not in the
//! store), the deadline still fires (clause 4) and a dropped future still leaves
//! nothing live (clause 5). What it cannot do is find the records a previous
//! attempt wrote: each attempt opens a store that no earlier attempt ever wrote
//! to, so every unit body runs again and the oracle's
//! `a_resume_does_not_rerun_a_completed_unit` must fail it.
//!
//! It is the NEW reference plus one mutation, never a forked copy: the runner
//! copies `reference/new-recovery.rs` beside this file as `mod reference`, and
//! `recover` delegates to it with a doctored store directory.

mod reference;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use ai_task_support::recovery::World;

pub use reference::RecoveryError;

/// How many attempts have been made, and therefore which private directory the
/// next one books itself under.
///
/// The mutation. Every counter in the reference is unchanged; this is the only
/// thing the mutant adds, and it exists so each attempt lands somewhere the
/// earlier ones never wrote.
static ATTEMPT: AtomicU32 = AtomicU32::new(0);

pub async fn recover(
    world: World,
    store_dir: PathBuf,
    deadline: Duration,
) -> Result<u64, RecoveryError> {
    let attempt = ATTEMPT.fetch_add(1, Ordering::SeqCst);
    // The reference stores under exactly the directory it is given; the mutant
    // gives it a fresh one per attempt, which is a store no earlier attempt ever
    // wrote to — so the resume has nothing to replay.
    let private = store_dir.join(format!("attempt-{attempt}"));
    std::fs::create_dir_all(&private).map_err(|error| {
        ai_task_support::diagnostic(format_args!("could not create {private:?}: {error}"));
        RecoveryError::NoStore
    })?;
    reference::recover(world, private, deadline).await
}