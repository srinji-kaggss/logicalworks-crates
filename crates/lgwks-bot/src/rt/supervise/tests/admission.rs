//! Admission and the report stream: capacity, cancellation, panics and the retention cap.

#[cfg(all(unix, feature = "process"))]
use super::super::SupervisorCancelled;
use super::super::outcome::TaskOutcome;
use super::super::repeat::repeat;
use super::super::supervisor::Supervisor;
use super::super::{Budget, TaskId, TrySpawnRefusal};
use super::{block_on, budget_of, drain, explode, settle, tasks_where};
use crate::rt::task::yield_now;
use std::future::pending;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[test]
fn a_cancelled_supervisor_refuses_admission_without_constructing_the_body() {
    block_on(async {
        // Cancellation is the terminal admission state: after it, no body
        // constructor runs and no slot is taken, on either spawn path. The
        // constructor is the observable, not the body: a body that writes a
        // marker or takes a lock would prove the old behavior, where the
        // constructor ran before anything looked at the token.
        let mut supervisor = Supervisor::new(2);
        supervisor.cancel();

        let built = Arc::new(AtomicU64::new(0));
        supervisor
            .spawn({
                let built = Arc::clone(&built);
                move |_token| {
                    built.fetch_add(1, Ordering::Relaxed);
                    async {}
                }
            })
            .await;
        let stats = supervisor.stats();
        assert_eq!(
            built.load(Ordering::Relaxed),
            0,
            "a cancelled supervisor must never construct the body it refuses"
        );
        assert_eq!(stats.spawned, 0, "a refused spawn must not be placed");
        assert_eq!(
            stats.refused, 1,
            "the waiting-spawn refusal must be counted"
        );

        let built_now = Arc::new(AtomicU64::new(0));
        let refused = supervisor.try_spawn({
            let built_now = Arc::clone(&built_now);
            move |_token| {
                built_now.fetch_add(1, Ordering::Relaxed);
                async {}
            }
        });
        assert_eq!(
            refused,
            Err(TrySpawnRefusal::Cancelled),
            "the immediate path must name cancellation, not capacity"
        );
        assert_eq!(
            built_now.load(Ordering::Relaxed),
            0,
            "the immediate path must also refuse before the constructor"
        );
        assert_eq!(supervisor.stats().refused, 2, "both refusals counted");
    });
}

/// Fill `supervisor`'s only slot with a body that waits for the returned
/// gate and then sets the returned done flag.
async fn occupy(supervisor: &mut Supervisor) -> (Arc<AtomicBool>, Arc<AtomicBool>) {
    let gate = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    supervisor
        .spawn({
            let gate = Arc::clone(&gate);
            let done = Arc::clone(&done);
            move |_token| async move {
                while !gate.load(Ordering::Relaxed) {
                    yield_now().await;
                }
                done.store(true, Ordering::Relaxed);
            }
        })
        .await;
    (gate, done)
}

#[test]
fn cancellation_closes_admission_even_after_a_slot_frees() {
    block_on(async {
        // The order is the point: cancel first, then let the only slot
        // free. A supervisor that checks capacity but not cancellation
        // admits at the freeing of the slot — the constructor below runs
        // and the counter moves. The fence must refuse at the owned
        // admission point regardless of what the pool is doing.
        let mut supervisor = Supervisor::new(1);
        let (gate, done) = occupy(&mut supervisor).await;
        supervisor.cancel();
        gate.store(true, Ordering::Relaxed);
        // Let the occupier finish and its permit return to the pool: the
        // body's own flag says it ran to its end, and the yields give the
        // wrapper around it — which holds the permit — the polls it needs
        // to complete behind it. The supervisor's counters are not the
        // signal here: a cancelled supervisor stops reaping, but the
        // permit lives in the task wrapper, not in the counters.
        for _ in 0..256 {
            if done.load(Ordering::Relaxed) {
                break;
            }
            yield_now().await;
        }
        assert!(
            done.load(Ordering::Relaxed),
            "the occupier must have ended before the admission attempt"
        );

        let built = Arc::new(AtomicU64::new(0));
        let refused = supervisor.try_spawn({
            let built = Arc::clone(&built);
            move |_token| {
                built.fetch_add(1, Ordering::Relaxed);
                async {}
            }
        });
        assert_eq!(
            refused,
            Err(TrySpawnRefusal::Cancelled),
            "a freed slot is not an admission offer after cancellation"
        );
        assert_eq!(
            built.load(Ordering::Relaxed),
            0,
            "no body may be constructed after cancellation, slot or not"
        );
        assert_eq!(supervisor.stats().spawned, 1, "only the occupier ran");
    });
}

#[test]
fn a_cancelled_supervisor_with_a_full_pool_refuses_without_constructing() {
    block_on(async {
        // The pool is full *and* the supervisor is cancelled. The wait
        // must not park: cancellation is checked before the pool, so the
        // refusal returns while the occupier still holds the only slot,
        // and the constructor never runs. Without any cancellation
        // fence this parks forever and the test never finishes.
        let mut supervisor = Supervisor::new(1);
        let (gate, done) = occupy(&mut supervisor).await;
        supervisor.cancel();

        let built = Arc::new(AtomicU64::new(0));
        let refused_before = supervisor.stats().refused;
        supervisor
            .spawn({
                let built = Arc::clone(&built);
                move |_token| {
                    built.fetch_add(1, Ordering::Relaxed);
                    async {}
                }
            })
            .await;
        assert_eq!(
            built.load(Ordering::Relaxed),
            0,
            "no body may be constructed for a cancelled supervisor, full pool or not"
        );
        assert_eq!(
            supervisor.stats().refused,
            refused_before + 1,
            "the waiting-spawn refusal must be counted"
        );
        assert_eq!(supervisor.stats().spawned, 1, "only the occupier ran");

        gate.store(true, Ordering::Relaxed);
        for _ in 0..256 {
            if done.load(Ordering::Relaxed) {
                break;
            }
            yield_now().await;
        }
    });
}

/// Cancellation closes the native fork, not just the async placement:
/// after it, `spawn_process` names cancellation and no child runs, so
/// the first instruction of a refused command does not execute (issue
/// #143 R11). Unix only, like the spawn path itself.
#[cfg(all(unix, feature = "process"))]
#[test]
fn a_cancelled_supervisor_starts_no_process() {
    use crate::rt::process::ProcessSpec;

    block_on(async {
        // Nanos plus a monotone sequence: not a pid (reused by the OS).
        static MARK_SEQ: AtomicU64 = AtomicU64::new(0);
        let nanos = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(since) => since.as_nanos(),
            Err(before) => before.duration().as_nanos(),
        };
        let seq = MARK_SEQ.fetch_add(1, Ordering::Relaxed);
        let marker = std::env::temp_dir().join(format!("lgwks-bot-cancel-{nanos}-{seq}.mark"));

        let mut supervisor = Supervisor::new(2);
        supervisor.cancel();
        let spawned_before = supervisor.stats().spawned;
        let refused_before = supervisor.stats().refused;

        let mut spec = ProcessSpec::new("sh");
        spec.arg("-c").arg(format!("touch {}", marker.display()));
        let refusal = supervisor.spawn_process(&spec).await;
        assert!(
            refusal.is_err(),
            "a cancelled supervisor must not start the process it was asked for"
        );
        assert!(
            refusal
                .as_ref()
                .err()
                .and_then(std::io::Error::get_ref)
                .and_then(|cause| cause.downcast_ref::<SupervisorCancelled>())
                .is_some(),
            "the refusal must name cancellation, got {refusal:?}"
        );
        assert_eq!(
            supervisor.stats().spawned,
            spawned_before,
            "no process may be placed after cancellation"
        );
        assert_eq!(
            supervisor.stats().refused,
            refused_before + 1,
            "the process refusal must be counted"
        );
        assert!(
            !marker.exists(),
            "no child ran: the marker {} must be absent",
            marker.display()
        );
        // The absence above is the assertion; the removal only avoids
        // litter, and either outcome leaves the path gone.
        let removed = std::fs::remove_file(&marker);
        assert!(
            removed.is_ok() || !marker.exists(),
            "the marker path must be gone: {}",
            marker.display()
        );
    });
}

#[test]
fn try_spawn_refuses_at_the_bound_instead_of_growing() {
    block_on(async {
        // The single slot is held by a body that never resolves, so the
        // refusal is attempted while the supervisor is genuinely full.
        let mut supervisor = Supervisor::new(1);
        let held = supervisor.try_spawn(|_token| async {
            pending::<()>().await;
        });
        assert!(held.is_ok(), "the first spawn must take the only slot");
        let refused = supervisor.try_spawn(|_token| async {});
        assert_eq!(
            refused,
            Err(TrySpawnRefusal::AtCapacity),
            "a second spawn past the bound must be refused, not queued"
        );
        let stats = supervisor.stats();
        assert_eq!(stats.refused, 1, "the refusal must be counted");
        assert_eq!(stats.spawned, 1, "a refused body must never be spawned");
        supervisor.cancel();
    });
}

#[test]
fn the_retained_set_stays_at_the_bound_across_many_spawns() {
    block_on(async {
        // Far more spawns than the bound. A `JoinSet` retains a finished
        // task until it is joined, so without reaping on every entry point
        // the in-flight count would climb to the total instead of staying
        // at the bound.
        let mut supervisor = Supervisor::new(1);
        for _ in 0..64 {
            supervisor.spawn(|_token| async {}).await;
        }
        let stats = supervisor.stats();
        assert_eq!(stats.spawned, 64, "every spawn must be counted");
        assert!(
            stats.in_flight() <= 2,
            "the retained set must stay at the bound, not grow with total \
                 spawns; got {} in flight of {} spawned",
            stats.in_flight(),
            stats.spawned
        );
        assert!(
            settle(&mut supervisor).await,
            "the remaining tasks must finish"
        );
        assert_eq!(
            supervisor.stats().in_flight(),
            0,
            "settling must leave nothing in flight"
        );
    });
}

#[test]
fn dropping_a_supervisor_cancels_the_tasks_it_owns() {
    block_on(async {
        let probe = {
            let mut supervisor = Supervisor::new(1);
            supervisor
                .spawn(|_token| async {
                    pending::<()>().await;
                })
                .await;
            supervisor.child_token()
        };
        assert!(
            probe.is_cancelled(),
            "a task's token must be cancelled when the supervisor is dropped"
        );
    });
}

#[test]
fn shutdown_drains_every_task() {
    block_on(async {
        let mut supervisor = Supervisor::new(4);
        supervisor
            .spawn(|token| async move {
                let _outcome = repeat(&token, Budget::Ongoing, |_tick| async {}).await;
            })
            .await;
        supervisor
            .spawn(|token| async move {
                let _outcome = repeat(&token, Budget::Ongoing, |_tick| async {}).await;
            })
            .await;
        // Would never return if `shutdown` waited on a cancelled task
        // instead of draining it.
        let report = supervisor.shutdown().await;
        assert_eq!(
            report.stats().spawned,
            2,
            "shutdown must report the counters it drained against"
        );
    });
}

#[test]
fn a_repeating_task_stops_at_its_budget_under_supervision() {
    block_on(async {
        let ticks = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&ticks);
        let mut supervisor = Supervisor::new(1);
        supervisor
            .spawn_repeating(budget_of(5), move |_tick| {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            })
            .await;
        assert!(
            settle(&mut supervisor).await,
            "a repeating task must finish once its budget is spent"
        );
        assert_eq!(
            ticks.load(Ordering::SeqCst),
            5,
            "a supervised repeating task must stop at its iteration budget"
        );
        supervisor.shutdown().await;
    });
}

#[test]
fn a_panicking_task_is_not_reported_as_a_success() {
    // The counterexample the defect was filed on: `async {}` and a body
    // that panics produced identical `Stats` and no report channel at all.
    // The panic is captured by the runtime into a `JoinError`, so it is
    // read here rather than propagated: a test that panics the suite
    // proves nothing about the API that is supposed to report the panic,
    // and takes the rest of the suite down with it.
    block_on(async {
        let mut supervisor = Supervisor::new(2);
        supervisor.spawn(|_token| async {}).await;
        supervisor
            .spawn(|_token| async {
                explode("worker failed before producing its receipt");
            })
            .await;
        assert!(
            settle(&mut supervisor).await,
            "a panicking task still ends, so nothing is left in flight"
        );

        let stats = supervisor.stats();
        // The accounting counters are identical, and that is the point:
        // `completed` is a resource count and says nothing about success.
        assert_eq!(stats.spawned, 2, "both tasks were started");
        assert_eq!(stats.completed, 2, "both tasks ended");
        assert_eq!(
            stats.succeeded, 1,
            "only the task that returned is a success: {stats:?}"
        );
        assert_eq!(stats.panicked, 1, "one task panicked: {stats:?}");

        let outcomes = drain(&mut supervisor);
        assert_eq!(outcomes.len(), 2, "every task reports exactly once");
        assert_eq!(
            tasks_where(&outcomes, TaskOutcome::is_panic),
            vec![TaskId(1)],
            "the report names the task that panicked, in spawn order: {outcomes:?}"
        );
        assert_eq!(
            tasks_where(&outcomes, TaskOutcome::is_success),
            vec![TaskId(0)],
            "the task that returned is reported as the one that returned: {outcomes:?}"
        );
        let messages: Vec<&str> = outcomes
            .iter()
            .filter_map(TaskOutcome::panic_message)
            .collect();
        assert_eq!(
            messages,
            vec!["worker failed before producing its receipt"],
            "the panic payload is preserved rather than replaced by a marker"
        );
    });
}

#[test]
fn a_panic_after_an_observable_effect_keeps_both_facts() {
    // A task can do its work and then die, and the supervisor has to be
    // able to say both things. Reporting only "one task ended" is what
    // made the effect untraceable: a caller reading the counter would
    // conclude the work was done.
    block_on(async {
        let effects = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&effects);
        let mut supervisor = Supervisor::new(1);
        supervisor
            .spawn(move |_token| {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    explode("died after the effect");
                }
            })
            .await;
        assert!(settle(&mut supervisor).await, "the task must end");

        assert_eq!(
            effects.load(Ordering::SeqCst),
            1,
            "the effect happened, so the test's premise holds"
        );
        assert_eq!(
            supervisor.stats().succeeded,
            0,
            "an effect that happened is not a task that finished"
        );
        let outcomes = drain(&mut supervisor);
        assert_eq!(
            outcomes,
            vec![TaskOutcome::Panicked {
                task: TaskId(0),
                message: String::from("died after the effect"),
            }],
            "the report carries the panic, and does not claim success"
        );
    });
}

#[test]
fn cooperative_cancellation_and_abort_are_told_apart() {
    block_on(async {
        let mut supervisor = Supervisor::new(2);
        // Cooperative: the body awaits its token, so the cancel makes it
        // return on its own and it is reported cancelled.
        supervisor
            .spawn(|token| async move {
                token.cancelled().await;
            })
            .await;
        // Not cooperative: the body never looks at its token and never
        // finishes, so the only way it ends is the abort.
        supervisor
            .spawn(|_token| async {
                loop {
                    yield_now().await;
                }
            })
            .await;

        let report = supervisor.shutdown().await;
        let stats = report.stats();
        assert_eq!(stats.spawned, 2, "both tasks were started");
        assert_eq!(stats.completed, 2, "both tasks ended");
        assert_eq!(
            stats.cancelled, 1,
            "the cooperative body returned under cancellation: {stats:?}"
        );
        assert_eq!(
            stats.aborted, 1,
            "the body that ignored its token had to be dropped: {stats:?}"
        );
        assert_eq!(
            stats.succeeded, 0,
            "neither task ran to the end of its own work: {stats:?}"
        );
        assert!(
            !report.is_clean(),
            "a shutdown that cancelled or aborted anything is not a clean finish"
        );
        assert_eq!(
            tasks_where(report.outcomes(), TaskOutcome::is_success),
            Vec::<TaskId>::new(),
            "no failed task may appear as successful: {:?}",
            report.outcomes()
        );
        assert_eq!(
            report
                .outcomes()
                .iter()
                .map(TaskOutcome::task)
                .collect::<Vec<TaskId>>(),
            vec![TaskId(0), TaskId(1)],
            "both terminal outcomes are reported, each attributed to its task"
        );
    });
}

#[test]
fn shutdown_reports_a_clean_finish_when_every_task_returned() {
    block_on(async {
        let mut supervisor = Supervisor::new(2);
        supervisor.spawn(|_token| async {}).await;
        supervisor.spawn(|_token| async {}).await;
        assert!(settle(&mut supervisor).await, "both tasks must finish");
        let report = supervisor.shutdown().await;
        assert_eq!(
            report.stats().succeeded,
            2,
            "both tasks returned before the cancel"
        );
        assert!(
            report.panicked().next().is_none(),
            "a clean finish has no panics to report"
        );
    });
}

#[test]
fn an_undrained_report_buffer_is_capped_and_counted() {
    block_on(async {
        // The bound is one, so at most one report is retained. A caller
        // that spawns 32 tasks and never drains must not be able to make
        // this module's memory grow with the total number of spawns.
        let mut supervisor = Supervisor::new(1);
        for _ in 0..32 {
            supervisor.spawn(|_token| async {}).await;
        }
        assert!(settle(&mut supervisor).await, "every task must finish");
        let stats = supervisor.stats();
        assert_eq!(stats.spawned, 32, "every spawn is still counted");
        assert_eq!(stats.succeeded, 32, "every success is still counted");
        assert_eq!(
            stats
                .succeeded
                .saturating_add(stats.cancelled)
                .saturating_add(stats.aborted)
                .saturating_add(stats.panicked),
            stats.completed,
            "the four outcomes must account for every task that ended"
        );
        let retained = drain(&mut supervisor).len();
        assert_eq!(
            retained, 1,
            "the buffer retains at most the in-flight bound"
        );
        assert_eq!(
            stats.reports_dropped, 31,
            "and every report it did not retain is counted: {stats:?}"
        );
    });
}

#[test]
fn a_capped_report_buffer_still_attributes_the_report_it_keeps() {
    block_on(async {
        let mut supervisor = Supervisor::new(1);
        // The first task takes the only slot, so the second is refused and
        // the first is the only task that can report.
        assert!(
            supervisor
                .try_spawn(|_token| async {
                    pending::<()>().await;
                })
                .is_ok(),
            "the first spawn must take the only slot"
        );
        assert_eq!(
            supervisor.try_spawn(|_token| async {}),
            Err(TrySpawnRefusal::AtCapacity),
            "the premise: the bound is genuinely reached"
        );
        let report = supervisor.shutdown().await;
        assert_eq!(
            report.outcomes().len(),
            1,
            "one task ended, so one report is retained"
        );
        assert_eq!(
            report.outcomes().first().map(TaskOutcome::task),
            Some(TaskId(0)),
            "the retained report still names its task"
        );
        assert_eq!(
            report.stats().aborted,
            1,
            "the body never observed its token, so it was dropped"
        );
    });
}

#[test]
fn shutdown_retains_every_report_even_past_the_cap() {
    block_on(async {
        // The cap is one, and the buffer is full when shutdown begins. A
        // drain that applied the cap would lose the second task's outcome
        // exactly when a caller is waiting for it, so the two retention
        // paths exist: `reap` is capped because the caller may spawn again,
        // and the drain is not because the total it can produce is already
        // bounded by the in-flight ceiling.
        let mut supervisor = Supervisor::new(1);
        supervisor.spawn(|_token| async {}).await;
        assert!(settle(&mut supervisor).await, "the first task must finish");
        assert_eq!(
            supervisor.stats().reports_dropped,
            0,
            "the premise: the one report so far fit in the buffer"
        );
        supervisor
            .spawn(|_token| async {
                pending::<()>().await;
            })
            .await;

        let report = supervisor.shutdown().await;
        assert_eq!(
            report.outcomes().len(),
            2,
            "shutdown must hand over both outcomes, not just the ones that \
                 fit: {:?}",
            report.outcomes()
        );
        assert_eq!(
            report.stats().reports_dropped,
            0,
            "the drain path is not capped"
        );
        assert_eq!(
            tasks_where(report.outcomes(), TaskOutcome::is_success),
            vec![TaskId(0)],
            "the task that returned is reported as the one that returned"
        );
        assert_eq!(
            tasks_where(report.outcomes(), |outcome| !outcome.is_success()),
            vec![TaskId(1)],
            "and the one that did not is attributed to the other task"
        );
    });
}
