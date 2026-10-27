//! Simulation family: the `together`, step, `for`, `let` and `run` words.
//!
//! The orchestration words beside `each` and `retry`: `together` joins owned
//! branches, `step` names a keyed unit, `for` gives every item its own scope,
//! `let` binds only a success, `run` calls a flow in the caller's scope, and
//! `fail` classifies. Every scenario runs flows the real `script!` macro
//! expanded, on the real `lgwks_bot::script` runtime. Nothing here reimplements
//! a word: what the seed controls is the input, which step or item fails and
//! how, and which tenants call. Time is not consulted.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `together_owned` | one poll of the flow starts every `together` branch, and dropping the flow drops every branch |
//! | `together_fail_fast` | the first failing branch ends `together` with its own failure, and no sibling runs on to its end |
//! | `step_own_key` | a step's key is distinct from its flow's and its sibling's, and a rerun reproduces it |
//! | `step_failure_located` | a failure inside a step is located at that step, and no later step runs |
//! | `for_scope_per_item` | `for` gives each item its own scope and key, named by its position |
//! | `for_in_order` | `for` runs its items in input order, one at a time |
//! | `let_fails_before_binding` | a failing `let x = <block>:` ends the flow with the block's failure before any later line runs |
//! | `run_in_callers_tenant` | a `run` callee sees the caller's tenant, and two tenants never share its key |
//! | `fail_is_classified` | `fail with` is permanent and `fail transiently with` retryable, each located and carrying its reason |
//!
//! Each declared test sweeps its band twice and requires identical trace
//! hashes, so a nondeterministic run fails even when every assertion passes.

#![cfg(feature = "script")]

use std::error::Error;
use std::future::Future;
use std::task::{Context, Waker};

use lgwks_bot::rt::sync::CancellationToken;
use lgwks_bot::script::FlowError;

use crate::band_family;
use crate::sim::{self, Band};
use crate::sim_script::{
    Item, Probe, bound, caller, drive, failure_of, in_turn, items, stepped, tenant_scope, trio,
    unexpected, verdict,
};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

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

band_family::band_family! {
    together_owned_band_00 => together_owned, 0;
    together_owned_band_01 => together_owned, 1;
    together_owned_band_02 => together_owned, 2;
    together_owned_band_03 => together_owned, 3;
    together_owned_band_04 => together_owned, 4;
    together_owned_band_05 => together_owned, 5;
    together_owned_band_06 => together_owned, 6;
    together_owned_band_07 => together_owned, 7;
    together_owned_band_08 => together_owned, 8;
    together_owned_band_09 => together_owned, 9;
    together_owned_band_10 => together_owned, 10;
    together_owned_band_11 => together_owned, 11;
    together_owned_band_12 => together_owned, 12;
    together_owned_band_13 => together_owned, 13;
    together_owned_band_14 => together_owned, 14;
    together_owned_band_15 => together_owned, 15;
    together_fail_fast_band_00 => together_fail_fast, 0;
    together_fail_fast_band_01 => together_fail_fast, 1;
    together_fail_fast_band_02 => together_fail_fast, 2;
    together_fail_fast_band_03 => together_fail_fast, 3;
    together_fail_fast_band_04 => together_fail_fast, 4;
    together_fail_fast_band_05 => together_fail_fast, 5;
    together_fail_fast_band_06 => together_fail_fast, 6;
    together_fail_fast_band_07 => together_fail_fast, 7;
    together_fail_fast_band_08 => together_fail_fast, 8;
    together_fail_fast_band_09 => together_fail_fast, 9;
    together_fail_fast_band_10 => together_fail_fast, 10;
    together_fail_fast_band_11 => together_fail_fast, 11;
    together_fail_fast_band_12 => together_fail_fast, 12;
    together_fail_fast_band_13 => together_fail_fast, 13;
    together_fail_fast_band_14 => together_fail_fast, 14;
    together_fail_fast_band_15 => together_fail_fast, 15;
    step_own_key_band_00 => step_own_key, 0;
    step_own_key_band_01 => step_own_key, 1;
    step_own_key_band_02 => step_own_key, 2;
    step_own_key_band_03 => step_own_key, 3;
    step_own_key_band_04 => step_own_key, 4;
    step_own_key_band_05 => step_own_key, 5;
    step_own_key_band_06 => step_own_key, 6;
    step_own_key_band_07 => step_own_key, 7;
    step_own_key_band_08 => step_own_key, 8;
    step_own_key_band_09 => step_own_key, 9;
    step_own_key_band_10 => step_own_key, 10;
    step_own_key_band_11 => step_own_key, 11;
    step_own_key_band_12 => step_own_key, 12;
    step_own_key_band_13 => step_own_key, 13;
    step_own_key_band_14 => step_own_key, 14;
    step_own_key_band_15 => step_own_key, 15;
    step_failure_located_band_00 => step_failure_located, 0;
    step_failure_located_band_01 => step_failure_located, 1;
    step_failure_located_band_02 => step_failure_located, 2;
    step_failure_located_band_03 => step_failure_located, 3;
    step_failure_located_band_04 => step_failure_located, 4;
    step_failure_located_band_05 => step_failure_located, 5;
    step_failure_located_band_06 => step_failure_located, 6;
    step_failure_located_band_07 => step_failure_located, 7;
    step_failure_located_band_08 => step_failure_located, 8;
    step_failure_located_band_09 => step_failure_located, 9;
    step_failure_located_band_10 => step_failure_located, 10;
    step_failure_located_band_11 => step_failure_located, 11;
    step_failure_located_band_12 => step_failure_located, 12;
    step_failure_located_band_13 => step_failure_located, 13;
    step_failure_located_band_14 => step_failure_located, 14;
    step_failure_located_band_15 => step_failure_located, 15;
    for_scope_per_item_band_00 => for_scope_per_item, 0;
    for_scope_per_item_band_01 => for_scope_per_item, 1;
    for_scope_per_item_band_02 => for_scope_per_item, 2;
    for_scope_per_item_band_03 => for_scope_per_item, 3;
    for_scope_per_item_band_04 => for_scope_per_item, 4;
    for_scope_per_item_band_05 => for_scope_per_item, 5;
    for_scope_per_item_band_06 => for_scope_per_item, 6;
    for_scope_per_item_band_07 => for_scope_per_item, 7;
    for_scope_per_item_band_08 => for_scope_per_item, 8;
    for_scope_per_item_band_09 => for_scope_per_item, 9;
    for_scope_per_item_band_10 => for_scope_per_item, 10;
    for_scope_per_item_band_11 => for_scope_per_item, 11;
    for_scope_per_item_band_12 => for_scope_per_item, 12;
    for_scope_per_item_band_13 => for_scope_per_item, 13;
    for_scope_per_item_band_14 => for_scope_per_item, 14;
    for_scope_per_item_band_15 => for_scope_per_item, 15;
    for_in_order_band_00 => for_in_order, 0;
    for_in_order_band_01 => for_in_order, 1;
    for_in_order_band_02 => for_in_order, 2;
    for_in_order_band_03 => for_in_order, 3;
    for_in_order_band_04 => for_in_order, 4;
    for_in_order_band_05 => for_in_order, 5;
    for_in_order_band_06 => for_in_order, 6;
    for_in_order_band_07 => for_in_order, 7;
    for_in_order_band_08 => for_in_order, 8;
    for_in_order_band_09 => for_in_order, 9;
    for_in_order_band_10 => for_in_order, 10;
    for_in_order_band_11 => for_in_order, 11;
    for_in_order_band_12 => for_in_order, 12;
    for_in_order_band_13 => for_in_order, 13;
    for_in_order_band_14 => for_in_order, 14;
    for_in_order_band_15 => for_in_order, 15;
    let_fails_before_binding_band_00 => let_fails_before_binding, 0;
    let_fails_before_binding_band_01 => let_fails_before_binding, 1;
    let_fails_before_binding_band_02 => let_fails_before_binding, 2;
    let_fails_before_binding_band_03 => let_fails_before_binding, 3;
    let_fails_before_binding_band_04 => let_fails_before_binding, 4;
    let_fails_before_binding_band_05 => let_fails_before_binding, 5;
    let_fails_before_binding_band_06 => let_fails_before_binding, 6;
    let_fails_before_binding_band_07 => let_fails_before_binding, 7;
    let_fails_before_binding_band_08 => let_fails_before_binding, 8;
    let_fails_before_binding_band_09 => let_fails_before_binding, 9;
    let_fails_before_binding_band_10 => let_fails_before_binding, 10;
    let_fails_before_binding_band_11 => let_fails_before_binding, 11;
    let_fails_before_binding_band_12 => let_fails_before_binding, 12;
    let_fails_before_binding_band_13 => let_fails_before_binding, 13;
    let_fails_before_binding_band_14 => let_fails_before_binding, 14;
    let_fails_before_binding_band_15 => let_fails_before_binding, 15;
    run_in_callers_tenant_band_00 => run_in_callers_tenant, 0;
    run_in_callers_tenant_band_01 => run_in_callers_tenant, 1;
    run_in_callers_tenant_band_02 => run_in_callers_tenant, 2;
    run_in_callers_tenant_band_03 => run_in_callers_tenant, 3;
    run_in_callers_tenant_band_04 => run_in_callers_tenant, 4;
    run_in_callers_tenant_band_05 => run_in_callers_tenant, 5;
    run_in_callers_tenant_band_06 => run_in_callers_tenant, 6;
    run_in_callers_tenant_band_07 => run_in_callers_tenant, 7;
    run_in_callers_tenant_band_08 => run_in_callers_tenant, 8;
    run_in_callers_tenant_band_09 => run_in_callers_tenant, 9;
    run_in_callers_tenant_band_10 => run_in_callers_tenant, 10;
    run_in_callers_tenant_band_11 => run_in_callers_tenant, 11;
    run_in_callers_tenant_band_12 => run_in_callers_tenant, 12;
    run_in_callers_tenant_band_13 => run_in_callers_tenant, 13;
    run_in_callers_tenant_band_14 => run_in_callers_tenant, 14;
    run_in_callers_tenant_band_15 => run_in_callers_tenant, 15;
    fail_is_classified_band_00 => fail_is_classified, 0;
    fail_is_classified_band_01 => fail_is_classified, 1;
    fail_is_classified_band_02 => fail_is_classified, 2;
    fail_is_classified_band_03 => fail_is_classified, 3;
    fail_is_classified_band_04 => fail_is_classified, 4;
    fail_is_classified_band_05 => fail_is_classified, 5;
    fail_is_classified_band_06 => fail_is_classified, 6;
    fail_is_classified_band_07 => fail_is_classified, 7;
    fail_is_classified_band_08 => fail_is_classified, 8;
    fail_is_classified_band_09 => fail_is_classified, 9;
    fail_is_classified_band_10 => fail_is_classified, 10;
    fail_is_classified_band_11 => fail_is_classified, 11;
    fail_is_classified_band_12 => fail_is_classified, 12;
    fail_is_classified_band_13 => fail_is_classified, 13;
    fail_is_classified_band_14 => fail_is_classified, 14;
    fail_is_classified_band_15 => fail_is_classified, 15;
}
