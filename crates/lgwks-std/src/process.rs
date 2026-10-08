//! Process-group termination and observation for supervised subprocesses.
//!
//! Stable `std` can kill a direct child (`Child::kill`) but cannot signal a
//! whole process group, so killing a shell leaves orphaned grandchildren.
//! It also cannot observe a child's exit without reaping it, which is the
//! primitive a supervisor needs to keep a group leader's id from being
//! reused under it: a zombie leader still holds its pid, so its group id
//! cannot be reissued until the supervisor has finished signalling. These
//! are the `process` feature's primitives. Unix-only; other targets report
//! [`std::io::ErrorKind::Unsupported`] rather than a fabricated success.

#[cfg(unix)]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io;

/// Send SIGKILL to every process in group `pgid`.
///
/// `pgid` is a process-group id as reported by the OS (a positive integer;
/// typically the leader's pid). An invalid id surfaces the OS error rather than
/// succeeding silently. This kills unconditionally (SIGKILL cannot be
/// caught); callers that need graceful shutdown must signal SIGTERM
/// themselves before falling back here.
#[cfg(all(unix, feature = "process"))]
pub fn kill_process_group(pgid: i32) -> io::Result<()> {
    // Reject before touching rustix: 0 names the CALLER's group (SIGKILL
    // would hit us) and negatives panic inside Pid::from_raw in debug.
    if pgid <= 0 {
        let refusal = Err(invalid_pgid());
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "kill_process_group: returning an error to the caller");
        return refusal;
    }
    let pid = pid_from_raw(pgid)?;
    rustix::process::kill_process_group(pid, rustix::process::Signal::KILL).map_err(errno_to_io)
}

/// Send SIGKILL to every process in group `pgid`.
///
/// The `process` capability is Unix-only; other targets report
/// [`std::io::ErrorKind::Unsupported`] rather than a fabricated success.
#[cfg(all(not(unix), feature = "process"))]
pub fn kill_process_group(pgid: i32) -> io::Result<()> {
    let _ = pgid;
    Err(unsupported("process kill_process_group is Unix-only"))
}

/// Whether `pgid` currently names a process group.
///
/// This is `kill(-pgid, 0)`: signal zero checks the group without delivering
/// anything to it. `EPERM` means the group exists but this process may not
/// signal it, so it counts as present; `ESRCH` means no process is in it. A
/// supervisor confirms a cleanup with this after `kill_process_group`, so a
/// group that cannot be observed is reported as an error, never as absent.
///
/// `pgid` must be positive: 0 names the caller's own group and a negative id
/// names a process, so both are refused as [`std::io::ErrorKind::InvalidInput`].
#[cfg(all(unix, feature = "process"))]
pub fn process_group_exists(pgid: i32) -> io::Result<bool> {
    if pgid <= 0 {
        let refusal = Err(invalid_pgid());
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "process_group_exists: returning an error to the caller");
        return refusal;
    }
    let pid = pid_from_raw(pgid)?;
    match rustix::process::test_kill_process_group(pid) {
        Ok(()) | Err(rustix::io::Errno::PERM) => Ok(true),
        Err(rustix::io::Errno::SRCH) => Ok(false),
        Err(errno) => Err(errno_to_io(errno)),
    }
}

/// Whether `pgid` currently names a process group.
///
/// The `process` capability is Unix-only; other targets report
/// [`std::io::ErrorKind::Unsupported`] rather than a fabricated answer.
#[cfg(all(not(unix), feature = "process"))]
pub fn process_group_exists(pgid: i32) -> io::Result<bool> {
    let _ = pgid;
    Err(unsupported("process process_group_exists is Unix-only"))
}

/// Whether child `pid` has exited, leaving it un-reaped.
///
/// This is `waitid(P_PID, pid, WEXITED | WNOHANG | WNOWAIT)`: it reports the
/// exit and *leaves the child in the waitable state*, so a following
/// `Child::wait` still returns the real exit status. That gap — exited but
/// not reaped — is what a supervisor needs: an unreaped (zombie) leader
/// still occupies its pid, so the group id cannot be reused by the OS while
/// the supervisor still owes signals to it. Reaping is what releases the id,
/// and this call never reaps.
///
/// `Ok(false)` means still running; `Ok(true)` means exited and still
/// waitable.
#[cfg(all(unix, feature = "process"))]
pub fn child_has_exited_without_reaping(pid: i32) -> io::Result<bool> {
    if pid <= 0 {
        let refusal = Err(invalid_pid());
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "child_has_exited_without_reaping: returning an error to the caller");
        return refusal;
    }
    let Some(target) = rustix::process::Pid::from_raw(pid) else {
        let refusal = Err(invalid_pid());
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "child_has_exited_without_reaping: returning an error to the caller");
        return refusal;
    };
    let observed = rustix::process::waitid(
        rustix::process::WaitId::Pid(target),
        rustix::process::WaitIdOptions::EXITED
            | rustix::process::WaitIdOptions::NOHANG
            | rustix::process::WaitIdOptions::NOWAIT,
    )
    .map_err(errno_to_io)?;
    Ok(observed.is_some())
}

/// Whether child `pid` has exited, leaving it un-reaped.
///
/// The `process` capability is Unix-only; other targets report
/// [`std::io::ErrorKind::Unsupported`] rather than a fabricated success.
#[cfg(all(not(unix), feature = "process"))]
pub fn child_has_exited_without_reaping(pid: i32) -> io::Result<bool> {
    let _ = pid;
    Err(unsupported(
        "process child_has_exited_without_reaping is Unix-only",
    ))
}

#[cfg(all(unix, feature = "process"))]
/// Convert a rustix errno into the std error surface.
fn errno_to_io(errno: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(errno.raw_os_error())
}

#[cfg(all(unix, feature = "process"))]
/// Convert a positive raw process-group id into rustix's checked pid type.
fn pid_from_raw(pgid: i32) -> io::Result<rustix::process::Pid> {
    rustix::process::Pid::from_raw(pgid).ok_or_else(invalid_pgid)
}

#[cfg(all(unix, feature = "process"))]
/// Construct the invalid-process-group-id error.
fn invalid_pgid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "process group id must be a positive pid",
    )
}

#[cfg(all(unix, feature = "process"))]
/// Construct the invalid-process-id error.
fn invalid_pid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "process id must be a positive pid",
    )
}

#[cfg(all(not(unix), feature = "process"))]
/// Construct the unsupported-operation error for non-Unix targets.
fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

// ── Descendant capture ──────────────────────────────────────────────────────
//
// A process group is the floor, not the tree. A descendant that calls `setsid`
// has left the group by construction, so no group signal reaches it, and the
// only way to stop one is to name its pid. These primitives are that naming:
// read the process table to find every process descended from a root, signal
// each of them by pid, and observe which of them are still present.
//
// Both halves are deliberately narrow. The capture is bounded by
// [`MAX_CAPTURED_DESCENDANTS`] and reports itself truncated past it, because a
// process table is a property of the host rather than of the caller's tree; and
// it is a **snapshot taken while the root is alive**, because that is the only
// moment a descendant still names it as a parent. A descendant orphaned before
// the capture has been adopted by the platform's init and is no longer reachable
// through the root — the caller is told which mechanism ran, and a caller that
// needs that case closed needs a kernel-level owner (a cgroup, or a Windows job
// object), neither of which this crate has.

/// The most process ids one descendant capture retains.
///
/// A bound on the process table rather than on the caller's tree: without one, a
/// host with many processes could make a supervisor's memory grow with the host
/// rather than with the tree it started. Past the bound the capture keeps the
/// first [`MAX_CAPTURED_DESCENDANTS`] ids and reports
/// [`DescendantSet::is_truncated`], so a caller learns it is holding a prefix
/// instead of trusting it as the whole tree.
pub const MAX_CAPTURED_DESCENDANTS: usize = 4096;

/// How deep one descendant capture walks before it stops descending.
///
/// A descendant tree deeper than this is a program that forks without bound,
/// which the capture reports as truncated rather than following for ever.
#[cfg(unix)]
const MAX_CAPTURED_DEPTH: usize = 64;

/// How many thread directories of one process the Linux walk reads.
///
/// `/proc/<pid>/task/<tid>/children` attributes a child to the thread that
/// forked it, so a threaded process needs every one of its thread directories
/// read before its child list is complete. The bound keeps a process with
/// thousands of threads from turning one capture into thousands of reads; a tree
/// past it is reported truncated.
#[cfg(all(target_os = "linux", feature = "process"))]
const MAX_CAPTURED_THREADS: usize = 64;

/// How a descendant capture read the process table.
///
/// The receipt names this, because "every process the supervisor started is
/// gone" is a claim about a mechanism and not only about an outcome: the same
/// green result means different things depending on whether the table was read
/// per-process or in one snapshot, or not read at all.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum ContainmentMechanism {
    /// No process table was read, so nothing outside the group was captured.
    ///
    /// The honest report when this target exposes no table, or when the read
    /// failed. A caller that needs the stronger claim has to say so rather than
    /// read an empty capture as "there were no descendants".
    #[default]
    ProcessGroupOnly,
    /// Linux: each process's own child list, from `/proc`.
    ///
    /// One read per process in the tree rather than one per process on the
    /// host, which is what keeps a cleanup cheap under concurrency. It needs a
    /// kernel built with `CONFIG_PROC_CHILDREN`; a kernel without it falls back
    /// to [`Self::ProcessTableSnapshot`], and a host with neither reports
    /// [`Self::ProcessGroupOnly`].
    ProcChildrenTree,
    /// A `pid`/`ppid` snapshot of the whole process table.
    ///
    /// The portable reading, used on every Unix without a per-process child list
    /// and as the fallback when that list cannot be read. It costs one `ps`
    /// invocation and one row per process on the host, so it is the slower of
    /// the two and the one a high-concurrency cleanup pays most for.
    ProcessTableSnapshot,
    /// Linux: the tree's cgroup v2 scope, stopped by `cgroup.kill`.
    ///
    /// No process table was read for the members this stopped: the kernel owns
    /// the membership, so a `setsid` escapee attached to the scope dies with it
    /// and no fork race applies to the members it holds. The completeness claim
    /// rests on the scope reading empty afterwards ([`CgroupScope::members`]),
    /// not on a table snapshot.
    CgroupKill,
    /// Linux: descendants the subreaper flag re-parented to the supervisor.
    ///
    /// An orphan adopted by this process is found as its child rather than
    /// through a walk from a leader that may already have exited, and a pid
    /// that is both captured and adopted is provably still the same process.
    /// The completeness claim rests on that intersection
    /// ([`adopted_descendants`]), not on the group alone.
    SubreaperAdoption,
}

impl ContainmentMechanism {
    /// The stronger of two mechanisms, for a capture that merged both.
    ///
    /// A kernel-level owner beats a table reading (it stops what no snapshot
    /// can name), a per-process walk beats a whole-table snapshot (it reads
    /// less and races less), and either beats no capture at all. Merging never
    /// reports the weaker mechanism of the two, because a caller reading the
    /// merged value would otherwise be told less than the capture actually
    /// established.
    #[must_use]
    pub const fn strongest(self, other: Self) -> Self {
        match (self, other) {
            (Self::CgroupKill, _) | (_, Self::CgroupKill) => Self::CgroupKill,
            (Self::SubreaperAdoption, _) | (_, Self::SubreaperAdoption) => Self::SubreaperAdoption,
            (Self::ProcChildrenTree, _) | (_, Self::ProcChildrenTree) => Self::ProcChildrenTree,
            (Self::ProcessTableSnapshot, _) | (_, Self::ProcessTableSnapshot) => {
                Self::ProcessTableSnapshot
            }
            (Self::ProcessGroupOnly, Self::ProcessGroupOnly) => Self::ProcessGroupOnly,
        }
    }

    /// `false` only for [`Self::ProcessGroupOnly`]: an empty capture under it
    /// says nothing about what descends from the root, while an empty reading
    /// under any other mechanism is evidence — a table snapshot, a per-process
    /// walk, an emptied cgroup scope, or an adoption intersection — rather
    /// than the absence of one.
    #[must_use]
    pub const fn read_a_table(self) -> bool {
        !matches!(self, Self::ProcessGroupOnly)
    }
}

/// Every process found descended from one root, and whether the walk was whole.
///
/// Sorted and deduplicated, so two captures of the same tree compare equal and a
/// merge is a set union. The root itself is **not** in the set: it leads the
/// group, and a caller that wants it stopped signals the group.
///
/// `is_truncated` is the difference between "this is the tree" and "this is the
/// prefix of the tree the capture had room for". A caller that needs the whole
/// tree must read it; a caller that must not signal a stranger can only trust
/// the set it holds, so the flag is reported rather than absorbed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct DescendantSet {
    /// How the ids were found.
    mechanism: ContainmentMechanism,
    /// The captured ids, sorted and deduplicated.
    pids: Vec<i32>,
    /// Whether a bound stopped the walk before the tree ended.
    truncated: bool,
}

impl DescendantSet {
    /// How the ids were found.
    #[must_use]
    pub const fn mechanism(&self) -> ContainmentMechanism {
        self.mechanism
    }

    /// The captured ids, sorted and deduplicated, excluding the root.
    #[must_use]
    pub fn pids(&self) -> &[i32] {
        &self.pids
    }

    /// How many ids the capture holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pids.len()
    }

    /// Whether the capture holds no ids.
    ///
    /// **Not** a claim that the root had no descendants: a capture that could
    /// not read the table is empty too, and
    /// [`Self::mechanism`] is what tells the two apart.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pids.is_empty()
    }

    /// Whether `pid` is one of the captured ids.
    #[must_use]
    pub fn contains(&self, pid: i32) -> bool {
        self.pids.binary_search(&pid).is_ok()
    }

    /// Whether a bound stopped the walk before the tree ended.
    ///
    /// `true` means the set is a **prefix** of the tree, so a caller that must
    /// account for every descendant has to treat the difference as unaccounted
    /// rather than as absent.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Fold another capture of the same root into this one.
    ///
    /// The union is what makes the capture cumulative across a cleanup's
    /// rounds: a fork that completed between two captures is added by the
    /// later one, and the merged mechanism is the strongest of the two so the
    /// receipt never understates what was read. Ids past
    /// [`MAX_CAPTURED_DESCENDANTS`] are refused by the bound and the merged set
    /// reports [`Self::is_truncated`].
    pub fn absorb(&mut self, other: &Self) {
        self.mechanism = self.mechanism.strongest(other.mechanism);
        self.truncated = self.truncated || other.truncated;
        for pid in &other.pids {
            match self.pids.binary_search(pid) {
                Ok(_) => {}
                Err(at) if self.pids.len() < MAX_CAPTURED_DESCENDANTS => {
                    self.pids.insert(at, *pid);
                }
                Err(_) => self.truncated = true,
            }
        }
        // Re-sorted rather than assumed: the union is only sorted if every half
        // was, and an invariant the caller cannot establish is not an invariant.
        self.pids.sort_unstable();
        self.pids.dedup();
    }
}

/// Every process descended from `root` at the moment of the call.
///
/// The root must be positive: `0` names the caller's own position in a pid walk
/// and a negative id names no process, so both are refused as
/// [`std::io::ErrorKind::InvalidInput`] before anything is read.
///
/// # What the result is and is not
///
/// It is a **snapshot of the parent relation** taken while `root` was alive, and
/// it says nothing about processes that were orphaned before the call: a
/// descendant whose own parent has already exited has been adopted by the
/// platform's init, so it no longer names `root` and cannot be reached from it.
/// On Linux `PR_SET_CHILD_SUBREAPER` would re-parent such an orphan to *this*
/// process instead, which makes it findable — and unattributable, because a
/// process that a supervisor never observed under its own root cannot be proved
/// to have come from that root rather than from another supervisor in the same
/// process. This function therefore reads the tree and does not adopt orphans;
/// the mechanism is reported so a caller can say which claim it is making.
///
/// # Errors
///
/// [`std::io::ErrorKind::InvalidInput`] for a non-positive `root`, and the
/// reading's own error when no process table could be read at all — a caller that
/// receives an error has captured nothing and must fall back to signalling the
/// group alone rather than read the absence as an empty tree.
#[cfg(all(unix, feature = "process"))]
pub fn capture_descendants(root: i32) -> io::Result<DescendantSet> {
    if root <= 0 {
        let refusal = Err(invalid_pid());
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "capture_descendants: returning an error to the caller");
        return refusal;
    }
    #[cfg(target_os = "linux")]
    if let Some(set) = capture_proc_children(root) {
        return Ok(set);
    }
    capture_table_snapshot(root)
}

/// Every process descended from `root` at the moment of the call.
///
/// The `process` capability is Unix-only; other targets report
/// [`std::io::ErrorKind::Unsupported`] rather than a fabricated empty tree,
/// because an empty set and a tree nobody looked for are different facts.
#[cfg(all(not(unix), feature = "process"))]
pub fn capture_descendants(root: i32) -> io::Result<DescendantSet> {
    let _ = root;
    Err(unsupported("process capture_descendants is Unix-only"))
}

/// Send SIGKILL to the single process `pid`.
///
/// The per-process counterpart of [`kill_process_group`], for a descendant that
/// left the group: a group signal cannot reach it and a pid can. It carries the
/// same obligation as every signal here — signal only a pid this supervisor owns
/// — and the same hazard, which is why a caller reads [`process_exists`] or a
/// fresh capture rather than reusing a pid it observed earlier.
///
/// `pid` must be positive; a non-positive id is refused before any signal leaves
/// the process, because `kill(0, …)` would signal the caller's own group.
#[cfg(all(unix, feature = "process"))]
pub fn kill_process(pid: i32) -> io::Result<()> {
    if pid <= 0 {
        let refusal = Err(invalid_pid());
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "kill_process: returning an error to the caller");
        return refusal;
    }
    let target = pid_from_raw(pid)?;
    rustix::process::kill_process(target, rustix::process::Signal::KILL).map_err(errno_to_io)
}

/// Send SIGKILL to the single process `pid`.
///
/// The `process` capability is Unix-only; other targets report
/// [`std::io::ErrorKind::Unsupported`].
#[cfg(all(not(unix), feature = "process"))]
pub fn kill_process(pid: i32) -> io::Result<()> {
    let _ = pid;
    Err(unsupported("process kill_process is Unix-only"))
}

/// Whether `pid` currently names a process.
///
/// Signal zero against a single process, so it delivers nothing and can be used
/// on a process this supervisor does not own without disturbing it — which is
/// what makes it the observation half of a descendant sweep. `EPERM` means the
/// process exists and this process may not signal it, so it counts as present;
/// `ESRCH` means no process holds the id.
///
/// The answer is about **this number at this instant**, not about a process
/// observed earlier: an id the OS has reissued answers for whoever holds it now.
/// A caller that needs "the process I captured" rather than "this number" must
/// keep the id pinned until it has finished signalling — see
/// [`child_has_exited_without_reaping`] for how the supervisor pins its leader.
///
/// `pid` must be positive; a non-positive id is refused as
/// [`std::io::ErrorKind::InvalidInput`].
#[cfg(all(unix, feature = "process"))]
pub fn process_exists(pid: i32) -> io::Result<bool> {
    if pid <= 0 {
        let refusal = Err(invalid_pid());
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "process_exists: returning an error to the caller");
        return refusal;
    }
    let target = pid_from_raw(pid)?;
    match rustix::process::test_kill_process(target) {
        Ok(()) | Err(rustix::io::Errno::PERM) => Ok(true),
        Err(rustix::io::Errno::SRCH) => Ok(false),
        Err(errno) => Err(errno_to_io(errno)),
    }
}

/// Whether `pid` currently names a process.
///
/// The `process` capability is Unix-only; other targets report
/// [`std::io::ErrorKind::Unsupported`].
#[cfg(all(not(unix), feature = "process"))]
pub fn process_exists(pid: i32) -> io::Result<bool> {
    let _ = pid;
    Err(unsupported("process process_exists is Unix-only"))
}

/// Every pid in `pids` that is still running, read in one pass of the table.
///
/// The batch form of [`process_exists`], and the difference between *present* and
/// *running* is the whole reason it exists. A process that has exited and is
/// waiting to be reaped still holds its id — `kill -0` on it succeeds, and it is
/// still a member of its group — so a cleanup that reported such a pid as a
/// survivor would report every shell that forked one child as a leak, and a test
/// that waited for its absence would wait for a reaper this crate does not own.
/// A zombie has run to its end: it occupies an id and does no work.
///
/// On Linux two more states are not running, because neither returns to user
/// code: a process part way through its exit (`PF_EXITING`), and one with a
/// `SIGKILL` delivered that the scheduler has not yet run it to act on. On a
/// loaded host a killed process can sit in the second state for longer than a
/// cleanup's rounds take, and reporting it as a survivor would name a pid that
/// ignored a signal nobody can ignore.
///
/// The conservative direction is the conservative one: a pid the table says
/// exists and whose state cannot be read is reported **running**, because a
/// receipt that claims less than it established is the honest one and a receipt
/// that claims more is the dangerous one.
///
/// `pids` may be empty, in which case no table is read and the answer is empty:
/// asking about nothing must not cost a process spawn.
#[cfg(all(unix, feature = "process"))]
pub fn running_processes(pids: &[i32]) -> io::Result<BTreeSet<i32>> {
    if pids.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut running = BTreeSet::new();
    #[cfg(target_os = "linux")]
    for pid in pids {
        match read_proc_stat(*pid) {
            Some(stat) => {
                if proc_stat_says_running(&stat) && !kill_is_pending(*pid) {
                    running.insert(*pid);
                }
            }
            // The state file was unreadable: claim nothing and report running,
            // which is the answer that understates what is still there.
            None => {
                if process_exists(*pid)? {
                    running.insert(*pid);
                }
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let snapshot = read_table_states()?;
        for pid in pids {
            match snapshot.get(pid) {
                Some(state) => {
                    if !state.starts_with('Z') {
                        running.insert(*pid);
                    }
                }
                None => {
                    if process_exists(*pid)? {
                        running.insert(*pid);
                    }
                }
            }
        }
    }
    Ok(running)
}

/// Every pid in `pids` that is still running, read in one pass of the table.
///
/// The `process` capability is Unix-only; other targets report
/// [`std::io::ErrorKind::Unsupported`].
#[cfg(all(not(unix), feature = "process"))]
pub fn running_processes(pids: &[i32]) -> io::Result<BTreeSet<i32>> {
    let _ = pids;
    Err(unsupported("process running_processes is Unix-only"))
}

/// `procfs(5)`'s state letter for a process that has exited and awaits its reap.
#[cfg(all(target_os = "linux", feature = "process"))]
const PROC_ZOMBIE: char = 'Z';

/// `PF_EXITING` in the task flags `/proc/<pid>/stat` records (field 9).
///
/// The kernel sets it as `do_exit` begins, before the process releases its
/// memory and files and long before it becomes a zombie. A process carrying it
/// never returns to user code: it has been stopped and is finishing its exit.
#[cfg(all(target_os = "linux", feature = "process"))]
const PF_EXITING: u64 = 0x0000_0004;

/// The task flags' position among the `stat` fields after the command name:
/// field 9, counted from the state letter (field 3).
#[cfg(all(target_os = "linux", feature = "process"))]
const FLAGS_FIELD_AFTER_NAME: usize = 6;

/// The `stat` line of `pid`, or `None` when it is unreadable.
#[cfg(all(target_os = "linux", feature = "process"))]
fn read_proc_stat(pid: i32) -> Option<String> {
    let path = std::path::Path::new("/proc")
        .join(pid.to_string())
        .join("stat");
    std::fs::read_to_string(path).ok()
}

/// Whether a `/proc/<pid>/stat` line describes a process still running user
/// code: not a zombie, and not part way through its exit.
///
/// A line that cannot be parsed reports running, the answer that understates
/// what a cleanup achieved.
#[cfg(all(target_os = "linux", feature = "process"))]
fn proc_stat_says_running(stat: &str) -> bool {
    // `pid (comm) state …`: `comm` may itself contain spaces and parentheses, so
    // the fields start after the **last** `)`.
    let Some(fields) = stat
        .rfind(')')
        .and_then(|end| stat.get(end.saturating_add(1)..))
    else {
        return true;
    };
    let mut fields = fields.split_ascii_whitespace();
    let Some(state) = fields.next().and_then(|field| field.chars().next()) else {
        return true;
    };
    if state == PROC_ZOMBIE {
        return false;
    }
    let flags = fields
        .nth(FLAGS_FIELD_AFTER_NAME.saturating_sub(1))
        .and_then(|field| field.parse::<u64>().ok());
    !matches!(flags, Some(flags) if flags & PF_EXITING != 0)
}

/// Whether `pid` has a `SIGKILL` delivered that it has not yet acted on.
///
/// A killed process stays in state `R` or `S` until the scheduler next runs it,
/// and on a loaded host that is long enough for a cleanup to observe it
/// "running" and report a survivor that ignored every signal — which a
/// `SIGKILL` cannot be. The kernel records the pending kill in the thread's and
/// the process's pending masks (`SigPnd`, `ShdPnd` in `/proc/<pid>/status`); a
/// process with it set never returns to user code. An unreadable status claims
/// nothing.
#[cfg(all(target_os = "linux", feature = "process"))]
fn kill_is_pending(pid: i32) -> bool {
    let path = std::path::Path::new("/proc")
        .join(pid.to_string())
        .join("status");
    std::fs::read_to_string(path).is_ok_and(|status| status_has_kill_pending(&status))
}

/// Whether a `/proc/<pid>/status` text carries `SIGKILL` in a pending mask.
#[cfg(all(target_os = "linux", feature = "process"))]
fn status_has_kill_pending(status: &str) -> bool {
    // A signal's bit in the masks is its number less one.
    let kill_bit = rustix::process::Signal::KILL
        .as_raw()
        .checked_sub(1)
        .and_then(|bit| u32::try_from(bit).ok())
        .and_then(|bit| 1u64.checked_shl(bit));
    let Some(kill_bit) = kill_bit else {
        return false;
    };
    status
        .lines()
        .filter_map(|line| {
            line.strip_prefix("SigPnd:")
                .or_else(|| line.strip_prefix("ShdPnd:"))
        })
        .filter_map(|mask| u64::from_str_radix(mask.trim(), 16).ok())
        .any(|mask| mask & kill_bit != 0)
}

/// The state `ps` records for every process, in one table read.
///
/// The portable reading, and the one used wherever there is no per-process state
/// file: on Linux `/proc/<pid>/stat` answers for a single pid without a process
/// spawn, and a spawn per survivor would be the wrong price for the answer.
#[cfg(all(not(target_os = "linux"), unix, feature = "process"))]
fn read_table_states() -> io::Result<BTreeMap<i32, String>> {
    let mut states = BTreeMap::new();
    for row in read_ps_table(&["pid=", "stat="], "read_table_states")? {
        let mut columns = row.split_ascii_whitespace();
        let (Some(pid), Some(stat)) = (columns.next(), columns.next()) else {
            continue;
        };
        let Ok(pid) = pid.parse::<i32>() else {
            continue;
        };
        states.insert(pid, stat.to_owned());
    }
    Ok(states)
}

/// The rows `ps -A -o <columns…>` printed, one [`String`] per process.
///
/// The one place this crate shells out to read a process table, so the shape of
/// that read is one decision rather than one per column set: the columns are
/// data, `caller` names the reading in the refusal so an operator reading the log
/// knows which of the two reads failed, and a non-zero exit is a refusal rather
/// than an empty table — an empty reading here would say "this host has no
/// processes" when the truth is "the reading failed".
#[cfg(all(unix, feature = "process"))]
fn read_ps_table(columns: &[&str], caller: &'static str) -> io::Result<Vec<String>> {
    let mut arguments = vec!["-A"];
    for column in columns {
        arguments.push("-o");
        arguments.push(column);
    }
    let output = run_ps(&arguments, caller)?;
    if !output.status.success() {
        let refusal = Err(io::Error::other(format!(
            "lgwks_std::process ({caller}): the process table could not be read with \
             `ps`, so no process outside the supervised group could be named",
        )));
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "read_ps_table: returning an error to the caller");
        return refusal;
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

/// Run `ps` with `arguments`, in UTC under the C locale, and return what it printed.
///
/// The one place this crate starts `ps`. The fixed environment makes a date
/// column read the same in every caller's time zone and language, which a start
/// token compared across two processes depends on. `caller` names the reading in
/// the refusal, and the error's kind is kept so a missing `ps` stays
/// distinguishable from a refused one.
#[cfg(all(unix, feature = "process"))]
fn run_ps(arguments: &[&str], caller: &'static str) -> io::Result<std::process::Output> {
    let spawned = std::process::Command::new("ps")
        .args(arguments)
        .env("TZ", "UTC0")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match spawned {
        Ok(output) => Ok(output),
        Err(error) => {
            let refusal = Err(io::Error::new(
                error.kind(),
                format!("lgwks_std::process ({caller}): `ps` could not be run: {error}"),
            ));
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "run_ps: returning an error to the caller");
            refusal
        }
    }
}

/// Capture the tree by reading each process's own child list (Linux).
///
/// Returns `None` — rather than an error — when the kernel exposes no such list,
/// because "this kernel has no per-process child list" is a fact about the
/// platform and not a failure to read one: the caller falls back to the whole
/// table, which every Linux userland has.
#[cfg(all(target_os = "linux", feature = "process"))]
fn capture_proc_children(root: i32) -> Option<DescendantSet> {
    // The probe is the root's own list: if this process has none, a kernel
    // without `CONFIG_PROC_CHILDREN` has none for anybody.
    read_proc_children(root).ok()?;
    Some(walk_descendants(
        root,
        ContainmentMechanism::ProcChildrenTree,
        |pid| match read_proc_children(pid) {
            Ok(children) => Some(children),
            // A process that exited between being named and being read has no
            // list any more, and its children now belong to init: nothing under
            // it is reachable through this root, which is the same answer an
            // empty list gives.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(Vec::new()),
            Err(error) => {
                #[cfg(feature = "trace")]
                crate::trace::debug!(
                    pid,
                    error = %error,
                    "descendant capture: a child list could not be read"
                );
                #[cfg(not(feature = "trace"))]
                let _unreported = error;
                None
            }
        },
    ))
}

/// The child pids `/proc/<pid>/task/<tid>/children` names, across its threads.
#[cfg(all(target_os = "linux", feature = "process"))]
fn read_proc_children(pid: i32) -> std::io::Result<Vec<i32>> {
    let task_dir = std::path::Path::new("/proc")
        .join(pid.to_string())
        .join("task");
    let mut children = Vec::new();
    let mut threads = 0_usize;
    for entry in std::fs::read_dir(task_dir)? {
        let entry = entry?;
        threads = threads.saturating_add(1);
        if threads > MAX_CAPTURED_THREADS {
            // A process with more threads than the bound is a partial read, and
            // the caller's truncation flag is where that becomes visible.
            break;
        }
        let Ok(text) = std::fs::read_to_string(entry.path().join("children")) else {
            continue;
        };
        children.extend(
            text.split_ascii_whitespace()
                .filter_map(|pid| pid.parse::<i32>().ok()),
        );
    }
    Ok(children)
}

/// Walk the tree breadth-first from `root` through `children_of`, bounded in
/// depth, in breadth and in retained ids.
///
/// `children_of` answers `Some(children)` for a list it read (empty when the
/// parent has none) and `None` for one it could not read; a `None` marks the set
/// as a prefix, since what is below that parent is unknown.
///
/// One walk for both readings, because the bounds are the contract and a bound
/// duplicated across two copies is two bounds that drift: the depth limit is
/// what stops a fork-without-bound program, the retained-id limit is what stops
/// a host with many processes from making a capture grow, and both report
/// themselves through [`DescendantSet::is_truncated`] rather than by returning
/// short.
#[cfg(all(unix, feature = "process"))]
fn walk_descendants(
    root: i32,
    mechanism: ContainmentMechanism,
    mut children_of: impl FnMut(i32) -> Option<Vec<i32>>,
) -> DescendantSet {
    let mut set = DescendantSet {
        mechanism,
        pids: Vec::new(),
        truncated: false,
    };
    let mut frontier = vec![root];
    let mut visited: BTreeSet<i32> = BTreeSet::from([root]);
    for _ in 0..MAX_CAPTURED_DEPTH {
        let mut next = Vec::new();
        for parent in frontier {
            let Some(listed) = children_of(parent) else {
                // A parent whose list could not be read may have children this
                // walk will never see, so the set is a prefix of the tree.
                set.truncated = true;
                continue;
            };
            for child in listed {
                if child <= 0 || !visited.insert(child) {
                    continue;
                }
                if set.pids.len() >= MAX_CAPTURED_DESCENDANTS {
                    set.truncated = true;
                    return finish(set);
                }
                set.pids.push(child);
                next.push(child);
            }
        }
        if next.is_empty() {
            return finish(set);
        }
        frontier = next;
    }
    // The depth bound ran out with processes still below it: a prefix, not a tree.
    set.truncated = true;
    finish(set)
}

/// Sort and deduplicate a captured id list.
#[cfg(all(unix, feature = "process"))]
fn finish(mut set: DescendantSet) -> DescendantSet {
    set.pids.sort_unstable();
    set.pids.dedup();
    set
}

/// Capture the tree from one `pid`/`ppid` snapshot of the whole process table.
///
/// The portable reading, and the one every Unix without a per-process child list
/// uses. It costs one `ps` invocation and one row per process on the host, and it
/// races more than the per-process walk: a process that forks between two rows
/// this snapshot read is a row the snapshot cannot contain. The walk is therefore
/// tried first wherever one exists, and this is the fallback and the default
/// everywhere else.
#[cfg(all(unix, feature = "process"))]
fn capture_table_snapshot(root: i32) -> io::Result<DescendantSet> {
    let rows = read_ps_table(&["pid=", "ppid="], "capture_table_snapshot")?;
    let mut children_of: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    for row in &rows {
        let mut columns = row.split_ascii_whitespace();
        let (Some(pid), Some(parent)) = (columns.next(), columns.next()) else {
            continue;
        };
        let (Ok(pid), Ok(parent)) = (pid.parse::<i32>(), parent.parse::<i32>()) else {
            continue;
        };
        if pid <= 0 {
            continue;
        }
        children_of.entry(parent).or_default().push(pid);
    }
    Ok(walk_descendants(
        root,
        ContainmentMechanism::ProcessTableSnapshot,
        // The snapshot holds every row on the host, so a parent with no row
        // naming it has no children.
        |pid| Some(children_of.remove(&pid).into_iter().flatten().collect()),
    ))
}

// ── Process identity and orphaned-group reaping ─────────────────────────────
//
// A supervisor that is itself killed with SIGKILL runs no destructor, so the
// process groups it started outlive it with nobody left who owns them. A
// successor can only reap them if the dead supervisor recorded *which* processes
// they were, and a pid alone does not say: after the leader exits the OS may hand
// its number to an unrelated process, and a successor that signals the number
// signals the stranger. So the record is the pid **and** the instant the OS says
// that pid's process started, and a successor signals only while both still
// match. A number reissued to another process has a different start instant, so
// it reads as reused and nothing is sent.

/// The longest start token a [`ProcessIdentity`] carries, in bytes.
///
/// Both readings this module produces fit in well under half of it (a Linux boot
/// id and a tick count, or `ps`'s ten-word start date), so the bound only refuses
/// a stored record that was damaged or written by something else, and keeps such
/// a record from making a successor allocate without limit.
pub const MAX_START_TOKEN_BYTES: usize = 128;

/// The prefix of a start instant read from `/proc` (Linux).
const PROC_SCHEME: &str = "proc:";

/// The prefix of a start instant read from `ps -o lstart`.
const PS_SCHEME: &str = "ps:";

/// The separator between a pid and its start token in the rendered form.
const IDENTITY_SEPARATOR: char = '/';

/// Which reading of the process table produced a start token.
///
/// A token is only comparable with another token from the same reading, so the
/// reading travels with it and a successor re-reads the start the same way the
/// record was made, rather than comparing a `/proc` tick count with a `ps` date
/// and calling the difference a reuse.
#[cfg(all(unix, feature = "process"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
enum StartScheme {
    /// Linux: the boot id and the start tick from `/proc/<pid>/stat`.
    Proc,
    /// `ps -o lstart`, read in UTC under the C locale.
    Ps,
}

/// A process, named by its pid and the instant the OS records it started.
///
/// The pair is what a successor needs to tell the process it was given from a
/// stranger that holds the same number later. It renders as `<pid>/<start>` and
/// parses back from that form, so it can be stored in a database row or a file
/// and read by a different process — including one started after the process
/// that recorded it was killed.
///
/// The start token is **opaque and host-local**: it compares equal only with a
/// token read on the same host by the same reading, and it orders nothing. On
/// Linux it is the boot id and the kernel's start tick, so it is exact and does
/// not survive a reboot as a false match. Elsewhere it is `ps`'s start date,
/// which has a resolution of one second: a pid the OS reissues within the same
/// second its previous holder started reads as the same process. Reissuing a
/// pid requires the allocator to wrap its whole range first, which no host does
/// within a second, and the limit is stated rather than assumed away.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ProcessIdentity {
    /// The process id.
    pid: i32,
    /// The start token, including the prefix naming the reading that made it.
    started: String,
}

impl ProcessIdentity {
    /// Rebuild an identity from a pid and the start token a previous
    /// [`identify_process`] reported.
    ///
    /// # Errors
    ///
    /// [`ProcessIdentityError`] when `pid` is not positive or `started` is not a
    /// start token this module produces: empty, longer than
    /// [`MAX_START_TOKEN_BYTES`], holding a byte outside printable ASCII, or
    /// carrying no known reading prefix. A record that fails here was damaged or
    /// written by something else, and reaping against it would be a guess.
    pub fn new(pid: i32, started: &str) -> Result<Self, ProcessIdentityError> {
        if pid <= 0 {
            let refusal = Err(ProcessIdentityError::NonPositivePid { pid });
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "ProcessIdentity::new: returning an error to the caller");
            return refusal;
        }
        validate_start(started)?;
        Ok(Self {
            pid,
            started: started.to_owned(),
        })
    }

    /// The process id.
    #[must_use]
    pub const fn pid(&self) -> i32 {
        self.pid
    }

    /// The start token, comparable only with one read on the same host.
    #[must_use]
    pub fn started(&self) -> &str {
        &self.started
    }

    /// The reading that produced the start token, decided by its prefix, which
    /// every constructor has already validated.
    #[cfg(all(unix, feature = "process"))]
    fn scheme(&self) -> StartScheme {
        if self.started.starts_with(PROC_SCHEME) {
            StartScheme::Proc
        } else {
            StartScheme::Ps
        }
    }
}

impl std::fmt::Display for ProcessIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{IDENTITY_SEPARATOR}{}", self.pid, self.started)
    }
}

impl std::str::FromStr for ProcessIdentity {
    type Err = ProcessIdentityError;

    /// Parse the `<pid>/<start>` form [`ProcessIdentity`]'s `Display` writes.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let Some((pid, started)) = text.split_once(IDENTITY_SEPARATOR) else {
            let refusal = Err(ProcessIdentityError::MissingSeparator);
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "ProcessIdentity::from_str: returning an error to the caller");
            return refusal;
        };
        // Only the digits `Display` writes: `i32::from_str` also takes a sign and
        // leading zeros, and a record read back must render as the text it was.
        let canonical = !pid.is_empty()
            && pid.bytes().all(|byte| byte.is_ascii_digit())
            && (pid == "0" || !pid.starts_with('0'));
        let Some(pid) = canonical.then(|| pid.parse::<i32>().ok()).flatten() else {
            let refusal = Err(ProcessIdentityError::MalformedPid);
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "ProcessIdentity::from_str: returning an error to the caller");
            return refusal;
        };
        Self::new(pid, started)
    }
}

/// Why a stored [`ProcessIdentity`] was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ProcessIdentityError {
    /// The text had no `/` between a pid and a start token.
    MissingSeparator,
    /// The pid was not an `i32` written as plain decimal digits.
    MalformedPid,
    /// The pid was zero or negative; neither names one process.
    NonPositivePid {
        /// The pid that was refused.
        pid: i32,
    },
    /// The start token was empty.
    EmptyStart,
    /// The start token was longer than [`MAX_START_TOKEN_BYTES`].
    StartTooLong {
        /// Its length in bytes.
        len: usize,
    },
    /// The start token held a byte outside printable ASCII.
    StartNotPrintable {
        /// The byte offset of the first such byte.
        at: usize,
    },
    /// The start token named no reading this module produces.
    UnknownScheme,
}

impl std::fmt::Display for ProcessIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::MissingSeparator => f.write_str("a process identity needs `<pid>/<start>`"),
            Self::MalformedPid => f.write_str("a process identity's pid is not a decimal i32"),
            Self::NonPositivePid { pid } => {
                write!(f, "a process identity's pid must be positive, got {pid}")
            }
            Self::EmptyStart => f.write_str("a process identity's start token is empty"),
            Self::StartTooLong { len } => write!(
                f,
                "a process identity's start token is {len} bytes, past the {MAX_START_TOKEN_BYTES}-byte bound"
            ),
            Self::StartNotPrintable { at } => write!(
                f,
                "a process identity's start token holds a non-printable byte at offset {at}"
            ),
            Self::UnknownScheme => {
                f.write_str("a process identity's start token names no reading this crate produces")
            }
        }
    }
}

impl std::error::Error for ProcessIdentityError {}

/// Refuse a start token this module could not have produced.
fn validate_start(started: &str) -> Result<(), ProcessIdentityError> {
    let refusal = if started.is_empty() {
        Some(ProcessIdentityError::EmptyStart)
    } else if started.len() > MAX_START_TOKEN_BYTES {
        Some(ProcessIdentityError::StartTooLong { len: started.len() })
    } else {
        started
            .bytes()
            .position(|byte| !byte.is_ascii_graphic())
            .map(|at| ProcessIdentityError::StartNotPrintable { at })
    };
    if let Some(error) = refusal {
        let refusal = Err(error);
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "validate_start: returning an error to the caller");
        return refusal;
    }
    if started.starts_with(PROC_SCHEME) || started.starts_with(PS_SCHEME) {
        Ok(())
    } else {
        let refusal = Err(ProcessIdentityError::UnknownScheme);
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "validate_start: returning an error to the caller");
        refusal
    }
}

/// The identity of the process `pid` names now, or `None` when no process holds it.
///
/// Call it while the process is known to be the one meant — for a supervisor,
/// before its child is reaped — and store the result; [`reap_orphaned_group`]
/// later compares it with whoever holds the number then. On Linux it reads
/// `/proc` (the boot id and the start tick) and falls back to `ps` when `/proc`
/// cannot be read; elsewhere it runs `ps -o lstart -p <pid>` once, in UTC under
/// the C locale so two readers in different time zones read the same token.
///
/// # Errors
///
/// [`std::io::ErrorKind::InvalidInput`] for a non-positive `pid`, and the
/// reading's own error when the table could not be read — never `None` for a
/// reading that failed, because "no such process" and "could not look" are
/// different answers to a caller about to signal.
#[cfg(all(unix, feature = "process"))]
pub fn identify_process(pid: i32) -> io::Result<Option<ProcessIdentity>> {
    if pid <= 0 {
        let refusal = Err(invalid_pid());
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "identify_process: returning an error to the caller");
        return refusal;
    }
    #[cfg(target_os = "linux")]
    let started = match proc_start(pid) {
        Ok(started) => started,
        Err(error) => {
            #[cfg(feature = "trace")]
            crate::trace::debug!(pid, error = %error, "identify_process: /proc was unreadable; reading ps");
            #[cfg(not(feature = "trace"))]
            let _unreported = error;
            ps_start(pid)?
        }
    };
    #[cfg(not(target_os = "linux"))]
    let started = ps_start(pid)?;
    Ok(started.map(|started| ProcessIdentity { pid, started }))
}

/// The identity of the process `pid` names now.
///
/// The `process` capability is Unix-only; other targets report
/// [`std::io::ErrorKind::Unsupported`].
#[cfg(all(not(unix), feature = "process"))]
pub fn identify_process(pid: i32) -> io::Result<Option<ProcessIdentity>> {
    let _ = pid;
    Err(unsupported("process identify_process is Unix-only"))
}

/// What [`reap_orphaned_group`] found and did.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OrphanReap {
    /// The recorded leader still held its pid, so its group, the leader itself
    /// and every descendant captured from it were sent `SIGKILL`.
    ///
    /// A member that had already exited when its signal was sent counts as
    /// stopped: the signal's purpose was met by the exit.
    Signalled {
        /// The descendants captured from the leader before the group signal,
        /// with the mechanism that found them.
        descendants: DescendantSet,
    },
    /// No process holds the recorded pid, so nothing was signalled.
    ///
    /// The leader is gone. Members of its group that outlived it are not
    /// reachable by this record: a group whose leader has exited can no longer
    /// be told apart from a later group under the same number, so it is left
    /// alone rather than guessed at.
    LeaderGone,
    /// Another process holds the recorded pid now, so nothing was signalled.
    LeaderReused {
        /// The identity of the process that holds the number now.
        holder: ProcessIdentity,
    },
}

/// Stop the process group `leader` led, if `leader` is still the process holding
/// its pid.
///
/// The successor's half of [`identify_process`]: a supervisor that was killed
/// before it could clean up leaves its children's groups running, and a later
/// process holding the identities it recorded calls this once per identity.
/// The current holder of the pid is read the way the record was made; only an
/// exact match is signalled, and then the leader's descendants are captured,
/// the group is killed, the leader is killed by pid in case it left its group,
/// and every captured descendant is killed by pid, which reaches one that called
/// `setsid`.
///
/// **Not claimed:** atomicity. The read and the signal are separate calls, so a
/// leader that exits between them and whose pid the OS reissues in that window
/// would be signalled under its successor's name; reissuing a pid requires the
/// allocator to wrap its whole range first. A group whose leader has already
/// gone is not reached at all (see [`OrphanReap::LeaderGone`]).
///
/// # Errors
///
/// The reading's own error when the current holder could not be identified,
/// [`std::io::ErrorKind::Unsupported`] for a `/proc` record read on a host
/// without `/proc`, and a signal's own error other than "no such process" —
/// `EPERM`, for one, says the group belongs to someone this process may not
/// signal, which is a fact the caller must see.
#[cfg(all(unix, feature = "process"))]
pub fn reap_orphaned_group(leader: &ProcessIdentity) -> io::Result<OrphanReap> {
    let holder = match leader.scheme() {
        StartScheme::Ps => ps_start(leader.pid)?,
        #[cfg(target_os = "linux")]
        StartScheme::Proc => proc_start(leader.pid)?,
        #[cfg(not(target_os = "linux"))]
        StartScheme::Proc => {
            let refusal = Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a /proc process identity can only be read on Linux",
            ));
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "reap_orphaned_group: returning an error to the caller");
            return refusal;
        }
    };
    let Some(started) = holder else {
        return Ok(OrphanReap::LeaderGone);
    };
    if started != leader.started {
        return Ok(OrphanReap::LeaderReused {
            holder: ProcessIdentity {
                pid: leader.pid,
                started,
            },
        });
    }
    let descendants = match capture_descendants(leader.pid) {
        Ok(captured) => captured,
        Err(error) => {
            // The group signal below still reaches every member that stayed in
            // the group; the empty capture says no table was read.
            #[cfg(feature = "trace")]
            crate::trace::debug!(pid = leader.pid, error = %error, "reap_orphaned_group: the descendants could not be captured");
            #[cfg(not(feature = "trace"))]
            let _unreported = error;
            DescendantSet::default()
        }
    };
    stopped_unless_refused(kill_process_group(leader.pid))?;
    stopped_unless_refused(kill_process(leader.pid))?;
    for pid in descendants.pids() {
        stopped_unless_refused(kill_process(*pid))?;
    }
    Ok(OrphanReap::Signalled { descendants })
}

/// Stop the process group `leader` led, if `leader` is still the process holding
/// its pid.
///
/// The `process` capability is Unix-only; other targets report
/// [`std::io::ErrorKind::Unsupported`].
#[cfg(all(not(unix), feature = "process"))]
pub fn reap_orphaned_group(leader: &ProcessIdentity) -> io::Result<OrphanReap> {
    let _ = leader;
    Err(unsupported("process reap_orphaned_group is Unix-only"))
}

/// A signal answered "no such process" reached a target that had already
/// stopped, which is what the signal was for; every other refusal stands.
#[cfg(all(unix, feature = "process"))]
fn stopped_unless_refused(signalled: io::Result<()>) -> io::Result<()> {
    match signalled {
        Err(error) if error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) => {
            Ok(())
        }
        other => other,
    }
}

/// The `/proc` start token of `pid`: the boot id and the kernel's start tick.
///
/// `Ok(None)` when `/proc/<pid>` does not exist; an error when the boot id or the
/// stat line cannot be read or parsed, which the caller treats as "could not
/// look" rather than as absence.
#[cfg(all(target_os = "linux", feature = "process"))]
fn proc_start(pid: i32) -> io::Result<Option<String>> {
    // `starttime` is field 22 of `stat`; counted from the state letter (field
    // 3), the first field after the command name, it is the twentieth.
    const START_FIELD_AFTER_NAME: usize = 19;
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let stat_path = std::path::Path::new("/proc")
        .join(pid.to_string())
        .join("stat");
    let stat = match std::fs::read_to_string(stat_path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            let refusal = Err(error);
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "proc_start: returning an error to the caller");
            return refusal;
        }
    };
    // `comm` may itself contain spaces and parentheses, so the fields start
    // after the **last** `)`.
    let ticks = stat
        .rfind(')')
        .and_then(|end| stat.get(end.saturating_add(1)..))
        .and_then(|fields| fields.split_ascii_whitespace().nth(START_FIELD_AFTER_NAME));
    let Some(ticks) = ticks else {
        let refusal = Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("/proc/{pid}/stat carried no start tick"),
        ));
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "proc_start: returning an error to the caller");
        return refusal;
    };
    Ok(Some(format!("{PROC_SCHEME}{}:{ticks}", boot.trim())))
}

/// The `ps -o lstart` start token of `pid`, read in UTC under the C locale.
///
/// `ps` prints nothing for a pid it does not list, which is either absence or a
/// failed read; signal zero decides which, so a table that could not be read is
/// never reported as an absent process.
#[cfg(all(unix, feature = "process"))]
fn ps_start(pid: i32) -> io::Result<Option<String>> {
    let pid_text = pid.to_string();
    let output = run_ps(&["-o", "lstart=", "-p", &pid_text], "ps_start")?;
    let text = String::from_utf8_lossy(&output.stdout);
    let words: Vec<&str> = text.split_ascii_whitespace().collect();
    if words.is_empty() {
        if process_exists(pid)? {
            let refusal = Err(io::Error::other(format!(
                "lgwks_std::process (ps_start): `ps` listed no start for live pid {pid}"
            )));
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "ps_start: returning an error to the caller");
            return refusal;
        }
        return Ok(None);
    }
    Ok(Some(format!("{PS_SCHEME}{}", words.join("-"))))
}

// ── Kernel-level containment: cgroup v2 and the child subreaper (Linux) ────
//
// A process group is the floor: a descendant that calls `setsid` leaves it by
// construction, and a capture is a snapshot that races the fork it is trying to
// name. The two kernel owners below close those gaps without a new dependency,
// using only `std::fs` and the `rustix/process` calls this module already
// makes:
//
// - a cgroup v2 scope the supervisor creates per tree: every member, including
//   a `setsid` escapee attached after the spawn, dies on one `cgroup.kill`
//   write, atomically, with no fork race for the members it holds;
// - the child subreaper flag plus an attribution-checked reap: an orphan whose
//   parent died before the cleanup is re-parented to *this* process instead of
//   init, and a pid that is both a captured descendant and a current child of
//   this process is provably still the same process (an unreaped child keeps
//   its pid unreissued), so it is safe to signal where a re-capture from a
//   reaped leader would not be.
//
// Where neither is available the supervisor falls back to the group and the
// capture, and the receipt says so. Nothing here is claimed on non-Linux
// targets: other targets report [`std::io::ErrorKind::Unsupported`].

/// The cgroup v2 mount this module scopes trees under.
///
/// A child cgroup is created per supervised tree, so one tree's kill never
/// touches another's members. The mount is read once per scope rather than
/// cached in a global: a container that gains delegation after the first spawn
/// must be usable by the second, and a global would freeze the first answer.
#[cfg(all(target_os = "linux", feature = "process"))]
const CGROUP_V2_MOUNT: &str = "/sys/fs/cgroup";

/// The longest scope name [`CgroupScope::create`] accepts, in bytes.
///
/// A scope name is a single directory entry under the v2 mount, so it must fit
/// one; the bound refuses a caller-built path rather than truncating it into a
/// name that collides with another tree's.
#[cfg(all(target_os = "linux", feature = "process"))]
const MAX_SCOPE_NAME_BYTES: usize = 64;

/// One supervised tree's cgroup v2 scope: its members die on one write.
///
/// Created per tree by the supervisor after the fork and removed when the scope
/// drops. Membership is inherited across `fork`, so a grandchild forked after
/// its parent was attached is a member too; a descendant forked in the window
/// between the spawn and the attach is not, which is why the supervisor keeps
/// its capture-and-signal rounds beside this rather than replacing them. The
/// kill is synchronous as far as the writer is concerned — the write returns
/// after every member was sent `SIGKILL` — and the supervisor still observes
/// the scope empty afterwards rather than trusting the return.
///
/// A scope whose supervisor was itself killed with `SIGKILL` runs no
/// destructor: its directory outlives it on the kernel's filesystem until the
/// host clears it. The directory is empty of members (the kill ran first, and
/// a member keeps its directory busy), so the residue is one empty directory,
/// never a running process.
#[cfg(all(target_os = "linux", feature = "process"))]
#[derive(Debug)]
pub struct CgroupScope {
    /// The scope directory; removed best-effort on drop.
    path: std::path::PathBuf,
}

#[cfg(all(target_os = "linux", feature = "process"))]
impl CgroupScope {
    /// Create the scope `name` under the cgroup v2 mount.
    ///
    /// `name` is one directory entry — alphanumeric, `-`, `_`, `.` and `+`,
    /// and neither `.` nor `..` — so a caller cannot smuggle a path into
    /// another tree's scope, the mount, or its parent. An unwritable or
    /// absent mount is
    /// [`std::io::ErrorKind::Unsupported`]: the supervisor treats that as "use
    /// the subreaper", not as a failure, because a container without delegation
    /// is a fact about the host rather than a refusal of this tree. Any other
    /// OS error stands.
    ///
    /// # Errors
    ///
    /// [`std::io::ErrorKind::InvalidInput`] for a name that is empty, too long,
    /// names `.` or `..`, or carries a byte outside the allowed set;
    /// [`std::io::ErrorKind::Unsupported`] when no child cgroup is creatable;
    /// the creation's own error otherwise.
    pub fn create(name: &str) -> io::Result<Self> {
        validate_scope_name(name)?;
        let path = std::path::Path::new(CGROUP_V2_MOUNT).join(name);
        match std::fs::create_dir(&path) {
            Ok(()) => Ok(Self { path }),
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    || error.kind() == io::ErrorKind::PermissionDenied
                    || error.kind() == io::ErrorKind::ReadOnlyFilesystem =>
            {
                let refusal = Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "lgwks_std::process (CgroupScope::create): no child cgroup is creatable under {CGROUP_V2_MOUNT} ({}), so this host has no cgroup containment",
                        error.kind(),
                    ),
                ));
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "CgroupScope::create: returning an error to the caller");
                refusal
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                // A scope from a supervisor that died without running its
                // destructor — or a name colliding with another live tree.
                // Reused only when it holds no members: a scope that still
                // holds processes belongs to whoever put them there, and this
                // tree must not attach to it (its kill would stop them) nor
                // claim its receipt. An unreadable member list refuses the
                // same way: what cannot be shown empty is not empty.
                let scope = Self { path };
                match scope.members() {
                    Ok(members) if members.is_empty() => Ok(scope),
                    Ok(members) => {
                        let refusal = Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            format!(
                                "lgwks_std::process (CgroupScope::create): the scope holds {} processes of another tree, so this tree runs without a scope",
                                members.len(),
                            ),
                        ));
                        #[cfg(feature = "trace")]
                        crate::trace::debug!(error = ?refusal.as_ref().err(), "CgroupScope::create: returning an error to the caller");
                        refusal
                    }
                    Err(error) => {
                        let refusal = Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            format!(
                                "lgwks_std::process (CgroupScope::create): the scope's members are unreadable ({error}), so this tree runs without a scope",
                            ),
                        ));
                        #[cfg(feature = "trace")]
                        crate::trace::debug!(error = ?refusal.as_ref().err(), "CgroupScope::create: returning an error to the caller");
                        refusal
                    }
                }
            }
            Err(error) => {
                let refusal = Err(error);
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "CgroupScope::create: returning an error to the caller");
                refusal
            }
        }
    }

    /// The scope directory, for inspection only.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Attach `pid` to this scope.
    ///
    /// The supervisor calls this with the pid its spawn just returned, before
    /// the child is awaited: every descendant forked afterwards inherits the
    /// membership. `pid` must be positive; a non-positive id is refused before
    /// anything is written.
    ///
    /// # Errors
    ///
    /// [`std::io::ErrorKind::InvalidInput`] for a non-positive `pid`, and the
    /// write's own error otherwise — including the case where `pid` exited
    /// between the spawn and the attach, which the supervisor reads as "this
    /// tree needs the capture rounds, not the scope".
    pub fn add(&self, pid: i32) -> io::Result<()> {
        if pid <= 0 {
            let refusal = Err(invalid_pid());
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "CgroupScope::add: returning an error to the caller");
            return refusal;
        }
        match std::fs::write(self.path.join("cgroup.procs"), pid.to_string()) {
            Ok(()) => Ok(()),
            Err(error) => {
                let refusal = Err(error);
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "CgroupScope::add: returning an error to the caller");
                refusal
            }
        }
    }

    /// Send `SIGKILL` to every member of this scope, atomically.
    ///
    /// This is `cgroup.kill`: one write reaches every member, including one
    /// that called `setsid` after it was attached, with no fork race for the
    /// members the scope holds. It is the supervisor's first signal, before its
    /// capture rounds; what it could not hold (a descendant forked before the
    /// attach) the rounds still cover.
    ///
    /// # Errors
    ///
    /// The write's own error. A refused write leaves the scope untouched and
    /// the supervisor falls back to its rounds.
    pub fn kill(&self) -> io::Result<()> {
        match std::fs::write(self.path.join("cgroup.kill"), "1") {
            Ok(()) => Ok(()),
            Err(error) => {
                let refusal = Err(error);
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "CgroupScope::kill: returning an error to the caller");
                refusal
            }
        }
    }

    /// The pids currently holding membership in this scope, sorted.
    ///
    /// The supervisor reads this after [`Self::kill`] to earn its receipt: an
    /// empty scope is the kernel's own confirmation that nothing of the tree
    /// is left, and a non-empty one names the pids the rounds must still
    /// account for. Unparseable rows are skipped — a row this reader cannot
    /// parse names no process it could signal — and an unreadable scope is the
    /// read's own error, never an empty scope.
    ///
    /// # Errors
    ///
    /// The read's own error when the scope's member list cannot be read at all.
    pub fn members(&self) -> io::Result<Vec<i32>> {
        match std::fs::read_to_string(self.path.join("cgroup.procs")) {
            Ok(text) => Ok(text
                .split_ascii_whitespace()
                .filter_map(|pid| pid.parse::<i32>().ok())
                .filter(|pid| *pid > 0)
                .collect()),
            Err(error) => {
                let refusal = Err(error);
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "CgroupScope::members: returning an error to the caller");
                refusal
            }
        }
    }
}

#[cfg(all(target_os = "linux", feature = "process"))]
impl Drop for CgroupScope {
    /// Remove the scope directory, best effort.
    ///
    /// A scope that still holds members refuses the removal and keeps them:
    /// the drop never kills, so a forgotten scope is an empty directory at
    /// worst, never a signalled tree. Errors are ignored because a destructor
    /// has nowhere truthful to report them.
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir(&self.path);
    }
}

/// Refuse a scope name that is not one safe directory entry.
#[cfg(all(target_os = "linux", feature = "process"))]
fn validate_scope_name(name: &str) -> io::Result<()> {
    // `.` names the mount itself and `..` its parent: accepting either would
    // attach trees to — and `kill()` — a scope this supervisor does not own,
    // up to every process on the host. Both are refused as input, never
    // resolved.
    let valid = !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= MAX_SCOPE_NAME_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'));
    if valid {
        Ok(())
    } else {
        let refusal = Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a cgroup scope name is one directory entry of letters, digits, '-', '_', '.' or '+', and neither '.' nor '..'",
        ));
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "validate_scope_name: returning an error to the caller");
        refusal
    }
}

/// The pids currently holding this process as their parent, sorted.
///
/// The subreaper flag makes this the adoptee list: every orphan re-parented
/// here appears beside the direct children this process forked itself. The
/// caller tells the two apart by intersecting with what it captured — a pid
/// it never named is somebody else's child, not an adoptee it may signal.
///
/// # Errors
///
/// The child-list read's own error, in which case nothing is claimed about
/// who this process parents.
#[cfg(all(target_os = "linux", feature = "process"))]
pub fn own_children() -> io::Result<Vec<i32>> {
    let own = i32::try_from(std::process::id()).map_err(|_| invalid_pid())?;
    match read_proc_children(own) {
        Ok(mut children) => {
            children.sort_unstable();
            Ok(children)
        }
        Err(error) => {
            let refusal = Err(error);
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "own_children: returning an error to the caller");
            refusal
        }
    }
}

/// Make orphaned descendants re-parent to this process instead of init.
///
/// This is `prctl(PR_SET_CHILD_SUBREAPER)`: a descendant whose parent exits
/// before the supervisor's cleanup is adopted by this process rather than by
/// the platform's init, so it stays findable — and attributable, which init's
/// adoptees are not. Idempotent and process-wide: setting it twice changes
/// nothing, and every supervisor in this process shares the adoptees, which is
/// why [`adopted_descendants`] intersects them with each tree's own capture
/// rather than trusting parenthood alone.
///
/// A supervisor calls this once before its first spawn; the flag survives
/// `exec` of nothing here (this process never execs) and needs no renewal.
///
/// # Errors
///
/// The `prctl` call's own error. A refused flag leaves re-parenting to init,
/// and the supervisor falls back to the group and the capture.
#[cfg(all(target_os = "linux", feature = "process"))]
pub fn enable_child_subreaper() -> io::Result<()> {
    // `PR_SET_CHILD_SUBREAPER` takes a flag, and rustix spells the flag as the
    // adopter's pid: `None` is zero, which *clears* the setting. Passing this
    // process's own pid sets it with the only adopter this supervisor can name.
    let own_pid = i32::try_from(std::process::id()).map_err(|_| invalid_pid())?;
    let own = rustix::process::Pid::from_raw(own_pid).ok_or_else(invalid_pid)?;
    match rustix::process::set_child_subreaper(Some(own)) {
        Ok(()) => Ok(()),
        Err(errno) => {
            let refusal = Err(errno_to_io(errno));
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "enable_child_subreaper: returning an error to the caller");
            refusal
        }
    }
}

/// Which of `candidates` are currently children of this process.
///
/// The attribution check that makes the subreaper safe to signal through: a
/// pid that is both a descendant this supervisor captured while its leader
/// lived *and* a current, unreaped child of this process is provably still the
/// same process — the OS cannot reissue the pid of a child nobody reaped — so
/// signalling it cannot reach a stranger. A candidate that is not a child of
/// this process is either still under its parent (and covered by the rounds)
/// or was reaped already (and gone); either way it is left alone here.
///
/// The answer is about this instant: a pid adopted after the call is not in
/// it, and the caller re-asks rather than reuses.
///
/// # Errors
///
/// [`std::io::ErrorKind::InvalidInput`] is never produced here — candidates
/// are not validated, non-positive ones simply never match a child — and the
/// only refusal is the child-list read's own error, in which case nothing is
/// claimed adopted.
#[cfg(all(target_os = "linux", feature = "process"))]
pub fn adopted_descendants(candidates: &[i32]) -> io::Result<BTreeSet<i32>> {
    let self_pid = std::process::id();
    let own = match read_proc_children(i32::try_from(self_pid).map_err(|_| invalid_pid())?) {
        Ok(children) => children,
        Err(error) => {
            let refusal = Err(error);
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "adopted_descendants: returning an error to the caller");
            return refusal;
        }
    };
    let own: BTreeSet<i32> = own.into_iter().collect();
    Ok(candidates
        .iter()
        .copied()
        .filter(|pid| own.contains(pid))
        .collect())
}

/// Reap each of `pids` that is a zombie child of this process.
///
/// The targeted counterpart of the orphan queue: `waitpid` on the exact pid,
/// so a live supervised leader is never stolen by a reap meant for an adopted
/// orphan. A pid that already exited is collected; one still running is left
/// alone; one that is not a child of this process is an error the caller reads
/// as "not mine to reap".
///
/// Returns the pids this call reaped, sorted.
///
/// # Errors
///
/// The wait's own error other than "no such process" for a pid that left while
/// it was named — that pid is gone, which is what the reap was for.
#[cfg(all(target_os = "linux", feature = "process"))]
pub fn reap_descendants(pids: &[i32]) -> io::Result<Vec<i32>> {
    let mut reaped = Vec::new();
    for pid in pids {
        let Some(target) = rustix::process::Pid::from_raw(*pid) else {
            continue;
        };
        match rustix::process::waitpid(Some(target), rustix::process::WaitOptions::NOHANG) {
            Ok(Some(_)) => reaped.push(*pid),
            Ok(None) => {}
            Err(rustix::io::Errno::CHILD) | Err(rustix::io::Errno::SRCH) => {}
            Err(errno) => {
                let refusal = Err(errno_to_io(errno));
                #[cfg(feature = "trace")]
                crate::trace::debug!(error = ?refusal.as_ref().err(), "reap_descendants: returning an error to the caller");
                return refusal;
            }
        }
    }
    reaped.sort_unstable();
    Ok(reaped)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    #[cfg(unix)]
    use std::os::unix::process::CommandExt as _;

    use super::*;

    #[test]
    fn invalid_pgid_is_rejected() {
        assert!(kill_process_group(0).is_err());
        assert!(kill_process_group(-1).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn nonexistent_group_surfs_os_error() {
        // i32::MAX is not a live group; the OS must refuse, not succeed.
        assert!(kill_process_group(i32::MAX).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn a_live_group_is_present_and_a_reaped_one_is_absent() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let group = i32::try_from(child.id())?;
        assert!(
            process_group_exists(group)?,
            "the live child owns its group"
        );
        child.kill()?;
        child.wait()?;
        assert!(!process_group_exists(group)?, "the reaped group is absent");
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn the_callers_group_and_nonpositive_ids_are_not_probed() {
        for refused in [0, -1] {
            assert_eq!(
                process_group_exists(refused).map_err(|error| error.kind()),
                Err(std::io::ErrorKind::InvalidInput),
                "group id {refused} names the caller or a process, not a group"
            );
        }
    }

    #[test]
    fn invalid_pid_is_rejected_for_the_exit_observation() {
        assert!(child_has_exited_without_reaping(0).is_err());
        assert!(child_has_exited_without_reaping(-1).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn a_non_child_pid_is_an_error_not_an_observation() {
        // i32::MAX is not our child, so waitid must refuse rather than
        // report an exit; a fabricated `true` would let a caller reap-skip a
        // process that is still running.
        assert!(child_has_exited_without_reaping(i32::MAX).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn a_live_child_is_reported_not_yet_exited() -> Result<(), Box<dyn std::error::Error>> {
        // A real child of this test process, held alive until the probe.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let pid = i32::try_from(child.id())?;
        assert!(
            !child_has_exited_without_reaping(pid)?,
            "live child reported exited"
        );
        child.kill()?;
        let status = child.wait()?;
        assert!(
            !status.success(),
            "the killed probe child must report its signal, not success"
        );
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn an_exited_child_is_reported_exited_and_stays_reapable()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut child = std::process::Command::new("true")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let pid = i32::try_from(child.id())?;
        // The child may need a moment to actually exit.
        let mut observed_exit = false;
        for _ in 0..100_000 {
            if child_has_exited_without_reaping(pid)? {
                observed_exit = true;
                break;
            }
            std::thread::yield_now();
        }
        assert!(
            observed_exit,
            "an exited child must be observed within the bound"
        );
        // The observation must not have consumed the exit: the reap still
        // returns the real status.
        let status = child.wait()?;
        assert!(status.success(), "`true` must report success after reaping");
        Ok(())
    }

    /// The id `i32::MAX` can never name a process on a supported platform, so a
    /// vacancy probe is decidable rather than a race with the next fork.
    const VACANT: i32 = i32::MAX;

    /// `ESRCH`, "no such process", on every Unix this feature targets.
    ///
    /// Read as the raw code rather than as an [`std::io::ErrorKind`]: macOS
    /// classifies `ESRCH` as uncategorised where Linux calls it `NotFound`, so
    /// the kind is a property of the platform and the errno is the fact.
    const ESRCH: i32 = 3;

    /// A tree the tests build and tear down: `sh` with `depth` backgrounded
    /// `sleep` descendants, each of which records its own pid.
    ///
    /// The pid files are what make the model checkable: the capture is compared
    /// against ids the tree itself wrote, rather than against a count the test
    /// already assumed.
    #[cfg(unix)]
    struct Tree {
        /// The leader, unreaped: its pid pins the tree until the test ends.
        leader: std::process::Child,
        /// The leader's id, which is also its group id.
        root: i32,
        /// The descendant pids, in fork order.
        descendants: Vec<i32>,
        /// The directory holding the pid files.
        dir: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl Tree {
        /// Grow a tree of `depth` descendants and wait until each has recorded
        /// its pid.
        fn grow(depth: usize) -> Result<Self, Box<dyn std::error::Error>> {
            let dir = std::env::temp_dir().join(format!(
                "lgwks-std-tree-{}-{}",
                std::process::id(),
                depth
            ));
            let _ignored = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir)?;
            let mut script = String::new();
            for level in 0..depth {
                let file = dir.join(format!("{level}.pid"));
                // `exec` and not a second process: the level is then exactly the
                // pid it recorded, so the model and the capture are comparable.
                let _written = write!(
                    script,
                    "sh -c 'echo $$ > {}; exec sleep 30' & ",
                    file.display()
                );
            }
            script.push_str("exec sleep 30");
            let leader = std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                // Its own group, or the drop-time group kill would name a group
                // this tree does not lead and leave every sleeper running.
                .process_group(0)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?;
            let root = i32::try_from(leader.id())?;
            let mut descendants = Vec::with_capacity(depth);
            for level in 0..depth {
                let file = dir.join(format!("{level}.pid"));
                let mut pid = None;
                for _ in 0..2000 {
                    if let Ok(text) = std::fs::read_to_string(&file) {
                        pid = text.trim().parse::<i32>().ok();
                        if pid.is_some() {
                            break;
                        }
                    }
                    std::thread::park_timeout(std::time::Duration::from_millis(5));
                }
                let pid = pid.ok_or_else(|| format!("level {level} never recorded its pid"))?;
                descendants.push(pid);
            }
            Ok(Self {
                leader,
                root,
                descendants,
                dir,
            })
        }
    }

    #[cfg(unix)]
    impl Drop for Tree {
        /// Leave nothing behind: the group kill takes the whole tree, and the
        /// leader is reaped by the wait that follows it.
        fn drop(&mut self) {
            let _ignored = kill_process_group(self.root);
            let _reaped = self.leader.wait();
            let _removed = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_non_positive_root_is_refused_before_any_table_is_read()
    -> Result<(), Box<dyn std::error::Error>> {
        for refused in [0, -1, i32::MIN] {
            assert_eq!(
                capture_descendants(refused).map_err(|error| error.kind()),
                Err(std::io::ErrorKind::InvalidInput),
                "root {refused} names the caller or no process, so no tree is walked"
            );
        }
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn a_non_positive_pid_is_refused_by_both_per_process_primitives()
    -> Result<(), Box<dyn std::error::Error>> {
        for refused in [0, -1, i32::MIN] {
            assert_eq!(
                process_exists(refused).map_err(|error| error.kind()),
                Err(std::io::ErrorKind::InvalidInput),
                "pid {refused} names the caller or no process, so nothing is probed"
            );
            assert_eq!(
                kill_process(refused).map_err(|error| error.kind()),
                Err(std::io::ErrorKind::InvalidInput),
                "pid {refused} must be refused before any signal leaves the process"
            );
        }
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn a_vacant_pid_is_absent_and_signalling_it_says_esrch()
    -> Result<(), Box<dyn std::error::Error>> {
        assert!(
            !process_exists(VACANT)?,
            "an id above every pid ceiling names no process"
        );
        assert_eq!(
            kill_process(VACANT).map_err(|error| error.raw_os_error()),
            Err(Some(ESRCH)),
            "signalling a vacant pid must say it does not exist, never succeed"
        );
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn the_capture_finds_every_descendant_of_a_live_root() -> Result<(), Box<dyn std::error::Error>>
    {
        let tree = Tree::grow(3)?;
        let captured = capture_descendants(tree.root)?;
        assert!(
            captured.mechanism().read_a_table(),
            "a live capture must name a mechanism that read a table, got {:?}",
            captured.mechanism()
        );
        assert!(
            !captured.is_truncated(),
            "a depth-3 tree is inside every bound"
        );
        let mut expected = tree.descendants.clone();
        expected.sort_unstable();
        assert_eq!(
            captured.pids(),
            expected.as_slice(),
            "the capture must be exactly the tree the pids describe"
        );
        assert!(
            !captured.contains(tree.root),
            "the root leads the group; a capture that listed it would double-signal the leader"
        );
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn a_descendant_that_left_the_group_is_still_captured() -> Result<(), Box<dyn std::error::Error>>
    {
        // The case a group signal cannot reach: the grandchild makes itself a
        // session leader, so it is in a different group from its parent and only
        // a pid signal can stop it. The capture is what makes it reachable.
        let dir = std::env::temp_dir().join(format!(
            "lgwks-std-escape-{}-{}",
            std::process::id(),
            match std::thread::current().name() {
                Some(name) => name.to_owned(),
                None => format!("{:?}", std::thread::current().id()),
            }
        ));
        let _ignored = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let pid_file = dir.join("escaped.pid");
        let mut leader = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "python3 -c 'import os,time; os.setsid(); open(\"{}\",\"w\").write(str(os.getpid())); time.sleep(30)' & exec sleep 30",
                pid_file.display()
            ))
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let root = i32::try_from(leader.id())?;
        let mut escaped = None;
        for _ in 0..4000 {
            if let Ok(text) = std::fs::read_to_string(&pid_file) {
                escaped = text.trim().parse::<i32>().ok();
                if escaped.is_some() {
                    break;
                }
            }
            std::thread::park_timeout(std::time::Duration::from_millis(5));
        }
        let Some(escaped) = escaped else {
            let _killed = kill_process_group(root);
            let _reaped = leader.wait();
            let _removed = std::fs::remove_dir_all(&dir);
            return Err("this host cannot leave a process group, so nothing was exercised".into());
        };
        let captured = capture_descendants(root)?;
        assert!(
            captured.contains(escaped),
            "a descendant that called setsid is still a child of the root, so the capture must \
             hold pid {escaped}: captured {:?} of root {root}",
            captured.pids()
        );
        assert!(
            process_exists(escaped)?,
            "the premise: the escapee is running and needs a pid signal to stop"
        );
        kill_process(escaped)?;
        let _killed = kill_process_group(root);
        let _reaped = leader.wait();
        let _removed = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn a_signal_to_one_captured_pid_stops_that_pid_and_leaves_its_siblings()
    -> Result<(), Box<dyn std::error::Error>> {
        let tree = Tree::grow(2)?;
        let captured = capture_descendants(tree.root)?;
        let Some(first) = captured.pids().first().copied() else {
            return Err("the capture held no descendant to signal".into());
        };
        kill_process(first)?;
        assert!(
            all_stop_running(&[first])?,
            "the signalled pid {first} must have stopped running; it may remain in the \
             table as an unreaped zombie, which is not a survivor"
        );
        let sibling = captured
            .pids()
            .iter()
            .copied()
            .find(|pid| *pid != first)
            .ok_or("the capture held one descendant, so no sibling could survive")?;
        assert_eq!(
            running_processes(&[sibling])?
                .into_iter()
                .collect::<Vec<i32>>(),
            vec![sibling],
            "a per-process signal must reach exactly one process: pid {sibling} is untouched"
        );
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn a_capture_of_a_live_root_never_reports_a_truncated_tree_of_its_own_shape()
    -> Result<(), Box<dyn std::error::Error>> {
        let tree = Tree::grow(1)?;
        let captured = capture_descendants(tree.root)?;
        assert!(
            !captured.is_truncated(),
            "a one-descendant tree is inside every declared bound, so the flag must stay clear"
        );
        assert!(
            captured.pids().len() <= MAX_CAPTURED_DESCENDANTS,
            "a capture is bounded by MAX_CAPTURED_DESCENDANTS, got {}",
            captured.pids().len()
        );
        Ok(())
    }

    #[test]
    fn absorbing_two_captures_unions_them_and_keeps_the_stronger_mechanism() {
        let mut first = DescendantSet {
            mechanism: ContainmentMechanism::ProcessTableSnapshot,
            pids: vec![30, 10],
            truncated: true,
        };
        let second = DescendantSet {
            mechanism: ContainmentMechanism::ProcChildrenTree,
            pids: vec![10, 20],
            truncated: false,
        };
        first.absorb(&second);
        assert_eq!(
            first.pids(),
            [10, 20, 30].as_slice(),
            "a merged capture is the sorted union of both"
        );
        assert_eq!(
            first.mechanism(),
            ContainmentMechanism::ProcChildrenTree,
            "the merged mechanism must not understate what was read"
        );
        assert!(
            first.is_truncated(),
            "a truncation in either half survives the merge"
        );
    }

    #[test]
    fn a_merge_past_the_declared_ceiling_is_refused_and_reported()
    -> Result<(), std::num::TryFromIntError> {
        let mut first = DescendantSet {
            mechanism: ContainmentMechanism::ProcChildrenTree,
            pids: (0..MAX_CAPTURED_DESCENDANTS)
                .map(i32::try_from)
                .collect::<Result<_, _>>()?,
            truncated: false,
        };
        let before = first.pids().len();
        first.absorb(&DescendantSet {
            mechanism: ContainmentMechanism::ProcChildrenTree,
            pids: vec![i32::MAX],
            truncated: false,
        });
        assert_eq!(
            first.pids().len(),
            before,
            "a capture at the ceiling must not grow past it"
        );
        assert!(
            first.is_truncated(),
            "a refused id must leave the set reported as a prefix, not as the whole tree"
        );
        Ok(())
    }

    /// A `stat` line names its process running unless it is a zombie or is
    /// exiting. The command name may hold spaces and parentheses.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_stat_line_is_running_unless_it_is_a_zombie_or_exiting() {
        let line = |state: char, flags: u64| {
            format!(
                "4242 (a (tricky) name) {state} 1 4242 4242 0 -1 {flags} 0 0 0 0 1 2 0 0 20 0 1 0 77"
            )
        };
        assert!(proc_stat_says_running(&line('R', 0x0040_0000)));
        assert!(proc_stat_says_running(&line('S', 0x0040_0040)));
        assert!(
            !proc_stat_says_running(&line('Z', 0x0040_0000)),
            "a zombie has stopped"
        );
        assert!(
            !proc_stat_says_running(&line('R', 0x0040_0004)),
            "a process in do_exit never returns to user code"
        );
        assert!(
            proc_stat_says_running("4242 (truncated"),
            "an unparsable line understates the cleanup rather than overstating it"
        );
    }

    /// `SIGKILL` (signal 9) is bit 8 of either pending mask.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_pending_kill_is_read_from_either_pending_mask() {
        let status = |thread: &str, shared: &str| {
            format!(
                "Name:\tsh\nState:\tS (sleeping)\nSigPnd:\t{thread}\nShdPnd:\t{shared}\nSigBlk:\t0000000000000000\n"
            )
        };
        assert!(status_has_kill_pending(&status(
            "0000000000000100",
            "0000000000000000"
        )));
        assert!(status_has_kill_pending(&status(
            "0000000000000000",
            "0000000000000100"
        )));
        assert!(
            !status_has_kill_pending(&status("0000000000000000", "0000000000004000")),
            "SIGTERM pending is not a kill"
        );
        assert!(
            !status_has_kill_pending("Name:\tsh\n"),
            "no mask claims nothing"
        );
    }

    /// A process killed while stopped has the kill pending until it is resumed,
    /// and is not running from the moment the kill is delivered.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_killed_process_is_not_running_even_before_it_is_scheduled()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut child = std::process::Command::new("sleep").arg("30").spawn()?;
        let pid = i32::try_from(child.id())?;
        assert_eq!(running_processes(&[pid])?.len(), 1, "the premise: it runs");
        kill_process(pid)?;
        assert!(
            running_processes(&[pid])?.is_empty(),
            "pid {pid} was sent SIGKILL: pending, exiting or a zombie, it runs no more user code"
        );
        let _reaped = child.wait();
        Ok(())
    }

    /// Whether every pid in `pids` stops running within twenty seconds.
    ///
    /// Running, not present: a killed process whose parent has not reaped it
    /// yet is a zombie, which has stopped and is not a survivor.
    #[cfg(unix)]
    fn all_stop_running(pids: &[i32]) -> Result<bool, Box<dyn std::error::Error>> {
        for _ in 0..4000 {
            if running_processes(pids)?.is_empty() {
                return Ok(true);
            }
            std::thread::park_timeout(std::time::Duration::from_millis(5));
        }
        Ok(false)
    }

    #[test]
    #[cfg(unix)]
    fn a_live_process_reads_one_identity_that_round_trips_through_its_text()
    -> Result<(), Box<dyn std::error::Error>> {
        let pid = i32::try_from(std::process::id())?;
        let first = identify_process(pid)?.ok_or("this process has no identity")?;
        let second = identify_process(pid)?.ok_or("this process lost its identity")?;
        assert_eq!(first, second, "one process read twice is one identity");
        assert_eq!(first.pid(), pid);
        let parsed: ProcessIdentity = first.to_string().parse()?;
        assert_eq!(parsed, first, "the stored text names the same process");
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn a_vacant_pid_has_no_identity_and_a_non_positive_one_is_refused()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(identify_process(VACANT)?, None, "no process holds {VACANT}");
        for refused in [0, -1, i32::MIN] {
            assert_eq!(
                identify_process(refused).map_err(|error| error.kind()),
                Err(std::io::ErrorKind::InvalidInput),
                "pid {refused} names no single process"
            );
        }
        Ok(())
    }

    #[test]
    fn a_damaged_record_is_refused_by_the_arm_that_names_its_damage() {
        let long = format!("ps:{}", "x".repeat(MAX_START_TOKEN_BYTES));
        let cases: [(&str, ProcessIdentityError); 9] = [
            ("42", ProcessIdentityError::MissingSeparator),
            ("4x2/ps:Mon", ProcessIdentityError::MalformedPid),
            ("+42/ps:Mon", ProcessIdentityError::MalformedPid),
            ("042/ps:Mon", ProcessIdentityError::MalformedPid),
            ("0/ps:Mon", ProcessIdentityError::NonPositivePid { pid: 0 }),
            ("42/", ProcessIdentityError::EmptyStart),
            (
                "42/ps:Mon Oct",
                ProcessIdentityError::StartNotPrintable { at: 6 },
            ),
            ("42/when:Mon", ProcessIdentityError::UnknownScheme),
            ("", ProcessIdentityError::MissingSeparator),
        ];
        for (text, expected) in cases {
            assert_eq!(
                text.parse::<ProcessIdentity>(),
                Err(expected),
                "{text:?} is refused by its own arm"
            );
        }
        assert_eq!(
            ProcessIdentity::new(42, &long),
            Err(ProcessIdentityError::StartTooLong {
                len: MAX_START_TOKEN_BYTES.saturating_add(3)
            }),
            "a token one prefix past the bound is refused before it is kept"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_matching_leader_has_its_group_and_every_descendant_stopped()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut tree = Tree::grow(3)?;
        let leader = identify_process(tree.root)?.ok_or("the live leader has no identity")?;
        let OrphanReap::Signalled { descendants } = reap_orphaned_group(&leader)? else {
            return Err("a leader that still holds its pid must be signalled".into());
        };
        let mut expected = tree.descendants.clone();
        expected.sort_unstable();
        assert_eq!(
            descendants.pids(),
            expected.as_slice(),
            "the reap captures exactly the tree below the leader"
        );
        let status = tree.leader.wait()?;
        assert!(!status.success(), "the leader ends by the reap's signal");
        assert!(
            all_stop_running(&expected)?,
            "every descendant of the reaped group stops running: {expected:?}"
        );
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn a_forged_start_on_a_live_pid_is_never_signalled() -> Result<(), Box<dyn std::error::Error>> {
        let mut tree = Tree::grow(1)?;
        let real = identify_process(tree.root)?.ok_or("the live leader has no identity")?;
        let forged = ProcessIdentity::new(real.pid(), &format!("{}0", real.started()))?;
        assert_eq!(
            reap_orphaned_group(&forged)?,
            OrphanReap::LeaderReused { holder: real },
            "a pid whose start differs from the record is another process"
        );
        assert!(
            tree.leader.try_wait()?.is_none(),
            "the process holding the number was not signalled"
        );
        assert_eq!(
            running_processes(&tree.descendants)?
                .into_iter()
                .collect::<Vec<i32>>(),
            tree.descendants,
            "nor was anything below it"
        );
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn a_leader_that_is_gone_is_left_alone() -> Result<(), Box<dyn std::error::Error>> {
        let mut child = std::process::Command::new("true")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let pid = i32::try_from(child.id())?;
        let recorded = identify_process(pid)?.ok_or("the child has no identity")?;
        child.wait()?;
        let reaped = reap_orphaned_group(&recorded)?;
        assert!(
            !matches!(reaped, OrphanReap::Signalled { .. }),
            "a reaped leader's record signals nothing, got {reaped:?}"
        );
        Ok(())
    }
}
