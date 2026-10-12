//! Supervision: background work that cannot leak and cannot run away.
//!
//! A caller writing a bot should be thinking about the bot. They should not be
//! thinking about whether a task outlives its owner, whether a fan-out has a
//! ceiling, whether a `JoinSet` is being drained, or whether a `loop` has an
//! exit. Two failure modes cause almost all of that thinking, and both are
//! designed out here rather than documented away.
//!
//! # Why a task cannot leak
//!
//! Four mechanisms, none of which the caller has to remember:
//!
//! - **Nothing is detached.** [`Supervisor::spawn`] returns no handle. A
//!   `JoinHandle` a caller might drop is the leak; there is no handle to drop.
//!   The supervisor owns completion and exposes it as [`Stats`] and as a
//!   bounded stream of [`TaskOutcome`] reports.
//! - **Nothing grows without a ceiling.** There is no unbounded constructor and
//!   no internal queue: [`Supervisor::new`] takes the in-flight bound, and
//!   [`Supervisor::spawn`] applies backpressure by awaiting a permit before it
//!   spawns. The pending work is the caller's own collection, not a buffer this
//!   module accumulates. [`Supervisor::try_spawn`] refuses instead of growing,
//!   and counts the refusal.
//! - **Completed tasks are reaped.** A `JoinSet` retains a finished task's slot
//!   until it is joined, so a set that is spawned into but never drained grows
//!   with total spawns rather than with live tasks. Every entry point reaps
//!   first, which keeps the retained set at the live bound rather than the
//!   lifetime total.
//! - **Drop stops everything.** [`Drop`] cancels the token and aborts the set,
//!   so a supervisor that goes out of scope takes its tasks with it. There is no
//!   `close()` to forget.
//!
//! No reference cycle is possible either: cancellation links a child **up** to
//! its parent and a parent holds nothing back, so a task holding its token does
//! not keep the supervisor alive. That is [`CancellationToken`]'s design, not
//! this module's, and it is the reason this module adds no `Weak` bookkeeping.
//!
//! # Why a caller can tell how a task ended
//!
//! Owning completion is only half of it. A `JoinSet` hands back a
//! `Result<(), JoinError>`, and an owner that joins the set and looks only at
//! `is_some()` throws that result away: a task that returned, a task that was
//! aborted, and a task that panicked before producing anything all become the
//! same increment. The caller then cannot distinguish work that finished from
//! work that died, which is the one thing they needed to know.
//!
//! So every task this module starts ends in exactly one [`TaskOutcome`], and
//! the outcome is attributable: it carries the [`TaskId`] the supervisor
//! assigned in spawn order, so "the second task I started panicked" is a
//! statement the API can make. The reports are held in a buffer capped at the
//! in-flight bound — a caller that never drains it loses detail, never memory,
//! and [`Stats::reports_dropped`] says how much. [`Supervisor::shutdown`]
//! returns the drained outcomes instead of discarding them.
//!
//! # Why a loop cannot run away
//!
//! [`repeat`] is the only loop this module asks a caller to write, and it
//! cannot be written without a [`Budget`]. Every iteration races the
//! cancellation token, so a cancel always wins even against a body that is
//! itself awaiting: the body is dropped, not waited on. [`Budget::Ongoing`] is
//! the unbounded case, and it is bounded in the way that matters: it is
//! cancellation-terminated, not free-running.
//!
//! Termination cannot rest on the body suspending. A body that resolves
//! immediately — the simplest repeating body there is — would otherwise make the
//! whole loop a single uninterruptible poll, and a cancellation ordered by
//! another task would not be delivered until the budget ran out. `repeat`
//! therefore performs a bounded number of iterations per poll and yields, which
//! is what keeps the cancellation and abort halves of the contract reachable.
//! The bound is documented on `repeat` itself; the interval is private, and no
//! caller has to know it.
//!
//! ```no_run
//! use lgwks_bot::rt::supervise::{Budget, Supervisor};
//!
//! # async fn run() {
//! let mut supervisor = Supervisor::new(4);
//! // `Ongoing` is the unbounded budget, and it still ends: the supervisor owns
//! // the token this loop is raced against, and `shutdown` cancels it.
//! supervisor
//!     .spawn_repeating(Budget::Ongoing, |tick| async move {
//!         let _tick = tick;
//!     })
//!     .await;
//! // The drained evidence, not just an empty set.
//! let report = supervisor.shutdown().await;
//! # }
//! ```
//!
//! # Limits
//!
//! [`Supervisor::spawn`] awaits a permit, so a caller that holds the last
//! permits inside a task this supervisor owns can deadlock with itself. The
//! bound is a backpressure contract, not a queue: a caller who wants to keep
//! producing past the ceiling should use [`Supervisor::try_spawn`] and decide
//! what the refusal means.

include!("supervise/types.rs");
include!("supervise/reports.rs");

/// The async half of per-tenant admission: one lock over the pure round
/// scheduler, plus the parked waiters it hands permits to.
///
/// Beside [`Supervisor`] rather than inside [`rt::tenancy`](super::tenancy)
/// because it needs the supervisor's own permit pool and cancellation token,
/// and passing those across a module boundary would make them `pub(crate)` for a
/// type that already owns them privately. The scheduling decision itself lives
/// in [`rt::tenancy`](super::tenancy) and is untouched by any of this.
#[cfg(feature = "script")]
mod tenancy_support {
    include!("supervise/tenancy_support.rs");
}

include!("supervise/supervisor.rs");
include!("supervise/impl_core.rs");
include!("supervise/impl_process.rs");
include!("supervise/impl_lifecycle.rs");
include!("supervise/impl_outcomes.rs");
include!("supervise/process_start.rs");
include!("supervise/process_group.rs");
include!("supervise/process_capture.rs");
include!("supervise/repeat.rs");

/// The Windows process path: a job object owns the child's tree (#263).
#[cfg(all(windows, feature = "process"))]
mod job;

/// Settling a job's tree to a receipt, held apart from the `kernel32` calls so
/// every host drives the same decision table against a scripted job.
#[cfg(all(feature = "process", any(windows, test)))]
mod job_cleanup;

/// Read a captured pipe to EOF, retaining at most `limit` bytes and counting
/// every byte.
///
/// The pipe is drained even after the ceiling is reached, so a child that
/// writes far more than the ceiling cannot block on a full pipe. The retained
/// buffer is sized once to the ceiling, so the retained capacity is bounded by
/// the policy rather than by what the child wrote.
///
/// `on_line`, when given, is called once for each newline-terminated line of
/// **stdout** as it arrives, before the ceiling is consulted and whether or not
/// the bytes are retained. It is what lets a service's readiness come from what
/// the child printed rather than from a timer: the observer sees the same bytes
/// the capture retains, in the same order, at the moment they were read — so it
/// is an observation of the run rather than a second read of the child's
/// output. A trailing fragment with no newline is not delivered: a partial line
/// is not a line, and a caller waiting for one must keep waiting for the
/// newline rather than being woken by half a message.
#[cfg(all(unix, feature = "process"))]
async fn capture<R>(
    reader: Option<R>,
    limit: Option<NonZeroUsize>,
    on_line: Option<LineObserver<'_>>,
) -> CapturedStream
where
    R: AsyncRead + Unpin,
{
    let (Some(mut reader), Some(limit)) = (reader, limit) else {
        // No pipe (inherited or null) or no capture policy: nothing retained.
        return CapturedStream::default();
    };
    let cap = limit.get();
    let mut bytes: Vec<u8> = Vec::with_capacity(cap);
    let mut total: u64 = 0;
    // Whether any read carried more than the retained buffer had room for.
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];
    // Bytes of the current line not yet terminated. Bounded by
    // `MAX_OBSERVED_LINE_BYTES`, so a child that writes an unterminated
    // megabyte cannot grow this without limit: the fragment is dropped once it
    // exceeds the bound and the count says how much was not observed.
    let mut fragment: Vec<u8> = Vec::new();
    let mut dropped_from_fragment: u64 = 0;
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                // A `usize` wider than 64 bits saturates the counter, the same
                // contract `saturating_add` keeps for a long-lived stream.
                total = match u64::try_from(read) {
                    Ok(count) => total.saturating_add(count),
                    Err(_) => u64::MAX,
                };
                let Some(chunk) = buffer.get(..read) else {
                    continue;
                };
                if let Some(observe) = on_line {
                    for line in lines_of(chunk, &mut fragment, &mut dropped_from_fragment) {
                        observe(line);
                    }
                }
                let room = cap.saturating_sub(bytes.len());
                truncated |= read > room;
                if room > 0 {
                    let take = room.min(read);
                    if let Some(head) = buffer.get(..take) {
                        bytes.extend_from_slice(head);
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    CapturedStream::from_parts(bytes, total, truncated)
}

/// The longest unterminated line the stdout observer will hold.
///
/// A child printing a megabyte without a newline is a child that is not
/// speaking the line protocol an observer waits for, and retaining that
/// megabyte on its behalf would make the observer the unbounded buffer the
/// capture ceiling exists to prevent. 8 KiB is longer than any line a service
/// announces its readiness with, and longer than the pipe read buffer, so a line
/// that fits in one read is never split across the bound.
#[cfg(all(unix, feature = "process"))]
const MAX_OBSERVED_LINE_BYTES: usize = 8 * 1024;

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    include!("supervise/tests/part_a.rs");
    include!("supervise/tests/part_b.rs");
    include!("supervise/tests/part_c.rs");
    include!("supervise/tests/part_d.rs");
}
