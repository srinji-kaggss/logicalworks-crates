//! Row 2 of issue #278 against the real public surface: clock skew between
//! two hosts.
//!
//! The simulation (`sim_skew`) sweeps the predicate space; this file drives
//! the shipped types two hosts would actually hold — a [`GrantSet`] minting
//! on one [`Clock`] and an [`Auth`] judged on another, plus a real
//! [`Broker`] fencing owner epochs — and asserts the same answers.
//!
//! The families:
//!
//! - two clocks at exactly ±skew hold a live lease, and an observer past the
//!   bound is refused as typed `CredentialExpired` naming the judging
//!   clock's reading;
//! - a lagging observer accepts: the judge reads the observer's clock, and a
//!   clock that runs behind cannot see the expiry — the liveness bias the
//!   bound prices, stated rather than hidden;
//! - a wall-clock pair agrees with itself: minted and judged on real time, a
//!   live lease holds;
//! - a replaced environment fences the old generation exactly, at any skew;
//! - the same-clock check still refuses without an allowance: agreement is
//!   assumed there, not bounded.

use std::error::Error;
use std::time::Duration;

use lgwks_bot::broker::Broker;
use lgwks_bot::cap::Cap;
use lgwks_bot::clock::Clock;
use lgwks_bot::effect::EnvironmentId;
use lgwks_bot::error::BotError;
use lgwks_bot::gate::GrantSet;
use lgwks_bot::skew::{SkewBound, epoch_holds, lease_holds};

use crate::sweep_fixtures::refuse;

type TestResult = Result<(), Box<dyn Error>>;

/// The environment identity this file's broker owns.
///
/// Fixed rather than minted: the fencing under test is about generations,
/// and a minted identity would spend entropy to name a fact under test that
/// does not vary it.
const ENV: &str = "2122232425262728292a2b2c2d2e2f30";

/// Seconds of lease on every proof this file mints.
const TTL_SECS: u64 = 3_600;

/// Seconds of skew allowance on every proof this file mints.
const SKEW_SECS: u64 = 60;

/// Half the lease, for the lagging observer: mid-lease on any clock.
const MID_LEASE_SECS: u64 = 1_800;

/// Mint a proof on `issuer`: a one-hour `bot.net` credential with a
/// sixty-second skew bound.
fn skewed_proof(issuer: &Clock) -> Result<lgwks_bot::cap::Auth, Box<dyn Error>> {
    Ok(GrantSet::empty()
        .grant(Cap::net())
        .grant_expiring(Cap::net(), Duration::from_secs(TTL_SECS))
        .issue_skewed(
            &[Cap::net()],
            issuer,
            SkewBound::new(Duration::from_secs(SKEW_SECS)),
        )?)
}

/// The deadline a proof minted at the origin names.
fn expires() -> Duration {
    Duration::from_secs(TTL_SECS)
}

/// Observers on both sides of a live lease — at exactly ±skew and inside it
/// — hold it, and the predicate agrees with the check on each.
#[test]
fn observers_within_skew_hold_a_live_lease() -> TestResult {
    let issuer = Clock::virtual_at(Duration::ZERO);
    let proof = skewed_proof(&issuer)?;
    for observer_at in [
        Duration::ZERO,
        Duration::from_secs(TTL_SECS - SKEW_SECS),
        Duration::from_secs(TTL_SECS),
        Duration::from_secs(TTL_SECS + SKEW_SECS - 1),
    ] {
        let observer = Clock::virtual_at(observer_at);
        proof.check_remote(&[Cap::net()], &observer)?;
        assert!(
            lease_holds(expires(), observer_at, Duration::from_secs(SKEW_SECS)),
            "the predicate and the check agree at observer reading {observer_at:?}"
        );
    }
    Ok(())
}

/// An observer past the bound is refused as a typed expiry naming the
/// judging clock's reading — and a lagging observer accepts, because the
/// judge reads the observer's clock and a clock that runs behind cannot see
/// the expiry. That accept is the liveness bias the bound prices: an expiry
/// lands at most the bound late on an ahead clock, and a behind clock is
/// blind to it.
#[test]
fn an_observer_past_the_bound_is_refused_and_a_lagging_one_accepts() -> TestResult {
    let issuer = Clock::virtual_at(Duration::ZERO);
    let proof = skewed_proof(&issuer)?;
    // Past the bound: the boundary itself refuses, since `holds` is strict.
    for observer_at in [
        Duration::from_secs(TTL_SECS + SKEW_SECS),
        Duration::from_secs(TTL_SECS + SKEW_SECS + 540),
    ] {
        let observer = Clock::virtual_at(observer_at);
        let Err(BotError::CredentialExpired {
            expired_at, now, ..
        }) = proof.check_remote(&[Cap::net()], &observer)
        else {
            return refuse(format!(
                "an observer at {observer_at:?} is past the bound and must be refused"
            ));
        };
        assert_eq!(now, observer_at, "the refusal names the judging clock");
        assert_eq!(expired_at, expires(), "the refusal names the deadline");
    }
    // Behind: the issuer's own clock has run an hour past the expiry, but the
    // lagging observer reads mid-lease and accepts. Stated, not hidden.
    let late_issuer = Clock::virtual_at(Duration::from_secs(2 * TTL_SECS));
    let lagging = Clock::virtual_at(Duration::from_secs(MID_LEASE_SECS));
    assert!(
        late_issuer.now() > expires(),
        "the issuer's own clock is past the expiry"
    );
    proof.check_remote(&[Cap::net()], &lagging)?;
    Ok(())
}

/// Two real wall clocks agree with themselves: minted and judged on real
/// time, a live lease holds and the same-clock check agrees.
#[test]
fn two_wall_clocks_judge_a_live_lease_as_live() -> TestResult {
    let issuer = Clock::wall();
    let proof = skewed_proof(&issuer)?;
    let observer = Clock::wall();
    proof.check_remote(&[Cap::net()], &observer)?;
    proof.check(&[Cap::net()])?;
    Ok(())
}

/// A replaced environment fences the old generation exactly, at any skew:
/// the warrant from before the replacement never authorises against the
/// current generation, and the current one does — no allowance in either
/// direction, because a generation is a counter rather than a clock reading.
#[test]
fn a_replaced_environment_fences_exactly_at_any_skew() -> TestResult {
    let id = EnvironmentId::from_hex(ENV)?;
    let mut broker = Broker::new();
    let first = broker.register(id)?;
    let second = broker.replace(id)?;
    assert_ne!(first, second, "a replacement moves the generation");
    assert!(
        epoch_holds(second.get(), second.get()),
        "the current generation fences"
    );
    assert!(
        !epoch_holds(first.get(), second.get()),
        "the superseded generation never fences, at any skew"
    );
    Ok(())
}

/// The same-clock check refuses an expired lease with no allowance to hide
/// behind: on one host, disagreement is a defect rather than drift.
#[test]
fn the_same_clock_check_refuses_without_an_allowance() -> TestResult {
    let clock = Clock::virtual_at(Duration::ZERO);
    let grants = GrantSet::empty()
        .grant(Cap::net())
        .grant_expiring(Cap::net(), Duration::from_secs(60));
    let proof = grants.issue_skewed(&[Cap::net()], &clock, SkewBound::ZERO)?;
    clock.advance(Duration::from_secs(60))?;
    let Err(BotError::CredentialExpired { .. }) = proof.check(&[Cap::net()]) else {
        return refuse("an expired lease must be refused on its own clock");
    };
    Ok(())
}
