//! Simulation family: `act gh::issue` under seeded receivers, reruns and
//! tenants.
//!
//! Each seed draws a receiver (one that answers, one that loses the answer
//! after holding the issue, one that refuses the create), how many times each
//! of two tenants reruns its open, and the order the tenants take turns. Both
//! tenants open on one repository under their own keys. The model is the rule
//! the action promises: one key, at most one held issue, whatever the reruns;
//! a refused create holds nothing, so a rerun may try again.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `one_key_holds_at_most_one_issue_under_reruns` | reruns, lost answers and two tenants never hold two issues for one key, and each run settles as the model says |
//!
//! The band is swept twice through `sim::assert_replays`; each pass opens on
//! fresh owners, so the second pass replays the first rather than finding its
//! issues.
//!
//! # A lagging listing, drawn per seed
//!
//! A lag seed draws 0 to 10 listing reads from the run's own stream and opens
//! on a flow clock the scenario advances; the receiver hides the created issue
//! from that many reads. Every read past the action's eight-read settle bound
//! is unreachable by the bound, so the fake applies `min(drawn, 8)`: a lag of 9
//! or 10 would report `unlisted` on the first run and still hide the issue
//! from the rerun's scan, which is a second create for one key — the exact
//! defect this family exists to catch. The drawn value is in the trace, so the
//! sweep records that 9 and 10 were drawn even when the bound is what ran.
//!
//! The fake receiver forks a shell per call — a lagged open reads the listing
//! eleven times — so the lag bands are two seeds each, eight in all: the host
//! caps this family at process starts, not at the tiers the action itself
//! would serve. The other receivers keep their six-seed bands.

#![cfg(all(unix, feature = "script", feature = "process", feature = "ephemeral"))]

use std::error::Error;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lgwks_bot::BotError;
use lgwks_bot::domain::gh::{IssueDraft, Settlement};
use lgwks_bot::rt::clock::Clock;

use crate::fake_gh_issues::creates_on;
use crate::gh_issue::{
    failed, fake_host, fake_host_with_clock, full_grant, open, open_on_clock, opened,
};
use crate::sim::{self, Band, Sim};

/// A family's result: an error fails it with the seed's text.
type SimResult = Result<(), Box<dyn Error>>;

/// How many seeds the family sweeps.
const SEEDS: u64 = 48;

/// Distinct owners for every pass, so no pass reads another's store.
static PASS: AtomicU64 = AtomicU64::new(0);

/// The receivers a seed draws from, by the repository name the fake reads.
const RECEIVERS: [&str; 3] = ["accept", "drop", "refuse"];

/// The action's settle bound, in listing reads.
///
/// The action keeps its bound private; this is the same eight, and a change to
/// one without the other is what the model below would stop agreeing with.
const SETTLE_READS: u32 = 8;

/// What the model says the `run`th open (from zero) of one key settles as,
/// when its listing lags `lag` reads.
fn expected(receiver: &str, lag: u32, run: u32) -> &'static str {
    match (receiver, run) {
        ("accept", 0) => "created",
        ("drop", 0) => "reconciled",
        ("refuse", _) => "indeterminate",
        ("lag", 0) if lag >= SETTLE_READS => "unlisted",
        ("lag", 0) => "created",
        _ => "found",
    }
}

/// One run's settlement as the model spells it.
fn settled(
    report: &lgwks_bot::task::Report<lgwks_bot::domain::gh::OpenedIssue>,
) -> Result<&'static str, String> {
    if let Ok(issue) = opened(report) {
        return Ok(match issue.settlement() {
            Settlement::Created => "created",
            Settlement::Found => "found",
            Settlement::Reconciled => "reconciled",
            Settlement::Unlisted => "unlisted",
            other => {
                lgwks_std::trace::debug!(settlement = ?other, "sim_gh_issue: an unmodelled settlement");
                return Err(format!("an unmodelled settlement {other:?}"));
            }
        });
    }
    match *failed(report)? {
        BotError::EffectIndeterminate { ref domain, .. } if domain == "gh::issue" => {
            Ok("indeterminate")
        }
        ref other => Err(format!("an unmodelled failure {other:?}")),
    }
}

/// One seed on a drawn receiver.
fn seed(sim: &mut Sim) -> SimResult {
    let receiver = *RECEIVERS
        .get(usize::try_from(sim.rng().below(3))?)
        .ok_or("three receivers")?;
    seed_on(sim, receiver)
}

/// One seed on `receiver`: two tenants, one repository, each rerunning its
/// own key in a seeded order.
///
/// A `lag` seed draws its listing lag here, from the run's own stream, and
/// opens on a clock this scenario advances: the settle schedule costs no real
/// time, and the trace records the drawn lag beside the settlements.
fn seed_on(sim: &mut Sim, receiver: &str) -> SimResult {
    let pass = PASS.fetch_add(1, Ordering::Relaxed);
    let owner = format!("sim{pass}");
    let drawn = if receiver == "lag" {
        sim.rng().below(11)
    } else {
        0
    };
    let lag = drawn.min(SETTLE_READS);
    let target = if receiver == "lag" {
        format!("lag-{lag}")
    } else {
        receiver.to_owned()
    };
    let repo = format!("{owner}/{target}");
    let clock = Clock::virtual_at(Duration::ZERO);
    let tenants = if receiver == "lag" {
        [
            fake_host_with_clock("acme", full_grant(), clock.clone())?,
            fake_host_with_clock("zenith", full_grant(), clock.clone())?,
        ]
    } else {
        [
            fake_host("acme", full_grant())?,
            fake_host("zenith", full_grant())?,
        ]
    };
    let drafts = [
        IssueDraft::new("acme-key", "Opened by acme", "")?,
        IssueDraft::new("zenith-key", "Opened by zenith", "")?,
    ];
    let mut left = [
        sim.rng().below(4).saturating_add(1),
        sim.rng().below(4).saturating_add(1),
    ];
    let mut done = [0_u32; 2];
    let runs = left
        .iter()
        .fold(0_u32, |sum, &each| sum.saturating_add(each));
    let mut trace = Vec::new();
    while left.iter().any(|&remaining| remaining > 0) {
        let mut turn = usize::try_from(sim.rng().below(2))?;
        if left.get(turn).copied() == Some(0) {
            turn ^= 1;
        }
        let host = tenants.get(turn).ok_or("two tenants")?;
        let draft = drafts.get(turn).ok_or("two drafts")?;
        let report = if receiver == "lag" {
            open_on_clock(host, &clock, &repo, draft)?
        } else {
            open(host, &repo, draft)?
        };
        let run = done.get(turn).copied().ok_or("two counts")?;
        let got = settled(&report)?;
        assert_eq!(
            got,
            expected(receiver, lag, run),
            "seed {}: tenant {turn}'s run {run} on {receiver} lagging {drawn}",
            sim.seed
        );
        trace.push(format!("{turn}{got}"));
        if let (Some(remaining), Some(count)) = (left.get_mut(turn), done.get_mut(turn)) {
            *remaining = remaining.saturating_sub(1);
            *count = count.saturating_add(1);
        }
    }
    let creates = creates_on(&owner)?;
    let model = if receiver == "refuse" {
        usize::try_from(runs)?
    } else {
        2
    };
    assert_eq!(
        creates, model,
        "seed {}: one create per key on {receiver} lagging {drawn}",
        sim.seed
    );
    sim.record(&format!(
        "{receiver}:{drawn}:{lag}:{}:{creates}",
        trace.join(",")
    ));
    Ok(())
}

#[test]
fn one_key_holds_at_most_one_issue_under_reruns() -> SimResult {
    sim::assert_replays(Band::new(0, SEEDS), seed)
}

/// Seeds per receiver band.
const BAND_SEEDS: u64 = 6;

/// Seeds per lagging band: a lagged open reads the listing up to eleven times,
/// so the fake's process budget buys fewer of them than the other receivers.
const LAG_BAND_SEEDS: u64 = 2;

/// Band `index` of `receiver`, on seeds disjoint from every other band.
fn receiver_band(receiver: &str, index: u64) -> SimResult {
    let first = SEEDS.saturating_add(index.saturating_mul(BAND_SEEDS));
    sim::assert_replays(Band::new(first, BAND_SEEDS), |sim| seed_on(sim, receiver))
}

/// Band `index` of the lagging receiver, on the shorter seed budget its
/// process starts allow.
fn lag_band(index: u64) -> SimResult {
    let first = SEEDS.saturating_add(index.saturating_mul(BAND_SEEDS));
    sim::assert_replays(Band::new(first, LAG_BAND_SEEDS), |sim| seed_on(sim, "lag"))
}

#[test]
fn an_answering_receiver_holds_one_issue_per_key_band_0() -> SimResult {
    receiver_band("accept", 0)
}

#[test]
fn an_answering_receiver_holds_one_issue_per_key_band_1() -> SimResult {
    receiver_band("accept", 1)
}

#[test]
fn an_answering_receiver_holds_one_issue_per_key_band_2() -> SimResult {
    receiver_band("accept", 2)
}

#[test]
fn an_answering_receiver_holds_one_issue_per_key_band_3() -> SimResult {
    receiver_band("accept", 3)
}

#[test]
fn a_lost_answer_is_reconciled_never_reposted_band_0() -> SimResult {
    receiver_band("drop", 4)
}

#[test]
fn a_lost_answer_is_reconciled_never_reposted_band_1() -> SimResult {
    receiver_band("drop", 5)
}

#[test]
fn a_lost_answer_is_reconciled_never_reposted_band_2() -> SimResult {
    receiver_band("drop", 6)
}

#[test]
fn a_lost_answer_is_reconciled_never_reposted_band_3() -> SimResult {
    receiver_band("drop", 7)
}

#[test]
fn a_refused_create_holds_nothing_and_may_be_retried_band_0() -> SimResult {
    receiver_band("refuse", 8)
}

#[test]
fn a_refused_create_holds_nothing_and_may_be_retried_band_1() -> SimResult {
    receiver_band("refuse", 9)
}

#[test]
fn a_refused_create_holds_nothing_and_may_be_retried_band_2() -> SimResult {
    receiver_band("refuse", 10)
}

#[test]
fn a_refused_create_holds_nothing_and_may_be_retried_band_3() -> SimResult {
    receiver_band("refuse", 11)
}

#[test]
fn a_lagging_listing_never_doubles_a_key_band_0() -> SimResult {
    lag_band(12)
}

#[test]
fn a_lagging_listing_never_doubles_a_key_band_1() -> SimResult {
    lag_band(13)
}

#[test]
fn a_lagging_listing_never_doubles_a_key_band_2() -> SimResult {
    lag_band(14)
}

#[test]
fn a_lagging_listing_never_doubles_a_key_band_3() -> SimResult {
    lag_band(15)
}
