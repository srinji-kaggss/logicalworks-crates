/// A child's process group, killed as a unit unless it is disarmed.
///
/// A `Child` is one process. A command a bot starts is usually a shell or a
/// wrapper that spawns its own children — a pipeline, a package manager, a
/// build — and `Child::kill` reaches only the first of them, leaving the rest
/// running with nobody holding their handles. Signalling the *group* is what
/// makes "stop the process" mean the whole tree.
///
/// The guard is armed when the group is created. It may signal only while the
/// leader is unreaped; after reaping, it uses signal-zero observation only.
/// The owner holds the process permit through the bounded absence observation.
#[cfg(all(unix, feature = "process"))]
struct ProcessGroup<'ops> {
    /// The group id, or `0` when the engine could not report the child's id.
    ///
    /// Zero is *refused* by `kill_process_group` rather than read as "this
    /// process's own group", so an unreadable id can never signal the
    /// supervisor's own process group.
    group: i32,
    /// Attribution for a later cleanup receipt.
    task: TaskId,
    /// The process admission lease, transferred to `owners` if cleanup remains.
    ///
    /// A [`Lease`] rather than a bare permit, so a cleanup obligation that
    /// outlives its task returns the capacity to the tenant it was charged to,
    /// not merely to a pool nobody is waiting on.
    permit: Option<Lease>,
    /// Supervisor-owned destination for unresolved cleanup.
    owners: Arc<CleanupOwners>,
    /// Whether a kill is still owed to the group.
    armed: bool,
    /// Whether the leader has been reaped and the numeric id released.
    leader_reaped: bool,
    /// Whether the leader was observed to have exited before the cleanup began,
    /// after which a walk from it can name nothing.
    leader_exited: bool,
    /// Whether the group was observed absent: by a signal refusal, or by the
    /// post-reap signal-zero probe.
    group_absent: bool,
    /// What the cleanup read, signalled, and could not account for.
    containment: Containment,
    /// Captured pids a signal has already been delivered to.
    ///
    /// The set is what keeps "signal every captured pid" from becoming "signal
    /// every captured pid once per round": a pid is signalled the first round it
    /// is captured in, and a pid that is still running afterwards is signalled
    /// again **only** because the observation proved it alive, which is the same
    /// rule the group follows for its own id.
    signalled_pids: std::collections::BTreeSet<i32>,
    /// The tree's cgroup v2 scope, when the mount allowed one.
    ///
    /// Killed first on every cleanup path, observed empty to earn the receipt,
    /// and removed when the guard drops. `None` where no child cgroup was
    /// creatable, where the capture rounds own the tree alone.
    #[cfg(all(target_os = "linux", feature = "process"))]
    cgroup: Option<CgroupScope>,
    /// The process-group signalling boundary, injectable for regression tests.
    signaller: &'ops dyn GroupSignaller,
    /// Non-mutating group existence boundary, injectable for regression tests.
    observer: &'ops dyn GroupObserver,
    /// The per-process containment boundary, injectable for regression tests.
    capture: &'ops dyn DescendantCapture,
}

/// One process-group signalling operation.
#[cfg(all(unix, feature = "process"))]
trait GroupSignaller: Sync {
    /// Send SIGKILL to `group`.
    fn signal(&self, group: i32) -> io::Result<()>;
}

/// One non-mutating process-group existence observation.
#[cfg(all(unix, feature = "process"))]
trait GroupObserver: Sync {
    /// Return whether `group` currently names a process group.
    fn exists(&self, group: i32) -> io::Result<bool>;
}

/// One capture's finding: which mechanism read it, and which pids it named.
///
/// The crate's own shape rather than
/// [`lgwks_std::process::DescendantSet`] itself, for two reasons: the estate type
/// is `#[non_exhaustive]` with private fields, so a test in this crate could not
/// build one and the seam could not be driven against a scripted tree at all; and
/// what the drain needs is three facts, not a process-table abstraction.
#[cfg(all(unix, feature = "process"))]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Capture {
    /// How the pids were found.
    mechanism: ContainmentMechanism,
    /// The pids the reading named, sorted and deduplicated.
    pids: Vec<i32>,
    /// Whether a bound stopped the reading before the tree ended.
    truncated: bool,
}

#[cfg(all(unix, feature = "process"))]
impl Capture {
    /// The finding the audited `lgwks_std::process` reading produced.
    fn from_set(set: &lgwks_std::process::DescendantSet) -> Self {
        Self {
            mechanism: set.mechanism(),
            pids: set.pids().to_vec(),
            truncated: set.is_truncated(),
        }
    }

    /// Whether the reading named nothing.
    fn is_empty(&self) -> bool {
        self.pids.is_empty()
    }
}

/// The per-process containment boundary: name the tree, stop one of its members,
/// and observe which of them are still running.
///
/// Three operations rather than one, because they answer three different
/// questions and each has its own failure mode: a capture that read nothing is
/// not a capture that found nothing, a signal to a pid that has ended is `ESRCH`
/// rather than a failure, and an observation is the only one of the three that is
/// safe to repeat. Injectable so the drain's decision table can be exercised
/// against every combination of those failures without forking a process.
#[cfg(all(unix, feature = "process"))]
trait DescendantCapture: Sync {
    /// Every process descended from `root` right now.
    fn capture(&self, root: i32) -> io::Result<Capture>;
    /// Send SIGKILL to one captured pid.
    fn signal(&self, pid: i32) -> io::Result<()>;
    /// Which of `pids` are still running, as opposed to present-but-reaped.
    fn running(&self, pids: &[i32]) -> io::Result<Vec<i32>>;
}

/// A cleanup obligation retained after its original task ends.
#[cfg(all(unix, feature = "process"))]
struct PendingCleanup {
    /// Stable task attribution.
    task: TaskId,
    /// Process-group id, used only for non-mutating existence observation.
    group: i32,
    /// Capacity remains charged until terminal absence is observed. A
    /// [`Lease`], so a retained obligation is still charged to the tenant whose
    /// admission it holds.
    _lease: Lease,
    /// Absence was observed before its task's terminal report was joined.
    absence_observed: bool,
    /// What the retained cleanup read and signalled, for its terminal receipt.
    containment: Containment,
}

/// Bounded owners for groups whose leader task could not prove cleanup.
/// Each entry owns its process permit, so retained obligations stay charged to
/// the configured native-resource ceiling.
#[cfg(all(unix, feature = "process"))]
#[derive(Default)]
struct CleanupOwners {
    /// Owners retained under the in-flight semaphore ceiling.
    pending: Mutex<Vec<PendingCleanup>>,
    /// Signalled on every registration, so a spawn waiting at the bound learns
    /// that a permit has moved to an owner that only a reap will release.
    registered: Notify,
    /// Spawned process tasks that have not yet returned; see [`LiveProcess`].
    live: std::sync::atomic::AtomicUsize,
}

/// One spawned process task that has not yet returned.
///
/// Moved into the task's future, so it is dropped when the task returns *or* is
/// aborted, and [`Supervisor::shutdown`] can tell a process still draining its
/// cleanup from a body that ignores its token.
#[cfg(all(unix, feature = "process"))]
struct LiveProcess(Arc<CleanupOwners>);

#[cfg(all(unix, feature = "process"))]
impl LiveProcess {
    /// Count one more live process task against `owners`.
    fn enter(owners: &Arc<CleanupOwners>) -> Self {
        owners
            .live
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Self(Arc::clone(owners))
    }
}

#[cfg(all(unix, feature = "process"))]
impl Drop for LiveProcess {
    fn drop(&mut self) {
        self.0
            .live
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

#[cfg(all(unix, feature = "process"))]
impl CleanupOwners {
    /// Transfer a live task's lease and cleanup obligation to this registry.
    fn register(&self, task: TaskId, group: i32, lease: Lease, containment: Containment) {
        crate::journal::owner::lock(&self.pending).push(PendingCleanup {
            task,
            group,
            _lease: lease,
            absence_observed: false,
            containment,
        });
        self.registered.notify_one();
    }

    /// Count currently retained obligations.
    fn pending_count(&self) -> usize {
        crate::journal::owner::lock(&self.pending).len()
    }

    /// Probe each owner once; present or unobservable groups keep their lease.
    fn drive(
        &self,
        observer: &dyn GroupObserver,
        task_is_live: impl Fn(TaskId) -> bool,
    ) -> Vec<(TaskId, Containment)> {
        let mut pending = crate::journal::owner::lock(&self.pending);
        let mut retained = Vec::with_capacity(pending.len());
        let mut settled = Vec::new();
        for mut owner in pending.drain(..) {
            let absent =
                owner.absence_observed || matches!(observer.exists(owner.group), Ok(false));
            if absent {
                if task_is_live(owner.task) {
                    owner.absence_observed = true;
                    retained.push(owner);
                } else {
                    settled.push((owner.task, owner.containment));
                }
            } else {
                retained.push(owner);
            }
        }
        *pending = retained;
        settled
    }
}

/// Resolve when a spawn waiting at the bound must stop and reap: after
/// [`CLEANUP_RECHECK`] while a cleanup owner holds a permit, or the moment one is
/// registered while none was.
///
/// A registration is a permit that no task ending will release, so a waiter
/// that armed no timer must hear about it. `notify_one` stores the wakeup when
/// nobody is waiting yet, so a registration between the count and the first
/// poll of `notified` is not lost.
#[cfg(all(unix, feature = "process"))]
async fn recheck(owners: &CleanupOwners) {
    if owners.pending_count() > 0 {
        crate::rt::time::sleep(CLEANUP_RECHECK).await;
    } else {
        owners.registered.notified().await;
    }
}

#[cfg(all(unix, feature = "process"))]
impl fmt::Debug for CleanupOwners {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CleanupOwners")
            .field("pending", &self.pending_count())
            .finish()
    }
}

#[cfg(feature = "process")]
/// The production process-group syscall adapter.
#[cfg(all(unix, feature = "process"))]
struct NativeGroupSignaller;

#[cfg(all(unix, feature = "process"))]
impl GroupSignaller for NativeGroupSignaller {
    fn signal(&self, group: i32) -> io::Result<()> {
        lgwks_std::process::kill_process_group(group)
    }
}

#[cfg(all(unix, feature = "process"))]
/// The production signal-zero process-group observer.
struct NativeGroupObserver;

#[cfg(all(unix, feature = "process"))]
impl GroupObserver for NativeGroupObserver {
    fn exists(&self, group: i32) -> io::Result<bool> {
        lgwks_std::process::process_group_exists(group)
    }
}

#[cfg(feature = "process")]
/// One static production adapter, so task futures hold no borrowed runtime state.
#[cfg(all(unix, feature = "process"))]
static NATIVE_GROUP_SIGNALLER: NativeGroupSignaller = NativeGroupSignaller;

#[cfg(all(unix, feature = "process"))]
/// The static observer used by process-group cleanup.
static NATIVE_GROUP_OBSERVER: NativeGroupObserver = NativeGroupObserver;

#[cfg(all(unix, feature = "process"))]
/// The production per-process containment adapter.
struct NativeDescendantCapture;

#[cfg(all(unix, feature = "process"))]
impl DescendantCapture for NativeDescendantCapture {
    /// Read the tree through the audited `lgwks_std::process` primitives, which
    /// is where the rustix edge and the bounded walk live.
    fn capture(&self, root: i32) -> io::Result<Capture> {
        lgwks_std::process::capture_descendants(root).map(|set| Capture::from_set(&set))
    }

    fn signal(&self, pid: i32) -> io::Result<()> {
        lgwks_std::process::kill_process(pid)
    }

    fn running(&self, pids: &[i32]) -> io::Result<Vec<i32>> {
        Ok(lgwks_std::process::running_processes(pids)?
            .into_iter()
            .collect())
    }
}

#[cfg(all(unix, feature = "process"))]
/// One static production capture, so a process future holds no borrowed state.
static NATIVE_DESCENDANT_CAPTURE: NativeDescendantCapture = NativeDescendantCapture;

/// The terminal receipt a retained cleanup earns when its group is observed
/// absent.
///
/// Not always `CleanupConfirmed`: an obligation transferred to the registry still
/// carries the containment report its task ended with, and a cleanup that left a
/// survivor has not reclaimed the tree however absent the group became. Naming
/// the mechanism here is what lets a consumer read one settled receipt and know
/// both.
#[cfg(all(unix, feature = "process"))]
fn settled_receipt(containment: &Containment) -> CleanupReceipt {
    if containment.survivors().is_empty() {
        CleanupReceipt::CleanupConfirmed
    } else {
        CleanupReceipt::CleanupSurvivors {
            survivors: containment.survivors().to_vec(),
        }
    }
}

/// What happened while observing a supervised child without releasing its pid.
#[derive(Clone, Copy)]
#[cfg(all(any(unix, windows), feature = "process"))]
enum ProcessObservation {
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
async fn observe_pid_without_reaping(
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

/// Called once for each newline-terminated line of a child's stdout, as the
/// child writes it.
///
/// The shape a service's readiness gate takes: a long-lived child announces the
/// address or fact it is ready at on one line, and the caller closes its own
/// gate on that line rather than on a timer.
///
/// `Send + Sync` because a process driver is also run from
/// [`Supervisor::spawn_process`]'s spawned task, which must be `Send`; a
/// non-`Send` observer would force the unobserved driver to have a different
/// signature from the observed one. The bound is honest rather than a
/// convenience: the observer receives a line it must not retain, it is called
/// synchronously on the task driving one child, and a readiness gate it closes
/// is an `Arc`-backed fact that *is* shareable. A caller whose gate needs
/// thread-local state cannot be one, and should not be.
///
/// The bytes exclude the line terminator and one trailing carriage return, so
/// the same line is observed identically on a CRLF writer and an LF one, and
/// they borrow from the read buffer rather than being copied: the observation and
/// the retained capture are the same bytes, seen once.
#[cfg(feature = "process")]
pub type LineObserver<'a> = &'a (dyn Fn(&[u8]) + Send + Sync + 'a);

/// The terminal observation of one supervised child, before it is mapped to a
/// public report.
///
/// One driver produces this shape for both [`Supervisor::spawn_process`] and
/// [`Supervisor::run_process`], so the two cannot drift into cleaning up a
/// group differently: the mapping to a `TaskEnd` or a `ProcessRun` happens at
/// the edge.
#[cfg(all(any(unix, windows), feature = "process"))]
struct ProcessEnd {
    /// What the observation saw.
    observation: ProcessObservation,
    /// The status the engine reported, when there is one.
    status: Option<ExitStatus>,
    /// The cleanup receipt and what it was based on.
    cleanup: ProcessCleanup,
    /// Captured stdout.
    stdout: CapturedStream,
    /// Captured stderr.
    stderr: CapturedStream,
}

/// Map a driven child's terminal observation to the supervisor's internal
/// task-end, so the public `TaskOutcome` is derived from one place.
#[cfg(all(any(unix, windows), feature = "process"))]
fn task_end(end: ProcessEnd) -> TaskEnd {
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
async fn drive_process(
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
#[cfg(all(any(unix, windows), feature = "process"))]
#[derive(Debug, Clone, Copy)]
struct RunBounds {
    /// How long the child may run before it is stopped.
    deadline: Option<Duration>,
    /// The most stdout bytes retained.
    out_limit: Option<NonZeroUsize>,
    /// The most stderr bytes retained.
    err_limit: Option<NonZeroUsize>,
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
async fn drive_process_observed(
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
    let capture_out = capture(child.stdout.take(), out_limit, on_line);
    let capture_err = capture(child.stderr.take(), err_limit, None);
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
        (observation, cleanup)
    };
    let (stdout, stderr, (observation, mut receipt)) = join3(capture_out, capture_err, wait).await;

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

