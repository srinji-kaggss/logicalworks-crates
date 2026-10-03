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
//! | `host_stops_never_poison_a_key` | a drawn stop point and disposition across two tenants: no key ever reattaches to a host stop, and every request reaches exactly one recorded terminal once driven on a healthy host, with each recorded step's body run once |
//! | `expired_deadlines_are_recorded` | a drawn declared budget across two tenants: the deadline *is* the request's own verdict, so `@terminal` is recorded, a repeat reattaches carrying `DeadlineExceeded`, and the recorded step's body never runs twice |
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

use lgwks_bot::task::{Disposition, InputDigest, RequestError, RequestKey, RunStore, Submission};

use request::{
    FirstStepRuns, counting_task, distinct_runs, drop_after_first_step, host_on,
    host_with_deadline, hosts_over, overrunning_task, parking_task, stop_mid_run, stored_host,
    two_step_task,
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

/// Where in a request's life a host's stop arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopPoint {
    /// Before the run is admitted: the host is already stopping.
    BeforeAdmission,
    /// After the first durable step's record is committed.
    AfterFirstStep,
}

impl StopPoint {
    /// The name the trace records, so a replay compares the same word a reader
    /// sees.
    const fn label(self) -> &'static str {
        match self {
            Self::BeforeAdmission => "before-admission",
            Self::AfterFirstStep => "after-first-step",
        }
    }
}

/// How many requests one seeded stop sweep drives.
const MAX_STOP_REQUESTS: u32 = 3;

/// A seeded stop sweep: a drawn stop point and disposition per request, over both
/// tenants sharing one store file.
///
/// Three facts must hold for every drawn request, and the seed decides which
/// case each one is rather than a scenario per case:
///
/// 1. **No key ever reattaches to a host stop.** A `Cancelled` or `Refused`
///    reattach is the defect this family exists to catch: it is a key whose only
///    recorded outcome is the host declining to finish it, which nothing can
///    ever complete.
/// 2. **Every request reaches exactly one recorded terminal** once driven on a
///    healthy host. Counted by reattaching and reading the disposition, because
///    a request whose terminal was never recorded reports `InFlight` forever.
/// 3. **Each recorded step's body ran once.** The counter is on the step's
///    *effect*, not on body entries, so a replayed record is a pass and a
///    re-run effect is a failure.
fn stop_scenario(sim_run: &mut sim::Sim) -> TestResult {
    let requests = sim_run.rng().between(1, MAX_STOP_REQUESTS);
    let scratch = Scratch::new("sim-req-stop")?;
    // One store handle, cloned into both tenants: two handles over one file
    // would be two writers, which the store's own length fence refuses.
    let store = RunStore::open(scratch.join("shared.runstore"))?;
    // How many requests reached a recorded terminal, so the trace can show the
    // sweep was not vacuous at a tier that drew no requests.
    let mut reaches_terminal = 0_u32;

    for index in 0..requests {
        let tenants = u32::try_from(TENANTS.len())?;
        let which = usize::try_from(index.checked_rem(tenants).ok_or("a nonzero tenant count")?)
            .map_err(|_| "the tenant index is in range")?;
        let tenant = TENANTS.get(which).ok_or("the tenant index is in range")?;
        // A fresh host per request, over one shared store handle: a *stopped*
        // host stays stopped, so a host that has already been used to draw a
        // `BeforeAdmission` stop could not drive anything afterwards. Two store
        // handles over one file would be two writers, which the store's own chain
        // fence refuses, so the file is opened once and the hosts share it.
        let host = host_on(tenant, store.clone())?;
        let point = if sim_run.rng().chance(500) {
            StopPoint::BeforeAdmission
        } else {
            StopPoint::AfterFirstStep
        };
        let payload = sim_run.rng().between(0, 1000);
        let key = RequestKey::new(&format!("stop-{tenant}-{index}"))?;
        let runs = FirstStepRuns::new();
        let recorded = Arc::new(AtomicBool::new(false));
        // The first run's body waits for a release that never comes, so the
        // host's stop is what ends it. The settling run uses a body that goes
        // on to the second step.
        let parked = two_step_task(
            Arc::clone(&recorded),
            Arc::new(AtomicBool::new(false)),
            runs.clone(),
        )?;
        let released = two_step_task(
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(true)),
            runs.clone(),
        )?;

        let first = match point {
            StopPoint::BeforeAdmission => {
                host.cancel();
                lgwks_bot::block_on(host.submit(&key, &parked, payload))?
            }
            StopPoint::AfterFirstStep => stop_mid_run(&host, &key, &parked, payload, &recorded)?,
        };
        let stopped = first
            .report()
            .map(lgwks_bot::task::Report::disposition)
            .ok_or("a first submission carries a report")?;
        assert!(
            matches!(stopped, Disposition::Cancelled | Disposition::Refused),
            "the drawn stop point must produce a host stop, drew {stopped} at {tenant}/{index}"
        );
        sim_run
            .trace
            .record(&format!("stopped {point:?} {stopped}"));
        sim_run
            .trace
            .record(&format!("stop-point {}", point.label()));

        // A fresh host over the same store file — a new *host*, which is what a
        // restart is, rather than a second writer, which the store's own chain
        // fence refuses. This is the whole claim: a host stop recorded as the
        // verdict would reattach here, forever.
        let reopened = host_on(tenant, store.clone())?;
        let seen = lgwks_bot::block_on(reopened.submit(&key, &released, payload))?;
        let seen_disposition = match seen {
            Submission::Reattached(report) => {
                return Err(format!(
                    "a request stopped at {point:?} reattached to {:?}; a host stop is not the \
                     request's outcome",
                    report.disposition()
                )
                .into());
            }
            Submission::InFlight(ref in_flight) => {
                sim_run
                    .trace
                    .record_u64("in-flight-records", u64::try_from(in_flight.records())?);
                None
            }
            Submission::Executed(ref report) => Some(report.disposition()),
            // `Submission` is `#[non_exhaustive]`: a new arm is a compile-time
            // prompt here, and a request whose settlement this family has not
            // been taught to read is not silently treated as unsettled.
            other => {
                return Err(format!(
                    "a request this family does not know how to read came back as {other:?}"
                )
                .into());
            }
        };
        sim_run.trace.record(&format!("after-stop {seen:?}"));
        assert_eq!(
            seen_disposition, None,
            "a stopped request is unsettled: the reopen must report it in flight, not settle it"
        );

        // Settle it on a host that can admit it, then reattach and read the one
        // recorded terminal.
        let run = seen.run_id().ok_or("a submission names a run")?;
        let settled = lgwks_bot::block_on(reopened.resume(run, &released, payload));
        assert_eq!(
            settled.disposition(),
            Disposition::Succeeded,
            "the resumed run reaches the request's own verdict"
        );
        let repeat = lgwks_bot::block_on(reopened.submit(&key, &released, payload))?;
        let reattached = match repeat {
            Submission::Reattached(report) => report.disposition(),
            other => {
                return Err(format!(
                    "a request driven to a verdict must record exactly one terminal; a later \
                     submission saw {other:?}"
                )
                .into());
            }
        };
        assert_eq!(
            reattached,
            Disposition::Succeeded,
            "the recorded terminal is the request's own verdict"
        );
        assert_eq!(
            runs.count(),
            1,
            "the first durable step's effect ran once across the stop, the settle and the reattach"
        );
        reaches_terminal = reaches_terminal.saturating_add(1);
        sim_run.trace.record_u64("effects", u64::from(runs.count()));
    }
    sim_run.trace.record_u64("requests", u64::from(requests));
    sim_run
        .trace
        .record_u64("settled", u64::from(reaches_terminal));
    Ok(())
}

/// A seeded sweep of the stop family.
fn host_stops_never_poison_a_key(band: Band) -> TestResult {
    sim::assert_replays(band, stop_scenario)?;
    Ok(())
}

/// The shortest budget a deadline sweep draws, in milliseconds.
///
/// Not zero: a budget of zero would refuse the run *before* its first step
/// recorded, which is a different observation — a run that never started cannot
/// show that a request's recorded work survives its own overrun. The floor keeps
/// every drawn request in the state this family is about: progress committed,
/// then the deadline ends it.
const MIN_DEADLINE_MILLIS: u32 = 20;

/// The longest budget a deadline sweep draws, in milliseconds.
///
/// Well above the floor so the sweep covers a range rather than one number, and
/// below any budget that would let a parked body finish — which it never does,
/// because the body waits on something that never arrives.
const MAX_DEADLINE_MILLIS: u32 = 200;

/// How many requests one seeded deadline sweep drives.
const MAX_DEADLINE_REQUESTS: u32 = 2;

/// A seeded deadline sweep: a drawn budget per request, over both tenants
/// sharing one store file.
///
/// The claim is the one a host stop does *not* make, and the two families sit
/// together so neither can be true by accident. A deadline is the run reaching
/// the budget its own declaration fixed: a fact about the request, not about
/// this host, so it is recorded and a later client reattaches to it. Every drawn
/// request must therefore show all four of:
///
/// 1. the submission reports `DeadlineExceeded`;
/// 2. the key's `@terminal` **is** recorded — proven by the reattach below
///    rather than by reading the store, because the reattach is the door a
///    caller actually comes through;
/// 3. a later submission of the same key reattaches carrying
///    `DeadlineExceeded`, and the body did not run a second time;
/// 4. the recorded first step's effect ran exactly once across the overrun, the
///    reattach and the sweep's other requests.
fn deadline_scenario(sim_run: &mut sim::Sim) -> TestResult {
    let requests = sim_run.rng().between(1, MAX_DEADLINE_REQUESTS);
    let scratch = Scratch::new("sim-req-deadline")?;
    // Each host opens the shared store file itself, so the two tenants meet on
    // one file without either holding a second writer to it.
    // Every request shares one counter, so "each recorded step ran once" is one
    // number for the whole sweep rather than a per-request tally a defect could
    // hide inside.
    let runs = FirstStepRuns::new();
    let mut reached_terminal = 0_u32;

    for index in 0..requests {
        let tenants = u32::try_from(TENANTS.len())?;
        let which = usize::try_from(index.checked_rem(tenants).ok_or("a nonzero tenant count")?)
            .map_err(|_| "the tenant index is in range")?;
        let tenant = TENANTS.get(which).ok_or("the tenant index is in range")?;
        let millis = sim_run
            .rng()
            .between(MIN_DEADLINE_MILLIS, MAX_DEADLINE_MILLIS);
        let payload = sim_run.rng().between(0, 1000);
        let key = RequestKey::new(&format!("deadline-{tenant}-{index}"))?;

        // A fresh host per request, because the declared budget is part of what
        // the request is and a host carries one budget for its whole life.
        let host = host_with_deadline(
            tenant,
            scratch.path(),
            std::time::Duration::from_millis(u64::from(millis)),
        )?;
        let work = overrunning_task(runs.clone())?;

        let first = lgwks_bot::block_on(host.submit(&key, &work, payload))?;
        let disposition = first
            .report()
            .map(lgwks_bot::task::Report::disposition)
            .ok_or("a first submission carries a report")?;
        assert_eq!(
            disposition,
            Disposition::DeadlineExceeded,
            "a body waiting on something that never arrives outruns its declared budget \
             ({millis}ms for {tenant}/{index}); got {disposition}"
        );
        sim_run.trace.record_u64("budget-millis", u64::from(millis));
        sim_run.trace.record(&format!("overran {disposition}"));

        // A fresh host over the same store file — a new *host*, which is what a
        // restart is, rather than a second writer, which the store's chain fence
        // refuses. It carries the *same* declared budget, because the budget is
        // part of what this request is: a repeat under a longer budget is a
        // different declaration, and letting the crate default in here would let
        // the body overrun for thirty seconds and report a fresh `Executed`
        // rather than reattaching to the recorded verdict. That is the defect
        // this assertion exists to catch, not a nuisance to work around.
        let reopened = host_with_deadline(
            tenant,
            scratch.path(),
            std::time::Duration::from_millis(u64::from(millis)),
        )?;
        let seen = lgwks_bot::block_on(reopened.submit(&key, &work, payload))?;
        let recorded = match seen {
            Submission::Reattached(report) => report.disposition(),
            other => {
                return Err(format!(
                    "a request that overran its budget reported {other:?} on a repeat; the \
                     deadline is the request's own verdict and must be recorded"
                )
                .into());
            }
        };
        assert_eq!(
            recorded,
            Disposition::DeadlineExceeded,
            "the reattached report carries the recorded deadline"
        );
        reached_terminal = reached_terminal.saturating_add(1);
        sim_run.trace.record(&format!("reattached {recorded}"));
    }

    // One count for the whole sweep, and exactly one effect per request: a
    // reattach that re-entered a body would show up here as an extra.
    assert_eq!(
        runs.count(),
        requests,
        "each request's recorded step ran exactly once across its overrun and its reattach"
    );
    assert_eq!(
        reached_terminal, requests,
        "every drawn request recorded its deadline, so none is left in flight forever"
    );
    sim_run.trace.record_u64("requests", u64::from(requests));
    sim_run
        .trace
        .record_u64("recorded", u64::from(reached_terminal));
    sim_run.trace.record_u64("effects", u64::from(runs.count()));
    Ok(())
}

/// A seeded sweep of the deadline family.
fn expired_deadlines_are_recorded(band: Band) -> TestResult {
    sim::assert_replays(band, deadline_scenario)?;
    Ok(())
}

/// The same seed produces the same trace hash, twice, over the stop family.
///
/// The stop family is the one whose whole claim is a *replay receipt*: a key
/// that reattaches to a host stop is exactly the kind of defect a single run
/// can miss, and a trace that hashes differently between two runs of one seed
/// cannot be compared against anything.
fn same_seed_replays_host_stops(band: Band) -> TestResult {
    sim::assert_replays(band, stop_scenario)?;
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
    host_stops_never_poison_a_key_band_00 => host_stops_never_poison_a_key, 0;
    host_stops_never_poison_a_key_band_01 => host_stops_never_poison_a_key, 1;
    host_stops_never_poison_a_key_band_02 => host_stops_never_poison_a_key, 2;
    host_stops_never_poison_a_key_band_03 => host_stops_never_poison_a_key, 3;
    same_seed_replays_host_stops_band_00 => same_seed_replays_host_stops, 0;
    same_seed_replays_host_stops_band_01 => same_seed_replays_host_stops, 1;
    same_seed_replays_host_stops_band_02 => same_seed_replays_host_stops, 2;
    same_seed_replays_host_stops_band_03 => same_seed_replays_host_stops, 3;
    expired_deadlines_are_recorded_band_16 => expired_deadlines_are_recorded, 16;
    expired_deadlines_are_recorded_band_17 => expired_deadlines_are_recorded, 17;
    expired_deadlines_are_recorded_band_18 => expired_deadlines_are_recorded, 18;
    expired_deadlines_are_recorded_band_19 => expired_deadlines_are_recorded, 19;
    expired_deadlines_are_recorded_band_20 => expired_deadlines_are_recorded, 20;
    expired_deadlines_are_recorded_band_21 => expired_deadlines_are_recorded, 21;
    expired_deadlines_are_recorded_band_22 => expired_deadlines_are_recorded, 22;
    expired_deadlines_are_recorded_band_23 => expired_deadlines_are_recorded, 23;
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
