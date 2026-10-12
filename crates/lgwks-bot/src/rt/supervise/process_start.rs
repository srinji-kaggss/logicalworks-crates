/// What a [`Supervisor`] will accept next, from one snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NextAction {
    /// There is a free permit; a spawn would start.
    Admit,
    /// Every permit is taken; a spawn would block until one is released.
    WaitForCapacity,
    /// The supervisor is cancelled and admits nothing further.
    ///
    /// A spawn would be refused rather than queued, so a caller that retries here
    /// waits for something that will never happen.
    Stopped,
}

/// One live task inside a [`SupervisorSnapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LiveTask {
    /// The supervisor's stable identity for this task, in spawn order.
    pub task: TaskId,
    /// What the task is currently doing.
    pub state: TaskState,
}

impl Drop for Supervisor {
    /// Cancel the token and abort the set.
    ///
    /// This is the guarantee that a supervisor which goes out of scope takes
    /// its tasks with it. It is deliberately synchronous: `Drop` cannot await,
    /// so tasks are aborted rather than joined. Use [`Supervisor::shutdown`]
    /// when the caller needs to know they have finished before continuing.
    ///
    /// The drop also sweeps adopted zombies: on Linux the subreaper flag
    /// reparents orphaned descendants to this process, and without a reap
    /// they stay `kill(pid, 0)`-present past the death they report. The sweep
    /// reaps only this process's own children outside the live and parked
    /// sets, so it cannot take a pid another owner still tracks. Best effort
    /// and single-pass — `Drop` cannot await the deaths, so a zombie still
    /// dying here is collected by the next cleanup's confirming sweep rather
    /// than waited on.
    fn drop(&mut self) {
        self.token.cancel();
        self.set.abort_all();
        #[cfg(all(target_os = "linux", feature = "process"))]
        let _collected = sweep_adopted();
    }
}

/// Build the private engine command for `spec` and start the child.
///
/// The one constructor both [`Supervisor::spawn_process`] and
/// [`Supervisor::run_process`] call, so the group, the confinement, the
/// reaping and the configured streams cannot drift between them.
///
/// The engine's `Command` describes the child — program, arguments,
/// environment, streams and the new process group — and the standard library
/// starts it. `tokio::process::Command::spawn` is banned workspace-wide by
/// `clippy.toml`, with [`Supervisor::spawn_process`] as its replacement; this
/// is that replacement's own start, so it goes through the std command the
/// engine already built rather than through the banned call. What the engine's
/// child added beyond the std one — async pipes, a kill when the handle is
/// dropped, and reaping an orphan — [`OwnedChild`] provides.
///
/// A confined spec replaces the program with the platform's sandbox wrapper
/// (see [`ProcessSpec::command`](crate::rt::process::ProcessSpec::command)),
/// so the group below still leads the wrapper and through it the target.
#[cfg(all(unix, feature = "process"))]
fn start(spec: &ProcessSpec) -> io::Result<(OwnedChild, i32)> {
    let mut command = spec.command()?;
    spec.configure(&mut command)?;
    // The group kill reaches what the child spawned; `OwnedChild`'s drop-time
    // kill reaches the direct child on the paths where no group kill ran.
    command.process_group(0);
    // Under the start-sweep lock from the spawn until the track below: a new
    // leader is sweepable before it is tracked, so the lock keeps every
    // adoption pass from collecting a leader that exits fast.
    #[cfg(all(target_os = "linux", feature = "process"))]
    let _start_sweep = crate::journal::owner::lock(&START_SWEEP);
    let child = OwnedChild::own(command.into_std().spawn()?)?;
    // Read before the child is awaited: an unreaped child still has its id,
    // and a group leader's group id is its own pid, which `process_group(0)`
    // arranged above. A refusal here drops — and so kills — the child.
    let group = child.group()?;
    // The sweep's exclusion first: from here until the driver reaps or parks
    // the leader, its pid is somebody's answer rather than an adoptee.
    #[cfg(all(target_os = "linux", feature = "process"))]
    track_leader(group);
    // Released before the collection below: the pass takes the same lock, and
    // a mutex is not reentrant, so holding it across the call would deadlock
    // the start on its own sweep.
    #[cfg(all(target_os = "linux", feature = "process"))]
    drop(_start_sweep);
    // A start is the second moment adopted zombies are collected, beside the
    // confirmation pass: a supervisor that only ever starts children still
    // releases the adoptees its earlier trees stranded.
    #[cfg(all(target_os = "linux", feature = "process"))]
    let _collected = sweep_adopted();
    Ok((child, group))
}

/// Serialises a start's spawn-to-track against every adoption sweep.
///
/// [`track_leader`] runs after [`OwnedChild::own`], and owning takes locks and
/// builds pipe readers: under concurrency the new leader is visible in the
/// process table — and therefore sweepable — before it is tracked. A sweep
/// pass landing in that window collects a leader that exits fast, and its
/// driver's exit observation fails with `ECHILD`, which surfaces as an
/// unobservable run with no exit code. Both sides hold this lock — the start
/// from the spawn until the track, the sweep for the whole scan-and-reap pass —
/// so a pass never interleaves a start. Short holds only, never across an
/// await: the spawn and the nonblocking reaps are both synchronous.
#[cfg(all(target_os = "linux", feature = "process"))]
static START_SWEEP: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// How many tree scopes this process has named, disambiguating reused pids.
///
/// A scope name carries its leader's pid, and pids are reused: the counter
/// keeps two trees from sharing a scope name when the second leader inherits
/// the first's number.
#[cfg(all(target_os = "linux", feature = "process"))]
static SCOPE_IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The subreaper flag, enabled once per process before the first spawn.
///
/// Process-wide by kernel design: every supervisor in this process shares the
/// adoptees, which is why the cleanup intersects them with each tree's own
/// capture rather than trusting parenthood alone.
#[cfg(all(target_os = "linux", feature = "process"))]
static SUBREAPER_ONCE: std::sync::Once = std::sync::Once::new();

/// Pids of supervised leaders this process started and has not yet reaped.
///
/// The subreaper flag adopts every orphan into this process, and an adopted
/// zombie holds its pid — where `kill -0` still reports it present — until
/// somebody waits on it. The sweep below is that somebody for the adoptees,
/// and this set is what keeps it from waiting on a leader whose driver still
/// owns the exit: a pid leaves the set when its driver reaps or parks it, so
/// a collected pid is never a driver's answer.
#[cfg(all(target_os = "linux", feature = "process"))]
static LIVE_LEADERS: std::sync::Mutex<std::collections::BTreeSet<i32>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Mark `pid` as a supervised leader whose exit its driver still owns.
///
/// Called with the pid [`start`] just returned, before any task can observe
/// the child: from here until the untrack, the sweep treats the pid as
/// somebody's answer rather than an adoptee.
#[cfg(all(target_os = "linux", feature = "process"))]
fn track_leader(pid: i32) {
    if pid > 0 {
        let _present = crate::journal::owner::lock(&LIVE_LEADERS).insert(pid);
    }
}

/// Release `pid`: its driver reaped or parked it, so the sweep must not wait
/// on it — and a later adoptee under a reused number starts unattributed.
#[cfg(all(target_os = "linux", feature = "process"))]
fn untrack_leader(pid: i32) {
    let _removed = crate::journal::owner::lock(&LIVE_LEADERS).remove(&pid);
}

/// Reap every zombie child no driver owns.
///
/// Adopted orphans land in this process whether or not any guard captured
/// them — a tree whose leader exited before any capture still orphans its
/// tree here — and a zombie holds its pid, where `kill -0` keeps reporting it
/// present, until it is waited on. This collects them: `waitpid` with `NOHANG`
/// only collects the already-dead, so live children are untouched, and leaders
/// and parked orphans are excluded by number, so no driver's exit is stolen.
/// Returns how many zombies were collected.
///
/// Best effort like every reap here: an unreadable child list collects
/// nothing, and a refused wait stops the pass rather than the cleanup.
///
/// **Residual:** a host-spawned child that is a zombie at a sweep moment is
/// collected too, and its owner's later `wait` reports it reaped rather than
/// collecting it itself. Live host children are never touched, and hosts that
/// wait promptly never notice; the edge is inherent to the process-wide
/// subreaper flag, whose adoptees would otherwise strand every tree's zombies
/// here.
/// Children of this process no driver owns: not live leaders, not parked orphans.
#[cfg(all(target_os = "linux", feature = "process"))]
fn unclaimed_children() -> Vec<i32> {
    let children = match lgwks_std::process::own_children() {
        Ok(children) => children,
        Err(error) => {
            lgwks_std::trace::debug!(%error, "sweep: the child list was unreadable; adopted zombies wait for the next sweep");
            return Vec::new();
        }
    };
    if children.is_empty() {
        return Vec::new();
    }
    let live = crate::journal::owner::lock(&LIVE_LEADERS).clone();
    let parked: std::collections::BTreeSet<i32> = crate::journal::owner::lock(&ORPHANS)
        .iter()
        .filter_map(|orphan| i32::try_from(orphan.id()).ok())
        .collect();
    children
        .into_iter()
        .filter(|pid| !live.contains(pid) && !parked.contains(pid))
        .collect()
}

/// Reap the unclaimed children of this process once, best effort.
///
/// One nonblocking pass over the children no driver owns — not live leaders,
/// not parked orphans — returning how many it collected. A refusal or an
/// unreadable child list waits for the next sweep rather than failing the
/// caller; the confirming sweep below is the one that waits out the deaths.
/// The whole pass holds the start-sweep lock, so it never interleaves a
/// start's spawn-to-track: without that a pass could collect a leader that
/// exits before its driver tracks it, stealing its driver's exit.
#[cfg(all(target_os = "linux", feature = "process"))]
fn sweep_adopted() -> usize {
    let _start_sweep = crate::journal::owner::lock(&START_SWEEP);
    let zombies = unclaimed_children();
    if zombies.is_empty() {
        return 0;
    }
    match lgwks_std::process::reap_descendants(&zombies) {
        Ok(reaped) => reaped.len(),
        Err(error) => {
            lgwks_std::trace::debug!(%error, "sweep: the adoption reap was refused; adopted zombies wait for the next sweep");
            0
        }
    }
}

/// Reap unclaimed children until none remain or the bound is spent.
///
/// The confirming half of [`sweep_adopted`]: a signalled process dies on the
/// kernel's schedule, and a single nonblocking reap attempted before the death
/// lands leaves a zombie no later pass collects. This waits for the deaths,
/// bounded to [`SWEEP_CONFIRM_PASSES`] × [`ADOPTION_REAP_INTERVAL`]: a death
/// lands in milliseconds, while a live adoptee
/// — a long-sleeping escapee the cleanup's own kill is still delivering —
/// never vanishes on its own, and waiting out the adoption bound here would
/// blow the shutdown grace a draining cleanup is owed (the drain test shuts
/// 32 trees at once against 50 ms). Past the bound whatever remains is the
/// observation rounds' to report, not this sweep's to wait away. Async like
/// the rest of the cleanup path, so the wait parks on the runtime clock
/// instead of blocking its thread; only ever waits on processes that are
/// already children of this process.
#[cfg(all(target_os = "linux", feature = "process"))]
async fn sweep_adopted_confirmed() -> usize {
    let mut collected = 0usize;
    for _ in 0..SWEEP_CONFIRM_PASSES {
        if unclaimed_children().is_empty() {
            break;
        }
        collected = collected.saturating_add(sweep_adopted());
        crate::rt::time::sleep(ADOPTION_REAP_INTERVAL).await;
    }
    collected
}

/// The wait between two reap passes over signalled adoptees.
#[cfg(all(target_os = "linux", feature = "process"))]
const ADOPTION_REAP_INTERVAL: Duration = Duration::from_millis(5);

/// Reap passes one drain's [`reap_adopted`] makes before it observes instead.
///
/// With [`ADOPTION_REAP_INTERVAL`] this is two seconds, the longest wait a
/// drain makes on its own bounds, so [`PROCESS_CLEANUP_GRACE`] is set past it.
#[cfg(all(target_os = "linux", feature = "process"))]
const ADOPTION_REAP_PASSES: u32 = 400;

/// Passes [`sweep_adopted_confirmed`] makes over the unclaimed children.
#[cfg(all(target_os = "linux", feature = "process"))]
const SWEEP_CONFIRM_PASSES: u32 = 10;

/// What [`reap_adopted`] left behind.
#[cfg(all(target_os = "linux", feature = "process"))]
struct AdoptionReap {
    /// Adopted pids still children of this process when the bound ran out.
    unreaped: Vec<i32>,
    /// Reap passes made, at most [`ADOPTION_REAP_PASSES`].
    passes: u32,
}

/// Reap signalled adoptees until none is still owed, or the bound is spent.
///
/// A pid is settled two ways: a pass here reaps it, or it is no longer a child
/// of this process at all. The second is the common case under shutdown, not
/// an edge: [`sweep_adopted`] collects every unclaimed child, and each draining
/// cleanup runs one sweep, so with thirty-two drains at once one drain's sweep
/// reaps another drain's adoptee. `waitpid` then answers `ECHILD`, which
/// `reap_descendants` passes over without naming, so the pid looks exactly like
/// one still dying. Read that way it held its drain for the whole bound, 400 ×
/// 5 ms, with no drain finishing meanwhile, and shutdown aborted a drain whose
/// tree was already gone (main run 37945731952, Linux container). So a pass
/// that leaves anything asks which of those pids are still this process's
/// children — a zombie still is until it is reaped — and keeps only those. An
/// unreadable child list keeps every pid owed, which costs the bound and never
/// claims a reap that did not happen.
#[cfg(all(target_os = "linux", feature = "process"))]
async fn reap_adopted(adopted: Vec<i32>) -> AdoptionReap {
    let mut unreaped = adopted;
    let mut passes = 0_u32;
    while passes < ADOPTION_REAP_PASSES {
        passes = passes.saturating_add(1);
        match lgwks_std::process::reap_descendants(&unreaped) {
            Ok(reaped) => unreaped.retain(|pid| reaped.binary_search(pid).is_err()),
            Err(error) => {
                lgwks_std::trace::debug!(%error, "confirm: the adoption reap was refused; retrying within the death bound");
            }
        }
        if !unreaped.is_empty() {
            match lgwks_std::process::adopted_descendants(&unreaped) {
                Ok(children) => unreaped.retain(|pid| children.contains(pid)),
                Err(error) => {
                    lgwks_std::trace::debug!(%error, "confirm: the child list was unreadable; every adoptee stays owed");
                }
            }
        }
        if unreaped.is_empty() {
            break;
        }
        crate::rt::time::sleep(ADOPTION_REAP_INTERVAL).await;
    }
    AdoptionReap { unreaped, passes }
}

/// Scope the just-spawned leader `pid` to its own cgroup, best effort.
///
/// `None` — an uncreatable mount, a refused attach — is the supervisor's cue
/// to run its capture rounds without the kernel owner, not a spawn failure:
/// containment degrades to the group and the capture, and the receipt names
/// the weaker mechanism rather than failing a start that succeeded. The
/// subreaper flag is enabled beside it, so an orphan the rounds never named
/// is still adopted by this process instead of init.
#[cfg(all(target_os = "linux", feature = "process"))]
fn scope_tree(pid: i32) -> Option<CgroupScope> {
    SUBREAPER_ONCE.call_once(|| {
        if let Err(error) = lgwks_std::process::enable_child_subreaper() {
            lgwks_std::trace::debug!(%error, "start: the subreaper flag was refused; orphans re-parent to init");
        }
    });
    if pid <= 0 {
        return None;
    }
    let id = SCOPE_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let scope = match CgroupScope::create(&format!("lgwks-{pid}-{id}")) {
        Ok(scope) => scope,
        Err(error) => {
            lgwks_std::trace::debug!(%error, pid, "start: no cgroup scope; the capture rounds own this tree");
            return None;
        }
    };
    if let Err(error) = scope.add(pid) {
        lgwks_std::trace::debug!(%error, pid, "start: the leader left before its scope attach; the capture rounds own this tree");
        return None;
    }
    Some(scope)
}

/// How often a child that has been signalled is checked for its exit while it
/// is being reaped. The same cadence [`observe_pid_without_reaping`] observes
/// a running child at, so reaping adds no finer timer than observing already
/// uses.
#[cfg(all(unix, feature = "process"))]
const REAP_POLL: Duration = Duration::from_millis(5);

/// Children killed when their owner was dropped but not yet exited.
///
/// A dropped handle cannot await its child, and a killed child that nobody
/// waits on stays a zombie and keeps its pid allocated. Each one is parked here
/// and reaped by the next [`OwnedChild`] that starts or drops — the same
/// contract as the engine's own orphan queue, which this replaces. Its length
/// is bounded by the children that were SIGKILLed and have not yet been torn
/// down by the kernel, which is momentary for every process not stuck in an
/// uninterruptible wait.
#[cfg(all(unix, feature = "process"))]
static ORPHANS: std::sync::Mutex<Vec<std::process::Child>> = std::sync::Mutex::new(Vec::new());

/// Reap every parked orphan that has exited, keeping the rest parked.
#[cfg(all(unix, feature = "process"))]
fn reap_orphans() {
    crate::journal::owner::lock(&ORPHANS)
        .retain_mut(|orphan| matches!(orphan.try_wait(), Ok(None)));
}

/// A started child this module owns: the std handle, its two captured pipes as
/// async readers, and whether it has been reaped.
///
/// Dropping an unreaped one kills the direct child and parks it in [`ORPHANS`],
/// so an aborted driver never leaves a running child or an unreaped zombie
/// behind it. The process-group guard covers the child's own descendants.
#[cfg(all(unix, feature = "process"))]
struct OwnedChild {
    /// The started child; `None` only once it has been reaped or parked.
    inner: Option<std::process::Child>,
    /// Its pid, which is also its process group's id.
    pid: u32,
    /// Its stdout, when the spec captured it.
    stdout: Option<ChildStdout>,
    /// Its stderr, when the spec captured it.
    stderr: Option<ChildStderr>,
}

#[cfg(all(unix, feature = "process"))]
impl OwnedChild {
    /// Own a just-started child, converting its captured pipes to async
    /// readers on this runtime's reactor.
    ///
    /// The child is owned before the conversion, so a conversion that fails
    /// drops an `OwnedChild` — killing and parking the child — rather than
    /// leaking a running process behind an error.
    fn own(mut inner: std::process::Child) -> io::Result<Self> {
        // A start is one of the two moments [`ORPHANS`] is collected, so a
        // supervisor that only ever starts children still releases the ones
        // its earlier drivers parked.
        reap_orphans();
        let stdout = inner.stdout.take();
        let stderr = inner.stderr.take();
        let mut child = Self {
            pid: inner.id(),
            inner: Some(inner),
            stdout: None,
            stderr: None,
        };
        child.stdout = stdout.map(ChildStdout::from_std).transpose()?;
        child.stderr = stderr.map(ChildStderr::from_std).transpose()?;
        Ok(child)
    }

    /// The child's pid as the `pid_t` its process group is signalled by.
    ///
    /// # Errors
    ///
    /// A pid that does not fit `pid_t` is not one this module can signal, so
    /// it is refused rather than mapped to an id that would name another group.
    fn group(&self) -> io::Result<i32> {
        i32::try_from(self.pid).map_err(io::Error::other)
    }

    /// Collect the child's exit status, releasing its pid.
    ///
    /// Not cancellable by the run's token on purpose: by the time this runs the
    /// child has exited or its group has been signalled, and a reap abandoned
    /// half way would leave the zombie this exists to collect. A dropped reap
    /// is still safe — the drop parks the child in [`ORPHANS`].
    async fn reap(&mut self) -> io::Result<std::process::ExitStatus> {
        loop {
            let Some(inner) = self.inner.as_mut() else {
                let refusal = Err(io::Error::other("the child was already reaped"));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), pid = self.pid, "owned child: reap after reap");
                return refusal;
            };
            if let Some(status) = inner.try_wait()? {
                self.inner = None;
                reap_orphans();
                #[cfg(all(target_os = "linux", feature = "process"))]
                if let Ok(pid) = i32::try_from(self.pid) {
                    untrack_leader(pid);
                }
                return Ok(status);
            }
            crate::rt::time::sleep(REAP_POLL).await;
        }
    }
}

#[cfg(all(unix, feature = "process"))]
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let Some(mut inner) = self.inner.take() else {
            return;
        };
        // A child that already exited is a zombie that still holds its pid, so
        // the kill cannot reach another process; the reap below is what
        // releases it either way.
        if let Err(error) = inner.kill() {
            lgwks_std::trace::debug!(%error, pid = self.pid, "owned child: direct kill on drop refused");
        }
        if !matches!(inner.try_wait(), Ok(Some(_))) {
            crate::journal::owner::lock(&ORPHANS).push(inner);
        }
        reap_orphans();
        // Parked or reaped: either way no driver owns this exit any more, so
        // the sweep must not mistake the number for an adoptee either.
        #[cfg(all(target_os = "linux", feature = "process"))]
        if let Ok(pid) = i32::try_from(self.pid) {
            untrack_leader(pid);
        }
    }
}

