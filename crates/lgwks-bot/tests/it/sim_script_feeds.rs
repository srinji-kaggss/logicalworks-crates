//! Simulation family: `whenever` and `justify` on the real run path.
//!
//! Each word in flows the real `script!` macro expanded, driven on the real
//! `lgwks_bot::script` runtime. What the seed controls is the input: the
//! feed's batches and its policy, and which evaluation the rule stops. Time is
//! not consulted.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `whenever_bounded` | past its buffer a feed sheds or refuses exactly as its policy says, counted |
//! | `whenever_in_order` | events are handled in arrival order, each in its own scope with its own key |
//! | `whenever_tenants_never_share_a_key` | two tenants fed the same events walk the same paths under disjoint keys |
//! | `justify_stops_before_the_body` | a step the rules stop fails with `Justified`, naming the rule, before its body runs |
//! | `justify_governs_its_block` | the rules govern only the block's own scope, never a sibling's |
//!
//! Each declared test sweeps its band twice and requires identical trace
//! hashes, so a nondeterministic run fails even when every assertion passes.

#![cfg(feature = "script")]

use std::collections::VecDeque;
use std::error::Error;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lgwks_bot::rt::sync::CancellationToken;
use lgwks_bot::script::{FlowError, Justify, PumpReport, Rule, Scope, Verdict};

use crate::band_family;
use crate::sim::{self, Band};
use crate::sim_script::{drive, locked, tenant_scope};
use crate::sim_script_words::{TestResult, note, unexpected};

/// A live feed: the batches still to arrive, and every event handled with the
/// scope it was handled in.
#[derive(Default)]
struct Feed {
    /// The batches still to arrive.
    batches: Mutex<VecDeque<Vec<u32>>>,
    /// Each event handled, with its path and key.
    handled: Mutex<Vec<(u32, String, String)>>,
}

impl Feed {
    /// The next batch, or `None` when the feed has ended.
    async fn next(&self) -> Option<Vec<u32>> {
        self.batches.lock().ok()?.pop_front()
    }

    /// Handle one event in `scope`.
    async fn handle(&self, scope: &Scope, tick: u32) -> Result<(), FlowError> {
        note(
            &self.handled,
            (tick, scope.path().to_owned(), scope.key().to_hex()),
        );
        Ok(())
    }
}

/// An upstream whose calls are counted.
#[derive(Default)]
struct Api {
    /// Calls that reached the body.
    calls: AtomicUsize,
}

impl Api {
    /// One call.
    async fn call(&self, _scope: &Scope) -> Result<u64, FlowError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(7)
    }
}

lgwks_bot::script! {
    flow drop_oldest(feed: &Feed) -> PumpReport:
        let report = whenever tick in feed.next().await, buffering 4, drop oldest:
            feed.handle(scope, tick).await?
        give back report

    flow drop_newest(feed: &Feed) -> PumpReport:
        let report = whenever tick in feed.next().await, buffering 4, drop newest:
            feed.handle(scope, tick).await?
        give back report

    flow refuse(feed: &Feed) -> PumpReport:
        let report = whenever tick in feed.next().await, buffering 4, refuse:
            feed.handle(scope, tick).await?
        give back report

    flow governed(api: &Api, rules: &Justify) -> u64:
        let code = justify with (rules):
            step call:
                api.call(scope).await?
        give back code

    flow siblings(api: &Api, rules: &Justify) -> u64:
        let free = step free:
            api.call(scope).await?
        let ruled = justify with (rules):
            step call:
                api.call(scope).await?
        give back free.saturating_add(ruled)

}

/// The overflow policies the three `whenever` flows write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    /// `drop oldest`.
    Oldest,
    /// `drop newest`.
    Newest,
    /// `refuse`.
    Refuse,
}

/// The buffer every `whenever` flow writes.
const BUFFER: usize = 4;

/// A seeded feed and the policy to run it under.
fn seeded_feed(sim: &mut sim::Sim) -> (Vec<Vec<u32>>, Policy) {
    let policy = match sim.rng().below(3) {
        0 => Policy::Oldest,
        1 => Policy::Newest,
        _ => Policy::Refuse,
    };
    let mut next = 0_u32;
    let mut batches = Vec::new();
    for _ in 0..sim.rng().between(1, 5) {
        let size = sim.rng().between(0, 9);
        batches.push(
            (0..size)
                .map(|_| {
                    next = next.saturating_add(1);
                    next
                })
                .collect(),
        );
    }
    (batches, policy)
}

/// What the gate hands on, kept by hand: each batch crosses a buffer of
/// [`BUFFER`] by `policy`, then drains. Returns the events handled in order and
/// the counts shed and refused.
fn feed_model(batches: &[Vec<u32>], policy: Policy) -> (Vec<u32>, u64, u64) {
    let (mut handled, mut dropped, mut refused) = (Vec::new(), 0_u64, 0_u64);
    for batch in batches {
        let mut gate: VecDeque<u32> = VecDeque::new();
        for &event in batch {
            if gate.len() < BUFFER {
                gate.push_back(event);
                continue;
            }
            match policy {
                Policy::Oldest => {
                    gate.pop_front();
                    dropped = dropped.saturating_add(1);
                    gate.push_back(event);
                }
                Policy::Newest => dropped = dropped.saturating_add(1),
                Policy::Refuse => {
                    refused = refused.saturating_add(1);
                    handled.extend(gate.pop_front());
                    gate.push_back(event);
                }
            }
        }
        handled.extend(gate);
    }
    (handled, dropped, refused)
}

/// A feed that will hand on `batches`, in order.
fn feed_of(batches: &[Vec<u32>]) -> Feed {
    Feed {
        batches: Mutex::new(batches.iter().cloned().collect()),
        ..Feed::default()
    }
}

/// Run `batches` through the flow written for `policy`.
fn run_feed(batches: &[Vec<u32>], policy: Policy) -> Result<(PumpReport, Feed), Box<dyn Error>> {
    let feed = feed_of(batches);
    let scope = tenant_scope("acme", CancellationToken::new())?;
    let report = match policy {
        Policy::Oldest => drive(drop_oldest(&scope, &feed))?,
        Policy::Newest => drive(drop_newest(&scope, &feed))?,
        Policy::Refuse => drive(refuse(&scope, &feed))?,
    };
    Ok((report, feed))
}

/// Past its buffer a feed sheds or refuses exactly as its policy says, and
/// counts what it shed.
fn whenever_bounded(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let (batches, policy) = seeded_feed(sim);
        let (report, _) = run_feed(&batches, policy)?;
        let (handled, dropped, refused) = feed_model(&batches, policy);
        assert_eq!(
            report.handled,
            u64::try_from(handled.len())?,
            "every retained event was handled"
        );
        assert_eq!(
            report.dropped, dropped,
            "{policy:?} shed exactly what overflowed"
        );
        assert_eq!(
            report.refused, refused,
            "{policy:?} refused exactly what overflowed"
        );
        sim.record(&format!(
            "policy={policy:?} batches={batches:?} report={report:?}"
        ));
        Ok(())
    })
}

/// Events are handled in arrival order, each in its own scope with its own
/// key.
fn whenever_in_order(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let (batches, policy) = seeded_feed(sim);
        let (_, feed) = run_feed(&batches, policy)?;
        let handled = locked(&feed.handled, "the event log was poisoned")?;
        let order: Vec<u32> = handled.iter().map(|entry| entry.0).collect();
        assert_eq!(
            order,
            feed_model(&batches, policy).0,
            "events are handled in arrival order"
        );
        let mut keys: Vec<&String> = handled.iter().map(|entry| &entry.2).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), handled.len(), "each event has a key of its own");
        for (index, entry) in handled.iter().enumerate() {
            assert!(
                entry.1.ends_with(&format!("/event#{index}")),
                "event {index} ran in {}",
                entry.1
            );
        }
        sim.record(&format!("policy={policy:?} order={order:?}"));
        Ok(())
    })
}

/// Two tenants fed the same events walk the same paths under keys neither
/// shares with the other.
fn whenever_tenants_never_share_a_key(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let (batches, _) = seeded_feed(sim);
        let first = format!("tenant-{}", sim.rng().below(1_000));
        let second = format!("{first}-other");
        let mut runs = Vec::with_capacity(2);
        for tenant in [&first, &second] {
            let feed = feed_of(&batches);
            let scope = tenant_scope(tenant, CancellationToken::new())?;
            drive(refuse(&scope, &feed))?;
            runs.push(locked(&feed.handled, "the event log was poisoned")?.clone());
        }
        let (Some(ours), Some(theirs)) = (runs.first(), runs.get(1)) else {
            return unexpected(String::from("a tenant's run left no log"));
        };
        let paths = |run: &[(u32, String, String)]| -> Vec<String> {
            run.iter().map(|entry| entry.1.clone()).collect()
        };
        assert_eq!(
            paths(ours),
            paths(theirs),
            "the same events walk the same paths"
        );
        for entry in ours {
            assert!(
                theirs.iter().all(|other| other.2 != entry.2),
                "{first} and {second} share the key of {}",
                entry.1
            );
        }
        sim.record(&format!("tenants={first},{second} events={}", ours.len()));
        Ok(())
    })
}

/// A buffer of zero, or past the ceiling, is refused before the feed is read:
/// no batch is taken and no event handled, at every seeded size.
#[test]
fn sim_a_buffer_outside_its_bounds_reads_nothing() -> TestResult {
    static SITE: lgwks_bot::script::Site = lgwks_bot::script::Site::new(
        lgwks_bot::script::StepKind::Whenever,
        1,
        "whenever tick in feed.next().await, buffering (cap), refuse:",
    );
    let mut sim = sim::Sim::new(0x0657_b0f5);
    for cap in [0, usize::MAX] {
        let batches = seeded_feed(&mut sim).0;
        let feed = feed_of(&batches);
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let outcome = drive(lgwks_bot::script::stream_at(
            &scope,
            "whenever",
            &SITE,
            cap,
            lgwks_bot::script::Overflow::Refuse,
            async || feed.next().await,
            async |scope: Scope, tick: u32| feed.handle(&scope, tick).await,
        ));
        match outcome {
            Err(FlowError::InvalidBound { .. }) => {}
            other => return unexpected(format!("cap {cap} ended {other:?}")),
        }
        let left = locked(&feed.batches, "the feed was poisoned")?.len();
        assert_eq!(left, batches.len(), "cap {cap}: no batch was taken");
        assert!(
            locked(&feed.handled, "the event log was poisoned")?.is_empty(),
            "cap {cap}: no event was handled"
        );
    }
    Ok(())
}

/// A rule that logs every path it is asked about and blocks the `stop_at`th
/// evaluation, or none when `stop_at` is zero.
fn gate_rule(log: &Arc<Mutex<Vec<String>>>, stop_at: usize) -> Justify {
    let log = Arc::clone(log);
    let seen = AtomicUsize::new(0);
    Justify::new(vec![Rule::new("gate", move |ctx| {
        note(&log, ctx.at.to_owned());
        let count = seen.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        if count == stop_at {
            Verdict::Block {
                reason: String::from("closed"),
            }
        } else {
            Verdict::Allow
        }
    })])
}

/// A step the rules stop fails with `Justified`, naming the rule, before its
/// body runs.
fn justify_stops_before_the_body(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        // 1 stops the block at its entry, 2 the step inside it, 0 nothing.
        let stop_at = usize::try_from(sim.rng().below(3))?;
        let log = Arc::new(Mutex::new(Vec::new()));
        let api = Api::default();
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let outcome = drive(governed(&scope, &api, &gate_rule(&log, stop_at)));
        let calls = api.calls.load(Ordering::Relaxed);
        match (stop_at, outcome) {
            (0, Ok(code)) => {
                assert_eq!(code, 7, "allowed, the body ran and gave back its value");
                assert_eq!(calls, 1, "the body ran once");
            }
            (1 | 2, Err(FlowError::Justified { rule, reason, .. })) => {
                assert_eq!(rule, "gate", "the refusal names the rule");
                assert_eq!(reason, "closed", "and carries its reason");
                assert_eq!(calls, 0, "the body never ran");
            }
            (stop, other) => return unexpected(format!("stop_at={stop} ended {other:?}")),
        }
        let asked = locked(&log, "the rule log was poisoned")?;
        sim.record(&format!("stop_at={stop_at} asked={asked:?} calls={calls}"));
        Ok(())
    })
}

/// The rules govern only the block's own scope, never a sibling's.
fn justify_governs_its_block(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let stop_at = usize::try_from(sim.rng().below(3))?;
        let log = Arc::new(Mutex::new(Vec::new()));
        let api = Api::default();
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let outcome = drive(siblings(&scope, &api, &gate_rule(&log, stop_at)));
        let asked = locked(&log, "the rule log was poisoned")?;
        assert!(!asked.is_empty(), "the rules were asked inside their block");
        for path in &asked {
            assert!(
                path.starts_with("siblings/justify"),
                "the rules were asked about {path}, outside their block"
            );
        }
        let calls = api.calls.load(Ordering::Relaxed);
        assert_eq!(
            outcome.is_ok(),
            stop_at == 0,
            "only a stop inside the block fails the flow"
        );
        assert_eq!(
            calls,
            if stop_at == 0 { 2 } else { 1 },
            "the sibling step ran either way"
        );
        sim.record(&format!("stop_at={stop_at} asked={asked:?} calls={calls}"));
        Ok(())
    })
}

band_family::band_family! {
    whenever_bounded_band_00 => whenever_bounded, 0;
    whenever_bounded_band_01 => whenever_bounded, 1;
    whenever_bounded_band_02 => whenever_bounded, 2;
    whenever_bounded_band_03 => whenever_bounded, 3;
    whenever_in_order_band_00 => whenever_in_order, 0;
    whenever_in_order_band_01 => whenever_in_order, 1;
    whenever_in_order_band_02 => whenever_in_order, 2;
    whenever_in_order_band_03 => whenever_in_order, 3;
    whenever_tenants_never_share_a_key_band_00 => whenever_tenants_never_share_a_key, 0;
    whenever_tenants_never_share_a_key_band_01 => whenever_tenants_never_share_a_key, 1;
    justify_stops_before_the_body_band_00 => justify_stops_before_the_body, 0;
    justify_stops_before_the_body_band_01 => justify_stops_before_the_body, 1;
    justify_stops_before_the_body_band_02 => justify_stops_before_the_body, 2;
    justify_stops_before_the_body_band_03 => justify_stops_before_the_body, 3;
    justify_governs_its_block_band_00 => justify_governs_its_block, 0;
    justify_governs_its_block_band_01 => justify_governs_its_block, 1;
    justify_governs_its_block_band_02 => justify_governs_its_block, 2;
    justify_governs_its_block_band_03 => justify_governs_its_block, 3;
}
