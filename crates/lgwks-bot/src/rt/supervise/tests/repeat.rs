//! The budgeted loop: budgets, cancellation and yielding.

use super::super::repeat::repeat;
use super::super::supervisor::Supervisor;
use super::super::{Budget, Outcome};
use super::{budget_of, settle, spawn_heartbeat, yield_until};
use crate::rt::cancel::CancellationToken;
use crate::rt::runtime::block_on;
use crate::rt::task::yield_now;
use std::future::pending;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[test]
fn a_large_finite_budget_still_yields_to_a_sibling_task() {
    // A body that resolves immediately never suspends the loop, so before
    // the loop yielded, one poll of it ran the entire budget and no other
    // task on the executor got to run at all. The budget is finite so the
    // old behaviour is a failed assertion here rather than a hung suite; a
    // current-thread runtime was the case that deadlocked outright, because
    // the task that would have cancelled could not even be scheduled.
    const BUDGET: u64 = 1_000_000;
    block_on(async {
        let beats = Arc::new(AtomicU64::new(0));
        let ran = Arc::new(AtomicU64::new(0));
        // The heartbeat count the body saw on its *first* iteration, and the
        // highest it saw on any later one. The heartbeat advancing between
        // two of the loop's own iterations is the observation that the
        // executor was shared: one uninterruptible poll of the loop sees a
        // single frozen value for its whole run, whatever that value is.
        let first_seen = Arc::new(AtomicU64::new(u64::MAX));
        let later_seen = Arc::new(AtomicU64::new(0));

        let mut supervisor = Supervisor::new(4);
        spawn_heartbeat(&mut supervisor, &beats).await;

        let body_ran = Arc::clone(&ran);
        let body_first = Arc::clone(&first_seen);
        let body_later = Arc::clone(&later_seen);
        let body_beats = Arc::clone(&beats);
        supervisor
            .spawn_repeating(budget_of(BUDGET), move |_tick| {
                let body_ran = Arc::clone(&body_ran);
                let body_first = Arc::clone(&body_first);
                let body_later = Arc::clone(&body_later);
                let body_beats = Arc::clone(&body_beats);
                async move {
                    body_ran.fetch_add(1, Ordering::SeqCst);
                    let now = body_beats.load(Ordering::SeqCst);
                    if now < body_first.load(Ordering::SeqCst) {
                        body_first.store(now, Ordering::SeqCst);
                    }
                    if now > body_later.load(Ordering::SeqCst) {
                        body_later.store(now, Ordering::SeqCst);
                    }
                }
            })
            .await;

        // Give the repeating worker its first poll before asserting on what
        // it observed: a worker cancelled before it starts proves nothing.
        assert!(
            yield_until(|| ran.load(Ordering::SeqCst) > 0, 1_000).await,
            "the repeating worker never took a single iteration"
        );
        assert!(
            yield_until(
                || later_seen.load(Ordering::SeqCst) > first_seen.load(Ordering::SeqCst),
                1_000
            )
            .await,
            "the repeating body ran {} of its {BUDGET} iterations seeing the heartbeat count \
                 frozen at {}, so one poll of it held the executor for the whole run",
            ran.load(Ordering::SeqCst),
            first_seen.load(Ordering::SeqCst)
        );

        supervisor.shutdown().await;
    });
}

#[test]
fn a_cancel_from_a_sibling_task_stops_an_ongoing_ready_body() {
    // The case from the issue, in miniature: a loop whose body resolves
    // immediately, so it never suspends of its own accord, and a canceller
    // on a *different* task. The canceller can only run if the loop hands
    // the executor back, so one uninterruptible poll of the loop makes the
    // cancel unobservable — which on a current-thread runtime is not a
    // delay, it is a deadlock, because the canceller may never run at all.
    //
    // The body carries a brake so an unbounded loop still ends when the
    // cancel cannot land, turning the old behaviour into a failed assertion
    // rather than a hung suite. The assertions then distinguish the two:
    // the brake is three orders of magnitude above what a yielding loop
    // needs.
    const BRAKE: u64 = 1_000_000;
    block_on(async {
        let root = CancellationToken::new();
        let ran = Arc::new(AtomicU64::new(0));

        let mut supervisor = Supervisor::new(2);
        // The canceller holds the root, so its `cancel` reaches the loop's
        // child token. It waits for the loop's first iteration before
        // cancelling, so the cancel provably lands on a running loop rather
        // than on a task that never started.
        let canceller_root = root.clone();
        let canceller_ran = Arc::clone(&ran);
        supervisor
            .spawn(move |_task_token| async move {
                while canceller_ran.load(Ordering::SeqCst) == 0 {
                    yield_now().await;
                }
                canceller_root.cancel();
            })
            .await;

        let loop_ran = Arc::clone(&ran);
        let loop_root = root.clone();
        let brake = root.clone();
        let outcome = repeat(&loop_root, Budget::Ongoing, move |_tick| {
            let brake = brake.clone();
            let loop_ran = Arc::clone(&loop_ran);
            async move {
                let count = loop_ran.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                if count >= BRAKE {
                    brake.cancel();
                }
            }
        })
        .await;

        assert!(
            outcome.was_cancelled(),
            "the loop ended as {outcome:?} rather than as cancelled"
        );
        let iterations = outcome.iterations();
        assert!(
            iterations < BRAKE,
            "the loop ran to its own {BRAKE}-iteration brake ({iterations} iterations) \
                 instead of observing the cancel from the sibling task"
        );

        // Drain the canceller, then check the body is not invoked again:
        // the cancel is a boundary, not a hint.
        assert!(
            settle(&mut supervisor).await,
            "the canceller task never finished"
        );
        let settled = ran.load(Ordering::SeqCst);
        for _ in 0..64 {
            yield_now().await;
        }
        assert_eq!(
            ran.load(Ordering::SeqCst),
            settled,
            "the body was invoked again after the loop observed the cancel"
        );
    });
}

#[test]
fn shutdown_lands_on_an_ongoing_ready_body() {
    // The shutdown half of the same defect: `Supervisor::shutdown` cancels
    // and drains, and before the loop yielded it could not even be reached
    // until the repeating task returned on its own.
    //
    // The brake holds the loop's *own* token — the one `repeat` races — so
    // an unbounded loop whose cancel cannot land ends at the brake rather
    // than running forever, which is what keeps this a failed assertion
    // instead of a hung suite.
    const BRAKE: u64 = 1_000_000;
    block_on(async {
        let ran = Arc::new(AtomicU64::new(0));
        let mut supervisor = Supervisor::new(1);
        let body_ran = Arc::clone(&ran);
        supervisor
            .spawn(move |token| async move {
                let brake = token.clone();
                let _outcome = repeat(&token, Budget::Ongoing, move |_tick| {
                    let brake = brake.clone();
                    let body_ran = Arc::clone(&body_ran);
                    async move {
                        let count = body_ran.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                        if count >= BRAKE {
                            brake.cancel();
                        }
                    }
                })
                .await;
            })
            .await;

        // Prove the loop is running before shutting down, so the shutdown
        // lands on a live body rather than on a task that never polled.
        assert!(
            yield_until(|| ran.load(Ordering::SeqCst) > 0, 1_000).await,
            "the repeating worker never took a single iteration"
        );
        supervisor.shutdown().await;

        let iterations = ran.load(Ordering::SeqCst);
        assert!(
            iterations < BRAKE,
            "the loop ran to its own {BRAKE}-iteration brake ({iterations} iterations) \
                 instead of being stopped by the shutdown"
        );
    });
}

#[test]
fn an_already_cancelled_token_never_invokes_the_body() {
    let token = CancellationToken::new();
    token.cancel();
    let ran = AtomicU64::new(0);
    let body_ran = &ran;
    let outcome = block_on(repeat(&token, Budget::Ongoing, move |_tick| async move {
        body_ran.fetch_add(1, Ordering::SeqCst);
    }));
    assert_eq!(
        outcome,
        Outcome::Cancelled { iterations: 0 },
        "a token cancelled before the loop starts must stop it with no iterations"
    );
    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "the body must not be invoked at all past a cancellation boundary"
    );
}

#[test]
fn a_budget_of_iterations_stops_at_the_limit() {
    let token = CancellationToken::new();
    let ticks = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&ticks);
    let outcome = block_on(repeat(&token, budget_of(3), move |_tick| {
        let counter = Arc::clone(&counter);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
        }
    }));
    assert_eq!(
        outcome,
        Outcome::Exhausted { iterations: 3 },
        "a three-iteration budget must run the body exactly three times"
    );
    assert_eq!(
        ticks.load(Ordering::SeqCst),
        3,
        "the body must have been entered once per iteration"
    );
}

/// Run an ongoing `repeat` whose body cancels its own token on entry and
/// then runs `rest`.
fn repeat_cancelling_itself<Rest, Fut>(rest: Rest) -> Outcome
where
    Rest: Fn() -> Fut,
    Fut: Future<Output = ()>,
{
    let token = CancellationToken::new();
    let trigger = token.clone();
    block_on(repeat(&token, Budget::Ongoing, move |_tick| {
        trigger.cancel();
        rest()
    }))
}

#[test]
fn an_ongoing_budget_stops_when_the_token_is_cancelled() {
    let outcome = repeat_cancelling_itself(|| async {});
    assert_eq!(
        outcome,
        Outcome::Cancelled { iterations: 1 },
        "a cancel during the first iteration must end the loop after it"
    );
}

#[test]
fn cancellation_interrupts_a_body_that_is_still_awaiting() {
    // The body never resolves. If cancellation were only observed between
    // iterations, the loop would have to await it to completion and the
    // test would hang rather than fail.
    let outcome = repeat_cancelling_itself(pending::<()>);
    assert!(
        outcome.was_cancelled(),
        "an in-flight body must be dropped at the cancel, got {outcome:?}"
    );
}

#[test]
fn a_budget_of_duration_expires() {
    let token = CancellationToken::new();
    let outcome = block_on(repeat(
        &token,
        Budget::For(Duration::ZERO),
        move |_tick| async {},
    ));
    assert_eq!(
        outcome,
        Outcome::Exhausted { iterations: 0 },
        "a zero duration must expire before the first iteration"
    );
}

#[test]
fn a_supervisor_reports_what_it_has_run() {
    block_on(async {
        let mut supervisor = Supervisor::new(2);
        supervisor.spawn(|_token| async {}).await;
        supervisor.spawn(|_token| async {}).await;
        assert!(settle(&mut supervisor).await, "both tasks must finish");
        let stats = supervisor.stats();
        assert_eq!(stats.spawned, 2, "both tasks must be counted as spawned");
        assert_eq!(
            stats.completed, 2,
            "both tasks must be counted as completed"
        );
        assert_eq!(
            stats.succeeded, 2,
            "both tasks returned, so both must be counted as succeeded"
        );
        assert_eq!(stats.in_flight(), 0, "nothing must remain in flight");
        assert_eq!(stats.refused, 0, "nothing must have been refused");
    });
}
