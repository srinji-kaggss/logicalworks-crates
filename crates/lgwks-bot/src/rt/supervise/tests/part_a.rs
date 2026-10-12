    #[cfg(all(unix, feature = "process"))]
    use super::CleanupOwners;
    #[cfg(all(unix, feature = "process"))]
    use super::CleanupReceipt;
    #[cfg(all(not(any(unix, windows)), feature = "process"))]
    use super::ProcessSpec;
    #[cfg(feature = "script")]
    use super::SpawnRefused;
    #[cfg(all(unix, feature = "process"))]
    use super::SupervisorCancelled;
    use super::{
        Budget, Clock, Lease, MAX_PANIC_MESSAGE_CHARS, Outcome, Supervisor, TaskId, TaskOutcome,
        TrySpawnRefusal, repeat, truncate_panic_message,
    };
    #[cfg(all(unix, feature = "process"))]
    use super::{
        CONTAINMENT_ROUNDS, Containment, ContainmentMechanism, DescendantCapture, EPERM, ESRCH,
        GroupObserver, GroupSignaller, ProcessGroup, ResidualRisk,
    };
    use crate::rt::cancel::CancellationToken;
    use crate::rt::runtime::block_on;
    use crate::rt::task::yield_now;
    use std::future::Future;
    use std::future::pending;
    use std::num::NonZeroU64;
    use std::sync::Arc;
    #[cfg(any(feature = "process", feature = "script"))]
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::task::Poll;
    use std::time::Duration;

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
        fn capture(&self, _root: i32) -> std::io::Result<super::Capture> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            if self.unreadable {
                let refusal = Err(std::io::Error::other("injected unreadable process table"));
                lgwks_std::trace::debug!(
                    error = ?refusal.as_ref().err(),
                    "scripted capture: returning an error to the caller"
                );
                return refusal;
            }
            Ok(super::Capture {
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
            Ok(pids
                .iter()
                .copied()
                .filter(|pid| self.immortal.contains(pid))
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

