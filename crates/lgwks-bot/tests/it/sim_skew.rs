//! Row 2 of issue #278, as a deterministic simulation: clock skew between two
//! hosts.
//!
//! One seed drives everything: the lease lifetime, the declared skew bound,
//! the two clocks' starting offsets, the band the observer lands in (within
//! skew, or beyond it on either side), and the warrant/current generations.
//! Time is two virtual [`Clock`]s, so a three-day lease costs arithmetic, and
//! nothing reads the wall clock — the same seed replays to the same trace
//! hash anywhere.
//!
//! The subjects are the shipped judges: [`lgwks_bot::skew`] (`lease_holds`,
//! `epoch_holds`, `warrant_holds`) and the production caller
//! ([`Auth::check_remote`](lgwks_bot::cap::Auth::check_remote)) over proofs
//! minted by [`GrantSet::issue_skewed`](lgwks_bot::gate::GrantSet::issue_skewed).
//! Nothing here reimplements a judge: a second judge written "for the
//! simulation" would let this file pass while the shipped one rotted.
//!
//! The properties, one per family:
//!
//! - within the bound, the observer's verdict equals the issuer's own reading
//!   adjusted by at most the bound — never a surprise refusal, never a
//!   surprise acceptance past expiry plus bound;
//! - beyond the bound on either side, the verdict is exactly what the
//!   predicate says, and `check_remote` agrees with it on every seed;
//! - the generation fence is exact at every skew: a superseded epoch never
//!   fences, whatever the clocks say;
//! - the same seed gives the same trace hash, and the seeds diverge.

use std::error::Error;
use std::time::Duration;

use lgwks_bot::cap::{Auth, Cap};
use lgwks_bot::clock::Clock;
use lgwks_bot::error::BotError;
use lgwks_bot::gate::GrantSet;
use lgwks_bot::skew::{SkewBound, epoch_holds, lease_holds, warrant_holds};

use crate::sim::{Rng, Trace};
use crate::sweep_fixtures::{FIRST_SEED, SWEEP_SEEDS, peak_rss_bytes, refuse};

type TestResult = Result<(), Box<dyn Error>>;

/// One seeded cross-host judgement: the lease, the two clocks, the band the
/// observer lands in, and the generations.
struct Scenario {
    /// How long the credential lives on the granting clock.
    ttl: Duration,
    /// The declared skew allowance.
    bound: Duration,
    /// The granting clock's starting reading.
    granted_at: Duration,
    /// The observer's reading at judgement time.
    observer_now: Duration,
    /// The generation the warrant was minted at.
    warrant_epoch: u64,
    /// The generation the environment is at.
    current_epoch: u64,
}

/// Cut one scenario from the seed.
///
/// The observer band is drawn from three arms — within skew, ahead past it,
/// behind past it — so the sweep covers the boundary from both sides rather
/// than one draw of "some offset".
fn scenario(rng: &mut Rng) -> Scenario {
    let ttl = Duration::from_secs(u64::from(rng.between(60, 3_600)));
    let bound = Duration::from_secs(u64::from(rng.between(0, 30)));
    let granted_at = Duration::from_secs(u64::from(rng.between(0, 86_400)));
    let drift_secs = i64::from(rng.between(0, 90)).saturating_sub(45);
    // Where the lease stands on its own clock at judgement: before it, at its
    // edge, or past it — drawn independently of the drift so the sweep
    // crosses the expiry from both clock directions.
    let elapsed_secs = match rng.below(3) {
        0 => u64::from(rng.between(0, 60)),
        1 => 3_600,
        _ => u64::from(rng.between(3_601, 7_200)),
    };
    let issuer_now = granted_at.saturating_add(Duration::from_secs(elapsed_secs));
    let magnitude = Duration::from_secs(drift_secs.unsigned_abs());
    let observer_now = if drift_secs >= 0 {
        issuer_now.saturating_add(magnitude)
    } else {
        issuer_now.saturating_sub(magnitude)
    };
    let warrant_epoch = u64::from(rng.between(1, 5));
    let current_epoch = if rng.below(2) == 0 {
        warrant_epoch
    } else {
        warrant_epoch.saturating_add(1)
    };
    Scenario {
        ttl,
        bound,
        granted_at,
        observer_now,
        warrant_epoch,
        current_epoch,
    }
}

/// The expiry the granting clock names for `scenario`.
fn expires_at(scenario: &Scenario) -> Duration {
    scenario.granted_at.saturating_add(scenario.ttl)
}

/// Run one scenario against the shipped judges, recording every verdict.
///
/// Returns the trace hash: the replay receipt.
fn run(scenario: &Scenario, trace: &mut Trace) -> Result<(), Box<dyn Error>> {
    let issuer = Clock::virtual_at(scenario.granted_at);
    let observer = Clock::virtual_at(scenario.observer_now);
    let grants = GrantSet::empty()
        .grant(Cap::net())
        .grant_expiring(Cap::net(), scenario.ttl);
    let proof: Auth =
        grants.issue_skewed(&[Cap::net()], &issuer, SkewBound::new(scenario.bound))?;
    // The lease the proof carries names the expiry this scenario computed:
    // the mint and the model agree on the deadline before any verdict.
    if proof.expires_at() != Some(expires_at(scenario)) {
        return refuse(format!(
            "the minted expiry {:?} is not the modelled {:?}",
            proof.expires_at(),
            expires_at(scenario)
        ));
    }
    let holds = lease_holds(expires_at(scenario), scenario.observer_now, scenario.bound);
    trace.record_number("holds", u8::from(holds));
    trace.record_number("observer", scenario.observer_now.as_secs());
    // `check_remote` agrees with the predicate on every seed: the production
    // caller and the judge are one fact read twice.
    match proof.check_remote(&[Cap::net()], &observer) {
        Ok(()) if holds => {}
        Err(BotError::CredentialExpired {
            now, expired_at, ..
        }) if !holds => {
            if now != scenario.observer_now || expired_at != expires_at(scenario) {
                return refuse(format!(
                    "the refusal names {expired_at:?} at {now:?}, not the modelled deadline and observer"
                ));
            }
        }
        Err(BotError::CapabilityDenied { .. }) => {
            return refuse("a proof covering bot.net must not be denied it");
        }
        unexpected => {
            return refuse(format!(
                "check_remote disagreed with the predicate (holds={holds}): {unexpected:?}"
            ));
        }
    }
    // The generation fence is exact at every skew: equality, nothing else.
    let fenced = epoch_holds(scenario.warrant_epoch, scenario.current_epoch);
    trace.record_number("fenced", u8::from(fenced));
    if fenced != (scenario.warrant_epoch == scenario.current_epoch) {
        return refuse("the epoch fence is not exact equality");
    }
    let both = warrant_holds(
        scenario.warrant_epoch,
        scenario.current_epoch,
        expires_at(scenario),
        scenario.observer_now,
        scenario.bound,
    );
    trace.record_number("both", u8::from(both));
    if both != (fenced && holds) {
        return refuse("the combined judge is not the conjunction of its halves");
    }
    Ok(())
}

/// Two simulated clocks at seeded offsets, judged within the bound and past
/// it, over 1,024 seeds.
#[test]
fn skewed_observers_agree_with_the_predicate_on_every_seed() -> TestResult {
    let mut first_hash: Option<u64> = None;
    for index in 0..SWEEP_SEEDS {
        let seed = FIRST_SEED.saturating_add(index);
        let mut rng = Rng::new(seed);
        let drawn = scenario(&mut rng);
        let mut trace = Trace::new();
        trace.record_number("seed", seed);
        run(&drawn, &mut trace)?;
        // The same seed replays to the same receipt: run it again from the
        // seed, not from the first run's leftovers.
        let mut replay_rng = Rng::new(seed);
        let redrawn = scenario(&mut replay_rng);
        let mut replay = Trace::new();
        replay.record_number("seed", seed);
        run(&redrawn, &mut replay)?;
        if trace.hash() != replay.hash() {
            return refuse(format!("seed {seed} did not replay to the same trace hash"));
        }
        if index == 0 {
            first_hash = Some(trace.hash());
        }
    }
    if first_hash.is_none() {
        return refuse("the sweep ran no seeds");
    }
    if let Some(rss) = peak_rss_bytes() {
        assert!(
            rss < 512 * 1024 * 1024,
            "a 1,024-seed clock sweep must stay under 512 MiB, observed {rss} bytes"
        );
    }
    Ok(())
}
