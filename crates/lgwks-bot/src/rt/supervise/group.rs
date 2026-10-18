//! The process-group guard: capture, signal and observe one supervised tree.

use super::lease::Lease;
use super::outcome::{CleanupReceipt, Containment, ResidualRisk};
use super::owners::{
    Capture, CleanupOwners, DescendantCapture, GroupObserver, GroupSignaller,
    NATIVE_DESCENDANT_CAPTURE, NATIVE_GROUP_OBSERVER, NATIVE_GROUP_SIGNALLER,
};
use super::start::REAP_POLL;
#[cfg(target_os = "linux")]
use super::start::{reap_adopted, sweep_adopted, sweep_adopted_confirmed};
use super::{
    CONTAINMENT_ROUNDS, DEATH_CONFIRM_PASSES, GROUP_SIGNAL_ATTEMPTS, GroupSignal, TaskId,
    is_absence, is_present_but_unsignalable,
};
use crate::rt::task::yield_now;
#[cfg(target_os = "linux")]
use lgwks_std::process::{CgroupScope, ContainmentMechanism};
use std::sync::Arc;

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
pub(super) struct ProcessGroup<'ops> {
    /// The group id, or `0` when the engine could not report the child's id.
    ///
    /// Zero is *refused* by `kill_process_group` rather than read as "this
    /// process's own group", so an unreadable id can never signal the
    /// supervisor's own process group.
    pub(super) group: i32,
    /// Attribution for a later cleanup receipt.
    pub(super) task: TaskId,
    /// The process admission lease, transferred to `owners` if cleanup remains.
    ///
    /// A [`Lease`] rather than a bare permit, so a cleanup obligation that
    /// outlives its task returns the capacity to the tenant it was charged to,
    /// not merely to a pool nobody is waiting on.
    pub(super) permit: Option<Lease>,
    /// Supervisor-owned destination for unresolved cleanup.
    pub(super) owners: Arc<CleanupOwners>,
    /// Whether a kill is still owed to the group.
    pub(super) armed: bool,
    /// Whether the leader has been reaped and the numeric id released.
    pub(super) leader_reaped: bool,
    /// Whether the leader was observed to have exited before the cleanup began,
    /// after which a walk from it can name nothing.
    pub(super) leader_exited: bool,
    /// Whether the group was observed absent: by a signal refusal, or by the
    /// post-reap signal-zero probe.
    pub(super) group_absent: bool,
    /// What the cleanup read, signalled, and could not account for.
    pub(super) containment: Containment,
    /// Captured pids a signal has already been delivered to.
    ///
    /// The set is what keeps "signal every captured pid" from becoming "signal
    /// every captured pid once per round": a pid is signalled the first round it
    /// is captured in, and a pid that is still running afterwards is signalled
    /// again **only** because the observation proved it alive, which is the same
    /// rule the group follows for its own id.
    pub(super) signalled_pids: std::collections::BTreeSet<i32>,
    /// The tree's cgroup v2 scope, when the mount allowed one.
    ///
    /// Killed first on every cleanup path, observed empty to earn the receipt,
    /// and removed when the guard drops. `None` where no child cgroup was
    /// creatable, where the capture rounds own the tree alone.
    #[cfg(all(target_os = "linux", feature = "process"))]
    pub(super) cgroup: Option<CgroupScope>,
    /// The process-group signalling boundary, injectable for regression tests.
    pub(super) signaller: &'ops dyn GroupSignaller,
    /// Non-mutating group existence boundary, injectable for regression tests.
    pub(super) observer: &'ops dyn GroupObserver,
    /// The per-process containment boundary, injectable for regression tests.
    pub(super) capture: &'ops dyn DescendantCapture,
}

#[cfg(all(unix, feature = "process"))]
impl<'ops> ProcessGroup<'ops> {
    /// The guard over process group `group`, armed.
    ///
    /// `group` is the id [`start`] validated for the child it just started.
    pub(super) fn of(
        group: i32,
        task: TaskId,
        lease: Lease,
        owners: Arc<CleanupOwners>,
    ) -> ProcessGroup<'static> {
        ProcessGroup {
            group,
            task,
            permit: Some(lease),
            owners,
            armed: true,
            leader_reaped: false,
            leader_exited: false,
            group_absent: false,
            containment: Containment::default(),
            signalled_pids: std::collections::BTreeSet::new(),
            #[cfg(all(target_os = "linux", feature = "process"))]
            cgroup: None,
            signaller: &NATIVE_GROUP_SIGNALLER,
            observer: &NATIVE_GROUP_OBSERVER,
            capture: &NATIVE_DESCENDANT_CAPTURE,
        }
    }

    /// The guard over process group `group`, armed, owning `scope`.
    ///
    /// The scoped form of [`Self::of`]: the same guard, plus the cgroup the
    /// spawn attached the leader to. Every cleanup path kills the scope first
    /// and observes it empty to earn the receipt.
    #[cfg(all(target_os = "linux", feature = "process"))]
    pub(super) fn of_scoped(
        group: i32,
        task: TaskId,
        lease: Lease,
        owners: Arc<CleanupOwners>,
        scope: Option<CgroupScope>,
    ) -> ProcessGroup<'static> {
        let mut guard = Self::of(group, task, lease, owners);
        guard.cgroup = scope;
        guard
    }

    /// Capture the tree, signal the group and every captured pid, and observe
    /// until nothing captured is still running or the bounded drain is exhausted.
    ///
    /// Four phases per round, and the order is the whole design:
    ///
    /// 1. **Capture**, once per round and before any signal of that round. Before
    ///    the *first* signal the leader is still alive and its descendants still
    ///    name it as their parent; after the leader exits they are adopted
    ///    elsewhere and no walk from the leader reaches them. Within the drain,
    ///    re-capturing each round is what catches a fork that completed while an
    ///    earlier round's signal was in flight.
    /// 2. **Signal the group.** The floor, and the only thing that reaches a
    ///    member forked after the round's capture.
    /// 3. **Signal every captured pid.** A `setsid` descendant is in no group this
    ///    supervisor owns, so a pid signal is the only thing that reaches it.
    /// 4. **Observe** which captured pids are still *running**, as opposed to
    ///    present and unreaped.
    ///
    /// The rounds replace the 64-yield retry the group alone needed. Each round
    /// proves more than the last — it admits the forks the previous round's window
    /// let through, and re-signals anything the previous round's observation
    /// proved alive — so the bound is on *rounds of evidence* rather than on how
    /// long a scheduler happened to take. A delivered signal is still not proof of
    /// cleanup: only the observations settle it, and the group's own absence is
    /// settled after the reap by [`Self::confirm_absence`], because an unreaped
    /// zombie leader keeps its group present by construction.
    pub(super) async fn cleanup(&mut self) -> CleanupReceipt {
        if self.leader_reaped {
            return CleanupReceipt::CleanupFailed;
        }
        if self.group <= 0 {
            return CleanupReceipt::CleanupFailed;
        }
        // The kernel owner first: one `cgroup.kill` write stops every member
        // the scope holds — including a `setsid` escapee — atomically, with no
        // fork race for them. What it could not hold (a descendant forked
        // before the attach) the capture rounds below still cover, and the
        // receipt names which owner ran.
        #[cfg(all(target_os = "linux", feature = "process"))]
        self.kill_scope();
        for round in 0..CONTAINMENT_ROUNDS {
            let tree = self.read_tree();
            if matches!(self.signal_group(), GroupSignal::Refused) {
                return CleanupReceipt::CleanupFailed;
            }
            if let Some(ref captured) = tree {
                self.signal_captured(captured);
                self.observe_captured(captured);
            }
            if self.no_survivors() {
                break;
            }
            if round.saturating_add(1) < CONTAINMENT_ROUNDS {
                yield_now().await;
            }
        }
        self.receipt()
    }

    /// Send SIGKILL to the scope's members through `cgroup.kill`.
    ///
    /// Best effort and first: a refused write leaves the scope untouched and
    /// the capture rounds own the tree, while a delivered one is what the
    /// receipt names. Never a failure — containment degrades, it does not
    /// refuse.
    #[cfg(all(target_os = "linux", feature = "process"))]
    pub(super) fn kill_scope(&mut self) {
        let Some(scope) = self.cgroup.as_ref() else {
            return;
        };
        match scope.kill() {
            Ok(()) => self
                .containment
                .record_owner(ContainmentMechanism::CgroupKill),
            Err(error) => {
                lgwks_std::trace::debug!(%error, task = ?self.task, "cleanup: the scope kill was refused; the capture rounds own this tree");
            }
        }
    }

    /// Read the leader's descendants now, folding them into this report.
    ///
    /// `None` when no process table could be read at all, which is a **fact about
    /// the mechanism** rather than an empty tree: it records
    /// [`ResidualRisk::TableUnreadable`], leaves the group signal as the only
    /// thing this cleanup can rely on, and stops the per-process phases of the
    /// round rather than reporting "every descendant was".
    ///
    /// `None` without a read once the leader is known to have exited: its
    /// children were re-parented at the exit, so the walk could only come back
    /// empty, and on a host whose reading is a whole-table `ps` it would cost a
    /// process spawn per supervised process to learn nothing (#345, #347).
    pub(super) fn read_tree(&mut self) -> Option<Capture> {
        if self.leader_exited {
            return None;
        }
        match self.capture.capture(self.group) {
            Ok(set) => {
                self.containment.record_capture(&set);
                Some(set)
            }
            Err(_) => {
                self.containment.note(ResidualRisk::TableUnreadable);
                None
            }
        }
    }

    /// Send SIGKILL to the group and report what the OS answered.
    pub(super) fn signal_group(&mut self) -> GroupSignal {
        match self.signaller.signal(self.group) {
            Ok(()) => GroupSignal::Delivered,
            Err(error) if is_absence(&error) => {
                self.group_absent = true;
                GroupSignal::Absent
            }
            // `EPERM` after a delivered SIGKILL is not a termination refusal: on
            // macOS/BSD a process group whose leader is an unreaped zombie
            // (exactly the state the first signal produced) reports `EPERM` for a
            // further `killpg` while the group — the zombie leader included — is
            // still present. It is the same fact `exists` reports as `Ok(true)`:
            // the group is still there. Reporting `Failed` would claim the OS
            // refused a kill it already delivered, and would report a deadline
            // kill as a cleanup failure (INV-BOT-19). The absence is settled by
            // the post-reap signal-zero probe.
            Err(error) if is_present_but_unsignalable(&error) => GroupSignal::Present,
            Err(_) => GroupSignal::Refused,
        }
    }

    /// SIGKILL every captured pid this cleanup has not signalled, or that the last
    /// observation proved alive.
    ///
    /// The "or proved alive" is the T21 fence. A pid captured in an earlier round
    /// and already signalled is not signalled again on its own: its process may
    /// have died and its id been reissued, and signalling a number is signalling
    /// whoever holds it now. A pid the last observation reported **running** is
    /// that same process, still holding its id, so the repeat is safe — and is the
    /// only thing that reaches a descendant that ignored the first.
    pub(super) fn signal_captured(&mut self, captured: &Capture) {
        let observed = self.containment.survivors.clone();
        for pid in &captured.pids {
            let fresh = !self.signalled_pids.contains(pid);
            if !fresh && !observed.contains(pid) {
                continue;
            }
            match self.capture.signal(*pid) {
                Ok(()) => {
                    self.signalled_pids.insert(*pid);
                    self.containment.record_signal();
                }
                // The process has ended: `ESRCH` is its answer rather than a
                // failure, and the pid is not signalled again.
                Err(error) if is_absence(&error) => {
                    self.signalled_pids.remove(pid);
                }
                // Anything else — including a refusal to signal somebody else's
                // process — leaves the pid unsignalled, and the round's own
                // observation then reports it: an unexplainable survivor belongs
                // in the receipt, not in a swallowed error.
                Err(_) => {}
            }
        }
    }

    /// Replace the survivor list with what the table says is still running.
    ///
    /// Signal-free, so it is safe to repeat and safe against a pid this supervisor
    /// no longer owns: the question is only ever asked about pids this cleanup
    /// captured. An unreadable observation claims nothing and reports every
    /// captured pid as running, the direction that understates the cleanup rather
    /// than overstating it.
    pub(super) fn observe_captured(&mut self, captured: &Capture) {
        if captured.is_empty() {
            self.containment.record_survivors(&[]);
            return;
        }
        self.observe_running(&captured.pids);
    }

    /// Replace the survivor list with which of `pids` are still running.
    ///
    /// The one observation both phases make: [`Self::observe_captured`] asks it
    /// of a fresh capture, [`Self::confirm_absence`] of the pids an earlier
    /// observation proved running. An unreadable table records every pid asked
    /// about as running, the answer that understates the cleanup.
    pub(super) fn observe_running(&mut self, pids: &[i32]) {
        match self.capture.running(pids) {
            Ok(running) => self.containment.record_survivors(&running),
            Err(_) => self.containment.record_survivors(pids),
        }
    }

    /// Whether no captured pid was still running at the last observation.
    pub(super) fn no_survivors(&self) -> bool {
        self.containment.survivors.is_empty()
    }

    /// The receipt this cleanup has earned from what its rounds observed.
    ///
    /// One decision in one place, because the arms are three facts. A survivor
    /// outranks the group's own state — a `setsid` descendant is still a process
    /// this supervisor started, however absent its group became — and the group is
    /// reported absent only once something actually observed it absent.
    pub(super) fn receipt(&mut self) -> CleanupReceipt {
        if !self.containment.survivors.is_empty() {
            return CleanupReceipt::CleanupSurvivors {
                survivors: self.containment.survivors.clone(),
            };
        }
        if self.group_absent {
            self.disarm();
            return CleanupReceipt::CleanupConfirmed;
        }
        CleanupReceipt::CleanupPending
    }

    /// Confirm absence after `child.wait()` has released the leader's pid.
    ///
    /// Both halves are **observation**: the probe sends no signal, so a reused
    /// identifier can never cause this supervisor to affect an unrelated group,
    /// and a still-present group consumes the same bounded budget and stays
    /// `Pending` rather than being promoted to proof of absence (INV-BOT-12,
    /// INV-BOT-13). The descendant half is observed for the same reason and
    /// because nothing more can be done about it here: the leader is reaped, so
    /// signalling any captured pid now could reach a reissued id.
    ///
    /// It observes the survivors the pinned phase captured and **does not capture
    /// again**. The leader's id is released by the reap, so a walk from it names
    /// whoever holds that number now — another process's children, attributed to
    /// this task — and the descendants it did have were re-parented away when it
    /// exited, so the walk could not find them either. It was also the dear half:
    /// a reaped root has no `/proc/<pid>/task`, so the Linux reading fell back to
    /// a whole-table `ps` spawn on every process a supervisor ran, which tripled
    /// `sim_review_path`'s 10,000-run tier on CI (62 s on main, 181 s with the
    /// re-capture). With no survivor the observation reads no table at all.
    ///
    /// The reap is what makes this pass able to succeed at all — an unreaped
    /// zombie leader keeps its group present, which is why the pinned phase could
    /// only ever report `Pending`.
    pub(super) async fn confirm_absence(&mut self) -> CleanupReceipt {
        if !self.leader_reaped || self.group <= 0 {
            return CleanupReceipt::CleanupFailed;
        }
        // Adopted orphans first: a descendant whose parent died before the
        // cleanup was re-parented to this process by the subreaper flag, so it
        // is killed and reaped here rather than left for init. Only pids the
        // capture named while the leader lived are touched — an unreaped child
        // keeps its pid unreissued, so each one is provably still the process
        // this tree started.
        #[cfg(all(target_os = "linux", feature = "process"))]
        self.adopt_orphans().await;
        // Stranded adoptees of earlier trees — orphans adopted after their own
        // guard's pass — are collected here rather than left as rows `kill -0`
        // still reports present. Confirming, not single-pass: a stranded
        // adoptee still dying on the kernel's schedule when the sweep first
        // looks would otherwise survive into the observation below as a false
        // survivor, so the sweep waits out the deaths within the same bound
        // the adoption reap uses.
        #[cfg(all(target_os = "linux", feature = "process"))]
        let _collected = sweep_adopted_confirmed().await;
        // The scope's own confirmation before the rounds: an empty member
        // list is the kernel's evidence that nothing of the tree is left, and
        // a non-empty one joins the survivors the rounds observe, so an early
        // return below never claims a tree whose scope still holds members.
        #[cfg(all(target_os = "linux", feature = "process"))]
        self.confirm_scope();
        for round in 0..CONTAINMENT_ROUNDS {
            if matches!(self.observer.exists(self.group), Ok(false)) {
                self.group_absent = true;
            }
            if !self.no_survivors() {
                let survivors = self.containment.survivors.clone();
                self.observe_running(&survivors);
            }
            if self.group_absent && self.no_survivors() {
                return self.receipt();
            }
            if round.saturating_add(1) < CONTAINMENT_ROUNDS {
                yield_now().await;
            }
        }
        self.await_deaths().await;
        self.receipt()
    }

    /// Wait out the death of a pid the rounds still read as running.
    ///
    /// The rounds yield and re-read, which costs microseconds, while a SIGKILLed
    /// process leaves the table on the scheduler's turn: on a loaded host the
    /// receipt named a survivor that was gone a few milliseconds later. So while
    /// a survivor is recorded, this parks on the runtime clock for [`REAP_POLL`]
    /// and observes the same pids again, at most [`DEATH_CONFIRM_PASSES`] times.
    /// It sends no signal, because the leader is reaped and a pid signalled now
    /// could belong to someone else, and it costs nothing when nothing is running.
    /// A pid that outlasts the bound stays named, so a descendant that never dies
    /// is still reported; once the survivors clear it also asks whether the group
    /// has gone, because the rounds that would have asked are spent.
    async fn await_deaths(&mut self) {
        if self.no_survivors() {
            return;
        }
        for _ in 0..DEATH_CONFIRM_PASSES {
            crate::rt::time::sleep(REAP_POLL).await;
            let survivors = self.containment.survivors.clone();
            self.observe_running(&survivors);
            if self.no_survivors() {
                break;
            }
        }
        if self.no_survivors()
            && !self.group_absent
            && matches!(self.observer.exists(self.group), Ok(false))
        {
            self.group_absent = true;
        }
    }

    /// Kill and reap the captured descendants this process adopted.
    ///
    /// One signal pass, bounded by the signalled set: orphans adopted after this
    /// call are not in it, and the receipt's rounds — not another pass — are
    /// what bound the work. A pid adopted here was captured while the leader
    /// lived and is an unreaped child now, so the signal cannot reach a
    /// stranger and the reap cannot steal a live leader's exit.
    ///
    /// The reap confirms rather than fires once: a signalled process dies on
    /// the kernel's schedule, not this task's, and under load the death lands
    /// after a single nonblocking reap attempted it — leaving a zombie no
    /// later pass collects while the receipt already claims the tree.
    /// [`reap_adopted`] waits for the deaths it caused, within
    /// [`ADOPTION_REAP_PASSES`], and this then observes only what is still
    /// genuinely alive; a pid that never dies is a survivor, not a missed reap.
    #[cfg(all(target_os = "linux", feature = "process"))]
    pub(super) async fn adopt_orphans(&mut self) {
        let adopted = self.signal_adopted();
        if adopted.is_empty() {
            return;
        }
        let settled = reap_adopted(adopted).await;
        if !settled.unreaped.is_empty() {
            lgwks_std::trace::debug!(task = ?self.task, passes = settled.passes, unreaped = settled.unreaped.len(), "confirm: adoptees outlived the death bound; observing them");
            self.observe_running(&settled.unreaped);
        }
    }

    /// Signal the adopted orphans without observing them.
    ///
    /// The drop-time half of [`Self::adopt_orphans`]: no observation follows,
    /// because a dropped guard has no report to carry one, so every adopted
    /// pid is signalled once and reaped once, best effort. The same
    /// attribution rule applies — captured while the leader lived, an unreaped
    /// child now — so no stranger is reachable.
    #[cfg(all(target_os = "linux", feature = "process"))]
    pub(super) fn kill_adopted(&mut self) {
        let adopted = self.signal_adopted();
        if adopted.is_empty() {
            return;
        }
        let _reaped = lgwks_std::process::reap_descendants(&adopted);
    }

    /// Signal the adopted descendants and name the owner that stopped them.
    ///
    /// The one signal these two paths share: which signalled pids this process
    /// adopted, each signalled once, and the subreaper adoption recorded as
    /// the mechanism. Returns the adopted pids for the caller to reap or
    /// observe; empty when nothing was adopted, so both callers stop there.
    #[cfg(all(target_os = "linux", feature = "process"))]
    pub(super) fn signal_adopted(&mut self) -> Vec<i32> {
        let adopted = self.adopted_signalled();
        if adopted.is_empty() {
            return adopted;
        }
        self.containment
            .record_owner(ContainmentMechanism::SubreaperAdoption);
        for pid in &adopted {
            if self.capture.signal(*pid).is_ok() {
                self.containment.record_signal();
            }
        }
        adopted
    }

    /// Which signalled pids are currently children of this process.
    ///
    /// The one attribution query both adoption paths share: a pid that is
    /// both captured and adopted is provably still the same process, because
    /// the OS cannot reissue the pid of a child nobody reaped. Unreadable is
    /// empty rather than an error, because both callers degrade to the rounds
    /// rather than refusing.
    #[cfg(all(target_os = "linux", feature = "process"))]
    pub(super) fn adopted_signalled(&self) -> Vec<i32> {
        if self.signalled_pids.is_empty() {
            return Vec::new();
        }
        let candidates: Vec<i32> = self.signalled_pids.iter().copied().collect();
        match lgwks_std::process::adopted_descendants(&candidates) {
            Ok(adopted) => adopted.into_iter().collect(),
            Err(error) => {
                lgwks_std::trace::debug!(%error, task = ?self.task, "cleanup: the adoption check was unreadable; the rounds own these pids");
                Vec::new()
            }
        }
    }

    /// Fold the scope's member list into the survivor report.
    ///
    /// Read before the absence rounds, so they observe what the kernel still
    /// holds: an empty scope confirms what the rounds are about to check, and
    /// a non-empty one is unioned with the survivors rather than replacing
    /// them, because the two readers — the kernel's membership and the table
    /// observation — answer different questions and neither absorbs the other.
    /// An unreadable scope leaves the rounds' answer standing.
    #[cfg(all(target_os = "linux", feature = "process"))]
    pub(super) fn confirm_scope(&mut self) {
        let Some(scope) = self.cgroup.as_ref() else {
            return;
        };
        let members = match scope.members() {
            Ok(members) => members,
            Err(error) => {
                lgwks_std::trace::debug!(%error, task = ?self.task, "confirm: the scope member list was unreadable; the rounds own this tree");
                return;
            }
        };
        if members.is_empty() {
            return;
        }
        let mut survivors: std::collections::BTreeSet<i32> =
            self.containment.survivors.iter().copied().collect();
        survivors.extend(members);
        let survivors: Vec<i32> = survivors.into_iter().collect();
        self.containment.record_survivors(&survivors);
    }

    /// Signal the whole group and every captured pid as a drop-time fallback.
    ///
    /// A dropped future cannot await, so this is one synchronous pass rather than
    /// the drain above: capture once, name each captured pid once, then repeat the
    /// group signal a bounded number of times while the unreaped leader still pins
    /// the id. The repeats exist for the one thing a capture cannot reach — a
    /// member the leader forks after the capture and before it dies — and they are
    /// bounded rather than open because the leader's death is the event that ends
    /// the window.
    ///
    /// The capture is a blocking table read on this path: on a host whose reading
    /// is a `ps` invocation it costs one process spawn, which is a price worth
    /// paying on a terminal path that has just abandoned its task and a price the
    /// drain above would otherwise pay repeatedly. It is deliberately **not**
    /// routed through a blocking pool: a refused pool admission would silently
    /// degrade containment to the group alone at exactly the concurrency where
    /// containment matters most.
    pub(super) fn kill(&mut self) {
        if self.leader_reaped || self.group <= 0 {
            return;
        }
        if let Some(ref captured) = self.read_tree() {
            for pid in &captured.pids {
                if self.capture.signal(*pid).is_ok() {
                    self.signalled_pids.insert(*pid);
                    self.containment.record_signal();
                }
            }
        }
        for _ in 0..GROUP_SIGNAL_ATTEMPTS {
            match self.signal_group() {
                // Absent and unsignalable are both "still there or already gone,
                // and neither is a reason to stop trying"; `Absent` ends it.
                GroupSignal::Absent => break,
                GroupSignal::Delivered | GroupSignal::Present => {
                    std::thread::yield_now();
                }
                GroupSignal::Refused => break,
            }
        }
        // The kernel owners, best effort and synchronous like the rest of this
        // path: the scope kill stops the members the capture above could not
        // name, and adopted orphans are signalled by pid. Neither allocates a
        // task or awaits, so neither can be refused by a runtime that is
        // already unwinding.
        #[cfg(all(target_os = "linux", feature = "process"))]
        self.kill_scope();
        #[cfg(all(target_os = "linux", feature = "process"))]
        self.kill_adopted();
        // Stranded adoptees, best effort and synchronous like the rest of this
        // path: a dropped guard cannot await, but it can wait on what is
        // already dead.
        #[cfg(all(target_os = "linux", feature = "process"))]
        let _collected = sweep_adopted();
    }

    /// Signal the group once more while a captured pipe is still open, if that
    /// is still safe and still useful.
    ///
    /// Safe only while the unreaped leader pins the id, which is why the driver
    /// calls this before the reap and why a reaped guard sends nothing. Useless
    /// once the group was observed absent or a signal was refused: neither a
    /// repeat nor a wait changes those answers. Returns whether a signal was
    /// sent, so the drain stops asking once the answer is no.
    pub(super) fn resignal_while_pinned(&mut self) -> bool {
        if self.leader_reaped || self.group <= 0 || self.group_absent {
            return false;
        }
        matches!(
            self.signal_group(),
            GroupSignal::Delivered | GroupSignal::Present
        )
    }

    /// Record that a captured pipe outlived the drain after cleanup.
    pub(super) fn mark_descriptor_held(&mut self) {
        self.containment.record_held_descriptor();
    }

    /// What this cleanup read, signalled, and could not account for.
    ///
    /// Read by the driver to carry the report onto the outcome, and by [`Drop`] to
    /// hand it to the retained-cleanup registry: an obligation that outlives its
    /// task has to carry the same report its task ended with, or the settled
    /// receipt would claim a mechanism nobody ran.
    pub(super) fn containment(&self) -> &Containment {
        &self.containment
    }

    /// Mark the group as already gone, so [`Drop`] does not signal it.
    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }

    /// Record that reaping released the group leader's numeric id.
    pub(super) fn mark_reaped(&mut self) {
        self.leader_reaped = true;
    }

    /// Record that the leader exited on its own before the cleanup began.
    ///
    /// Every later capture is skipped and the report carries
    /// [`ResidualRisk::LeaderExited`], so a cleanup of a process that finished
    /// by itself claims the group it signalled and not a tree it never saw.
    pub(super) fn mark_exited(&mut self) {
        self.leader_exited = true;
        self.containment.note(ResidualRisk::LeaderExited);
    }
}

#[cfg(all(unix, feature = "process"))]
impl Drop for ProcessGroup<'_> {
    /// Kill the group if a kill is still owed to it.
    ///
    /// This is the last line of the guarantee, and it is deliberately
    /// synchronous: a task aborted before it reached its own kill still takes
    /// its process group with it as its frame unwinds.
    fn drop(&mut self) {
        if self.armed && self.group > 0 {
            if !self.leader_reaped {
                self.kill();
            }
            if let Some(permit) = self.permit.take() {
                self.owners
                    .register(self.task, self.group, permit, self.containment.clone());
            }
        }
        // The cgroup scope, if any, drops with the guard: its members were
        // killed by the paths above, and the retained obligation the permit
        // transfers to probes the group, never the directory.
    }
}
