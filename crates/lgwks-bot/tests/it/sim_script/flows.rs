//! The flow-shape families: together's owned branches and fail-fast, step keys and failure location, for's per-item scopes and order, let binding, run in the caller's tenant, and fail classification.

use super::*;

/// One of `trio`'s branches: item `index`, waiting a seeded number of turns.
fn branch_item(sim: &mut sim::Sim, index: u32, least_turns: u32) -> Item {
    Item {
        value: u64::from(index),
        turns: sim.rng().between(least_turns, 6),
        ..Item::default()
    }
}

/// `together`'s branches are futures of this task, not spawned tasks: one
/// poll of the flow starts every branch, and dropping the flow drops every
/// branch with it.
fn together_owned(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let first = branch_item(sim, 0, 1);
        let second = branch_item(sim, 1, 1);
        let third = branch_item(sim, 2, 1);
        // A branch of `t` turns finishes on its `t + 1`th poll, so no branch
        // can finish within the fewest turns any of them takes.
        let fewest = first.turns.min(second.turns).min(third.turns);
        let polls = sim.rng().between(1, fewest);
        let probe = Probe::default();
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let mut running = Box::pin(trio(&scope, &probe, first, second, third));
        let mut context = Context::from_waker(Waker::noop());
        for _ in 0..polls {
            assert!(
                running.as_mut().poll(&mut context).is_pending(),
                "no branch finishes within its turns"
            );
        }
        assert_eq!(
            probe.started.get(),
            3,
            "polling the flow started every branch: they are its own futures"
        );
        assert_eq!(probe.live.get(), 3, "and all three are live inside it");
        drop(running);
        probe.assert_settled();
        assert_eq!(
            probe.abandoned.get(),
            3,
            "dropping the flow dropped every branch it owned"
        );
        sim.record(&format!(
            "turns={},{},{} polls={polls}",
            first.turns, second.turns, third.turns
        ));
        Ok(())
    })
}

/// The first branch of `together` to fail ends it: the failure is that
/// branch's own, and no sibling runs on to its end.
fn together_fail_fast(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let failing = sim.rng().between(0, 2);
        // The failing branch fails on its first poll; every sibling needs at
        // least two, so a sibling reaching its end is one `together` let run.
        let mut parts = [
            branch_item(sim, 0, 1),
            branch_item(sim, 1, 1),
            branch_item(sim, 2, 1),
        ];
        for (index, part) in parts.iter_mut().enumerate() {
            if u32::try_from(index)? == failing {
                part.turns = 0;
                part.fails = true;
            }
        }
        let [first, second, third] = parts;
        let probe = Probe::default();
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let error = failure_of(
            drive(trio(&scope, &probe, first, second, third)),
            "a failing branch must fail the flow",
        )?;
        assert!(
            matches!(error, FlowError::Failed { .. }),
            "the branch's own failure comes back, not a cancellation: {error}"
        );
        assert!(
            error
                .to_string()
                .contains(&format!("part {failing} refused")),
            "the failure is the failing branch's: {error}"
        );
        assert_eq!(
            probe.finished.get(),
            0,
            "no sibling ran on to its end after the first failure"
        );
        probe.assert_settled();
        sim.record(&format!(
            "failing={failing} started={} at={}",
            probe.started.get(),
            error.at()
        ));
        Ok(())
    })
}

/// A step's key is its own: distinct from its flow's and its sibling's, and
/// the same on a rerun, so a step is retried or resumed by its key alone.
fn step_own_key(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let tenant = format!("tenant-{}", sim.rng().below(1_000));
        let turns = sim.rng().between(0, 6);
        let keys_of = || -> Result<Vec<String>, Box<dyn Error>> {
            let probe = Probe::default();
            let scope = tenant_scope(&tenant, CancellationToken::new())?;
            drive(stepped(&scope, &probe, turns, 0))?;
            Ok(probe.keys.take())
        };
        let keys = keys_of()?;
        let again = keys_of()?;
        assert_eq!(
            keys.len(),
            3,
            "the flow, `fetch` and `store` each saw a key"
        );
        let mut distinct = keys.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), 3, "each step's key is its own: {keys:?}");
        assert_eq!(keys, again, "a rerun reproduces every step's key");
        sim.record(&format!("tenant={tenant} keys={keys:?}"));
        Ok(())
    })
}

/// A failure inside a step is located at that step, and no later step runs.
fn step_failure_located(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let failing = sim.rng().between(0, 2);
        let turns = sim.rng().between(0, 6);
        let probe = Probe::default();
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let outcome = drive(stepped(&scope, &probe, turns, failing));
        let entered = probe.keys.borrow().len();
        match (failing, outcome) {
            (0, Ok(value)) => {
                assert_eq!(
                    value, turns,
                    "with no refusal the flow gives back its value"
                );
                assert_eq!(entered, 3, "and both steps ran");
            }
            (1, Err(error)) => {
                assert_eq!(error.at(), "stepped/fetch", "located at `fetch`: {error}");
                assert_eq!(entered, 2, "`store` never ran after `fetch` failed");
            }
            (2, Err(error)) => {
                assert_eq!(error.at(), "stepped/store", "located at `store`: {error}");
                assert_eq!(entered, 3, "`fetch` ran before `store` failed");
            }
            (_, other) => unexpected(&format!("failing={failing} outcome={other:?}"))?,
        }
        sim.record(&format!(
            "failing={failing} turns={turns} entered={entered}"
        ));
        Ok(())
    })
}

/// The path `in_turn` gives item `index`.
fn item_path(index: usize) -> String {
    format!("in_turn/for:item#{index}")
}

/// `for` gives every item its own scope: one key per item, none shared, each
/// scope named by its item's position, and a rerun reproduces the keys.
fn for_scope_per_item(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let list = items(sim, 64);
        let walk = || -> Result<(Vec<String>, Vec<String>), Box<dyn Error>> {
            let probe = Probe::default();
            let scope = tenant_scope("acme", CancellationToken::new())?;
            let paths = drive(in_turn(&scope, &probe, list.clone()))?;
            Ok((probe.keys.take(), paths))
        };
        let (keys, mut paths) = walk()?;
        let (again, _) = walk()?;
        assert_eq!(keys.len(), list.len(), "one key per item");
        let mut distinct = keys.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), keys.len(), "no two items share a key");
        paths.sort_unstable();
        let mut named: Vec<String> = (0..list.len()).map(item_path).collect();
        named.sort_unstable();
        assert_eq!(paths, named, "every item's scope is named by its position");
        assert_eq!(keys, again, "a rerun reproduces every item's key");
        sim.record(&format!("n={} first={:?}", list.len(), keys.first()));
        Ok(())
    })
}

/// `for` runs its items in input order, one at a time.
fn for_in_order(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let list = items(sim, 64);
        let count = list.len();
        let probe = Probe::default();
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let paths = drive(in_turn(&scope, &probe, list))?;
        let ordered: Vec<String> = (0..count).map(item_path).collect();
        assert_eq!(paths, ordered, "the items ran in input order");
        assert!(
            probe.peak.get() <= 1,
            "one item at a time, but {} ran at once",
            probe.peak.get()
        );
        assert_eq!(probe.finished.get(), count, "every item ran to its end");
        probe.assert_settled();
        sim.record(&format!("n={count} peak={}", probe.peak.get()));
        Ok(())
    })
}

/// `let x = <block>:` binds only a block that succeeded: when the block fails,
/// the flow ends with that failure and no line after the `let` runs.
fn let_fails_before_binding(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let fails = sim.rng().chance(500);
        let item = Item {
            value: u64::from(sim.rng().below(1_000)),
            turns: sim.rng().between(0, 6),
            fails,
            ..Item::default()
        };
        let probe = Probe::default();
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let outcome = drive(bound(&scope, &probe, item));
        match outcome {
            Ok(value) if !fails => {
                assert_eq!(
                    value,
                    item.value.wrapping_mul(2),
                    "`x` is the block's value"
                );
                assert_eq!(probe.finished.get(), 1, "the line after the `let` ran");
            }
            Err(ref error) if fails => {
                assert!(
                    matches!(*error, FlowError::Failed { .. }),
                    "the block's own failure ends the flow: {error}"
                );
                assert_eq!(error.at(), "bound/compute", "located at the block");
                assert_eq!(probe.started.get(), 0, "no line after the failed `let` ran");
            }
            ref other => unexpected(&format!("fails={fails} outcome={other:?}"))?,
        }
        sim.record(&format!("fails={fails} turns={}", item.turns));
        Ok(())
    })
}

/// `run` calls a flow inside the caller's scope: the callee sees the caller's
/// tenant, and two tenants calling the same flow never share its key.
fn run_in_callers_tenant(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let first = format!("tenant-{}", sim.rng().below(1_000));
        let second = format!("{first}-other");
        let call = |name: &str| -> Result<(String, Vec<String>), Box<dyn Error>> {
            let probe = Probe::default();
            let scope = tenant_scope(name, CancellationToken::new())?;
            let seen = drive(caller(&scope, &probe))?;
            Ok((seen, probe.keys.take()))
        };
        let (ours, our_keys) = call(&first)?;
        let (theirs, their_keys) = call(&second)?;
        assert_eq!(ours, first, "the callee ran in the first caller's tenant");
        assert_eq!(theirs, second, "and in the second caller's");
        assert_eq!(our_keys.len(), 1, "the callee saw one key");
        assert_ne!(
            our_keys, their_keys,
            "two tenants never share the callee's key"
        );
        sim.record(&format!("tenant={first} key={our_keys:?}"));
        Ok(())
    })
}

/// `fail with` is permanent and `fail transiently with` is retryable, each
/// located at its flow and carrying its reason, so a caller classifies the
/// failure without reading the callee.
fn fail_is_classified(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let transient = sim.rng().chance(500);
        let reason = sim.rng().below(1_000);
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let error = failure_of(
            drive(verdict(&scope, transient, reason)),
            "`fail` must fail the flow",
        )?;
        assert_eq!(
            error.is_retryable(),
            transient,
            "retryable exactly when written `transiently`: {error}"
        );
        assert!(
            matches!(
                (transient, &error),
                (true, FlowError::Transient { .. }) | (false, FlowError::Failed { .. })
            ),
            "the variant names the class: {error:?}"
        );
        assert_eq!(error.at(), "verdict", "located at the flow that failed");
        assert!(
            error.to_string().contains(&format!("reason {reason}")),
            "the reason survives: {error}"
        );
        sim.record(&format!("transient={transient} reason={reason}"));
        Ok(())
    })
}

sixteen_bands!(
    together_owned,
    together_fail_fast,
    step_own_key,
    step_failure_located,
    for_scope_per_item,
    for_in_order,
    let_fails_before_binding,
    run_in_callers_tenant,
    fail_is_classified,
);
