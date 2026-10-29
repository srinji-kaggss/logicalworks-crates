//! Deterministic simulation tests for the ink lab and the residency runtime.
//!
//! Every test drives the real public API through [`harness::Tenant`], where
//! one seed controls time, input, faults and randomness. Each test runs many
//! seeds and checks the invariants after every operation. A failure prints
//! the seed, and that seed replays the identical trace.

mod harness;
mod units;

use drishti::ink_lab::{Canvas, Effect, HostSpec};
use drishti::runtime::{Budgets, Runtime, RuntimeError};
use harness::{Faults, Rng, TRACE_SEED, Tenant, mix, trace_of};

/// Operations per seed in the per-effect scenarios.
const OPERATIONS: u64 = 400;

/// Seeds per per-effect scenario.
const SEEDS: u64 = 250;

/// Run `seeds` chaos seeds over `effects`, checking invariants throughout.
fn chaos(effects: &[Effect], base: u64, seeds: u64) {
    for seed in base..base.saturating_add(seeds) {
        let mut tenant = Tenant::new(seed, effects, Faults::ALL);
        tenant.run(OPERATIONS);
    }
}

/// Check that each seed replays exactly and that seeds are distinguishable.
fn replay(effects: &[Effect], base: u64, seeds: u64) {
    let mut distinct = std::collections::BTreeSet::new();
    for seed in base..base.saturating_add(seeds) {
        let first = trace_of(seed, effects, Faults::ALL, OPERATIONS);
        let second = trace_of(seed, effects, Faults::ALL, OPERATIONS);
        assert_eq!(first, second, "seed {seed} did not replay");
        distinct.insert(first);
    }
    assert_eq!(
        u64::try_from(distinct.len()).ok(),
        Some(seeds),
        "two seeds produced the same trace"
    );
}

/// Under reduced motion, every host settles: a second present has no demand.
fn reduced_motion_settles(effect: Effect, base: u64, seeds: u64) {
    for seed in base..base.saturating_add(seeds) {
        let mut rng = Rng::new(seed);
        let mut runtime = Runtime::new(seed, Budgets::conservative());
        for _ in 0..rng.below(4).saturating_add(1) {
            let canvas = Canvas::new(rng.range(4.0, 400.0), rng.range(4.0, 300.0));
            assert!(
                runtime
                    .add_host(HostSpec::new(effect, canvas, true))
                    .is_ok(),
                "seed {seed}: host refused under the conservative profile"
            );
        }
        // Let the effect run a seeded while, then ask for reduced motion.
        for _ in 0..rng.below(200) {
            let dt = rng.range(4.0, 40.0);
            assert!(runtime.present(dt).is_ok(), "seed {seed}: present refused");
        }
        runtime.set_reduce_motion(true);
        let resolved = runtime.present(16.0);
        assert!(resolved.is_ok(), "seed {seed}: resolve refused");
        if let Ok(report) = resolved {
            for upload in report.uploads() {
                let frame = &upload.frame;
                assert!(
                    frame.blur.abs() < 1e-12 && frame.displacement.abs() < 1e-12,
                    "seed {seed}: {} resolved mid-liquid",
                    effect.name()
                );
                assert!(
                    (frame.alpha - 1.0).abs() < 1e-12,
                    "seed {seed}: faded resolve"
                );
            }
        }
        let settled = runtime.present(16.0);
        assert!(
            matches!(settled, Ok(report) if report.idle),
            "seed {seed}: {} kept demanding work after resolving",
            effect.name()
        );
    }
}

/// One chaos, one reduced-motion and one replay scenario per effect.
macro_rules! effect_scenarios {
    ($($module:ident => $effect:expr, $base:expr;)*) => {$(
        mod $module {
            use super::{chaos, reduced_motion_settles, replay, Effect, SEEDS};

            #[test]
            fn sim_chaos_holds_every_invariant() {
                chaos(&[$effect], $base, SEEDS);
            }

            #[test]
            fn sim_reduced_motion_settles_and_sleeps() {
                reduced_motion_settles($effect, $base, SEEDS);
            }

            #[test]
            fn sim_same_seed_same_trace() {
                replay(&[$effect], $base, 60);
            }

            /// Keep the effect named so a renamed variant fails to compile.
            const _: Effect = $effect;
        }
    )*};
}

effect_scenarios! {
    mark => Effect::Mark, 1_000;
    text => Effect::Text, 2_000;
    wipe => Effect::Wipe, 3_000;
    drops => Effect::Drops, 4_000;
    wobble => Effect::Wobble, 5_000;
    loader => Effect::Loader, 6_000;
    pour => Effect::Pour, 7_000;
    cursor => Effect::Cursor, 8_000;
    ripple => Effect::Ripple, 9_000;
    count => Effect::Count, 10_000;
    reveal => Effect::Reveal, 11_000;
}

#[test]
fn sim_mixed_effects_under_every_fault() {
    chaos(&Effect::all(), 20_000, 1_000);
}

#[test]
fn sim_mixed_effects_replay_and_diverge() {
    replay(&Effect::all(), 21_000, 300);
}

/// Run `tenants` tenants at once, interleaved by a scheduler seed, and check
/// each one's trace equals its solo trace: no tenant observes another.
fn tenants_are_isolated(tenants: u64, operations: u64, base: u64) {
    let effects = [Effect::Loader, Effect::Count, Effect::Wobble];
    let mut live: Vec<Tenant> = (base..base.saturating_add(tenants))
        .map(|seed| Tenant::new(seed, &effects, Faults::ALL))
        .collect();
    let mut scheduler = Rng::new(base ^ 0x5EED);
    let length = tenants;
    let total = length.saturating_mul(operations);
    let mut steps = vec![0_u64; live.len()];
    let mut done = 0_u64;
    while done < total {
        let index = usize::try_from(scheduler.below(length)).ok();
        let Some(index) = index else { break };
        let (Some(tenant), Some(count)) = (live.get_mut(index), steps.get_mut(index)) else {
            break;
        };
        if *count < operations {
            tenant.operate();
            *count = count.saturating_add(1);
            done = done.saturating_add(1);
        } else {
            // Finish the rest in order so the loop always terminates.
            for (tenant, count) in live.iter_mut().zip(steps.iter_mut()) {
                while *count < operations {
                    tenant.operate();
                    *count = count.saturating_add(1);
                    done = done.saturating_add(1);
                }
            }
        }
    }
    for (seed, tenant) in (base..).zip(&live) {
        let solo = trace_of(seed, &effects, Faults::ALL, operations);
        assert_eq!(
            tenant.trace(),
            solo,
            "tenant {seed} was affected by its neighbours"
        );
    }
}

#[test]
fn sim_tenants_100_interleaved_stay_isolated() {
    tenants_are_isolated(100, 200, 30_000);
}

#[test]
fn sim_tenants_1_000_interleaved_stay_isolated() {
    tenants_are_isolated(1_000, 60, 31_000);
}

#[test]
fn sim_tenants_10_000_interleaved_stay_isolated() {
    tenants_are_isolated(10_000, 12, 41_000);
}

#[test]
fn sim_tenants_100_000_live_at_once_stay_isolated() {
    tenants_are_isolated(100_000, 3, 200_000);
}

#[test]
fn sim_5_000_tenant_provisions_respect_their_budgets() {
    let mut runtimes = Vec::with_capacity(5_000);
    for seed in 0_u64..5_000 {
        let mut rng = Rng::new(seed);
        let mut budgets = Budgets::conservative();
        budgets.max_hosts = rng.below_u32(12).saturating_add(1);
        let mut runtime = Runtime::new(seed, budgets);
        let mut admitted = 0_u32;
        for _ in 0..20 {
            let effect = rng.pick(&Effect::all());
            let Some(effect) = effect else { continue };
            if runtime
                .add_host(HostSpec::new(effect, Canvas::new(120.0, 90.0), true))
                .is_ok()
            {
                admitted = admitted.saturating_add(1);
            }
        }
        assert_eq!(
            admitted, budgets.max_hosts,
            "tenant {seed} admitted the wrong host count"
        );
        runtimes.push(runtime);
    }
    for (seed, runtime) in (0_u64..).zip(runtimes.iter_mut()) {
        match runtime.present(16.0) {
            Ok(report) => assert!(
                report.receipts().len() <= 13,
                "tenant {seed} presented more hosts than it may admit"
            ),
            Err(error) => assert_eq!(
                error,
                drishti::runtime::RuntimeError::Backpressure,
                "tenant {seed} refused for the wrong reason"
            ),
        }
    }
}

#[test]
fn sim_parallel_workers_match_the_serial_trace() {
    let effects = Effect::all();
    let seeds: Vec<u64> = (50_000..50_400).collect();
    let serial: Vec<u64> = seeds
        .iter()
        .map(|&seed| trace_of(seed, &effects, Faults::ALL, 150))
        .collect();
    let mut parallel: Vec<u64> = Vec::with_capacity(seeds.len());
    std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for chunk in seeds.chunks(50) {
            workers.push(scope.spawn(move || {
                chunk
                    .iter()
                    .map(|&seed| trace_of(seed, &effects, Faults::ALL, 150))
                    .collect::<Vec<u64>>()
            }));
        }
        for worker in workers {
            let joined = worker.join();
            assert!(joined.is_ok(), "a simulation worker panicked");
            if let Ok(traces) = joined {
                parallel.extend(traces);
            }
        }
    });
    assert_eq!(
        serial, parallel,
        "parallel workers diverged from the serial trace"
    );
}

#[test]
fn sim_budget_saturation_refuses_then_recovers() {
    for seed in 60_000_u64..60_300 {
        let mut budgets = Budgets::conservative();
        budgets.max_in_flight = 1;
        let mut runtime = Runtime::new(seed, budgets);
        let hosts: Vec<_> = (0..3)
            .filter_map(|_| {
                runtime
                    .add_host(HostSpec::new(Effect::Loader, Canvas::new(60.0, 60.0), true))
                    .ok()
            })
            .collect();
        assert_eq!(hosts.len(), 3, "seed {seed}: admission");
        let before = runtime.totals();
        assert!(
            runtime.present(16.0).is_err(),
            "seed {seed}: saturation not refused"
        );
        assert_eq!(runtime.totals(), before, "seed {seed}: refusal committed");
        // Relieve the pressure: only one host stays on screen.
        for &host in hosts.iter().skip(1) {
            runtime.set_visible(host, false);
        }
        assert!(
            runtime.present(16.0).is_ok(),
            "seed {seed}: did not recover after relief"
        );
    }
}

#[test]
fn sim_device_loss_storm_keeps_epochs_monotonic() {
    for seed in 70_000_u64..70_300 {
        let mut tenant = Tenant::new(seed, &Effect::all(), Faults::ALL);
        let mut last = tenant.runtime().epoch();
        for _ in 0..OPERATIONS {
            tenant.operate();
            let epoch = tenant.runtime().epoch();
            assert!(epoch >= last, "seed {seed}: epoch went backwards");
            last = epoch;
        }
    }
}

#[test]
fn sim_malformed_deltas_only_scenario() {
    let faults = Faults {
        malformed_deltas: true,
        hostile_options: false,
        device_faults: false,
        tight_budgets: false,
    };
    let mut refused: u64 = 0;
    for seed in 80_000_u64..80_400 {
        let mut tenant = Tenant::new(seed, &Effect::all(), faults);
        tenant.run(OPERATIONS);
        refused = refused.saturating_add(tenant.refusals());
    }
    // The harness checks each refusal is exactly the malformed delta's; this
    // checks the scenario actually reached the refusal path.
    assert!(refused > 0, "no malformed delta was ever refused");
}

#[test]
fn sim_hostile_options_only_scenario() {
    let faults = Faults {
        malformed_deltas: false,
        hostile_options: true,
        device_faults: false,
        tight_budgets: false,
    };
    let mut rejected: u64 = 0;
    for seed in 90_000_u64..90_400 {
        let mut tenant = Tenant::new(seed, &Effect::all(), faults);
        tenant.run(OPERATIONS);
        // Hostile options are refused at the call; frames are refused only by
        // the documented in-flight/shape/byte budgets, never as malformed.
        assert!(
            matches!(
                tenant.last_refusal(),
                None | Some(RuntimeError::Backpressure)
            ),
            "seed {seed}: present refused with {:?}",
            tenant.last_refusal()
        );
        rejected = rejected.saturating_add(tenant.rejected_options());
    }
    assert!(rejected > 0, "no hostile option set was ever refused");
}

#[test]
fn sim_trace_hash_is_sensitive_to_one_operation() {
    for seed in 95_000_u64..95_200 {
        let mut short = Tenant::new(seed, &Effect::all(), Faults::ALL);
        short.run(99);
        let mut long = Tenant::new(seed, &Effect::all(), Faults::ALL);
        long.run(100);
        assert_ne!(
            short.trace(),
            long.trace(),
            "seed {seed}: one operation left no trace"
        );
        assert_ne!(
            mix(TRACE_SEED, seed),
            short.trace(),
            "seed {seed}: trace never moved"
        );
    }
}
