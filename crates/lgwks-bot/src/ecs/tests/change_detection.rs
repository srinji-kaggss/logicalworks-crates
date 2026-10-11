//! `ecs` tests: change detection: what a moved, held or failed source fires.

use super::*;

/// The sequence these tests use: two ticks of 200, two of 503, one of 200.
/// It holds still for a tick at a time, which is what makes a change filter
/// falsifiable: a sequence that moved on every tick would make "changed"
/// and "ran" indistinguishable.
const SCRIPT: [u16; 5] = [200, 200, 503, 503, 200];

/// A yielding source turned once on a live wave, then again after the wave
/// expired, answers with its value; a source still pending on that turn is
/// the one reported stalled.
///
/// This is the interleaving a loaded host produces and an idle one does
/// not: the reaper fires between a source's `Pending` and the turn it woke
/// itself for. `join` waits on the real reaper, which only returns once the
/// wave has expired because both polls are still alive, so the interleaving
/// is forced rather than raced, and a wave that cut off the yielding source
/// unpolled fails here on every run.
#[test]
fn an_expired_wave_still_reads_a_source_ready_on_its_next_turn() -> TestResult {
    let deadline = Duration::from_millis(1);
    let watchdog = PollWatchdog::new(deadline, 2);
    let clock = Clock::wall();
    let armed = AtomicBool::new(false);
    let mut cx = Context::from_waker(Waker::noop());
    let yields = Rc::new(Cell::new(1_u32));
    let mut yielding = WavePoll {
        chain: 0,
        watchdog: &watchdog,
        slot: 0,
        installed: false,
        poll: Box::pin(std::future::poll_fn({
            let yields = Rc::clone(&yields);
            move |turn: &mut Context<'_>| {
                if yields.get() > 0 {
                    yields.set(yields.get().saturating_sub(1));
                    turn.waker().wake_by_ref();
                    Poll::Pending
                } else {
                    Poll::Ready(Ok(None))
                }
            }
        })),
    };
    let mut wedged = WavePoll {
        chain: 1,
        watchdog: &watchdog,
        slot: 1,
        installed: false,
        poll: Box::pin(std::future::pending()),
    };
    assert!(
        yielding
            .turn(&mut cx, &clock, &armed, deadline)
            .is_pending(),
        "a source that yields is pending on its first turn"
    );
    assert!(
        wedged.turn(&mut cx, &clock, &armed, deadline).is_pending(),
        "a wedged source is pending on its first turn"
    );
    assert!(
        armed.load(Ordering::Relaxed),
        "a wave with a pending poll starts its reaper"
    );
    watchdog.join();
    assert!(
        watchdog.expired(),
        "the reaper returned on the wave's expiry"
    );
    assert!(
        matches!(
            yielding.turn(&mut cx, &clock, &armed, deadline),
            Poll::Ready(Ok(None))
        ),
        "a source ready on the turn after expiry answers rather than stalls"
    );
    assert!(
        matches!(
            wedged.turn(&mut cx, &clock, &armed, deadline),
            Poll::Ready(Err(BotError::PollStalled { chain: 1, .. }))
        ),
        "a source still pending after its last turn is the one cut off"
    );
    assert_eq!(
        yields.get(),
        0,
        "the yielding source was turned exactly twice"
    );
    Ok(())
}

#[test]
fn a_condition_fires_only_on_the_tick_its_source_moves() -> TestResult {
    let counter = Rc::new(Cell::new(0));
    let mut bot = EcsBot::builder("scripted")
        .observe(Script::new(SCRIPT.to_vec()))
        .on(|value: &u16| *value >= 500, Count(Rc::clone(&counter)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    let mut fired = Vec::new();
    let mut revisions = Vec::new();
    for _ in 0..5 {
        fired.push(bot.tick()?);
        revisions.push(
            bot.revisions()
                .first()
                .copied()
                .ok_or("a bot with one chain reports one revision")?,
        );
    }

    // `Revision` moves on the ticks the value actually moved: the first
    // poll, the step to 503, and the step back to 200.
    assert_eq!(
        revisions,
        vec![1, 1, 2, 2, 3],
        "Revision must track value movement, not system execution"
    );
    // 503 is held for two ticks; the condition is true on both, and the
    // effect runs once, on the transition.
    assert_eq!(fired, vec![0, 0, 1, 0, 0]);
    assert_eq!(counter.get(), 1);
    Ok(())
}

/// A chain whose source settles must settle.
///
/// This is the regression for a defect found by `bench/`'s fairness gate and
/// by nothing else: the bot fired 896,000 effects where the hand-rolled
/// baseline fired 17,920 for identical input, a 50x over-run that every
/// test in this crate passed through.
///
/// The cause was a lost baseline. A transition *takes* the observed value out
/// of its slot, and while the transition is retained that binding is where
/// the chain's newest value lives. When the transition finished and was
/// dropped instead, the value went with it, leaving the chain with nothing to
/// compare against. The next tick read "no baseline" as "the source moved",
/// re-opened the chain and fired the entry again — and again, every tick
/// after, forever.
///
/// So the assertion is not "it fired" but "it fired **once**", over a run
/// long enough that a per-tick re-fire is unmistakable. `Holds` is what makes
/// it decidable: a source that advances every poll cannot tell a chain that
/// settled from a chain that is still working.
#[test]
fn a_settled_chain_does_not_fire_again_on_every_later_tick() -> TestResult {
    let counter = Rc::new(Cell::new(0));
    let mut bot = counting_bot("held", &counter, test_effects()?)?;

    // The first tick is the movement: the chain opens and the effect runs.
    assert_eq!(bot.tick()?, 1, "the movement opens the chain");

    // Twenty more ticks in which nothing whatever happens. A lost baseline
    // shows up here as `1` rather than `0` on the second tick and every one
    // after it.
    for tick in 0_u32..20 {
        assert_eq!(
            bot.tick()?,
            0,
            "tick {}: the source held still, so the chain had nothing to run",
            tick.saturating_add(2)
        );
    }

    assert_eq!(
        counter.get(),
        1,
        "the effect is owed once per movement, not once per tick"
    );
    // The revision is the substrate's own record of movement, and it is the
    // other half of the same claim: one movement, one bump.
    assert_eq!(
        bot.revisions().first().copied(),
        Some(1),
        "Revision counts movements, so a settled chain's is still 1"
    );
    Ok(())
}

/// A detached digest cannot prove that an async poll returned the same
/// snapshot. The bot must continue to poll until an observer can bind its
/// revision to the returned value in one operation.
#[test]
fn a_source_with_a_detached_digest_is_polled_while_it_holds_still() -> TestResult {
    let polls = Rc::new(Cell::new(0));
    let counter = Rc::new(Cell::new(0));
    let mut bot = EcsBot::builder("lazy")
        .observe(Counted::new(200, Rc::clone(&polls)))
        .on(|value: &u16| *value >= 200, Count(Rc::clone(&counter)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    assert_eq!(bot.tick()?, 1, "the first tick polls");
    assert_eq!(polls.get(), 1, "and polls exactly once");

    for tick in 0_u32..20 {
        assert_eq!(
            bot.tick()?,
            0,
            "tick {}: nothing moved",
            tick.saturating_add(2)
        );
    }
    assert_eq!(
        polls.get(),
        21,
        "a detached digest cannot suppress a later async poll"
    );
    assert_eq!(counter.get(), 1, "and must not fire the effect again");
    Ok(())
}

/// An A → B → A source race must not leave B held while stable A is skipped.
///
/// The first tick reads the legacy fingerprint at A, lets the observer
/// sample B, returns the live source to A, and only then admits B. The next
/// tick has to poll and deliver A. The retired cache used the earlier A
/// fingerprint for B, saw A as quiet on the next tick, and left B held.
#[test]
fn a_detached_digest_cannot_suppress_the_final_stable_aba_state() -> TestResult {
    let value = Rc::new(Cell::new(0));
    let phase = Rc::new(Cell::new(0));
    let polls = Rc::new(Cell::new(0));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut bot = EcsBot::builder("aba")
        .observe(AbaSource::new(
            Rc::clone(&value),
            Rc::clone(&phase),
            Rc::clone(&polls),
        ))
        .on(|_: &u16| true, Record(Rc::clone(&seen)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    let mut tick = Box::pin(bot.tick_async());
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    if !matches!(tick.as_mut().poll(&mut cx), Poll::Pending) {
        return Err("the first poll must pause before sampling B".into());
    }
    value.set(1);
    if !matches!(tick.as_mut().poll(&mut cx), Poll::Pending) {
        return Err("the source must pause after sampling B".into());
    }
    value.set(0);
    match tick.as_mut().poll(&mut cx) {
        Poll::Ready(Ok(fired)) => assert_eq!(fired, 1, "the sampled B action fires"),
        Poll::Ready(Err(error)) => return Err(format!("the B tick failed: {error}").into()),
        Poll::Pending => return Err("the third poll must admit the sampled B value".into()),
    }
    drop(tick);
    assert_eq!(*seen.borrow(), vec![1], "the first tick delivers sampled B");

    assert_eq!(bot.tick()?, 1, "the next tick must observe stable A");
    assert_eq!(
        *seen.borrow(),
        vec![1, 0],
        "stable A is delivered instead of being suppressed by B's stale digest"
    );
    assert_eq!(polls.get(), 2, "both B and the final stable A were polled");
    Ok(())
}

/// Value comparison still avoids another effect when a source holds still.
#[test]
fn a_source_value_that_moves_is_polled_again_and_fires() -> TestResult {
    let polls = Rc::new(Cell::new(0));
    let counter = Rc::new(Cell::new(0));
    let source = Counted::new(200, Rc::clone(&polls));
    let value = Rc::clone(&source.value);
    let mut bot = EcsBot::builder("lazy-moving")
        .observe(source)
        .on(|value: &u16| *value >= 200, Count(Rc::clone(&counter)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    assert_eq!(bot.tick()?, 1, "the first value fires");
    assert_eq!(bot.tick()?, 0, "and holds");
    assert_eq!(
        polls.get(),
        2,
        "the second tick polls and compares its value"
    );

    value.set(503);
    assert_eq!(bot.tick()?, 1, "the movement is seen, not skipped");
    assert_eq!(
        polls.get(),
        3,
        "the tick paid for a value to see the movement"
    );

    assert_eq!(bot.tick()?, 0, "and settles again");
    assert_eq!(polls.get(), 4, "the stable value is polled and compared");
    assert_eq!(counter.get(), 2, "two movements, two effects");
    Ok(())
}

/// A source without a digest keeps the same value-comparison behavior.
#[test]
fn a_source_without_a_digest_is_polled_every_tick() -> TestResult {
    let counter = Rc::new(Cell::new(0));
    let mut bot = counting_bot("unguarded", &counter, test_effects()?)?;

    assert_eq!(bot.tick()?, 1);
    for _ in 0..5 {
        assert_eq!(bot.tick()?, 0);
    }
    // Every tick polls; the effect still fires once because the value did
    // not move.
    assert_eq!(
        counter.get(),
        1,
        "the value decides the chain, not the digest"
    );
    Ok(())
}

#[test]
fn building_without_the_grant_is_refused() -> TestResult {
    let counter = Rc::new(Cell::new(0));
    match EcsBot::builder("ungranted")
        .observe(Script::new(SCRIPT.to_vec()))
        .on(|value: &u16| *value >= 500, Count(counter))
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())
    {
        Ok(_) => Err("a source requiring `bot.net` built without it".into()),
        Err(error) => {
            assert!(
                matches!(error, BotError::CapabilityDenied { .. }),
                "expected CapabilityDenied, got {error:?}"
            );
            Ok(())
        }
    }
}

/// Admission as one answer rather than a loop.
///
/// This bot is short of `bot.net` on its source and `bot.fs` on its action.
/// Admission that returned at the first unmet requirement told the caller
/// `bot.net`, and `bot.fs` only appeared on the next build — so a person
/// repairing a bot discovered its requirements one refusal at a time. The
/// gate already knows both; one refusal now carries both, and each says
/// which domain declared it.
#[test]
fn admission_names_every_unmet_requirement_and_the_domain_that_declared_it() -> TestResult {
    match EcsBot::builder("short")
        .observe(Holds::new(1))
        .on(
            |value: &u16| *value > 0,
            NeedsCaps {
                caps: vec![Cap::fs()],
                domain: "test::writes",
            },
        )
        .with_effects(test_effects()?)
        .build(&GrantSet::empty())
    {
        Ok(_) => Err("a bot requiring two ungranted capabilities built".into()),
        Err(BotError::CapabilityDenied { deficit }) => {
            let named: Vec<(&str, Option<&str>)> = deficit
                .shortages()
                .map(|shortage| {
                    (
                        shortage.required().as_str(),
                        shortage.demand().map(Demand::domain),
                    )
                })
                .collect();
            assert_eq!(
                named,
                vec![
                    (Cap::NET, Some("test::holds")),
                    (Cap::FS, Some("test::writes")),
                ],
                "one refusal must name both requirements and both domains: {deficit}"
            );
            // And the repair is one call, not one per refusal.
            assert!(
                deficit
                    .to_grant_set()
                    .admit(&[Cap::net(), Cap::fs()])
                    .is_ok(),
                "the deficit must derive the whole repair: {deficit}"
            );
            Ok(())
        }
        Err(other) => Err(format!("expected a capability denial, got {other:?}").into()),
    }
}

#[test]
fn a_poll_error_is_returned_and_commits_nothing() -> TestResult {
    let counter = Rc::new(Cell::new(0));
    let mut bot = EcsBot::builder("exhausting")
        .observe(Exhausting {
            remaining: Rc::new(Cell::new(1)),
            caps: vec![Cap::net()],
        })
        .on(|value: &u16| *value >= 500, Count(Rc::clone(&counter)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    assert_eq!(
        bot.tick()?,
        0,
        "the first poll answers 200 and fires nothing"
    );
    let after_success = bot.revisions();

    // The second poll fails. The tick reports it, and the observed value
    // from the first tick must survive: a failed tick leaves the world as
    // it was rather than half-updated.
    let error = refused(bot.tick(), |_| {
        String::from("a failing poll was reported as a successful tick")
    })?;
    assert!(
        matches!(error, BotError::DomainError { .. }),
        "expected the observer's own error, got {error:?}"
    );
    assert_eq!(
        bot.revisions(),
        after_success,
        "a failed tick must not commit a new Revision"
    );
    assert_eq!(
        counter.get(),
        0,
        "a failed poll fires nothing: the action must not have run"
    );
    Ok(())
}

/// A failed sibling poll must not teach the fingerprint cache about a
/// value the fold refused to hold.
///
/// The counterexample, stated as issue #99 states it: A reports payload `1`
/// with digest `Some(1)` and B fails its first poll. The first tick fires
/// nothing under the all-or-nothing observation contract. Before the
/// repair A's digest was published anyway, so the second tick read A as
/// quiet and skipped it despite holding no committed payload — A's action
/// never ran unless A changed again.
#[test]
fn a_failed_sibling_does_not_publish_the_successful_sources_digest() -> TestResult {
    let left_polls = Rc::new(Cell::new(0));
    let right_polls = Rc::new(Cell::new(0));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let left = Switched::new(1, false, Rc::clone(&left_polls));
    let mid = Switched::new(9, true, Rc::clone(&right_polls));
    let mid_fail = Rc::clone(&mid.fail);
    let mut bot = EcsBot::builder("sibling")
        .observe(left)
        .on(|value: &u16| *value >= 1, Record(Rc::clone(&seen)))
        .observe(mid)
        .on(
            |value: &u16| *value >= 9,
            Record(Rc::new(RefCell::new(Vec::new()))),
        )
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    let error = refused(bot.tick(), |fired| {
        format!("a failing sibling was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::DomainError { .. }),
        "expected the sibling's own error, got {error:?}"
    );
    assert!(
        seen.borrow().is_empty(),
        "the failed fold must not commit A's payload"
    );
    assert_eq!(left_polls.get(), 1, "A was polled once on the first tick");
    assert_eq!(right_polls.get(), 1, "and so was B");

    mid_fail.set(false);
    assert_eq!(bot.tick()?, 2, "both chains recover and both actions fire");
    assert_eq!(
        *seen.borrow(),
        vec![1],
        "A's uncommitted payload is committed on recovery, with 1 as its input"
    );
    assert_eq!(
        left_polls.get(),
        2,
        "A is polled again: its digest must not have been published early"
    );
    assert_eq!(right_polls.get(), 2, "B recovers on a poll of its own");
    Ok(())
}

/// The second half of issue #99's public scenario: a *previously committed*
/// value changes while a sibling fails, and the recovery must keep the
/// transition to the new value rather than the older committed one.
#[test]
fn a_committed_source_that_moves_during_a_sibling_failure_is_not_lost() -> TestResult {
    let left_polls = Rc::new(Cell::new(0));
    let right_polls = Rc::new(Cell::new(0));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let left = Switched::new(0, false, Rc::clone(&left_polls));
    let left_value = Rc::clone(&left.value);
    let mid = Switched::new(9, false, Rc::clone(&right_polls));
    let mid_fail = Rc::clone(&mid.fail);
    let mut bot = EcsBot::builder("recovery")
        .observe(left)
        .on(|value: &u16| *value >= 1, Record(Rc::clone(&seen)))
        .observe(mid)
        .on(|_: &u16| true, Record(Rc::new(RefCell::new(Vec::new()))))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    assert_eq!(bot.tick()?, 1, "the first tick commits A=0 and B=9");
    assert_eq!(
        *seen.borrow(),
        Vec::<u16>::new(),
        "A=0 does not meet its condition"
    );
    assert_eq!(left_polls.get(), 1);

    left_value.set(1);
    mid_fail.set(true);
    let error = refused(bot.tick(), |fired| {
        format!("a failing sibling was reported as {fired} fired")
    })?;
    assert!(
        matches!(error, BotError::DomainError { .. }),
        "expected the sibling's own error, got {error:?}"
    );
    assert!(
        seen.borrow().is_empty(),
        "the failed fold must not commit A=1"
    );

    mid_fail.set(false);
    assert_eq!(
        bot.tick()?,
        1,
        "recovery commits A's move; B is unchanged and fires nothing"
    );
    assert_eq!(
        *seen.borrow(),
        vec![1],
        "the transition to 1 is retained, not the older committed 0"
    );
    assert_eq!(
        left_polls.get(),
        3,
        "A is polled on the failing tick and again on recovery"
    );
    Ok(())
}

/// Several siblings fail on one tick; every successful source must still
/// deliver its payload once the fold can commit.
#[test]
fn several_failed_siblings_do_not_suppress_the_sources_that_succeeded() -> TestResult {
    let polls_left = Rc::new(Cell::new(0));
    let polls_mid = Rc::new(Cell::new(0));
    let polls_right = Rc::new(Cell::new(0));
    let seen_left = Rc::new(RefCell::new(Vec::new()));
    let seen_right = Rc::new(RefCell::new(Vec::new()));
    let left = Switched::new(1, false, Rc::clone(&polls_left));
    let mid = Switched::new(2, true, Rc::clone(&polls_mid));
    let right = Switched::new(3, false, Rc::clone(&polls_right));
    // A second failing source, so "the first error wins" is not the only
    // thing the fold is being asked to survive.
    let far = Switched::new(4, true, Rc::new(Cell::new(0)));
    let mid_fail = Rc::clone(&mid.fail);
    let far_fail = Rc::clone(&far.fail);
    let mut bot = EcsBot::builder("multi-fail")
        .observe(left)
        .on(|value: &u16| *value >= 1, Record(Rc::clone(&seen_left)))
        .observe(mid)
        .on(|_: &u16| true, Record(Rc::new(RefCell::new(Vec::new()))))
        .observe(right)
        .on(|value: &u16| *value >= 3, Record(Rc::clone(&seen_right)))
        .observe(far)
        .on(|_: &u16| true, Record(Rc::new(RefCell::new(Vec::new()))))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    if let Ok(fired) = bot.tick() {
        return Err(format!("failed siblings were reported as {fired} fired").into());
    }
    assert!(
        seen_left.borrow().is_empty(),
        "nothing commits on a failed fold"
    );
    assert!(
        seen_right.borrow().is_empty(),
        "nothing commits on a failed fold"
    );
    assert_eq!(polls_left.get(), 1);
    assert_eq!(polls_right.get(), 1);

    mid_fail.set(false);
    far_fail.set(false);
    assert_eq!(bot.tick()?, 4, "every chain recovers and fires");
    assert_eq!(*seen_left.borrow(), vec![1], "A delivers its payload");
    assert_eq!(*seen_right.borrow(), vec![3], "C delivers its payload");
    assert_eq!(
        polls_left.get(),
        2,
        "A was re-polled after the aborted fold"
    );
    assert_eq!(
        polls_right.get(),
        2,
        "C was re-polled after the aborted fold"
    );
    Ok(())
}

/// A tick dropped mid-poll leaves the next tick able to observe normally.
#[test]
fn cancelling_a_poll_leaves_the_next_observation_usable() -> TestResult {
    let polls = Rc::new(Cell::new(0));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let yielded = Rc::new(Cell::new(false));
    let source = Yielding::new(200, Rc::clone(&yielded), Rc::clone(&polls));
    let mut bot = EcsBot::builder("cancel")
        .observe(source)
        .on(|value: &u16| *value >= 200, Record(Rc::clone(&seen)))
        .with_effects(test_effects()?)
        .build(&net_grants())?;

    // Drive the tick until the source parks, then drop it on the floor.
    {
        let mut tick = Box::pin(bot.tick_async());
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        match tick.as_mut().poll(&mut cx) {
            Poll::Ready(_) => {
                return Err("the yielding source resolved before it was cancelled".into());
            }
            Poll::Pending => {}
        }
        drop(tick);
    }
    assert!(seen.borrow().is_empty(), "a cancelled tick commits nothing");

    assert_eq!(bot.tick()?, 1, "the recovery tick completes and fires");
    assert_eq!(*seen.borrow(), vec![200], "with the payload as its input");
    assert_eq!(
        polls.get(),
        2,
        "the source is polled once per tick across the cancel"
    );

    assert_eq!(bot.tick()?, 0, "and the next tick compares the same value");
    assert_eq!(polls.get(), 3, "the next tick polls after a cancellation");
    Ok(())
}
