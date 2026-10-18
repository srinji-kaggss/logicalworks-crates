//! The supervisor's unit tests: the shared seams and helpers, and one module per concern.

#[cfg(all(unix, feature = "process"))]
use super::group::ProcessGroup;
#[cfg(all(unix, feature = "process"))]
use super::lease::Lease;
#[cfg(all(unix, feature = "process"))]
use super::outcome::Containment;
use super::outcome::TaskOutcome;
#[cfg(all(unix, feature = "process"))]
use super::owners::{CleanupOwners, DescendantCapture, GroupObserver, GroupSignaller};
use super::supervisor::Supervisor;
use super::{Budget, TaskId};
#[cfg(all(not(any(unix, windows)), feature = "process"))]
use crate::rt::process::ProcessSpec;
#[cfg(all(not(any(unix, windows)), feature = "process"))]
use crate::rt::runtime::block_on;
use crate::rt::task::yield_now;
#[cfg(all(unix, feature = "process"))]
use lgwks_std::process::ContainmentMechanism;
use std::num::NonZeroU64;
use std::sync::Arc;
#[cfg(all(unix, feature = "process"))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(all(not(any(unix, windows)), feature = "process"))]
#[test]
fn non_unix_process_group_spawn_is_refused_as_unsupported() {
    let mut supervisor = Supervisor::new(1);
    let result = block_on(supervisor.spawn_process(&ProcessSpec::new("unused")));
    assert_eq!(
        result.as_ref().err().map(std::io::Error::kind),
        Some(std::io::ErrorKind::Unsupported),
        "non-Unix callers receive an explicit safe refusal before starting a process"
    );
    assert_eq!(supervisor.stats().spawned, 0);
}

#[cfg(all(not(any(unix, windows)), feature = "process"))]
#[test]
fn non_unix_process_group_run_is_refused_as_unsupported() {
    let mut supervisor = Supervisor::new(1);
    let result = block_on(supervisor.run_process(&ProcessSpec::new("unused")));
    assert!(
        result.as_ref().err().is_some_and(|error| match *error {
            crate::rt::process::ProcessRunError::NotStarted { ref source } => {
                source.kind() == std::io::ErrorKind::Unsupported
            }
            _ => false,
        }),
        "the result-bearing runner must refuse on non-Unix before the fork, got {result:?}"
    );
    assert_eq!(
        supervisor.stats().spawned,
        0,
        "a refused run must start nothing"
    );
}

/// Panic on the calling task, carrying `message` as the payload.
///
/// `resume_unwind` and not `panic!`: the workspace forbids `panic` outright
/// with no suppression path, and this is the documented form for a
/// deliberate panic — it reports on the task that observes it and never
/// aborts the process. A `panic-in-a-task` test that let the panic reach
/// the test harness would prove nothing about the API that is supposed to
/// report it, and would take the rest of the suite down with it.
fn explode(message: &'static str) {
    std::panic::resume_unwind(Box::new(message));
}

/// A budget of `iterations`. Zero cannot be expressed, so it reads as the
/// unbounded case rather than as a budget that never runs.
fn budget_of(iterations: u64) -> Budget {
    match NonZeroU64::new(iterations) {
        Some(limit) => Budget::Iterations(limit),
        None => Budget::Ongoing,
    }
}

#[cfg(all(unix, feature = "process"))]
struct RecordingGroupSignaller {
    calls: AtomicUsize,
}

#[cfg(all(unix, feature = "process"))]
impl GroupSignaller for RecordingGroupSignaller {
    fn signal(&self, _group: i32) -> std::io::Result<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// A group whose leader is an unreaped zombie: macOS/BSD report `EPERM` for
/// a further `killpg` while the group is still present.
#[cfg(all(unix, feature = "process"))]
struct EpermGroupSignaller;

#[cfg(all(unix, feature = "process"))]
impl GroupSignaller for EpermGroupSignaller {
    fn signal(&self, _group: i32) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the group leader is a zombie",
        ))
    }
}

/// An error that is neither absence nor the zombie case: a genuine refusal.
#[cfg(all(unix, feature = "process"))]
struct UnexpectedGroupSignaller;

#[cfg(all(unix, feature = "process"))]
impl GroupSignaller for UnexpectedGroupSignaller {
    fn signal(&self, _group: i32) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "injected unexpected failure",
        ))
    }
}

#[cfg(all(unix, feature = "process"))]
struct SequenceGroupObserver {
    calls: AtomicUsize,
    present_before_absent: usize,
}

#[cfg(all(unix, feature = "process"))]
impl GroupObserver for SequenceGroupObserver {
    fn exists(&self, _group: i32) -> std::io::Result<bool> {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(call < self.present_before_absent)
    }
}

/// A capture driven by the test rather than by the process table.
///
/// Every combination the drain can meet — a tree nobody can read, a pid that
/// refuses its signal, a pid that stays running whatever is sent, a truncated
/// reading — is a value here rather than a process, which is what makes the
/// decision table testable at all. The counters are how a test observes what
/// the drain *did*, as opposed to what it reported.
#[cfg(all(unix, feature = "process"))]
#[derive(Default)]
struct ScriptedCapture {
    /// The pids every reading names.
    tree: Vec<i32>,
    /// The pids a signal is refused for, as `ESRCH` would be for a process
    /// that has already ended.
    gone: Vec<i32>,
    /// The pids that keep running whatever is sent to them.
    immortal: Vec<i32>,
    /// The pids that read as running until `dying_for` observations have been
    /// taken and as gone after: a SIGKILLed process leaves the table on the
    /// scheduler's turn, not on the signaller's.
    dying: Vec<i32>,
    /// How many observations a `dying` pid outlasts.
    dying_for: usize,
    /// How many observations the drain asked for, across both of its phases.
    observed: AtomicUsize,
    /// Whether every reading fails, the way a target with no process table
    /// reports.
    unreadable: bool,
    /// Whether the reading is truncated at its bound.
    truncated: bool,
    /// How many readings the drain took.
    reads: AtomicUsize,
    /// How many signals the drain delivered.
    signals: AtomicUsize,
}

#[cfg(all(unix, feature = "process"))]
impl ScriptedCapture {
    /// A reading that names `tree`.
    fn of(tree: &[i32]) -> Self {
        Self {
            tree: tree.to_vec(),
            ..Self::default()
        }
    }

    /// Readings, so a test can count them.
    fn reads(&self) -> usize {
        self.reads.load(Ordering::Relaxed)
    }

    /// Delivered signals, so a test can count them.
    fn signals(&self) -> usize {
        self.signals.load(Ordering::Relaxed)
    }
}

#[cfg(all(unix, feature = "process"))]
impl DescendantCapture for ScriptedCapture {
    fn capture(&self, _root: i32) -> std::io::Result<super::owners::Capture> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        if self.unreadable {
            let refusal = Err(std::io::Error::other("injected unreadable process table"));
            lgwks_std::trace::debug!(
                error = ?refusal.as_ref().err(),
                "scripted capture: returning an error to the caller"
            );
            return refusal;
        }
        Ok(super::owners::Capture {
            mechanism: ContainmentMechanism::ProcessTableSnapshot,
            pids: self.tree.clone(),
            truncated: self.truncated,
        })
    }

    fn signal(&self, pid: i32) -> std::io::Result<()> {
        if self.gone.contains(&pid) {
            let refusal = Err(std::io::Error::from_raw_os_error(3));
            lgwks_std::trace::debug!(
                pid,
                error = ?refusal.as_ref().err(),
                "scripted signal: returning an error to the caller"
            );
            return refusal;
        }
        self.signals.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn running(&self, pids: &[i32]) -> std::io::Result<Vec<i32>> {
        let seen = self.observed.fetch_add(1, Ordering::Relaxed);
        Ok(pids
            .iter()
            .copied()
            .filter(|pid| {
                self.immortal.contains(pid) || (self.dying.contains(pid) && seen < self.dying_for)
            })
            .collect())
    }
}

#[cfg(all(unix, feature = "process"))]
/// A report from a whole reading that found no descendant.
///
/// What a real drain carries when the process table was read and the tree was
/// empty. `Containment::default()` is the *absence* of a report — a group-only
/// cleanup — and a registry entry holding one is never clean, which is the
/// point: an obligation that never captured anything has not shown it stopped
/// the whole tree.
fn whole_capture() -> Containment {
    Containment {
        mechanism: ContainmentMechanism::ProcessTableSnapshot,
        captured: 0,
        signalled: 0,
        survivors: Vec::new(),
        residual: None,
    }
}

#[cfg(all(unix, feature = "process"))]
/// The task ids one drive settled, for the attribution assertions.
///
/// The settled pairs carry the report each obligation ended with; these tests
/// are about which task a receipt belongs to, and a shared reader keeps the
/// pattern off both call sites.
fn settled_tasks(settled: &[(TaskId, Containment)]) -> Vec<TaskId> {
    settled.iter().map(|pair| pair.0).collect()
}

#[cfg(all(unix, feature = "process"))]
/// A group armed for one test, with every seam it drives.
///
/// One constructor rather than seven literals: the drain grew three fields, and
/// a literal per test is how half of them end up unset in exactly one arm.
fn armed_group<'ops>(
    group: i32,
    task: TaskId,
    leader_reaped: bool,
    permit: Option<Lease>,
    owners: Arc<CleanupOwners>,
    seams: Seams<'ops>,
) -> ProcessGroup<'ops> {
    ProcessGroup {
        group,
        task,
        permit,
        owners,
        armed: true,
        leader_reaped,
        leader_exited: false,
        group_absent: false,
        containment: Containment::default(),
        signalled_pids: std::collections::BTreeSet::new(),
        #[cfg(all(target_os = "linux", feature = "process"))]
        cgroup: None,
        death_poll: std::time::Duration::ZERO,
        signaller: seams.signaller,
        observer: seams.observer,
        capture: seams.capture,
    }
}

#[cfg(all(unix, feature = "process"))]
/// The three boundaries a cleanup test drives, as one value.
///
/// They are one thing to a reader: what the drain is allowed to *see* and
/// *touch*. Passing them as three arguments beside the group's own identity
/// made every call site a list of eight values whose order nothing checked.
#[derive(Copy, Clone)]
struct Seams<'ops> {
    /// The group-signalling boundary.
    signaller: &'ops dyn GroupSignaller,
    /// The group-existence boundary.
    observer: &'ops dyn GroupObserver,
    /// The per-process containment boundary.
    capture: &'ops dyn DescendantCapture,
}

#[cfg(all(unix, feature = "process"))]
/// The seams a group-only test drives: a signaller and observer of the test's
/// own, and a reading that names nothing.
fn group_seams<'ops>(
    signaller: &'ops dyn GroupSignaller,
    observer: &'ops dyn GroupObserver,
) -> Seams<'ops> {
    Seams {
        signaller,
        observer,
        capture: &NO_CAPTURE,
    }
}

#[cfg(all(unix, feature = "process"))]
/// The seams a containment test drives, with its own reading.
fn capture_seams<'ops>(
    signaller: &'ops dyn GroupSignaller,
    observer: &'ops dyn GroupObserver,
    capture: &'ops dyn DescendantCapture,
) -> Seams<'ops> {
    Seams {
        signaller,
        observer,
        capture,
    }
}

#[cfg(all(unix, feature = "process"))]
/// The reading every group-only test uses: a table that named nothing.
///
/// The reading every group-only test uses: a table that named nothing.
///
/// A `static` rather than a `const`, because the seams hold a reference to it
/// and a `const` would be a fresh temporary per use. Its counters are never
/// read by a test that uses it: a group-only test is about the group.
static NO_CAPTURE: ScriptedCapture = ScriptedCapture {
    tree: Vec::new(),
    gone: Vec::new(),
    immortal: Vec::new(),
    dying: Vec::new(),
    dying_for: 0,
    observed: AtomicUsize::new(0),
    unreadable: false,
    truncated: false,
    reads: AtomicUsize::new(0),
    signals: AtomicUsize::new(0),
};

#[cfg(all(unix, feature = "process"))]
struct FailingGroupObserver;

#[cfg(all(unix, feature = "process"))]
impl GroupObserver for FailingGroupObserver {
    fn exists(&self, _group: i32) -> std::io::Result<bool> {
        Err(std::io::Error::other("injected observation failure"))
    }
}

/// A guard over the fixed test group 42, with its own lease and owners.
#[cfg(all(unix, feature = "process"))]
fn test_group<'ops>(
    signaller: &'ops dyn GroupSignaller,
    observer: &'ops dyn GroupObserver,
    leader_reaped: bool,
) -> ProcessGroup<'ops> {
    armed_group(
        42,
        TaskId(0),
        leader_reaped,
        test_lease(),
        Arc::new(CleanupOwners::default()),
        group_seams(signaller, observer),
    )
}

/// A containment test's guard: test group 42 with an unreaped leader, its
/// own lease and owners, and the reading the test scripts.
#[cfg(all(unix, feature = "process"))]
fn capture_group<'ops>(
    signaller: &'ops dyn GroupSignaller,
    observer: &'ops dyn GroupObserver,
    capture: &'ops dyn DescendantCapture,
) -> ProcessGroup<'ops> {
    armed_group(
        42,
        TaskId(0),
        false,
        test_lease(),
        Arc::new(CleanupOwners::default()),
        capture_seams(signaller, observer, capture),
    )
}

/// An observer that reports the group present `present_before_absent`
/// times, then absent.
#[cfg(all(unix, feature = "process"))]
const fn sequence(present_before_absent: usize) -> SequenceGroupObserver {
    SequenceGroupObserver {
        calls: AtomicUsize::new(0),
        present_before_absent,
    }
}

/// A signaller that counts its calls and always succeeds.
#[cfg(all(unix, feature = "process"))]
const fn recording() -> RecordingGroupSignaller {
    RecordingGroupSignaller {
        calls: AtomicUsize::new(0),
    }
}

#[cfg(all(unix, feature = "process"))]
fn test_lease() -> Option<Lease> {
    let permit = Arc::new(crate::rt::sync::Semaphore::new(1))
        .try_acquire_owned()
        .ok()?;
    Some(Lease::plain(permit))
}

/// Drive the current task until nothing the supervisor started is still
/// running.
///
/// Returns `false` if the spin cap was reached, which would mean a task
/// never completed. A test that asserts on the return value turns a hang
/// into a failure instead of a timeout.
///
/// It must be awaited on the runtime that spawned the tasks. [`block_on`]
/// builds a current-thread runtime per call, so a task spawned in one call
/// is aborted when that call's runtime is dropped and cannot be observed
/// from the next, which is why every test here wraps its whole body in a
/// single `block_on` rather than reaching for one per step.
async fn settle(supervisor: &mut Supervisor) -> bool {
    let mut spins: u32 = 0;
    loop {
        supervisor.reap();
        if supervisor.stats().in_flight() == 0 {
            return true;
        }
        if spins >= 100_000 {
            return false;
        }
        spins = spins.saturating_add(1);
        yield_now().await;
    }
}

/// Drain every terminal outcome the supervisor is holding, oldest first.
fn drain(supervisor: &mut Supervisor) -> Vec<TaskOutcome> {
    let mut outcomes = Vec::new();
    while let Some(outcome) = supervisor.next_report() {
        outcomes.push(outcome);
    }
    outcomes
}

/// The identities of every outcome satisfying `keep`, in completion order.
///
/// Attribution is asserted through this rather than by matching on the
/// outcome variants: completion order between two tasks that finish in the
/// same scheduling pass is the engine's business, not this module's
/// contract, so a test that pinned it would be pinning the wrong thing.
fn tasks_where(outcomes: &[TaskOutcome], keep: impl Fn(&TaskOutcome) -> bool) -> Vec<TaskId> {
    outcomes
        .iter()
        .filter(|outcome| keep(outcome))
        .map(TaskOutcome::task)
        .collect()
}

/// Yield to the executor until `predicate` holds, or `limit` yields have
/// happened.
///
/// Returns whether the predicate held. A bounded spin rather than a timer:
/// the executor under test here is the one a timer would need, so waiting
/// for the thing being measured is the failure mode this helper exists to
/// avoid.
async fn yield_until(mut predicate: impl FnMut() -> bool, limit: u32) -> bool {
    let mut yields: u32 = 0;
    while !predicate() {
        if yields >= limit {
            return false;
        }
        yields = yields.saturating_add(1);
        yield_now().await;
    }
    true
}

/// Start a task that counts heartbeat increments until it is cancelled.
///
/// It only advances when the executor schedules it, which is what makes it
/// an observation of executor sharing rather than of wall-clock time.
async fn spawn_heartbeat(supervisor: &mut Supervisor, beats: &Arc<AtomicU64>) {
    let beats = Arc::clone(beats);
    supervisor
        .spawn(move |token| async move {
            while !token.is_cancelled() {
                beats.fetch_add(1, Ordering::SeqCst);
                yield_now().await;
            }
        })
        .await;
}

mod admission;
#[cfg(all(unix, feature = "process"))]
mod cleanup;
mod repeat;
#[cfg(all(unix, feature = "process"))]
mod sim;
mod tenancy;

// The cases INVARIANTS.md cites by path (INV-BOT-19, INV-BOT-151) live here,
// at `rt::supervise::tests::<name>`, so the `invariants` lane resolves them.

#[cfg(all(unix, feature = "process"))]
#[test]
fn an_unsignalable_present_group_stays_pending_rather_than_failed() {
    use super::outcome::CleanupReceipt;
    use crate::rt::runtime::block_on;

    // macOS/BSD report EPERM for a further killpg against a group whose
    // leader is an unreaped zombie. That is "the group is still there", not
    // "the OS refused the kill", and reading it as a failure would report a
    // deadline stop as a cleanup failure on those platforms.
    let signaller = EpermGroupSignaller;
    let observer = sequence(usize::MAX);
    let mut group = test_group(&signaller, &observer, false);
    assert_eq!(
        block_on(group.cleanup()),
        CleanupReceipt::CleanupPending,
        "an unsignalable but still-present group is pending, not a failed kill"
    );
    assert!(
        group.armed,
        "an unresolved group must stay armed for the drop-time fallback"
    );
    drop(group);
}

#[cfg(all(unix, feature = "process"))]
#[test]
fn an_unexpected_signal_error_is_a_failed_cleanup() {
    use super::outcome::CleanupReceipt;
    use crate::rt::runtime::block_on;

    let signaller = UnexpectedGroupSignaller;
    let observer = sequence(0);
    let mut group = test_group(&signaller, &observer, false);
    assert_eq!(
        block_on(group.cleanup()),
        CleanupReceipt::CleanupFailed,
        "an unexpected errno remains a refused termination"
    );
    drop(group);
}

/// A tenanted admission joins at most `REAP_PER_ADMISSION` finished
/// tasks, so a tenant whose bodies end at once cannot charge its joins to
/// the neighbour that admits next (#268's adversarial row), and the bounded
/// reap still keeps the retained set at the in-flight ceiling.
///
/// The flood is created, not raced: every attacker body has finished before
/// the neighbour's admission, so an unbounded reap would join all of them
/// inside that one call. The second half drives 10,000 admissions of bodies
/// that end at once, alternating tenants, and reads the retained set after
/// each: it never exceeds the ceiling, because every call that finds two or
/// more finished tasks joins two and places one.
#[cfg(feature = "script")]
#[test]
fn a_tenanted_admission_joins_a_bounded_backlog_and_keeps_the_set_at_the_ceiling()
-> Result<(), Box<dyn std::error::Error>> {
    use super::REAP_PER_ADMISSION;
    use crate::rt::runtime::block_on;
    use crate::rt::tenancy::TenancyPolicy;
    use crate::script::Tenant;
    use std::sync::atomic::AtomicUsize;

    const LIMIT: usize = 64;
    const FLOOD: usize = 32;
    const ADMISSIONS: usize = 10_000;
    // The bound is only a bound if it is smaller than the flood: an
    // unbounded reap joins all `FLOOD` below, so a constant at or past it
    // fails this test at compile time rather than passing vacuously.
    const _: () = assert!(
        REAP_PER_ADMISSION < FLOOD,
        "a per-admission reap must be smaller than the flood it bounds"
    );

    block_on(async {
        let mut supervisor = Supervisor::with_tenancy(LIMIT, TenancyPolicy::new(FLOOD, LIMIT));
        let attacker = Tenant::new("attacker")?;
        let neighbour = Tenant::new("neighbour")?;
        let ended = Arc::new(AtomicUsize::new(0));
        for _ in 0..FLOOD {
            let ended = Arc::clone(&ended);
            supervisor
                .spawn_for(&attacker, move |_token| async move {
                    ended.fetch_add(1, Ordering::SeqCst);
                })
                .await
                .map_err(|refusal| format!("a flood body was refused: {refusal}"))?;
        }
        assert!(
            yield_until(|| ended.load(Ordering::SeqCst) >= FLOOD, 10_000).await,
            "the flood bodies never ended, so the premise of this test did not hold"
        );
        // Let every ended body's task complete in the set, not just run.
        for _ in 0..FLOOD {
            yield_now().await;
        }
        let before = supervisor.stats().succeeded;
        supervisor
            .spawn_for(&neighbour, |_token| async {})
            .await
            .map_err(|refusal| format!("the neighbour was refused: {refusal}"))?;
        let joined = supervisor.stats().succeeded.saturating_sub(before);
        assert!(
            joined <= u64::try_from(REAP_PER_ADMISSION)?,
            "the neighbour's admission joined {joined} of the flood's finished tasks; \
                 at most {REAP_PER_ADMISSION} may land on its critical path"
        );

        let mut largest = 0_usize;
        for admission in 0..ADMISSIONS {
            let tenant = if admission % 2 == 0 {
                &attacker
            } else {
                &neighbour
            };
            supervisor
                .spawn_for(tenant, |_token| async {})
                .await
                .map_err(|refusal| format!("admission {admission} was refused: {refusal}"))?;
            largest = largest.max(supervisor.set.len());
            yield_now().await;
        }
        assert!(
            largest <= LIMIT,
            "the retained set reached {largest} tasks against an in-flight ceiling of {LIMIT}"
        );
        let report = supervisor.shutdown().await;
        assert_eq!(
            report.stats.succeeded,
            u64::try_from(FLOOD + 1 + ADMISSIONS)?,
            "every body is joined and counted by shutdown"
        );
        Ok(())
    })
}
