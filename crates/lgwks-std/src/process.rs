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

use std::collections::{BTreeMap, BTreeSet};
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
}

impl ContainmentMechanism {
    /// The stronger of two mechanisms, for a capture that merged both.
    ///
    /// A per-process walk beats a whole-table snapshot (it reads less and races
    /// less), and either beats no capture at all. Merging never reports the
    /// weaker mechanism of the two, because a caller reading the merged value
    /// would otherwise be told less than the capture actually established.
    #[must_use]
    pub const fn strongest(self, other: Self) -> Self {
        match (self, other) {
            (Self::ProcChildrenTree, _) | (_, Self::ProcChildrenTree) => Self::ProcChildrenTree,
            (Self::ProcessTableSnapshot, _) | (_, Self::ProcessTableSnapshot) => {
                Self::ProcessTableSnapshot
            }
            (Self::ProcessGroupOnly, Self::ProcessGroupOnly) => Self::ProcessGroupOnly,
        }
    }

    /// `false` only for [`Self::ProcessGroupOnly`]: an empty capture under it
    /// says nothing about what descends from the root, while an empty capture
    /// under either other mechanism is evidence that nothing does.
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
        match read_proc_state(*pid) {
            Some(state) => {
                if state != PROC_ZOMBIE {
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

/// The state letter `/proc/<pid>/stat` records, or `None` when it is unreadable.
#[cfg(all(target_os = "linux", feature = "process"))]
fn read_proc_state(pid: i32) -> Option<char> {
    let path = std::path::Path::new("/proc")
        .join(pid.to_string())
        .join("stat");
    let stat = std::fs::read_to_string(path).ok()?;
    // `pid (comm) state …`: `comm` may itself contain spaces and parentheses, so
    // the state is the first field after the **last** `)`.
    let after_name = stat.get(stat.rfind(')')?.saturating_add(1)..)?;
    after_name.split_ascii_whitespace().next()?.chars().next()
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
    let spawned = std::process::Command::new("ps")
        .args(&arguments)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let output = match spawned {
        Ok(output) => output,
        Err(error) => {
            // The kind is kept so a caller can still tell a missing `ps` from a
            // refused one; the reading that failed is named in the message.
            let refusal = Err(io::Error::new(
                error.kind(),
                format!("lgwks_std::process ({caller}): `ps` could not be run: {error}"),
            ));
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "read_ps_table: returning an error to the caller");
            return refusal;
        }
    };
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
        let mut stopped = false;
        for _ in 0..4000 {
            if running_processes(&[first])?.is_empty() {
                stopped = true;
                break;
            }
            std::thread::park_timeout(std::time::Duration::from_millis(5));
        }
        assert!(
            stopped,
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
}
