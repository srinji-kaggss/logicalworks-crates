//! Simulation family: `act gh::issue` under seeded receivers, reruns and
//! tenants.
//!
//! Each seed draws a receiver (one that answers, one that loses the answer
//! after holding the issue, one that is killed between the POST and its reply,
//! one that refuses the create, and the same-but-lagging forms), a listing lag
//! of 0 to 10 reads, how many times each of two tenants reruns its open, and
//! the order the tenants take turns. Both tenants open on one repository under
//! their own keys. The model is the rule the action promises: one key, at most
//! one held issue, whatever the reruns and however long the listing lags; a
//! refused create holds nothing, so a rerun may try again.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `one_key_holds_at_most_one_issue_under_reruns` | reruns, lost answers, kills and two tenants never hold two issues for one key, and each run settles as the model says |
//!
//! The band is swept twice through `sim::assert_replays`; each pass opens on
//! fresh owners, so the second pass replays the first rather than finding its
//! issues.
//!
//! # A lagging listing, drawn per seed
//!
//! Every seed draws its listing lag from 0 to 10 reads from the run's own
//! stream, and every open runs on a flow clock the scenario advances, so the
//! action's settle schedule costs no real time. The receiver hides the created
//! issue from that many listing reads (`lag-N`, `droplag-N`, `kill-N`). The lag
//! is applied as drawn, never clamped: the action settles both before a create
//! and after a lost answer, so a lag of 9 or 10 is covered by the bound rather
//! than hidden by it, and the drawn value is recorded in the trace so a sweep
//! says which lags it reached.
//!
//! The fake receiver forks a shell per call, and a settled open reads the
//! listing up to sixteen times, so the family pays in process starts rather
//! than in seconds. Each tenant opens at most twice, which is the fewest that
//! still holds a duplicate to its own key, and the host caps this family by
//! process starts rather than by the tiers the action itself would serve.

#![cfg(all(unix, feature = "script", feature = "process", feature = "ephemeral"))]

use std::error::Error;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lgwks_bot::BotError;
use lgwks_bot::domain::gh::{IssueDraft, Settlement};
use lgwks_bot::rt::clock::Clock;

use crate::fake_gh_issues::{creates_on, held_on};
use crate::gh_issue::{failed, fake_host_with_clock, full_grant, open_on_clock, opened};
use crate::sim::{self, Band, Sim};

/// A family's result: an error fails it with the seed's text.
type SimResult = Result<(), Box<dyn Error>>;

/// How many seeds the family sweeps.
const SEEDS: u64 = 48;

/// Distinct owners for every pass, so no pass reads another's store.
static PASS: AtomicU64 = AtomicU64::new(0);

/// The receivers a seed draws from, by the repository name the fake reads.
///
/// The first three keep their answer; `lag` holds it and hides the issue from
/// the listing; `droplag` and `kill` lose the answer and hide it too, which is
/// the pair #658 is about — a rerun with no number, looking at a lagging
/// listing.
const RECEIVERS: [&str; 6] = ["accept", "drop", "refuse", "lag", "droplag", "kill"];

/// The action's settle bound, in listing reads.
///
/// The action keeps its bound private; this is the same eight, and a change to
/// one without the other is what the model below would stop agreeing with.
const SETTLE_READS: u32 = 8;

/// The highest listing lag a seed draws, in reads.
const MAX_DRAWN_LAG: u32 = 10;

/// What the model says the `run`th open (from zero) of one key settles as,
/// when its listing lags `lag` reads.
fn expected(receiver: &str, lag: u32, run: u32) -> &'static str {
    match (receiver, run) {
        ("accept", 0) => "created",
        ("drop", 0) => "reconciled",
        ("refuse", _) => "indeterminate",
        ("lag", 0) if lag >= SETTLE_READS => "unlisted",
        ("lag", 0) => "created",
        // A lost answer spends the read-back's settle on the same lag, so a
        // lag past the bound is still indeterminate on the first run — and the
        // rerun's own settle then finds it, which is what keeps one create.
        ("droplag" | "kill", 0) if lag >= SETTLE_READS => "indeterminate",
        ("droplag" | "kill", 0) => "reconciled",
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
        .get(usize::try_from(sim.rng().below(6))?)
        .ok_or("six receivers")?;
    seed_on(sim, receiver)
}

/// The repository name a seed opens on: the drawn lag is part of it for the
/// receivers whose listing lags, so the fake reads the lag from the name.
fn target(receiver: &str, drawn: u32) -> String {
    match receiver {
        "lag" => format!("lag-{drawn}"),
        "droplag" => format!("droplag-{drawn}"),
        "kill" => format!("kill-{drawn}"),
        other => other.to_owned(),
    }
}

/// One seed on `receiver`: two tenants, one repository, each rerunning its
/// own key in a seeded order.
///
/// The lag is drawn here, from the run's own stream, for every receiver, and
/// recorded even when the receiver keeps its answer: a sweep that drew 9 and
/// 10 must say so, whatever the bound then did.
fn seed_on(sim: &mut Sim, receiver: &str) -> SimResult {
    let pass = PASS.fetch_add(1, Ordering::Relaxed);
    let owner = format!("sim{pass}");
    let drawn = sim.rng().below(MAX_DRAWN_LAG.saturating_add(1));
    let repo_part = target(receiver, drawn);
    let repo = format!("{owner}/{repo_part}");
    // One clock for every receiver: the settle has no real schedule to wait
    // for, and the scenario owns every wait on it.
    let clock = Clock::virtual_at(Duration::ZERO);
    let tenants = [
        fake_host_with_clock("acme", full_grant(), clock.clone())?,
        fake_host_with_clock("zenith", full_grant(), clock.clone())?,
    ];
    let drafts = [
        IssueDraft::new("acme-key", "Opened by acme", "")?,
        IssueDraft::new("zenith-key", "Opened by zenith", "")?,
    ];
    let mut left = [
        sim.rng().below(2).saturating_add(1),
        sim.rng().below(2).saturating_add(1),
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
        let report = open_on_clock(host, &clock, &repo, draft)?;
        let run = done.get(turn).copied().ok_or("two counts")?;
        let got = settled(&report)?;
        assert_eq!(
            got,
            expected(receiver, drawn, run),
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
    // One create per key, whatever the reruns. A refused create holds nothing,
    // so the refusals are the one family where the count is the run count.
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
    // The count says how many POSTs were issued; the store says how many
    // issues exist, which is the fact a rerun lives with.
    let held = held_on(&owner, &repo_part)?;
    let held_model = if receiver == "refuse" { 0 } else { 2 };
    assert_eq!(
        held, held_model,
        "seed {}: one held issue per key on {receiver} lagging {drawn}",
        sim.seed
    );
    sim.record(&format!(
        "{receiver}:{drawn}:{creates}:{held}:{}",
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

/// Band `index` of `receiver`, on seeds disjoint from every other band.
fn receiver_band(receiver: &str, index: u64) -> SimResult {
    let first = SEEDS.saturating_add(index.saturating_mul(BAND_SEEDS));
    sim::assert_replays(Band::new(first, BAND_SEEDS), |sim| seed_on(sim, receiver))
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
    receiver_band("lag", 12)
}

#[test]
fn a_lagging_listing_never_doubles_a_key_band_1() -> SimResult {
    receiver_band("lag", 13)
}

#[test]
fn a_lagging_listing_never_doubles_a_key_band_2() -> SimResult {
    receiver_band("lag", 14)
}

#[test]
fn a_lagging_listing_never_doubles_a_key_band_3() -> SimResult {
    receiver_band("lag", 15)
}

#[test]
fn a_lost_answer_under_a_lagging_listing_never_doubles_a_key_band_0() -> SimResult {
    receiver_band("droplag", 16)
}

#[test]
fn a_lost_answer_under_a_lagging_listing_never_doubles_a_key_band_1() -> SimResult {
    receiver_band("droplag", 17)
}

#[test]
fn a_lost_answer_under_a_lagging_listing_never_doubles_a_key_band_2() -> SimResult {
    receiver_band("droplag", 18)
}

#[test]
fn a_lost_answer_under_a_lagging_listing_never_doubles_a_key_band_3() -> SimResult {
    receiver_band("droplag", 19)
}

#[test]
fn a_killed_create_under_a_lagging_listing_never_doubles_a_key_band_0() -> SimResult {
    receiver_band("kill", 20)
}

#[test]
fn a_killed_create_under_a_lagging_listing_never_doubles_a_key_band_1() -> SimResult {
    receiver_band("kill", 21)
}

#[test]
fn a_killed_create_under_a_lagging_listing_never_doubles_a_key_band_2() -> SimResult {
    receiver_band("kill", 22)
}

#[test]
fn a_killed_create_under_a_lagging_listing_never_doubles_a_key_band_3() -> SimResult {
    receiver_band("kill", 23)
}

#[test]
fn an_answering_receiver_holds_one_issue_per_key_band_4() -> SimResult {
    receiver_band("accept", 24)
}

#[test]
fn an_answering_receiver_holds_one_issue_per_key_band_5() -> SimResult {
    receiver_band("accept", 25)
}

#[test]
fn a_lost_answer_is_reconciled_never_reposted_band_4() -> SimResult {
    receiver_band("drop", 26)
}

#[test]
fn a_lost_answer_is_reconciled_never_reposted_band_5() -> SimResult {
    receiver_band("drop", 27)
}

#[test]
fn a_refused_create_holds_nothing_and_may_be_retried_band_4() -> SimResult {
    receiver_band("refuse", 28)
}

#[test]
fn a_refused_create_holds_nothing_and_may_be_retried_band_5() -> SimResult {
    receiver_band("refuse", 29)
}

#[test]
fn a_lagging_listing_never_doubles_a_key_band_4() -> SimResult {
    receiver_band("lag", 30)
}

#[test]
fn a_lagging_listing_never_doubles_a_key_band_5() -> SimResult {
    receiver_band("lag", 31)
}

#[test]
fn a_lost_answer_under_a_lagging_listing_never_doubles_a_key_band_4() -> SimResult {
    receiver_band("droplag", 32)
}

#[test]
fn a_lost_answer_under_a_lagging_listing_never_doubles_a_key_band_5() -> SimResult {
    receiver_band("droplag", 33)
}

#[test]
fn a_killed_create_under_a_lagging_listing_never_doubles_a_key_band_4() -> SimResult {
    receiver_band("kill", 34)
}

#[test]
fn a_killed_create_under_a_lagging_listing_never_doubles_a_key_band_5() -> SimResult {
    receiver_band("kill", 35)
}
