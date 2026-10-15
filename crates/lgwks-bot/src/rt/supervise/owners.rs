//! The containment seams, and the registry for cleanups that outlive their task.

use super::lease::Lease;
use super::outcome::{CleanupReceipt, Containment};
use super::{CLEANUP_RECHECK, TaskId};
use crate::rt::sync::Notify;
use lgwks_std::process::ContainmentMechanism;
use std::fmt;
use std::io;
use std::sync::{Arc, Mutex};

/// One process-group signalling operation.
#[cfg(all(unix, feature = "process"))]
pub(super) trait GroupSignaller: Sync {
    /// Send SIGKILL to `group`.
    fn signal(&self, group: i32) -> io::Result<()>;
}

/// One non-mutating process-group existence observation.
#[cfg(all(unix, feature = "process"))]
pub(super) trait GroupObserver: Sync {
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
pub(super) struct Capture {
    /// How the pids were found.
    pub(super) mechanism: ContainmentMechanism,
    /// The pids the reading named, sorted and deduplicated.
    pub(super) pids: Vec<i32>,
    /// Whether a bound stopped the reading before the tree ended.
    pub(super) truncated: bool,
}

#[cfg(all(unix, feature = "process"))]
impl Capture {
    /// The finding the audited `lgwks_std::process` reading produced.
    pub(super) fn from_set(set: &lgwks_std::process::DescendantSet) -> Self {
        Self {
            mechanism: set.mechanism(),
            pids: set.pids().to_vec(),
            truncated: set.is_truncated(),
        }
    }

    /// Whether the reading named nothing.
    pub(super) fn is_empty(&self) -> bool {
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
pub(super) trait DescendantCapture: Sync {
    /// Every process descended from `root` right now.
    fn capture(&self, root: i32) -> io::Result<Capture>;
    /// Send SIGKILL to one captured pid.
    fn signal(&self, pid: i32) -> io::Result<()>;
    /// Which of `pids` are still running, as opposed to present-but-reaped.
    fn running(&self, pids: &[i32]) -> io::Result<Vec<i32>>;
}

/// A cleanup obligation retained after its original task ends.
#[cfg(all(unix, feature = "process"))]
pub(super) struct PendingCleanup {
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
pub(super) struct CleanupOwners {
    /// Owners retained under the in-flight semaphore ceiling.
    pub(super) pending: Mutex<Vec<PendingCleanup>>,
    /// Signalled on every registration, so a spawn waiting at the bound learns
    /// that a permit has moved to an owner that only a reap will release.
    pub(super) registered: Notify,
    /// Spawned process tasks that have not yet returned; see [`LiveProcess`].
    pub(super) live: std::sync::atomic::AtomicUsize,
}

/// One spawned process task that has not yet returned.
///
/// Moved into the task's future, so it is dropped when the task returns *or* is
/// aborted, and [`Supervisor::shutdown`] can tell a process still draining its
/// cleanup from a body that ignores its token.
#[cfg(all(unix, feature = "process"))]
pub(super) struct LiveProcess(Arc<CleanupOwners>);

#[cfg(all(unix, feature = "process"))]
impl LiveProcess {
    /// Count one more live process task against `owners`.
    pub(super) fn enter(owners: &Arc<CleanupOwners>) -> Self {
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
    pub(super) fn register(
        &self,
        task: TaskId,
        group: i32,
        lease: Lease,
        containment: Containment,
    ) {
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
    pub(super) fn pending_count(&self) -> usize {
        crate::journal::owner::lock(&self.pending).len()
    }

    /// Probe each owner once; present or unobservable groups keep their lease.
    pub(super) fn drive(
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
pub(super) async fn recheck(owners: &CleanupOwners) {
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
pub(super) struct NativeGroupSignaller;

#[cfg(all(unix, feature = "process"))]
impl GroupSignaller for NativeGroupSignaller {
    fn signal(&self, group: i32) -> io::Result<()> {
        lgwks_std::process::kill_process_group(group)
    }
}

#[cfg(all(unix, feature = "process"))]
/// The production signal-zero process-group observer.
pub(super) struct NativeGroupObserver;

#[cfg(all(unix, feature = "process"))]
impl GroupObserver for NativeGroupObserver {
    fn exists(&self, group: i32) -> io::Result<bool> {
        lgwks_std::process::process_group_exists(group)
    }
}

#[cfg(feature = "process")]
/// One static production adapter, so task futures hold no borrowed runtime state.
#[cfg(all(unix, feature = "process"))]
pub(super) static NATIVE_GROUP_SIGNALLER: NativeGroupSignaller = NativeGroupSignaller;

#[cfg(all(unix, feature = "process"))]
/// The static observer used by process-group cleanup.
pub(super) static NATIVE_GROUP_OBSERVER: NativeGroupObserver = NativeGroupObserver;

#[cfg(all(unix, feature = "process"))]
/// The production per-process containment adapter.
pub(super) struct NativeDescendantCapture;

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
pub(super) static NATIVE_DESCENDANT_CAPTURE: NativeDescendantCapture = NativeDescendantCapture;

/// The terminal receipt a retained cleanup earns when its group is observed
/// absent.
///
/// Not always `CleanupConfirmed`: an obligation transferred to the registry still
/// carries the containment report its task ended with, and a cleanup that left a
/// survivor has not reclaimed the tree however absent the group became. Naming
/// the mechanism here is what lets a consumer read one settled receipt and know
/// both.
#[cfg(all(unix, feature = "process"))]
pub(super) fn settled_receipt(containment: &Containment) -> CleanupReceipt {
    if containment.survivors().is_empty() {
        CleanupReceipt::CleanupConfirmed
    } else {
        CleanupReceipt::CleanupSurvivors {
            survivors: containment.survivors().to_vec(),
        }
    }
}
