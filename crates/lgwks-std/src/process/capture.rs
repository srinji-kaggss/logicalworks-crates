//! Descendant capture, the running check, and the process-table readings.
//!
//! A process group is the floor, not the tree. A descendant that calls `setsid`
//! has left the group by construction, so no group signal reaches it, and the
//! only way to stop one is to name its pid. These primitives are that naming:
//! read the process table to find every process descended from a root, signal
//! each of them by pid, and observe which of them are still present.
//!
//! Both halves are deliberately narrow. The capture is bounded by
//! [`MAX_CAPTURED_DESCENDANTS`] and reports itself truncated past it, because a
//! process table is a property of the host rather than of the caller's tree; and
//! it is a **snapshot taken while the root is alive**, because that is the only
//! moment a descendant still names it as a parent. A descendant orphaned before
//! the capture has been adopted by the platform's init and is no longer reachable
//! through the root — the caller is told which mechanism ran, and a caller that
//! needs that case closed needs a kernel-level owner (a cgroup, or a Windows job
//! object), neither of which this crate has.

#[cfg(unix)]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io;

#[cfg(all(not(unix), feature = "process"))]
use super::unsupported;
#[cfg(all(unix, feature = "process"))]
use super::{errno_to_io, invalid_pid, pid_from_raw};

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
    /// and as the fallback when that list cannot be read. On macOS it is one
    /// `sysctl(KERN_PROC)` syscall and on Linux one `/proc/<pid>/stat` read per
    /// process (#345); on any other Unix it costs one `ps` invocation. Either
    /// way it reads one row per process on the host, so it is
    /// the slower of the two readings and the one a high-concurrency cleanup pays
    /// most for.
    ProcessTableSnapshot,
    /// Linux: the tree's cgroup v2 scope, stopped by `cgroup.kill`.
    ///
    /// No process table was read for the members this stopped: the kernel owns
    /// the membership, so a `setsid` escapee attached to the scope dies with it
    /// and no fork race applies to the members it holds. The completeness claim
    /// rests on the scope reading empty afterwards (`CgroupScope::members`),
    /// not on a table snapshot.
    CgroupKill,
    /// Linux: descendants the subreaper flag re-parented to the supervisor.
    ///
    /// An orphan adopted by this process is found as its child rather than
    /// through a walk from a leader that may already have exited, and a pid
    /// that is both captured and adopted is provably still the same process.
    /// The completeness claim rests on that intersection
    /// (`adopted_descendants`), not on the group alone.
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
    pub(super) mechanism: ContainmentMechanism,
    /// The captured ids, sorted and deduplicated.
    pub(super) pids: Vec<i32>,
    /// Whether a bound stopped the walk before the tree ended.
    pub(super) truncated: bool,
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
/// The per-process counterpart of [`super::kill_process_group`], for a descendant that
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
/// [`super::child_has_exited_without_reaping`] for how the supervisor pins its leader.
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
/// Off Linux the states come from one whole-table snapshot — a `sysctl` on
/// macOS, a `ps` spawn elsewhere — so a signal-zero probe runs first and a set
/// whose every pid is already absent returns empty without reading anything. A
/// probe is not a state: a pid the probe finds present is still decided by the
/// table, and a pid that exits between the probe and the snapshot is decided the
/// way the table reads it. Absence needs no table: no live process can name a
/// pid nobody holds as its parent, and a zombie holds its id, so an absent pid
/// is not running however the table would have spelled it.
///
/// `pids` may be empty, in which case no table is read and the answer is empty:
/// asking about nothing must not cost a table read.
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
        // The probe before the table (#345): signal zero costs a syscall and
        // answers absence exactly. A pid nobody holds is not running — a
        // zombie would hold its id — so a set the probe proves wholly absent
        // needs no table. A pid the probe finds present is still decided by
        // the table, which is what tells a zombie from a process that runs.
        let mut present = Vec::with_capacity(pids.len());
        for pid in pids {
            if process_exists(*pid)? {
                present.push(*pid);
            }
        }
        if present.is_empty() {
            return Ok(running);
        }
        let zombies = read_table_zombies()?;
        for pid in present {
            match zombies.get(&pid).copied() {
                Some(true) => {}
                Some(false) => {
                    running.insert(pid);
                }
                None => {
                    if process_exists(pid)? {
                        running.insert(pid);
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
pub(super) fn proc_stat_says_running(stat: &str) -> bool {
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
    match std::fs::read_to_string(path) {
        Ok(status) => status_has_kill_pending(&status),
        Err(error) => {
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?error, pid, "an unreadable status claims no pending kill");
            #[cfg(not(feature = "trace"))]
            drop(error);
            false
        }
    }
}

/// Whether a `/proc/<pid>/status` text carries `SIGKILL` in a pending mask.
#[cfg(all(target_os = "linux", feature = "process"))]
pub(super) fn status_has_kill_pending(status: &str) -> bool {
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

/// Whether each process in the table is a zombie, in one table read (macOS).
///
/// One `sysctl(KERN_PROC_ALL)` syscall (#345): the kernel's own `p_stat`, with
/// no process spawn, where the `ps` reading cost a fork and an exec per call.
#[cfg(all(target_os = "macos", feature = "process"))]
fn read_table_zombies() -> io::Result<BTreeMap<i32, bool>> {
    Ok(super::darwin::table()?
        .into_iter()
        .map(|row| (row.pid, row.zombie))
        .collect())
}

/// Whether each process in the table is a zombie, from one `ps` read.
///
/// The reading for a Unix with neither a per-process state file nor a kernel
/// table this crate reads directly.
#[cfg(all(
    not(target_os = "linux"),
    not(target_os = "macos"),
    unix,
    feature = "process"
))]
fn read_table_zombies() -> io::Result<BTreeMap<i32, bool>> {
    let mut states = BTreeMap::new();
    for row in read_ps_table(&["pid=", "stat="], "read_table_zombies")? {
        let mut columns = row.split_ascii_whitespace();
        let (Some(pid), Some(stat)) = (columns.next(), columns.next()) else {
            continue;
        };
        let Ok(pid) = pid.parse::<i32>() else {
            continue;
        };
        states.insert(pid, stat.starts_with('Z'));
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
/// processes" when the truth is "the reading failed". macOS reads its table
/// through `sysctl` and Linux through `/proc` instead (#345).
#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "linux"),
    feature = "process"
))]
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
pub(super) fn run_ps(arguments: &[&str], caller: &'static str) -> io::Result<std::process::Output> {
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
pub(super) fn read_proc_children(pid: i32) -> std::io::Result<Vec<i32>> {
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

/// Every `(pid, ppid)` pair of the host's process table, in one read.
///
/// macOS: one `sysctl(KERN_PROC_ALL)` syscall (#345), with no process spawn.
#[cfg(all(target_os = "macos", feature = "process"))]
fn parent_pairs() -> io::Result<Vec<(i32, i32)>> {
    Ok(super::darwin::table()?
        .into_iter()
        .map(|row| (row.pid, row.ppid))
        .collect())
}

/// Every `(pid, ppid)` pair of the host's process table, read from `/proc`.
///
/// Linux: one `/proc/<pid>/stat` read per process, with no process spawn
/// (#345). This is the fallback the per-process child walk takes when the
/// root's own `/proc` entry is gone (it exited, or never existed); before, that
/// fallback spawned `ps`, which a host without `ps` refused. A pid that exits
/// between the listing and its read has no row, which is what it is.
#[cfg(all(target_os = "linux", feature = "process"))]
fn parent_pairs() -> io::Result<Vec<(i32, i32)>> {
    let mut pairs = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        if let Some(parent) = read_proc_stat(pid).as_deref().and_then(stat_parent) {
            pairs.push((pid, parent));
        }
    }
    Ok(pairs)
}

/// The parent pid a `/proc/<pid>/stat` line names: field 4, the second after
/// the command name, which may itself hold spaces and parentheses.
#[cfg(all(target_os = "linux", feature = "process"))]
pub(super) fn stat_parent(stat: &str) -> Option<i32> {
    stat.rfind(')')
        .and_then(|end| stat.get(end.saturating_add(1)..))
        .and_then(|fields| fields.split_ascii_whitespace().nth(1))
        .and_then(|parent| parent.parse::<i32>().ok())
}

/// Every `(pid, ppid)` pair of the host's process table, from one `ps` read.
///
/// The reading for a Unix that is neither Linux nor macOS.
#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "linux"),
    feature = "process"
))]
fn parent_pairs() -> io::Result<Vec<(i32, i32)>> {
    let mut pairs = Vec::new();
    for row in read_ps_table(&["pid=", "ppid="], "capture_table_snapshot")? {
        let mut columns = row.split_ascii_whitespace();
        let (Some(pid), Some(parent)) = (columns.next(), columns.next()) else {
            continue;
        };
        if let (Ok(pid), Ok(parent)) = (pid.parse::<i32>(), parent.parse::<i32>()) {
            pairs.push((pid, parent));
        }
    }
    Ok(pairs)
}

/// Capture the tree from one `pid`/`ppid` snapshot of the whole process table.
///
/// The portable reading, and the one every Unix without a per-process child list
/// uses. It reads one row per process on the host — one `sysctl` on macOS,
/// `/proc` on Linux, one `ps` invocation elsewhere — and it races more than the
/// per-process walk: a
/// process that forks while the snapshot is taken is a row the snapshot cannot
/// contain. The walk is therefore tried first wherever one exists, and this is
/// the fallback and the default everywhere else.
#[cfg(all(unix, feature = "process"))]
fn capture_table_snapshot(root: i32) -> io::Result<DescendantSet> {
    let mut children_of: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    for (pid, parent) in parent_pairs()? {
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
