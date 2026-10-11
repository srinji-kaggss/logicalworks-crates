//! The process-group drain: every receipt arm against scripted seams.

#[cfg(all(unix, feature = "process"))]
use super::super::CONTAINMENT_ROUNDS;
use super::super::TaskId;
use super::super::lease::Lease;
#[cfg(feature = "process")]
use super::super::outcome::ResidualRisk;
use super::super::outcome::{CleanupReceipt, TaskOutcome};
use super::super::supervisor::Supervisor;
#[cfg(all(unix, feature = "process"))]
use super::{
    EpermGroupSignaller, FailingGroupObserver, ScriptedCapture, UnexpectedGroupSignaller,
    armed_group, capture_group, group_seams, recording, sequence, settled_tasks, test_group,
    whole_capture,
};
use crate::rt::runtime::block_on;
#[cfg(feature = "process")]
use lgwks_std::process::ContainmentMechanism;
use std::sync::Arc;
use std::sync::atomic::Ordering;

#[cfg(all(unix, feature = "process"))]
#[test]
fn a_reaped_or_recycled_group_id_is_never_signalled() {
    let signaller = recording();
    let observer = sequence(0);
    let mut group = test_group(&signaller, &observer, true);
    assert_eq!(
        block_on(group.cleanup()),
        CleanupReceipt::CleanupFailed,
        "the stale identity is refused rather than signalled"
    );
    drop(group);
    assert_eq!(
        signaller.calls.load(Ordering::Relaxed),
        0,
        "cleanup and Drop must not signal after the owner has reaped the leader"
    );
}

#[cfg(all(unix, feature = "process"))]
#[test]
fn an_unsignalable_present_group_stays_pending_rather_than_failed() {
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

/// A group whose drain reports a named survivor, and the receipt that says so.
#[cfg(all(unix, feature = "process"))]
#[test]
fn a_descendant_that_keeps_running_is_named_in_the_receipt() {
    let signaller = recording();
    let observer = sequence(usize::MAX);
    let capture = ScriptedCapture {
        tree: vec![70, 71],
        immortal: vec![71],
        ..ScriptedCapture::default()
    };
    let mut group = capture_group(&signaller, &observer, &capture);
    assert_eq!(
        block_on(group.cleanup()),
        CleanupReceipt::CleanupSurvivors {
            survivors: vec![71],
        },
        "a captured descendant that ignored every signal must be named, not absorbed \
             into a pending group"
    );
    let report = group.containment();
    assert_eq!(
        report.captured(),
        2,
        "the report must account for both captured pids"
    );
    assert_eq!(
        report.signalled(),
        1 + CONTAINMENT_ROUNDS,
        "one signal for the descendant that stopped and one per round for the one that \
             did not: a repeat is only ever sent while an observation proves the pid alive"
    );
    assert_eq!(
        report.survivors(),
        [71],
        "the survivor list is the running pid and nothing else"
    );
    assert_eq!(
        report.mechanism(),
        ContainmentMechanism::ProcessTableSnapshot,
        "the mechanism that read the table is the one reported"
    );
    assert_eq!(
        report.residual_risk(),
        None,
        "a whole reading with no survivor carries no residual risk"
    );
    assert!(
        !report.is_complete(),
        "a named survivor is by definition an incomplete containment"
    );
    // Nothing more can be done about it after the reap, so the pass observes
    // rather than signals and keeps the receipt it earned. It observes the
    // survivor by pid and never walks the tree again: a walk from a reaped
    // leader names whoever holds its id now.
    let pinned_reads = capture.reads();
    group.mark_reaped();
    assert_eq!(
        block_on(group.confirm_absence()),
        CleanupReceipt::CleanupSurvivors {
            survivors: vec![71],
        },
        "a survivor is never promoted to a clean cleanup by a later observation"
    );
    assert_eq!(
        capture.reads(),
        pinned_reads,
        "the post-reap pass observes the captured survivor and reads no tree"
    );
    assert_eq!(
        capture.signals(),
        1 + CONTAINMENT_ROUNDS,
        "the survivor is signalled once in the round that captured it and once per later \
             round, and only because an observation proves it alive; the descendant that \
             stopped is signalled exactly once"
    );
    assert!(
        group.armed,
        "an unresolved cleanup stays armed for its registry transfer"
    );
    drop(group);
}

/// A captured descendant that had already ended is not a survivor.
#[cfg(all(unix, feature = "process"))]
#[test]
fn a_captured_pid_that_had_ended_is_not_reported_as_a_survivor() {
    let signaller = recording();
    let observer = sequence(0);
    let capture = ScriptedCapture {
        tree: vec![70],
        gone: vec![70],
        ..ScriptedCapture::default()
    };
    let mut group = capture_group(&signaller, &observer, &capture);
    assert_eq!(
        block_on(group.cleanup()),
        CleanupReceipt::CleanupPending,
        "the group is still pinned by its unreaped leader, so the pinned phase is pending"
    );
    let pinned_reads = capture.reads();
    group.mark_reaped();
    assert_eq!(
        block_on(group.confirm_absence()),
        CleanupReceipt::CleanupConfirmed,
        "an absent group and no running descendant is a complete cleanup"
    );
    assert_eq!(
        capture.reads(),
        pinned_reads,
        "a clean post-reap pass reads no tree: on Linux a walk from a reaped leader \
             was a whole-table `ps` spawn per process"
    );
    assert!(
        group.containment().is_complete(),
        "the report must name the mechanism it ran and carry no survivor"
    );
    drop(group);
}

/// A leader that exited on its own has had its children re-parented, so the
/// cleanup reads no table, still signals the group, and names why its report
/// is not the tree (#347).
#[cfg(all(unix, feature = "process"))]
#[test]
fn a_leader_that_exited_on_its_own_reads_no_table_and_claims_no_tree() {
    let signaller = recording();
    let observer = sequence(0);
    let capture = ScriptedCapture {
        tree: vec![70],
        ..ScriptedCapture::default()
    };
    let mut group = capture_group(&signaller, &observer, &capture);
    group.mark_exited();
    assert_eq!(
        block_on(group.cleanup()),
        CleanupReceipt::CleanupPending,
        "the exited leader is unreaped, so its group is still pinned and pending"
    );
    assert_eq!(
        capture.reads(),
        0,
        "a walk from an exited leader can name nothing, so no table is read"
    );
    assert_eq!(
        capture.signals(),
        0,
        "nothing was named, so no pid is signalled"
    );
    assert!(
        signaller.calls.load(Ordering::Relaxed) >= 1,
        "the group is still signalled: a member that stayed in it is still reachable"
    );
    assert_eq!(
        group.containment().residual_risk(),
        Some(ResidualRisk::LeaderExited),
        "the report names the limit rather than presenting an empty capture as an empty tree"
    );
    assert_eq!(
        group.containment().mechanism(),
        ContainmentMechanism::ProcessGroupOnly,
        "no table was read"
    );
    group.mark_reaped();
    assert_eq!(
        block_on(group.confirm_absence()),
        CleanupReceipt::CleanupConfirmed,
        "the group itself is observed gone after the reap"
    );
    assert!(
        !group.containment().is_complete(),
        "a confirmed group is not a confirmed tree when the tree was never seen"
    );
    drop(group);
}

/// A pid is signalled once per drain, not once per observation of it.
#[cfg(all(unix, feature = "process"))]
#[test]
fn a_captured_pid_is_signalled_once_and_not_once_per_observation() {
    let signaller = recording();
    let observer = sequence(usize::MAX);
    let capture = ScriptedCapture::of(&[70, 71, 72]);
    let mut group = capture_group(&signaller, &observer, &capture);
    assert_eq!(
        block_on(group.cleanup()),
        CleanupReceipt::CleanupPending,
        "nothing is running, so the pinned phase is pending on the group alone"
    );
    assert_eq!(
        capture.signals(),
        3,
        "three captured pids, three signals: a repeat would be a signal to an id whose \
             process has since ended"
    );
    assert_eq!(
        capture.reads(),
        1,
        "the drain stops on the first round once nothing captured is running"
    );
    drop(group);
}

/// No process table at all is a stated limit, not an empty tree.
#[cfg(all(unix, feature = "process"))]
#[test]
fn an_unreadable_process_table_leaves_the_group_as_the_only_mechanism() {
    let signaller = recording();
    let observer = sequence(usize::MAX);
    let capture = ScriptedCapture {
        unreadable: true,
        ..ScriptedCapture::default()
    };
    let mut group = capture_group(&signaller, &observer, &capture);
    assert_eq!(
        block_on(group.cleanup()),
        CleanupReceipt::CleanupPending,
        "the group was signalled and nothing else could be read"
    );
    let report = group.containment();
    assert_eq!(
        report.mechanism(),
        ContainmentMechanism::ProcessGroupOnly,
        "no table was read, so the mechanism must say the group was all there was"
    );
    assert_eq!(
        report.residual_risk(),
        Some(ResidualRisk::TableUnreadable),
        "the limit is named rather than left to be inferred from an empty capture"
    );
    assert_eq!(
        report.captured(),
        0,
        "nothing was captured, so nothing may be reported as captured"
    );
    assert!(
        !report.is_complete(),
        "a group-only cleanup never claims the whole tree"
    );
    drop(group);
}

/// A truncated reading is a prefix, and the receipt says so.
#[cfg(all(unix, feature = "process"))]
#[test]
fn a_truncated_capture_is_reported_as_a_prefix_and_not_as_the_tree() {
    let signaller = recording();
    let observer = sequence(usize::MAX);
    let capture = ScriptedCapture {
        tree: vec![70, 71],
        truncated: true,
        ..ScriptedCapture::default()
    };
    let mut group = capture_group(&signaller, &observer, &capture);
    assert_eq!(
        block_on(group.cleanup()),
        CleanupReceipt::CleanupPending,
        "the group is pinned by its unreaped leader"
    );
    assert_eq!(
        group.containment().residual_risk(),
        Some(ResidualRisk::CaptureTruncated),
        "a reading stopped at its bound must be reported as a prefix"
    );
    assert!(
        !group.containment().is_complete(),
        "a prefix cannot claim the whole tree"
    );
    drop(group);
}

#[cfg(all(unix, feature = "process"))]
#[test]
fn bounded_group_termination_keeps_cleanup_owned_until_absence_is_observed() {
    let signaller = recording();
    let observer = sequence(1);
    let mut group = test_group(&signaller, &observer, false);
    assert_eq!(
        block_on(group.cleanup()),
        CleanupReceipt::CleanupPending,
        "the bounded termination receipt must not claim observed absence"
    );
    let before_reap = signaller.calls.load(Ordering::Relaxed);
    assert_eq!(
        before_reap, 1,
        "the drain stops as soon as nothing captured is still running, so a group with no \
             descendants is signalled once and not once per configured round"
    );
    assert!(
        group.armed,
        "exhausting the signal budget is not proof that the group disappeared"
    );
    group.mark_reaped();
    assert_eq!(
        block_on(group.confirm_absence()),
        CleanupReceipt::CleanupConfirmed,
        "a signal-zero probe after reaping observes eventual group absence"
    );
    assert_eq!(observer.calls.load(Ordering::Relaxed), 2);
    drop(group);
    assert_eq!(
        signaller.calls.load(Ordering::Relaxed),
        before_reap,
        "no signal may follow reaping and release of the numeric id"
    );
}

#[cfg(all(unix, feature = "process"))]
#[test]
fn pending_cleanup_owner_keeps_its_permit_until_later_absence_receipt()
-> Result<(), Box<dyn std::error::Error>> {
    use super::super::owners::CleanupOwners;
    use crate::rt::sync::Semaphore;

    let semaphore = Arc::new(Semaphore::new(1));
    let Some(permit) = Arc::clone(&semaphore).try_acquire_owned().ok() else {
        return Err("the test semaphore started without its one permit".into());
    };
    let owners = Arc::new(CleanupOwners::default());
    let task = TaskId(9);
    let cleanup_observer = sequence(usize::MAX);
    let signaller = recording();
    let mut group = armed_group(
        42,
        task,
        false,
        Some(Lease::plain(permit)),
        Arc::clone(&owners),
        group_seams(&signaller, &cleanup_observer),
    );

    assert_eq!(
        block_on(group.cleanup()),
        CleanupReceipt::CleanupPending,
        "exhausting bounded signal attempts remains pending"
    );
    group.mark_reaped();
    assert_eq!(
        block_on(group.confirm_absence()),
        CleanupReceipt::CleanupPending,
        "a bounded post-reap pass cannot claim absence"
    );
    let later_observer = sequence(1);
    drop(group);
    assert_eq!(
        semaphore.available_permits(),
        0,
        "dropping a pending task-local guard must transfer, not release, its lease"
    );
    assert_eq!(owners.pending_count(), 1, "cleanup remains owned");
    assert!(
        owners.drive(&FailingGroupObserver, |_| false).is_empty(),
        "an observation error cannot release cleanup ownership"
    );
    assert_eq!(semaphore.available_permits(), 0);
    assert!(
        owners.drive(&later_observer, |_| false).is_empty(),
        "a still-present group cannot produce a terminal receipt"
    );
    assert_eq!(semaphore.available_permits(), 0);
    assert!(
        owners
            .drive(&later_observer, |candidate| candidate == task)
            .is_empty(),
        "absence before the task report must retain attribution and capacity"
    );
    assert_eq!(semaphore.available_permits(), 0);
    let settled = owners.drive(&later_observer, |_| false);
    // Attribution and capacity are this test's subject; what the report says
    // about the tree is the containment family's subject.
    assert_eq!(
        settled_tasks(&settled),
        vec![task],
        "later absence is attributed to its task"
    );
    assert_eq!(
        semaphore.available_permits(),
        1,
        "terminal proof releases capacity"
    );
    assert_eq!(owners.pending_count(), 0);
    assert_eq!(
        signaller.calls.load(Ordering::Relaxed),
        1,
        "one signal from the drain, and none after the reap: the id is released then, and \
             the drop-time fallback must not signal a number the OS may reissue"
    );
    Ok(())
}

#[cfg(all(unix, feature = "process"))]
#[test]
fn cleanup_failed_transfers_its_lease_until_absence_is_observed()
-> Result<(), Box<dyn std::error::Error>> {
    use super::super::owners::CleanupOwners;
    use crate::rt::sync::Semaphore;

    let semaphore = Arc::new(Semaphore::new(1));
    let Some(permit) = Arc::clone(&semaphore).try_acquire_owned().ok() else {
        return Err("the test semaphore started without its one permit".into());
    };
    let owners = Arc::new(CleanupOwners::default());
    let task = TaskId(10);
    let observer = sequence(0);
    let signaller = recording();
    let mut group = armed_group(
        43,
        task,
        true,
        Some(Lease::plain(permit)),
        Arc::clone(&owners),
        group_seams(&signaller, &observer),
    );

    assert_eq!(
        block_on(group.cleanup()),
        CleanupReceipt::CleanupFailed,
        "a guard whose leader was reaped cannot signal a possibly reused id"
    );
    drop(group);
    assert_eq!(owners.pending_count(), 1, "failed cleanup remains owned");
    assert_eq!(semaphore.available_permits(), 0);
    assert_eq!(
        settled_tasks(&owners.drive(&observer, |_| false)),
        vec![task],
        "an absent group settles the obligation it was transferred for"
    );
    assert_eq!(semaphore.available_permits(), 1);
    assert_eq!(signaller.calls.load(Ordering::Relaxed), 0);
    Ok(())
}

/// An adoptee another waiter reaped first is settled on the first pass,
/// not waited on for the whole death bound.
///
/// The other waiter is the test's own `wait`, standing in for a second
/// drain's sweep: after it, `waitpid` on the pid answers `ECHILD`, which is
/// what held a drain for 400 passes and had shutdown abort it.
#[cfg(all(target_os = "linux", feature = "process"))]
#[test]
fn an_adoptee_another_waiter_reaped_is_settled_on_the_first_pass()
-> Result<(), Box<dyn std::error::Error>> {
    let mut child = std::process::Command::new("sleep").arg("30").spawn()?;
    let pid = i32::try_from(child.id())?;
    child.kill()?;
    let _status = child.wait()?;

    let settled = block_on(super::super::start::reap_adopted(vec![pid]));

    assert!(
        settled.unreaped.is_empty(),
        "a pid this process no longer holds is not owed a reap: {:?}",
        settled.unreaped
    );
    assert_eq!(
        settled.passes, 1,
        "it is settled by the pass that finds it gone, not by the bound"
    );
    Ok(())
}

/// The control: an adoptee still this process's child is reaped here, so
/// settling a pid nobody holds did not stop the reap of one somebody does.
#[cfg(all(target_os = "linux", feature = "process"))]
#[test]
fn a_killed_adoptee_still_held_is_reaped_by_the_drain() -> Result<(), Box<dyn std::error::Error>> {
    let mut child = std::process::Command::new("sleep").arg("30").spawn()?;
    let pid = i32::try_from(child.id())?;
    child.kill()?;

    let settled = block_on(super::super::start::reap_adopted(vec![pid]));

    assert!(
        settled.unreaped.is_empty(),
        "a killed child dies inside the bound and is reaped: {:?}",
        settled.unreaped
    );
    assert!(
        lgwks_std::process::adopted_descendants(&[pid])?.is_empty(),
        "the reap collected the zombie, so the pid is no longer this process's child"
    );
    Ok(())
}

/// A quiet window outlasts the longest wait one drain makes on its own
/// bounds, so a drain still inside a bound it was given is never aborted as
/// stalled (INV-BOT-156).
#[cfg(all(target_os = "linux", feature = "process"))]
#[test]
fn the_cleanup_grace_outlasts_a_drains_own_longest_wait() {
    let reap = super::super::start::ADOPTION_REAP_INTERVAL
        .saturating_mul(super::super::start::ADOPTION_REAP_PASSES);
    let sweep = super::super::start::ADOPTION_REAP_INTERVAL
        .saturating_mul(super::super::start::SWEEP_CONFIRM_PASSES);
    assert!(
        super::super::PROCESS_CLEANUP_GRACE > reap.saturating_add(sweep),
        "grace {:?} must outlast the adoption reap {reap:?} plus the sweep {sweep:?}",
        super::super::PROCESS_CLEANUP_GRACE
    );
}

#[cfg(all(unix, feature = "process"))]
#[test]
fn shutdown_report_keeps_pending_cleanup_and_emits_later_terminal_receipt()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::rt::sync::Semaphore;

    let semaphore = Arc::new(Semaphore::new(1));
    let Some(permit) = Arc::clone(&semaphore).try_acquire_owned().ok() else {
        return Err("the test semaphore started without its one permit".into());
    };
    let task = TaskId(11);
    let report = block_on(Supervisor::new(1).shutdown());
    report
        .cleanup_owners
        .register(task, 44, Lease::plain(permit), whole_capture());
    assert_eq!(report.pending_cleanup_count(), 1);
    assert!(
        !report.is_clean(),
        "an unresolved native resource is not clean"
    );
    let Err(mut report) = report.into_outcomes() else {
        return Err("into_outcomes consumed a report with pending cleanup".into());
    };
    assert_eq!(
        report.pending_cleanup_count(),
        1,
        "Err must retain ownership"
    );
    assert_eq!(semaphore.available_permits(), 0);

    assert_eq!(
        report.reap_pending_cleanups_with(&FailingGroupObserver),
        0,
        "an observer error is not a terminal receipt"
    );
    let observer = sequence(1);
    assert_eq!(report.reap_pending_cleanups_with(&observer), 0);
    assert_eq!(report.pending_cleanup_count(), 1);
    assert_eq!(report.reap_pending_cleanups_with(&observer), 1);
    assert_eq!(report.pending_cleanup_count(), 0);
    assert_eq!(semaphore.available_permits(), 1);
    assert!(report.is_clean());
    let outcomes = match report.into_outcomes() {
        Ok(outcomes) => outcomes,
        Err(_) => return Err("settled cleanup report remained unavailable".into()),
    };
    assert_eq!(
        outcomes,
        vec![TaskOutcome::CleanupSettled {
            task,
            cleanup: CleanupReceipt::CleanupConfirmed,
            containment: whole_capture(),
        }]
    );
    Ok(())
}

/// The drain's re-signal fires only while the unreaped leader pins the id, and
/// stops once the answer can no longer change.
///
/// Source: issue #263 (a straggler forked in the window `killpg` misses on XNU
/// held a captured pipe) and POSIX `kill(2)`: a group id may be signalled only
/// while something holds it, which the unreaped leader does. Each guard
/// condition is exercised alone, so each one is shown to decide by itself.
#[cfg(all(unix, feature = "process"))]
#[test]
fn the_drain_resignals_only_a_pinned_group_that_can_still_answer() {
    let observer = sequence(usize::MAX);

    let pinned = recording();
    let mut group = test_group(&pinned, &observer, false);
    assert!(
        group.resignal_while_pinned(),
        "a pinned, present group is re-signalled"
    );
    assert_eq!(
        pinned.calls.load(Ordering::Relaxed),
        1,
        "exactly one signal per slice"
    );
    group.disarm();
    drop(group);

    let reaped = recording();
    let mut group = test_group(&reaped, &observer, true);
    assert!(
        !group.resignal_while_pinned(),
        "a reaped leader no longer pins the id"
    );
    group.disarm();
    drop(group);
    assert_eq!(
        reaped.calls.load(Ordering::Relaxed),
        0,
        "a released id is never signalled"
    );

    let unnamed = recording();
    let mut group = test_group(&unnamed, &observer, false);
    group.group = 0;
    assert!(
        !group.resignal_while_pinned(),
        "id 0 would name this process's own group"
    );
    assert_eq!(unnamed.calls.load(Ordering::Relaxed), 0);
    group.disarm();
    drop(group);

    let absent = recording();
    let mut group = test_group(&absent, &observer, false);
    group.group_absent = true;
    assert!(
        !group.resignal_while_pinned(),
        "an observed-absent group is not asked again"
    );
    assert_eq!(absent.calls.load(Ordering::Relaxed), 0);
    group.disarm();
    drop(group);

    let zombie = EpermGroupSignaller;
    let mut group = test_group(&zombie, &observer, false);
    assert!(
        group.resignal_while_pinned(),
        "EPERM from a zombie-led group means present, so the drain keeps asking"
    );
    group.disarm();
    drop(group);

    let refusing = UnexpectedGroupSignaller;
    let mut group = test_group(&refusing, &observer, false);
    assert!(
        !group.resignal_while_pinned(),
        "a refused signal will not be accepted on a repeat"
    );
    group.disarm();
    drop(group);
}

/// A held pipe is recorded as evidence over whatever limit came before it.
///
/// Source: issue #263. `note` is first-wins for limits of what was looked at;
/// a held descriptor is a holder seen alive, and must not be hidden behind
/// `LeaderExited`, which every self-exiting run records first.
#[cfg(all(unix, feature = "process"))]
#[test]
fn a_held_descriptor_replaces_the_leader_exited_limit() {
    let signaller = recording();
    let observer = sequence(usize::MAX);
    let mut group = test_group(&signaller, &observer, false);
    group.mark_exited();
    assert_eq!(
        group.containment().residual_risk(),
        Some(ResidualRisk::LeaderExited)
    );
    group.mark_descriptor_held();
    assert_eq!(
        group.containment().residual_risk(),
        Some(ResidualRisk::DescriptorHeld)
    );
    assert!(!group.containment().is_complete());
    group.disarm();
}
