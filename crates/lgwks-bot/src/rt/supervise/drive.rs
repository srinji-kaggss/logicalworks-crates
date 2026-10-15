//! Driving one supervised child: observing its exit, draining its pipes, reaping it.

use super::LineObserver;
use super::group::ProcessGroup;
use super::outcome::CleanupReceipt;
use super::start::OwnedChild;
use super::supervisor::{ProcessCleanup, TaskEnd};
use crate::rt::cancel::CancellationToken;
use crate::rt::clock::Clock;
use crate::rt::io::{AsyncRead, AsyncReadExt};
use crate::rt::process::CapturedStream;
use crate::rt::task::yield_now;
use std::io;
use std::num::NonZeroUsize;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::time::Duration;

/// What happened while observing a supervised child without releasing its pid.
#[derive(Clone, Copy)]
#[cfg(all(unix, feature = "process"))]
pub(super) enum ProcessObservation {
    /// The child exited; the status is retrieved after cleanup on Unix.
    Exited,
    /// The supervisor's cancellation token was set.
    Cancelled,
    /// The process deadline elapsed before an exit was observed.
    Deadline,
    /// The platform could not perform non-reaping exit observation.
    Unobservable,
}

/// Poll `pid`'s exit state without reaping it, keeping its pid allocated until
/// the process-group termination phase is complete.
///
/// Takes the numeric id rather than the engine's child, so the caller can keep
/// the child free to be waited on once the group has been signalled.
#[cfg(all(unix, feature = "process"))]
pub(super) async fn observe_pid_without_reaping(
    clock: &Clock,
    pid: i32,
    deadline: Option<Duration>,
    token: &CancellationToken,
) -> ProcessObservation {
    if pid <= 0 {
        return ProcessObservation::Unobservable;
    }
    // The deadline is anchored on the governing clock's own timeline, so a
    // caller-advanceable clock exhausts it in one arithmetic step and a wall
    // clock exhausts it when real time says so — the same refusal either way.
    let started_at = clock.now();
    let deadline = deadline.map(|duration| started_at.saturating_add(duration));
    loop {
        if token.is_cancelled() {
            return ProcessObservation::Cancelled;
        }
        if deadline.is_some_and(|at| clock.now() >= at) {
            return ProcessObservation::Deadline;
        }
        match lgwks_std::process::child_has_exited_without_reaping(pid) {
            Ok(true) => return ProcessObservation::Exited,
            Ok(false) => {}
            Err(_) => return ProcessObservation::Unobservable,
        }
        if token
            .run_until_cancelled(crate::rt::time::sleep(Duration::from_millis(5)))
            .await
            .is_none()
        {
            return ProcessObservation::Cancelled;
        }
    }
}

/// The terminal observation of one supervised child, before it is mapped to a
/// public report.
///
/// One driver produces this shape for both [`Supervisor::spawn_process`] and
/// [`Supervisor::run_process`], so the two cannot drift into cleaning up a
/// group differently: the mapping to a `TaskEnd` or a `ProcessRun` happens at
/// the edge.
#[cfg(all(unix, feature = "process"))]
pub(super) struct ProcessEnd {
    /// What the observation saw.
    pub(super) observation: ProcessObservation,
    /// The status the engine reported, when there is one.
    pub(super) status: Option<ExitStatus>,
    /// The cleanup receipt and what it was based on.
    pub(super) cleanup: ProcessCleanup,
    /// Captured stdout.
    pub(super) stdout: CapturedStream,
    /// Captured stderr.
    pub(super) stderr: CapturedStream,
}

/// Map a driven child's terminal observation to the supervisor's internal
/// task-end, so the public `TaskOutcome` is derived from one place.
#[cfg(all(unix, feature = "process"))]
pub(super) fn task_end(end: ProcessEnd) -> TaskEnd {
    // The signal phase ran while the leader was unreaped; signals are over by the
    // time this is constructed. A `Pending` cleanup stays in this task through the
    // bounded post-reap, signal-zero probe.
    let ProcessEnd {
        observation,
        status,
        cleanup,
        ..
    } = end;
    match observation {
        ProcessObservation::Exited => match status {
            Some(ref observed) if observed.success() => TaskEnd::CompletedWithCleanup { cleanup },
            status => TaskEnd::Failed { status, cleanup },
        },
        ProcessObservation::Cancelled => TaskEnd::CancelledWithCleanup { cleanup },
        ProcessObservation::Deadline | ProcessObservation::Unobservable => TaskEnd::Failed {
            status: None,
            cleanup,
        },
    }
}

/// Drive one started child to a terminal observation, observing nothing.
///
/// This is the driver behind [`Supervisor::spawn_process`], whose body runs on
/// a spawned task that must be `Send` — and an observed driver's parameter is a
/// `dyn Fn`, which is not. The observation is therefore an argument of
/// [`drive_process_observed`] and *this* call supplies `None`, so there is one
/// body of process driving rather than two and the spawned path is a name for
/// the unobserved one rather than a second implementation of it.
#[cfg(all(unix, feature = "process"))]
pub(super) async fn drive_process(
    clock: &Clock,
    child: OwnedChild,
    token: &CancellationToken,
    group: ProcessGroup<'static>,
    bounds: RunBounds,
) -> ProcessEnd {
    drive_process_observed(clock, child, token, group, bounds, None).await
}

/// The bounds one process run is held to, as its spec declares them.
///
/// One value rather than three parameters: [`Supervisor::prepare_process`]
/// resolves all three from the same spec, so a deadline can never travel with
/// another run's capture ceilings.
#[cfg(all(unix, feature = "process"))]
#[derive(Debug, Clone, Copy)]
pub(super) struct RunBounds {
    /// How long the child may run before it is stopped.
    pub(super) deadline: Option<Duration>,
    /// The most stdout bytes retained.
    pub(super) out_limit: Option<NonZeroUsize>,
    /// The most stderr bytes retained.
    pub(super) err_limit: Option<NonZeroUsize>,
}

/// Drive one started child to a terminal observation: drain its captured
/// streams, waiting for its exit (or the deadline, or cancellation), signal its
/// process group, and reap it.
///
/// This is the single body behind [`Supervisor::run_process`] and
/// [`Supervisor::run_process_observed`]. The three phases run concurrently as
/// [`join3`]: the two pipe reads must make progress while the child runs, or a
/// child that writes more than a pipe buffer would block forever waiting for a
/// reader that is waiting for it to exit.
///
/// `on_line` is forwarded to the **stdout** read only. It is not a second read
/// of the child's output: it is called from inside the read that retains that
/// output, on the bytes that read just observed, so an observation and a
/// capture can never disagree about what the child wrote.
///
/// The deadline and the two capture ceilings arrive as one [`RunBounds`],
/// resolved from a single spec by [`Supervisor::prepare_process`].
#[cfg(all(unix, feature = "process"))]
pub(super) async fn drive_process_observed(
    clock: &Clock,
    mut child: OwnedChild,
    token: &CancellationToken,
    mut group: ProcessGroup<'static>,
    bounds: RunBounds,
    on_line: Option<LineObserver<'_>>,
) -> ProcessEnd {
    let RunBounds {
        deadline,
        out_limit,
        err_limit,
    } = bounds;
    let pid = child.group().ok();
    let pipes = OpenPipes::default();
    let abandon = CancellationToken::new();
    let capture_out = capture(
        child.stdout.take(),
        out_limit,
        on_line,
        &abandon,
        &pipes.stdout,
    );
    let capture_err = capture(
        child.stderr.take(),
        err_limit,
        None,
        &abandon,
        &pipes.stderr,
    );
    // Reading the pipes and waiting for the child are independent futures over
    // disjoint state; only the group is borrowed, and that borrow ends when
    // this future completes.
    let wait = async {
        let observation = match pid {
            Some(pid) => observe_pid_without_reaping(clock, pid, deadline, token).await,
            None => ProcessObservation::Unobservable,
        };
        if matches!(observation, ProcessObservation::Exited) {
            group.mark_exited();
        }
        let cleanup = group.cleanup().await;
        drain_pipes(&mut group, &pipes, &abandon).await;
        (observation, cleanup)
    };
    let (stdout, stderr, (observation, mut receipt)) = join3(capture_out, capture_err, wait).await;
    if !pipes.closed() {
        group.mark_descriptor_held();
    }

    if matches!(receipt, CleanupReceipt::CleanupFailed)
        && !matches!(observation, ProcessObservation::Exited)
    {
        // Do not wait on a child whose group signal could not be delivered. The
        // owned child is dropped immediately after this return, and
        // `kill_on_drop` remains the direct-child fallback.
        let containment = group.containment().clone();
        drop(group);
        return ProcessEnd {
            observation,
            status: None,
            cleanup: ProcessCleanup {
                receipt,
                containment,
            },
            stdout,
            stderr,
        };
    }
    let (status, cleanup) = if matches!(receipt, CleanupReceipt::CleanupFailed) {
        // Keep the leader unreaped while the armed guard makes its final
        // synchronous kill attempt. The receipt reports the syscall failure;
        // dropping the child then relinquishes it to the platform's orphan
        // reaper rather than signalling a stale numeric group id.
        let containment = group.containment().clone();
        drop(group);
        let status = child.reap().await.ok();
        (
            status,
            ProcessCleanup {
                receipt,
                containment,
            },
        )
    } else {
        let status = child.reap().await.ok();
        group.mark_reaped();
        // Every unresolved arm goes through the post-reap pass, not only
        // `Pending`: a survivor list is exactly what a further observation can
        // clear, since the platform reaps what stopped running once the leader is
        // gone. No arm is promoted to `Confirmed` by anything but that pass.
        if !matches!(receipt, CleanupReceipt::CleanupConfirmed) {
            receipt = group.confirm_absence().await;
        }
        let containment = group.containment().clone();
        (
            status,
            ProcessCleanup {
                receipt,
                containment,
            },
        )
    };
    ProcessEnd {
        observation,
        status,
        cleanup,
        stdout,
        stderr,
    }
}

/// How long one slice of the post-cleanup drain waits before it looks again.
///
/// The same 5 ms the exit observation polls at: short enough that a pipe that
/// closes a moment after the cleanup costs the run one slice, and long enough
/// that a held pipe costs a bounded number of group signals.
const PIPE_DRAIN_SLICE: Duration = Duration::from_millis(5);

/// How many slices the drain after cleanup waits for both pipes to close.
///
/// 20 slices of [`PIPE_DRAIN_SLICE`] is 100 ms: a killed group's members close
/// their pipe ends within one scheduling round of their death, so a pipe still
/// open after 100 ms of re-signalling is held by a process the group signal
/// cannot reach. The bound is what keeps that process from deciding how long
/// the run lasts (#263: one held pipe kept a 400 ms run alive for 30.6 s).
const PIPE_DRAIN_SLICES: u32 = 20;

/// Whether each captured pipe reached its end.
///
/// Set by the capture that owns the pipe and read by the drain that bounds
/// it. Atomics rather than cells because a spawned process task must be
/// `Send`, and the three futures that share these run on that one task.
#[cfg(all(unix, feature = "process"))]
#[derive(Default)]
struct OpenPipes {
    /// Whether stdout reached its end, or was never a pipe.
    stdout: AtomicBool,
    /// Whether stderr reached its end, or was never a pipe.
    stderr: AtomicBool,
}

#[cfg(all(unix, feature = "process"))]
impl OpenPipes {
    /// Whether both pipes reached their end.
    fn closed(&self) -> bool {
        self.stdout.load(AtomicOrdering::Acquire) && self.stderr.load(AtomicOrdering::Acquire)
    }
}

/// Give the captured pipes a bounded drain after cleanup, then abandon them.
///
/// The cleanup stopped what the group signal and the capture rounds reach. A
/// pipe end can still be held by a descendant they do not reach, and a read to
/// end-of-file would then last as long as that descendant: a `setsid` daemon,
/// a job a job-control shell moved to its own group, or a child forked in the
/// window `killpg` misses on XNU. So the drain waits at most
/// [`PIPE_DRAIN_SLICES`] slices for both pipes to close, re-signalling the
/// group on each slice while the unreaped leader still pins its id — that is
/// what catches a forked straggler that is still a member. When the bound
/// passes, `abandon` ends both reads with what they hold, and the driver
/// records [`ResidualRisk::DescriptorHeld`](crate::rt::supervise::ResidualRisk::DescriptorHeld).
///
/// The precedent is `lgwks_deps::metadata::run_bounded`, which ends its call at
/// the deadline even while a grandchild holds the pipe.
#[cfg(all(unix, feature = "process"))]
async fn drain_pipes(group: &mut ProcessGroup<'_>, pipes: &OpenPipes, abandon: &CancellationToken) {
    // One reschedule first: a pipe that closed with the group has delivered its
    // end-of-file to the reactor, and the captures see it once they are polled
    // again, so the common case pays no slice at all.
    yield_now().await;
    let mut may_signal = true;
    for _ in 0..PIPE_DRAIN_SLICES {
        if pipes.closed() {
            return;
        }
        if may_signal {
            may_signal = group.resignal_while_pinned();
        }
        crate::rt::time::sleep(PIPE_DRAIN_SLICE).await;
    }
    if !pipes.closed() {
        abandon.cancel();
    }
}

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
///
/// `abandon` ends the read early with what it holds, and `closed` is set only
/// when the read reached the pipe's end (or there was no pipe to read), so the
/// driver can tell a drained pipe from an abandoned one. A read error also sets
/// it: the pipe can no longer be read, so nothing is held open on its account.
#[cfg(all(unix, feature = "process"))]
pub(super) async fn capture<R>(
    reader: Option<R>,
    limit: Option<NonZeroUsize>,
    on_line: Option<LineObserver<'_>>,
    abandon: &CancellationToken,
    closed: &AtomicBool,
) -> CapturedStream
where
    R: AsyncRead + Unpin,
{
    let (Some(mut reader), Some(limit)) = (reader, limit) else {
        // No pipe (inherited or null) or no capture policy: nothing retained,
        // and dropping the reader closes this end, so nothing is held.
        closed.store(true, AtomicOrdering::Release);
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
        // The read is cancel-safe: abandoning it loses no byte already read.
        let Some(result) = abandon.run_until_cancelled(reader.read(&mut buffer)).await else {
            break;
        };
        match result {
            Ok(0) => {
                closed.store(true, AtomicOrdering::Release);
                break;
            }
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
            Err(_) => {
                closed.store(true, AtomicOrdering::Release);
                break;
            }
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
pub(super) const MAX_OBSERVED_LINE_BYTES: usize = 8 * 1024;

/// Append `chunk` to `fragment`, yielding each newline-terminated line.
///
/// Split out of [`capture`] so the splitting rule is one definition rather than
/// one copy per captured stream, and so it can be exercised without a child.
///
/// `fragment` holds the unterminated tail of the last line. It is capped at
/// [`MAX_OBSERVED_LINE_BYTES`]: a longer tail is dropped rather than grown, and
/// `dropped` accumulates what was not observed so a caller can tell a short line
/// from a line this observer refused to assemble.
#[cfg(all(unix, feature = "process"))]
pub(super) fn lines_of<'a>(
    chunk: &'a [u8],
    fragment: &'a mut Vec<u8>,
    dropped: &mut u64,
) -> impl Iterator<Item = &'a [u8]> {
    let mut complete = Vec::new();
    for (index, byte) in chunk.iter().enumerate() {
        if *byte != b'\n' {
            if fragment.len() < MAX_OBSERVED_LINE_BYTES {
                fragment.push(*byte);
            } else {
                *dropped = dropped.saturating_add(1);
            }
            continue;
        }
        let end = index.saturating_sub(fragment.len());
        if let Some(line) = chunk.get(end..index) {
            complete.push(strip_carriage_return(line));
        }
        fragment.clear();
    }
    complete.into_iter()
}

/// Drop one trailing carriage return, so a CRLF line and an LF line observe the
/// same bytes.
#[cfg(all(unix, feature = "process"))]
pub(super) fn strip_carriage_return(line: &[u8]) -> &[u8] {
    match line.split_last() {
        Some((&b'\r', head)) => head,
        _ => line,
    }
}

/// Poll three futures to completion on the calling task, returning their
/// outputs together.
///
/// The engine's `join!` is behind the `macros` feature, which the `process`
/// feature does not require, so this is the small combinator that build needs.
/// Nothing is spawned and nothing is `Send`-bound: a child's two pipes must be
/// drained while it is observed, and the futures that do so borrow the caller's
/// child and group, which a spawned task could not.
#[cfg(all(unix, feature = "process"))]
pub(super) async fn join3<A, B, C>(
    first: A,
    second: B,
    third: C,
) -> (A::Output, B::Output, C::Output)
where
    A: Future,
    B: Future,
    C: Future,
{
    let mut first = std::pin::pin!(first);
    let mut second = std::pin::pin!(second);
    let mut third = std::pin::pin!(third);
    let mut first_out: Option<A::Output> = None;
    let mut second_out: Option<B::Output> = None;
    let mut third_out: Option<C::Output> = None;
    std::future::poll_fn(|context| {
        if first_out.is_none()
            && let std::task::Poll::Ready(value) = first.as_mut().poll(context)
        {
            first_out = Some(value);
        }
        if second_out.is_none()
            && let std::task::Poll::Ready(value) = second.as_mut().poll(context)
        {
            second_out = Some(value);
        }
        if third_out.is_none()
            && let std::task::Poll::Ready(value) = third.as_mut().poll(context)
        {
            third_out = Some(value);
        }
        if first_out.is_some() && second_out.is_some() && third_out.is_some() {
            let first_value = first_out.take();
            let second_value = second_out.take();
            let third_value = third_out.take();
            return match (first_value, second_value, third_value) {
                (Some(first_value), Some(second_value), Some(third_value)) => {
                    std::task::Poll::Ready((first_value, second_value, third_value))
                }
                _ => std::task::Poll::Pending,
            };
        }
        std::task::Poll::Pending
    })
    .await
}
