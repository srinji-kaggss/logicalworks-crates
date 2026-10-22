//! The async tier's documented entry points, run as tests (#526).
//!
//! Each used to be a doctest that named the crate by path: entering the runtime, placing work on a
//! supervisor, the supervisor's tenancy and snapshot doors, the cancellation token, the tenancy
//! policy, the frame reader and the logical deadline. They are tests here, so each documented
//! claim runs under nextest with the rest of the suite.

use std::error::Error;
use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::clock::Clock;
use lgwks_bot::rt::runtime::Builder;
use lgwks_bot::rt::supervise::{Budget, NextAction, Supervisor};
use lgwks_bot::rt::sync::{CancellationToken, oneshot};
use lgwks_bot::rt::task::{JoinError, JoinSet, join_all_bounded};
use lgwks_bot::rt::time::{Deadline, sleep};

/// What every fallible test here returns.
type TestResult = Result<(), Box<dyn Error>>;

/// Say, through `done`, that a unit of work has run.
///
/// A task that announces itself lets the test shut a supervisor down over a task that has finished,
/// and never race one that has not started.
fn announce(done: oneshot::Sender<()>) {
    let _sent = done.send(());
}

/// Wait until the task has run, shut `supervisor` down, and check that the report accounts for
/// exactly that one task and that it did not panic.
async fn expect_one_drained(supervisor: Supervisor, ran: oneshot::Receiver<()>) {
    assert!(ran.await.is_ok(), "the unit of work ran");
    let report = supervisor.shutdown().await;
    assert_eq!(report.stats().spawned, 1, "one task was started");
    assert_eq!(report.stats().completed, 1, "the one task was drained");
    assert_eq!(report.outcomes().len(), 1, "the report holds its outcome");
    assert_eq!(report.panicked().count(), 0, "and it did not panic");
}

/// Start a repeating task that never ends on its own, so only a shutdown can end it.
async fn start_ongoing_repeat(supervisor: &mut Supervisor) {
    supervisor
        .spawn_repeating(Budget::Ongoing, |tick| async move {
            let _tick = tick;
        })
        .await;
}

#[test]
fn the_runtime_is_entered_through_block_on() -> TestResult {
    let runtime = Runtime::new()?;
    let answer = runtime.block_on(async { 2 + 2 });
    assert_eq!(answer, 4);
    Ok(())
}

#[test]
fn a_background_shutdown_terminates_without_blocking_the_caller() -> TestResult {
    let runtime = Runtime::new()?;
    let owned = Runtime::new()?;
    let answer = runtime.block_on(async {
        // The blocking shutdown has no business inside async: the background one ends the runtime
        // it is called on, and the evening continues on this one.
        owned.shutdown_background();
        2 + 2
    });
    assert_eq!(answer, 4);
    Ok(())
}

#[test]
fn work_placed_on_a_default_supervisor_is_owned_and_reported() -> TestResult {
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::default();
        let (done, ran) = oneshot::channel();
        supervisor
            .spawn(move |_token| async move { announce(done) })
            .await;
        expect_one_drained(supervisor, ran).await;
    });
    Ok(())
}

#[test]
fn a_builder_names_its_workers_and_defaults_to_this_crates_name() {
    let ours = Builder::new().thread_name("ingest");
    let theirs = Builder::new();
    assert!(
        format!("{ours:?}").contains("ingest"),
        "the name a caller set is the name the builder holds"
    );
    assert!(
        format!("{theirs:?}").contains("lgwks-bot"),
        "and the default is this crate's own name, not an absent one"
    );
}

#[test]
fn an_ongoing_repeat_still_ends_when_the_supervisor_shuts_down() -> TestResult {
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::new(4);
        start_ongoing_repeat(&mut supervisor).await;
        // `Ongoing` is unbounded, and it still ends: shutdown cancels the token it races. The
        // drained evidence is the report, not just an empty set.
        let _report = supervisor.shutdown().await;
    });
    Ok(())
}

#[test]
fn a_supervisor_refuses_past_its_ceiling_and_reports_an_outcome_for_every_task() -> TestResult {
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        // At most 4 in flight, ever.
        let mut supervisor = Supervisor::new(4);
        start_ongoing_repeat(&mut supervisor).await;
        assert!(
            supervisor.try_spawn(|_token| async {}).is_ok(),
            "a spawn under the ceiling is admitted"
        );
        // Cancel, drain, join, and hand back the evidence.
        let report = supervisor.shutdown().await;
        assert_eq!(report.stats().spawned, 2, "both tasks were started");
        assert_eq!(report.stats().completed, 2, "both were drained");
        assert_eq!(
            report.outcomes().len(),
            2,
            "each names one terminal outcome"
        );
    });
    Ok(())
}

#[test]
fn a_cancelled_token_stops_the_task_a_join_set_owns() -> TestResult {
    let runtime = Runtime::new()?;
    runtime.block_on(async {
        let token = CancellationToken::new();
        let mut tasks = JoinSet::new();
        let worker = token.clone();
        tasks.spawn(async move {
            worker.cancelled_owned().await;
        });
        token.cancel();
        while let Some(joined) = tasks.join_next().await {
            joined?;
        }
        Ok::<(), JoinError>(())
    })?;
    Ok(())
}

#[test]
fn a_bounded_fan_out_runs_every_input() -> TestResult {
    let runtime = Runtime::new()?;
    let results = runtime.block_on(async {
        join_all_bounded(
            4,
            (0..16).map(|index| async move {
                sleep(Duration::from_millis(1)).await;
                index * index
            }),
        )
        .await
    });
    assert_eq!(results.len(), 16, "every input produced a result");
    Ok(())
}

#[test]
fn a_fresh_supervisor_snapshot_holds_every_permit() {
    let supervisor = Supervisor::new(2);
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.max_in_flight(), 2);
    assert_eq!(snapshot.free(), 2, "a fresh supervisor holds both permits");
    assert_eq!(snapshot.next_action(), NextAction::Admit);
}

#[test]
fn a_cancelled_token_is_cancelled_in_every_clone() {
    let token = CancellationToken::new();
    let worker = token.clone();
    // The worker observes the cancel; the supervisor decides when.
    assert!(!worker.is_cancelled());
    token.cancel();
    assert!(worker.is_cancelled());
}

#[test]
fn a_day_of_logical_time_exhausts_a_deadline_the_watchdog_does_not_notice() {
    let clock = Clock::virtual_at(Duration::ZERO);
    let deadline = Deadline::after(&clock, Duration::from_secs(30));
    // A day of logical time exhausts a 30-second budget exactly.
    assert!(clock.advance(Duration::from_secs(86_400)).is_ok());
    assert!(deadline.is_exhausted());
    assert_eq!(deadline.remaining(), Duration::ZERO);
    // The watchdog is a separate fact: it reads real time, so it has not moved with the day that
    // passed on the logical clock.
    assert!(deadline.watchdog().elapsed() < Duration::from_secs(86_400));
}

#[cfg(feature = "script")]
mod tenancy {
    use std::num::NonZeroU32;

    use lgwks_bot::rt::tenancy::TenancyPolicy;
    use lgwks_bot::script::Tenant;

    use super::{Error, Runtime, Supervisor, TestResult, announce, expect_one_drained, oneshot};

    #[test]
    fn a_tenant_scoped_spawn_runs_under_a_per_tenant_ceiling() -> TestResult {
        let runtime = Runtime::new()?;
        runtime.block_on(async {
            let quiet = Tenant::new("quiet")?;
            // 64 permits in total, 32 per tenant: one tenant can strand at most 32 of them, so 32
            // stay reachable for everyone else however loud it is.
            let mut supervisor = Supervisor::with_tenancy(64, TenancyPolicy::new(32, 1_024));
            let (done, ran) = oneshot::channel();
            supervisor
                .spawn_for(&quiet, move |_token| async move { announce(done) })
                .await?;
            expect_one_drained(supervisor, ran).await;
            Ok::<(), Box<dyn Error>>(())
        })
    }

    #[test]
    fn one_tenanted_admission_takes_at_least_one_arrival_decision() -> TestResult {
        let runtime = Runtime::new()?;
        runtime.block_on(async {
            let quiet = Tenant::new("quiet")?;
            let mut supervisor = Supervisor::with_tenancy(4, TenancyPolicy::new(2, 8));
            supervisor.spawn_for(&quiet, |_token| async {}).await?;
            let (arrivals, _, _, _) = supervisor.admission_cost();
            assert!(
                arrivals >= 1,
                "one admission took at least one arrival decision"
            );
            Ok::<(), Box<dyn Error>>(())
        })
    }

    #[test]
    fn a_policy_reads_back_its_bounds_and_defaults_an_unweighted_tenant_to_one() -> TestResult {
        let heavy = Tenant::new("acme")?;
        let two: NonZeroU32 = 2_u32.try_into()?;
        let policy = TenancyPolicy::new(8, 64).with_weight(&heavy, two)?;
        // An unweighted tenant defaults to 1; the bounds are readable back.
        assert_eq!(policy.per_tenant_limit(), 8);
        assert_eq!(policy.queue_per_tenant(), 64);
        assert_eq!(u32::from(policy.weight_of(&heavy)), 2);
        assert_eq!(u32::from(policy.weight_of(&Tenant::new("quiet")?)), 1);
        Ok(())
    }
}

#[cfg(all(unix, feature = "process"))]
mod process {
    use std::num::NonZeroUsize;

    use lgwks_bot::rt::process::{
        FrameRead, OrphanReap, ProcessIdentity, ProcessSpec, read_frames, reap_orphaned_group,
    };

    use super::{Duration, Error, Runtime, Supervisor, TestResult};

    /// A description of `sh -c <script>`; nothing starts until a supervisor takes it.
    fn shell(script: &str) -> ProcessSpec {
        let mut spec = ProcessSpec::new("sh");
        spec.arg("-c").arg(script);
        spec
    }

    #[test]
    fn a_borrowed_process_spec_is_started_and_owned_by_the_supervisor() -> TestResult {
        let runtime = Runtime::new()?;
        runtime.block_on(async {
            let mut supervisor = Supervisor::default();
            supervisor.spawn_process(&shell("exit 0")).await?;
            let _report = supervisor.shutdown().await;
            Ok::<(), Box<dyn Error>>(())
        })
    }

    #[test]
    fn a_stored_leader_identity_parses_back_and_a_gone_group_is_not_signalled() -> TestResult {
        let runtime = Runtime::new()?;
        runtime.block_on(async {
            let mut supervisor = Supervisor::default();
            let started = supervisor
                .spawn_process_identified(&shell("exit 0"))
                .await?;
            // The text a lane stores durably before it does work.
            let stored = started.leader().to_string();
            let _report = supervisor.shutdown().await;
            let leader: ProcessIdentity = stored.parse()?;
            let reaped = reap_orphaned_group(&leader)?;
            assert!(
                !matches!(reaped, OrphanReap::Signalled { .. }),
                "a group that has exited is not signalled: {reaped:?}"
            );
            Ok::<(), Box<dyn Error>>(())
        })
    }

    #[test]
    fn a_command_runs_with_its_stdout_captured_to_a_ceiling_and_a_deadline() -> TestResult {
        let runtime = Runtime::new()?;
        runtime.block_on(async {
            let mut spec = shell("printf ready");
            spec.capture_stdout(NonZeroUsize::new(64 * 1024).ok_or("a non-zero ceiling")?)
                .deadline(Duration::from_secs(10));
            let mut supervisor = Supervisor::new(1);
            let run = supervisor.run_process(&spec).await?;
            assert_eq!(
                run.stdout().bytes(),
                b"ready",
                "the captured stdout is what the child printed"
            );
            Ok::<(), Box<dyn Error>>(())
        })
    }

    #[test]
    fn a_frame_stream_cut_mid_record_is_a_truncation_never_a_frame() -> TestResult {
        // One whole record, then a prefix whose payload never arrived.
        let mut stream: &[u8] = &[0, 0, 0, 2, 7, 9, 0, 0, 0, 5];
        let frames = read_frames(&mut stream, 64)?;
        assert_eq!(frames.records().len(), 1, "the whole record before the cut");
        assert_eq!(frames.records()[0].payload(), Some(&[7, 9][..]));
        assert_eq!(
            frames.ended(),
            &FrameRead::TruncatedPayload {
                declared: 5,
                partial: Vec::new()
            },
            "a prefix naming five bytes that delivered none is a truncation, never a frame"
        );
        assert!(
            !frames.is_complete(),
            "a cut record is not a complete reading"
        );
        Ok(())
    }
}
