//! Probe: are the stated API invariants actually unrepresentable
//! through the `lgwks_bot::rt` facade, or merely re-exported?
//!
//! # Disposition: archived, deliberately not a built target
//!
//! This file is `examples/probes/invariant_probe.rs`, not `examples/…`, so
//! cargo does not auto-discover it. The reason is that its body is, by
//! construction, code the lint contract refuses, and the refusal is what
//! answers the question above.
//!
//! When this probe was written, `disallowed_methods` and `disallowed_types`
//! were warn-by-default and no `-D` promoted them, so every block below
//! *compiled*, and that compilation was the finding: the API bans were
//! nowhere fatal, and ten violations across the workspace were passing CI.
//! `Cargo.toml` now sets both to `deny` (2026-09-20), which is the remediation
//! this probe was written to force.
//!
//! That change is also why the file cannot build. Each claim's disposition:
//!
//! 1. `unbounded_channel()` **is** reachable: `rt::sync::mpsc` re-exports the
//!    engine's whole module, so the ban lives in `clippy.toml`, not in the type.
//!    Calling it is now a hard error, so the probe can no longer demonstrate the
//!    call; the ban refuses it, which is the point.
//! 2. Fire-and-forget **is** representable: a `JoinHandle` is droppable and
//!    nothing records the task (now `rt::task::spawn`, the sanctioned path).
//!    "Zero un-tracked background tasks" is a convention the caller keeps.
//! 3. A std `Mutex` held across `.await` is **no longer** representable:
//!    `await_holding_lock = "forbid"` refuses it, so the block that used to
//!    demonstrate otherwise cannot be written at all, a stricter outcome than
//!    the probe was built to detect.
//!
//! The body is kept verbatim as the audit record, minus the reasonless
//! `#[allow(clippy::unwrap_used)]` it opened with: that attribute was itself a
//! defect (a suppression with no reason), and against a `forbid` lint it is a
//! hard E0453 that no rewriting of this file could ever make legal.

fn main() {
    // INVARIANT CLAIM: "unbounded_channel() is forbidden."
    // If this line compiles, the facade re-exports the unbounded constructor.
    let (_tx, _rx) = lgwks_bot::rt::sync::mpsc::unbounded_channel::<u8>();

    // INVARIANT CLAIM: "Zero Un-Tracked Background Tasks."
    // If this compiles, fire-and-forget is representable: the JoinHandle is
    // droppable and nothing records the task.
    let rt = lgwks_bot::Runtime::new().unwrap();
    rt.block_on(async {
        let _dropped_on_the_floor = lgwks_bot::rt::task::spawn(async {});
        drop(_dropped_on_the_floor);
    });

    // INVARIANT CLAIM: "No Mutex across .await" / std Mutex banned in async.
    // If this compiles, the std Mutex is reachable and unprotected.
    let m = std::sync::Mutex::new(0u8);
    rt.block_on(async {
        let _guard = m.lock().unwrap();
        lgwks_bot::rt::task::yield_now().await;
    });
}
