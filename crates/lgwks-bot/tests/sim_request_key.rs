//! Simulation family: request-keyed durable submission under a seeded sweep.
//!
//! Every scenario drives the real `Host`, the real `RunStore` over a real file,
//! the real `submit`, and the real `remember`. Nothing here reimplements a
//! receipt, a run identity or a resume. What the seed controls is the shape of
//! the request space: how many keys, what payload each carries, and how the two
//! tenants share one store file.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `collisions_across_two_tenants` | one key + same input reattaches, one key + different input conflicts naming both digests, and every key derives a distinct run |
//! | `drop_and_reattach` | a waiter dropped mid-effect leaves an in-flight request a later client reattaches to, with the receipt and the first record intact |
//! | `tier_submissions` | a drawn tier of distinct keys over two tenants derives as many distinct runs, and the requested/reached/ceiling levels are recorded together |
//! | `same_seed_replays` | the same seed produces the same trace hash, twice |
//!
//! # What is real and what is seeded
//!
//! The store, the file, the `sync_all`, the derived run identity and the
//! receipt are real. The seed decides only the *inputs*: key count, payload and
//! tenant interleaving. No clock reading enters the trace, which is what makes
//! the hash a receipt: two runs of one seed must agree whatever the machine is
//! doing. The high-volume tiers (1,000 and 10,000 submissions) are a separate,
//! opt-in measurement (`concurrent_submissions_across_tiers`) because they are
//! minutes of real `fsync`s; the swept family stays at the level a routine run
//! can afford.

#![cfg(all(feature = "script", feature = "ephemeral"))]

mod sim;

// The band-declaration macro, defined once for the whole layer.
#[path = "sim/bands.rs"]
mod band_family;

// The scratch directory, shared with the resume targets.
#[path = "support/resume.rs"]
mod shared;

// The request fixtures themselves.
#[path = "support/request.rs"]
mod request;

use std::cell::Cell;
use std::error::Error;
use std::io::Write as _;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use lgwks_bot::task::{InputDigest, RequestError, RequestKey, RunStore, Submission};

use request::{
    counting_task, distinct_runs, drop_after_first_step, host_on, hosts_over, parking_task,
    stored_host,
};
use shared::{Scratch, Summary, peak_rss_mib};

use sim::Band;

type TestResult = Result<(), Box<dyn Error>>;

/// The two tenants every scenario shares one store file between.
const TENANTS: [&str; 2] = ["alpha", "beta"];

/// The most keys one collision scenario draws.
const MAX_KEYS: u32 = 3;

/// The most submissions one swept tier draws.
///
/// Small on purpose: each submission commits three `fsync`-ed records, so this
/// family sweeps the *shape* of a tier cheaply while the high-volume tiers are
/// the opt-in measurement below.
const MAX_TIER: u32 = 12;

/// The ceiling this family declares for its tier, so the requested, reached and
/// ceiling levels can be read together (the INV-BOT-16 rule).
const DECLARED_TIER_CEILING: u64 = 10_000;

/// Two tenants, one store file: reattach, conflict and distinct runs.
fn collision_scenario(sim_run: &mut sim::Sim) -> TestResult {
    let keys = sim_run.rng().between(1, MAX_KEYS);
    let payload = sim_run.rng().between(0, 1000);
    let scratch = Scratch::new("sim-req")?;
    // One store handle, cloned into both tenants: two handles over one file
    // would be two writers, which the store's own length fence refuses.
    let store = RunStore::open(scratch.join("shared.runstore"))?;
    let counter = Rc::new(Cell::new(0));
    let work = counting_task(Rc::clone(&counter))?;

    let mut runs: Vec<String> = Vec::new();
    for tenant in TENANTS {
        let host = host_on(tenant, store.clone())?;
        for index in 0..keys {
            let key = RequestKey::new(&format!("{tenant}-{index}"))?;

            let first = lgwks_bot::block_on(host.submit(&key, &work, payload))?;
            assert!(
                matches!(first, Submission::Executed(_)),
                "a first submission executes"
            );
            let run = first.run_id().ok_or("a submission names a run")?;
            assert_eq!(
                first.report().and_then(|report| report.output().copied()),
                Some(payload),
                "the body's value is the recorded value"
            );
            let hex = run.id().to_hex();
            assert!(
                !runs.contains(&hex),
                "every key derives a distinct run; {tenant}/{index} collided with {hex}"
            );
            sim_run.trace.record(&format!("run {tenant} {index} {hex}"));
            runs.push(hex);

            let duplicate = lgwks_bot::block_on(host.submit(&key, &work, payload))?;
            assert!(
                matches!(duplicate, Submission::Reattached(_)),
                "the same key and input reattaches"
            );
            assert_eq!(
                duplicate.run_id(),
                Some(run),
                "a reattach names the same derived run"
            );

            let other = payload.saturating_add(1);
            let conflict = lgwks_bot::block_on(host.submit(&key, &work, other))
                .err()
                .ok_or("a different payload under one key must conflict")?;
            match conflict {
                RequestError::Conflict(named) => {
                    assert_eq!(
                        named.existing(),
                        InputDigest::of(&payload),
                        "the conflict names the bound input's digest"
                    );
                    assert_eq!(
                        named.requested(),
                        InputDigest::of(&other),
                        "the conflict names the refused input's digest"
                    );
                }
                other => {
                    return Err(format!("expected a conflict, got {other:?}").into());
                }
            }
        }
    }
    assert_eq!(
        counter.get(),
        u32::try_from(TENANTS.len())?.saturating_mul(keys),
        "only the first submission of each key ran the body"
    );
    sim_run.trace.record_u64("bodies", u64::from(counter.get()));
    Ok(())
}

/// Drop a waiter mid-effect and reattach to the in-flight request.
fn drop_scenario(sim_run: &mut sim::Sim) -> TestResult {
    let payload = sim_run.rng().between(0, 1000);
    let scratch = Scratch::new("sim-req-drop")?;
    let host = stored_host("sim", scratch.path())?;
    let recorded = Arc::new(AtomicBool::new(false));
    let work = parking_task(Arc::clone(&recorded))?;
    let key = RequestKey::new(&format!("key-{payload}"))?;

    drop_after_first_step(Box::pin(host.submit(&key, &work, payload)), &recorded);

    let later = lgwks_bot::block_on(host.submit(&key, &work, payload))?;
    match later {
        Submission::InFlight(in_flight) => {
            assert!(
                in_flight.records() >= 2,
                "the receipt and the first step's record survive the drop"
            );
            sim_run
                .trace
                .record(&format!("inflight {}", in_flight.run().id().to_hex()));
            sim_run
                .trace
                .record_u64("records", u64::try_from(in_flight.records()).unwrap_or(0));
        }
        other => return Err(format!("expected an in-flight report, got {other:?}").into()),
    }
    Ok(())
}

/// A drawn tier of distinct keys over two tenants derives as many distinct runs.
fn tier_scenario(sim_run: &mut sim::Sim) -> TestResult {
    let requested = sim_run.rng().between(8, MAX_TIER);
    let scratch = Scratch::new("sim-req-tier")?;
    let store = RunStore::open(scratch.join("shared.runstore"))?;
    let counter = Rc::new(Cell::new(0));
    let work = counting_task(Rc::clone(&counter))?;
    let hosts = hosts_over(&TENANTS, &store)?;

    let mut runs: Vec<String> = Vec::new();
    let mut parity = false;
    for index in 0..requested {
        let which = usize::from(parity);
        let host = hosts.get(which).ok_or("the tenant index is in range")?;
        let key = RequestKey::new(&format!("tier-{index}"))?;
        let submission = lgwks_bot::block_on(host.submit(&key, &work, index))?;
        let run = submission.run_id().ok_or("a submission names a run")?;
        assert_eq!(
            submission
                .report()
                .map(|report| report.disposition().is_success()),
            Some(true),
            "every distinct key runs and succeeds"
        );
        runs.push(run.id().to_hex());
        parity = !parity;
    }

    assert_eq!(
        distinct_runs(&runs),
        runs.len(),
        "every distinct key derived a distinct run"
    );
    assert_eq!(
        counter.get(),
        requested,
        "every distinct key ran its body exactly once"
    );
    // Requested, reached and ceiling together, so no reader is told a number
    // nobody ran (INV-BOT-16).
    sim_run
        .trace
        .record_u64("tier-requested", u64::from(requested));
    sim_run
        .trace
        .record_u64("tier-reached", u64::from(requested));
    sim_run
        .trace
        .record_u64("tier-ceiling", DECLARED_TIER_CEILING);
    Ok(())
}

/// A seeded sweep of the collision family.
fn collisions_across_two_tenants(band: Band) -> TestResult {
    sim::assert_replays(band, collision_scenario)?;
    Ok(())
}

/// A seeded sweep of the drop-and-reattach family.
fn drop_and_reattach(band: Band) -> TestResult {
    sim::assert_replays(band, drop_scenario)?;
    Ok(())
}

/// A seeded sweep of the tier family.
fn tier_submissions(band: Band) -> TestResult {
    sim::assert_replays(band, tier_scenario)?;
    Ok(())
}

/// The same seed produces the same trace hash, twice, over the collision and
/// drop bodies.
///
/// The tier family carries its own `assert_replays` sweep, so its determinism
/// is proven there; folding it in here as well would only pay its `fsync` cost
/// twice for a fact already established.
fn same_seed_replays(band: Band) -> TestResult {
    sim::assert_replays(band, |sim_run| {
        collision_scenario(sim_run)?;
        drop_scenario(sim_run)?;
        Ok(())
    })?;
    Ok(())
}

band_family::band_family! {
    collisions_across_two_tenants_band_00 => collisions_across_two_tenants, 0;
    collisions_across_two_tenants_band_01 => collisions_across_two_tenants, 1;
    collisions_across_two_tenants_band_02 => collisions_across_two_tenants, 2;
    collisions_across_two_tenants_band_03 => collisions_across_two_tenants, 3;
    collisions_across_two_tenants_band_04 => collisions_across_two_tenants, 4;
    collisions_across_two_tenants_band_05 => collisions_across_two_tenants, 5;
    collisions_across_two_tenants_band_06 => collisions_across_two_tenants, 6;
    collisions_across_two_tenants_band_07 => collisions_across_two_tenants, 7;
    drop_and_reattach_band_08 => drop_and_reattach, 8;
    drop_and_reattach_band_09 => drop_and_reattach, 9;
    drop_and_reattach_band_10 => drop_and_reattach, 10;
    drop_and_reattach_band_11 => drop_and_reattach, 11;
    tier_submissions_band_12 => tier_submissions, 12;
    tier_submissions_band_13 => tier_submissions, 13;
    tier_submissions_band_14 => tier_submissions, 14;
    tier_submissions_band_15 => tier_submissions, 15;
    same_seed_replays_band_00 => same_seed_replays, 0;
    same_seed_replays_band_01 => same_seed_replays, 1;
    same_seed_replays_band_02 => same_seed_replays, 2;
    same_seed_replays_band_03 => same_seed_replays, 3;
    same_seed_replays_band_04 => same_seed_replays, 4;
    same_seed_replays_band_05 => same_seed_replays, 5;
    same_seed_replays_band_06 => same_seed_replays, 6;
    same_seed_replays_band_07 => same_seed_replays, 7;
    same_seed_replays_band_08 => same_seed_replays, 8;
    same_seed_replays_band_09 => same_seed_replays, 9;
    same_seed_replays_band_10 => same_seed_replays, 10;
    same_seed_replays_band_11 => same_seed_replays, 11;
    same_seed_replays_band_12 => same_seed_replays, 12;
    same_seed_replays_band_13 => same_seed_replays, 13;
    same_seed_replays_band_14 => same_seed_replays, 14;
    same_seed_replays_band_15 => same_seed_replays, 15;
}

/// The environment variable that gates the high-volume submission measurement.
const SCALE_ENV: &str = "LGWKS_REQUEST_SCALE";

/// The submission tiers the opt-in measurement drives.
///
/// The list stops at ten thousand because that is the largest tier this file
/// actually measured; a tier nobody ran is not a tier this file claims.
const SCALE_TIERS: [usize; 3] = [100, 1_000, 10_000];

/// Concurrent request-keyed submissions across every tier, two tenants
/// interleaved over one store file.
///
/// The hyperscale row for durable submission: what has to hold at ten thousand
/// is that every distinct key derives its own run, nothing is lost, and the two
/// tenants never read each other's receipt. Every submission commits through the
/// store's one owner thread, so the question is whether the derived identities
/// stay distinct under load, not whether the disk can take it.
///
/// Opt-in, like the run store's own scale measurement, because it is minutes of
/// real `fsync`s. It is `#[ignore]`d rather than silently skipped, and it prints
/// what it measured.
///
/// ```text
/// LGWKS_REQUEST_SCALE=1 cargo test -p lgwks_bot --features script,ephemeral \
///     --test sim_request_key concurrent_submissions_across_tiers -- --ignored --nocapture
/// ```
#[test]
#[ignore = "the request-key submission scale measurement; run it with \
            LGWKS_REQUEST_SCALE=1 and --ignored"]
fn concurrent_submissions_across_tiers() -> TestResult {
    if std::env::var_os(SCALE_ENV).is_none() {
        return Err(format!(
            "{SCALE_ENV} is not set, so the tier measurement did not run; set it to measure"
        )
        .into());
    }
    for tier in SCALE_TIERS {
        measure_tier(tier)?;
    }
    Ok(())
}

/// One tier: `tier` distinct keys, two tenants interleaved, over one store.
fn measure_tier(tier: usize) -> TestResult {
    let scratch = Scratch::new("req-scale")?;
    let store = RunStore::open(scratch.join("shared.runstore"))?;
    let counter = Rc::new(Cell::new(0));
    let work = counting_task(Rc::clone(&counter))?;
    let hosts = hosts_over(&TENANTS, &store)?;

    let started = Instant::now();
    let mut samples: Vec<u128> = Vec::with_capacity(tier);
    let mut runs: Vec<String> = Vec::with_capacity(tier);
    let mut parity = false;
    for index in 0..tier {
        let which = usize::from(parity);
        let host = hosts.get(which).ok_or("the tenant index is in range")?;
        let key = RequestKey::new(&format!("scale-{index}"))?;
        let at = Instant::now();
        let submission = lgwks_bot::block_on(host.submit(
            &key,
            &work,
            u32::try_from(index).unwrap_or(u32::MAX),
        ))?;
        samples.push(at.elapsed().as_micros());
        let run = submission.run_id().ok_or("a submission names a run")?;
        runs.push(run.id().to_hex());
        parity = !parity;
    }
    let elapsed = started.elapsed();

    assert_eq!(
        distinct_runs(&runs),
        runs.len(),
        "every key at tier {tier} derived a distinct run"
    );
    assert_eq!(
        counter.get(),
        u32::try_from(tier)?,
        "every key at tier {tier} ran its body once"
    );

    let summary = Summary::of(&mut samples);
    let rss = peak_rss_mib();
    let mut line = std::io::stdout().lock();
    let _written = writeln!(
        line,
        "tier {tier}: requested {tier} reached {} ceiling {} in {:.2?}  |  {}  |  peak RSS {rss}",
        runs.len(),
        DECLARED_TIER_CEILING,
        elapsed,
        summary.line("per-submit"),
    );
    Ok(())
}
