//! Simulation family: the justification WAF, the needs graph, compensation
//! and the bounded stream on the real run path.
//!
//! The runtime words beside the bounds: justification rules that allow, slow,
//! park or block a run with its reason code; a needs-ordered dependency graph
//! whose failures block only downstream; compensations that undo finished
//! steps in reverse order; and a bounded live feed whose bursts follow the
//! drawn overflow policy. Every scenario runs the real runtime functions on a
//! real scope — no `script!` expansion, no reimplementation. What the seed
//! controls is the input: the allowance, the pacing, the suspension point,
//! the graph's works and failing lane, the failing step, the gate's cap and
//! policy, and the feed's batches. Time is not consulted: a run that merely
//! ran slower reads exactly as a fast one.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `justification_sequence_replays` | the same seed reaches the same allow/slow/park/block sequence, and the rules are consulted in order |
//! | `graph_schedule_respects_needs_capacity_and_bound` | needs respected, each node once, bounded concurrency, failures blocking only downstream, the greedy critical-path schedule round for round, and its makespan inside the Graham bound |
//! | `compensation_replays_in_reverse` | a seeded failing step undoes exactly the finished steps in reverse order, and a seeded success undoes nothing |
//! | `stream_gate_bounds_the_feed` | every event is handled in order, bursts past the cap follow the drawn policy, and the report counts what the policy shed |
//!
//! Each declared test sweeps its band twice and requires identical trace
//! hashes, so a nondeterministic run fails even when every assertion passes.

#![cfg(feature = "script")]

use std::cell::RefCell;
use std::error::Error;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lgwks_bot::rt::sync::CancellationToken;
use lgwks_bot::script::{
    Allowance, FlowError, GraphNode, Justify, NodeOutcome, Overflow, PumpReport, Rule, StreamGate,
    compensated, graph, pump, retry,
};

use crate::band_family;
use crate::sim::{self, Band};
use crate::sim_script::{Turns, drive, locked, tenant_scope};

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// The justification rules, the allowance they spend, and a seeded attempt
/// budget: the same seed reaches the same allow/slow/park/block sequence.
fn justification_sequence_replays(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let cap = u64::from(sim.rng().between(1, 4));
        let every = u64::from(sim.rng().between(2, 4));
        let suspend_after = u64::from(sim.rng().between(1, 4));
        let attempts: u32 = 6;
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let logged = |name: &'static str, inner: Rule, log: &Arc<Mutex<Vec<String>>>| {
            let logged = Arc::clone(log);
            let inner = Arc::new(Mutex::new(inner));
            Rule::new(name, move |ctx| {
                let verdict = match inner.lock() {
                    Ok(held) => held.decide(ctx),
                    Err(_) => return lgwks_bot::script::Verdict::Allow,
                };
                let word = if matches!(verdict, lgwks_bot::script::Verdict::Allow) {
                    "allow"
                } else if matches!(verdict, lgwks_bot::script::Verdict::Slow { .. }) {
                    "slow"
                } else if matches!(verdict, lgwks_bot::script::Verdict::Park { .. }) {
                    "park"
                } else {
                    "block"
                };
                if let Ok(mut held) = logged.lock() {
                    held.push(format!("{name}:{word}"));
                }
                verdict
            })
        };
        let allowance = Arc::new(Allowance::new(cap));
        let justify = Justify::new(vec![
            logged("pace", Rule::pace("pace", every, Duration::ZERO), &log),
            logged(
                "budget",
                Rule::allowance("budget", Arc::clone(&allowance), "poll-budget-spent"),
                &log,
            ),
            logged(
                "quiet",
                Rule::suspend("quiet", suspend_after, "quiet-hours"),
                &log,
            ),
        ]);
        let scope = tenant_scope("acme", CancellationToken::new())?.with_justification(justify);
        let budget = lgwks_bot::script::attempts(attempts)
            .map_err(|error| format!("{attempts} is an attempt count: {error}"))?;
        let outcome = drive(retry(
            &scope,
            "storm",
            budget,
            Duration::ZERO,
            |_, _| async { Err::<(), FlowError>(FlowError::transient("the upstream is down")) },
        ));
        // The independent model: the same counters, kept by hand.
        let mut pace_seen: u64 = 0;
        let mut remaining = cap;
        let mut quiet_seen: u64 = 0;
        let mut expected: Vec<String> = Vec::new();
        let mut stopped: Option<(&str, &str)> = None;
        for _ in 1..=attempts {
            pace_seen = pace_seen.saturating_add(1);
            if pace_seen.is_multiple_of(every) {
                expected.push(String::from("pace:slow"));
                continue;
            }
            expected.push(String::from("pace:allow"));
            if remaining > 0 {
                remaining = remaining.saturating_sub(1);
                expected.push(String::from("budget:allow"));
            } else {
                expected.push(String::from("budget:block"));
                stopped = Some(("budget", "poll-budget-spent"));
                break;
            }
            quiet_seen = quiet_seen.saturating_add(1);
            if quiet_seen > suspend_after {
                expected.push(String::from("quiet:park"));
                stopped = Some(("quiet", "quiet-hours"));
                break;
            }
            expected.push(String::from("quiet:allow"));
        }
        let logged = locked(&log, "the decision log was poisoned")?;
        assert_eq!(
            logged, expected,
            "the runtime consults the rules in order and the seed replays the sequence"
        );
        match (outcome, stopped) {
            (Err(FlowError::Justified { rule, reason, .. }), Some((at_rule, at_reason))) => {
                assert_eq!(rule, at_rule, "the refusal names the deciding rule");
                assert_eq!(reason, at_reason, "the refusal carries its reason code");
            }
            (Err(FlowError::Exhausted { attempts: made, .. }), None) => {
                assert_eq!(made, attempts, "without a stop the whole budget is spent");
            }
            (outcome, stopped) => {
                let refusal: TestResult =
                    Err(format!("unexpected end: {outcome:?} after {stopped:?}").into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "sweep: returning an error to the caller");
                return refusal;
            }
        }
        sim.record(&format!(
            "cap={cap} every={every} after={suspend_after} log={logged:?}"
        ));
        Ok(())
    })
}

/// One seeded lane graph: roots fan out, a join waits for them, a publish
/// waits for the join, and one seeded node fails.
///
/// The probe counts concurrency: every body enters and leaves through it, so
/// the peak it saw is the concurrency the schedule actually ran.
struct LaneGraph {
    /// The nodes, in declaration order.
    nodes: Vec<GraphNode<'static, u64>>,
    /// Each node's name and the names it waits for.
    edges: Vec<(&'static str, Vec<&'static str>)>,
    /// Each node's name and its unit work cost.
    works: Vec<(&'static str, u64)>,
    /// The total unit work.
    total: u64,
    /// The slot capacity the graph runs under.
    slots: usize,
    /// The seeded failing node, when the seed draws one.
    failing: Option<&'static str>,
}

fn lane_graph(
    sim: &mut sim::Sim,
    live: &Arc<AtomicUsize>,
    peak: &Arc<AtomicUsize>,
    started: &Arc<AtomicUsize>,
) -> Result<LaneGraph, Box<dyn Error>> {
    // `docs` needs only `fetch-a` and `lint` needs nothing, so a slow
    // `fetch-b` must never hold either back: the schedule is greedy, not waved.
    const NAMES: [&str; 6] = ["fetch-a", "fetch-b", "merge", "publish", "lint", "docs"];
    const LANES: [(&str, &[&str]); 6] = [
        ("fetch-a", &[]),
        ("fetch-b", &[]),
        ("merge", &["fetch-a", "fetch-b"]),
        ("publish", &["merge"]),
        ("lint", &[]),
        ("docs", &["fetch-a"]),
    ];
    let failing = usize::try_from(sim.rng().below(7))?;
    let failing = NAMES.get(failing).copied();
    let slots = usize::try_from(sim.rng().between(1, 3))?;
    let mut total: u64 = 0;
    let mut works: Vec<(&'static str, u64)> = Vec::new();
    let mut nodes: Vec<GraphNode<'static, u64>> = Vec::new();
    let mut edges: Vec<(&'static str, Vec<&'static str>)> = Vec::new();
    for (index, lane) in LANES.iter().enumerate() {
        let work = u64::from(sim.rng().between(1, 3));
        total = total.saturating_add(work);
        works.push((lane.0, work));
        edges.push((lane.0, lane.1.to_vec()));
        let fails = failing == Some(lane.0);
        let value = u64::try_from(index)?;
        let live = Arc::clone(live);
        let peak = Arc::clone(peak);
        let started = Arc::clone(started);
        nodes.push(
            GraphNode::new(lane.0, lane.1, move |_| {
                let live = Arc::clone(&live);
                let peak = Arc::clone(&peak);
                let started = Arc::clone(&started);
                Box::pin(async move {
                    let now = live.fetch_add(1, Ordering::Relaxed).saturating_add(1);
                    peak.fetch_max(now, Ordering::Relaxed);
                    started.fetch_add(1, Ordering::Relaxed);
                    // The node holds its slot for exactly `work` rounds.
                    Turns(u32::try_from(work.saturating_sub(1)).map_err(|error| {
                        FlowError::failed(format!("a lane's work fits a u32: {error}"))
                    })?)
                    .await;
                    let result = if fails {
                        Err(FlowError::failed("a seeded lane failed"))
                    } else {
                        Ok(value)
                    };
                    live.fetch_sub(1, Ordering::Relaxed);
                    result
                })
            })
            .with_work(work),
        );
    }
    Ok(LaneGraph {
        nodes,
        edges,
        works,
        total,
        slots,
        failing,
    })
}

/// The critical-path cost of `edges` with per-node `works`: the longest chain
/// of work, the static bound the executed schedule is measured against.
fn critical_path(
    edges: &[(&'static str, Vec<&'static str>)],
    works: &[(&'static str, u64)],
) -> u64 {
    let mut cost = vec![0_u64; edges.len()];
    let mut changed = true;
    while changed {
        changed = false;
        for (index, edge) in edges.iter().enumerate() {
            let mut best = 0_u64;
            for need in &edge.1 {
                for (position, entry) in edges.iter().enumerate() {
                    if entry.0 == *need
                        && let Some(held) = cost.get(position)
                    {
                        best = best.max(*held);
                    }
                }
            }
            let mut work = 0_u64;
            for entry in works {
                if entry.0 == edge.0 {
                    work = entry.1;
                }
            }
            let ranked = best.saturating_add(work);
            if cost.get(index).is_some_and(|held| ranked > *held) {
                if let Some(slot) = cost.get_mut(index) {
                    *slot = ranked;
                }
                changed = true;
            }
        }
    }
    let mut longest = 0_u64;
    for value in cost {
        longest = longest.max(value);
    }
    longest
}

/// Divide `value` by `by` rounding up, or fail naming why.
fn ceil_div(value: u64, by: u64) -> Result<u64, String> {
    let shifted = value
        .checked_add(by.saturating_sub(1))
        .ok_or_else(|| String::from("the wave work overflows"))?;
    shifted
        .checked_div(by)
        .ok_or_else(|| String::from("the slot count is zero"))
}

/// The graph word on the real run path: needs respected, each node once,
/// bounded concurrency, failures blocking only downstream, and the static
/// schedule inside the Graham bound.
fn graph_schedule_respects_needs_capacity_and_bound(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let live: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
        let peak: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
        let started: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
        let setup = lane_graph(sim, &live, &peak, &started)?;
        let LaneGraph {
            nodes,
            edges,
            works,
            total,
            slots,
            failing,
        } = setup;
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let report = drive(graph(&scope, "lanes", slots, nodes))?;
        let ran = report
            .succeeded()
            .len()
            .saturating_add(report.failed().len());
        assert_eq!(
            ran.saturating_add(report.blocked().len()),
            edges.len(),
            "every declared node ran or was blocked, exactly once"
        );
        assert_eq!(
            started.load(Ordering::Relaxed),
            ran,
            "every ran node started its body once"
        );
        // Needs respected: every ran node's needs succeeded, and every blocked
        // node names only needs that did not succeed.
        for edge in &edges {
            match report.outcome(edge.0) {
                Some(outcome) => match *outcome {
                    NodeOutcome::Ran(_) => {
                        for need in &edge.1 {
                            assert!(
                                report.succeeded().contains(need),
                                "{} ran while its need {need} did not succeed",
                                edge.0
                            );
                        }
                    }
                    NodeOutcome::Blocked { ref by } => {
                        assert!(!by.is_empty(), "{} is blocked but names nothing", edge.0);
                        for unmet in by {
                            assert!(
                                edge.1.iter().any(|need| need == unmet),
                                "{} names {unmet}, which it never waited on",
                                edge.0
                            );
                        }
                    }
                    _ => {
                        let refusal: TestResult =
                            Err(format!("{} has an outcome this sim does not know", edge.0).into());
                        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "sweep: returning an error to the caller");
                        return refusal;
                    }
                },
                None => {
                    let refusal: TestResult = Err(format!("{} has no outcome", edge.0).into());
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "sweep: returning an error to the caller");
                    return refusal;
                }
            }
        }
        // The failure blocks exactly its downstream.
        let mut downstream: Vec<&'static str> = Vec::new();
        if let Some(failed) = failing {
            let mut open = vec![failed];
            while let Some(next) = open.pop() {
                for edge in &edges {
                    if edge.1.contains(&next) && !downstream.contains(&edge.0) && edge.0 != failed {
                        downstream.push(edge.0);
                        open.push(edge.0);
                    }
                }
            }
            downstream.sort_unstable();
        }
        let mut blocked = report.blocked();
        blocked.sort_unstable();
        assert_eq!(
            blocked, downstream,
            "a failed lane blocks everything downstream and nothing else"
        );
        // Bounded concurrency.
        let observed = peak.load(Ordering::Relaxed);
        assert!(
            observed <= slots,
            "at most {slots} nodes ran at once, saw {observed}"
        );
        // Greedy by critical path: the runtime started exactly what the hand
        // model starts, round by round, and the model's makespan sits inside
        // Graham's bound, ceil(total / slots) + the critical path. A lane of
        // work `w` waits `w - 1` turns, so it holds its slot `w` rounds.
        let (expected, makespan) = greedy_model(&edges, &works, slots, failing)?;
        assert_eq!(
            report.waves(),
            expected.as_slice(),
            "every free slot took the tallest ready node the moment it was ready"
        );
        let critical = critical_path(&edges, &works);
        let bound = ceil_div(total, u64::try_from(slots)?)?.saturating_add(critical);
        assert!(
            makespan <= bound,
            "the schedule takes {makespan} rounds against a bound of {bound}"
        );
        sim.record(&format!(
            "slots={slots} failing={failing:?} starts={:?} blocked={blocked:?} peak={observed} rounds={makespan}/{bound}",
            report.waves()
        ));
        Ok(())
    })
}

/// The nodes started at each round that started any, in start order.
type Starts = Vec<Vec<&'static str>>;

/// The greedy schedule kept by hand: at each round the free slots take the
/// ready nodes with the heaviest chain of work below them, declaration order
/// breaking ties; a node of work `w` holds its slot `w` rounds; a failed
/// node's downstream never becomes ready. Returns the nodes started at each
/// round that started any, and the round the last node finished.
fn greedy_model(
    edges: &[(&'static str, Vec<&'static str>)],
    works: &[(&'static str, u64)],
    slots: usize,
    failing: Option<&'static str>,
) -> Result<(Starts, u64), Box<dyn Error>> {
    let work_of = |name: &str| -> Result<u64, Box<dyn Error>> {
        works
            .iter()
            .find(|entry| entry.0 == name)
            .map(|entry| entry.1)
            .ok_or_else(|| format!("{name} has no work").into())
    };
    let mut height: Vec<u64> = Vec::with_capacity(edges.len());
    for edge in edges {
        height.push(work_of(edge.0)?);
    }
    // Heights settle from the leaves up; the graph has no cycle, so this ends.
    let mut changed = true;
    while changed {
        changed = false;
        for (at, edge) in edges.iter().enumerate() {
            let below = edges
                .iter()
                .enumerate()
                .filter(|other| other.1.1.contains(&edge.0))
                .filter_map(|other| height.get(other.0))
                .fold(0_u64, |heaviest, chain| heaviest.max(*chain));
            let tall = work_of(edge.0)?.saturating_add(below);
            if let Some(held) = height.get_mut(at)
                && tall > *held
            {
                *held = tall;
                changed = true;
            }
        }
    }
    let mut succeeded: Vec<&'static str> = Vec::new();
    let mut closed: Vec<&'static str> = Vec::new();
    let mut running: Vec<(&'static str, u64)> = Vec::new();
    let mut starts: Vec<Vec<&'static str>> = Vec::new();
    let mut now: u64 = 0;
    loop {
        let finished: Vec<&'static str> = running
            .iter()
            .filter(|entry| entry.1 == now)
            .map(|entry| entry.0)
            .collect();
        running.retain(|entry| entry.1 != now);
        for name in finished {
            if Some(name) != failing {
                succeeded.push(name);
                continue;
            }
            // The failure closes itself and everything downstream of it.
            let mut open = vec![name];
            closed.push(name);
            while let Some(next) = open.pop() {
                for edge in edges {
                    if edge.1.contains(&next) && !closed.contains(&edge.0) {
                        closed.push(edge.0);
                        open.push(edge.0);
                    }
                }
            }
        }
        let mut ready: Vec<(u64, usize, &'static str)> = Vec::new();
        for (at, edge) in edges.iter().enumerate() {
            let begun = closed.contains(&edge.0)
                || succeeded.contains(&edge.0)
                || running.iter().any(|entry| entry.0 == edge.0);
            if !begun && edge.1.iter().all(|need| succeeded.contains(need)) {
                let tall = height.get(at).copied().ok_or("a node has no height")?;
                ready.push((tall, at, edge.0));
            }
        }
        ready.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
        let mut batch = Vec::new();
        for entry in ready.into_iter().take(slots.saturating_sub(running.len())) {
            running.push((entry.2, now.saturating_add(work_of(entry.2)?)));
            batch.push(entry.2);
        }
        if !batch.is_empty() {
            starts.push(batch);
        }
        let Some(next) = running.iter().map(|entry| entry.1).min() else {
            return Ok((starts, now));
        };
        now = next;
    }
}

/// Compensation on the real run path: a seeded failing step undoes exactly
/// the finished steps in reverse order, and a seeded success undoes nothing.
fn compensation_replays_in_reverse(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let steps = ["reserve", "charge", "issue"];
        let fail_at = usize::try_from(sim.rng().below(4))?;
        let undone: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let failing: Option<&str> = if fail_at < steps.len() {
            steps.get(fail_at).copied()
        } else {
            None
        };
        let outcome = drive(compensated(&scope, "booking", |_scope, compensations| {
            let undone = Arc::clone(&undone);
            async move {
                for step in steps {
                    if Some(step) == failing {
                        let refusal = Err::<&'static str, FlowError>(FlowError::failed(step));
                        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "sweep: failing the seeded step");
                        return refusal;
                    }
                    let undone = Arc::clone(&undone);
                    let name = step;
                    compensations.add(name, move |_undo| {
                        let undone = Arc::clone(&undone);
                        Box::pin(async move {
                            match undone.lock() {
                                Ok(mut held) => held.push(name),
                                Err(error) => {
                                    let refusal = Err::<(), FlowError>(FlowError::failed(format!(
                                        "the undo order was poisoned: {error}"
                                    )));
                                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "sweep: the undo order was poisoned");
                                    return refusal;
                                }
                            }
                            Ok(())
                        })
                    })?;
                }
                Ok("ticket")
            }
        }));
        let undone = locked(&undone, "the undo order was poisoned")?;
        if let Some(failed) = failing {
            match outcome {
                Err(error) => assert!(
                    error.to_string().contains(failed),
                    "the caller gets the failing step's error: {error}"
                ),
                Ok(_) => {
                    let refusal: TestResult =
                        Err(String::from("the failing step must fail the run").into());
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "sweep: returning an error to the caller");
                    return refusal;
                }
            }
            let mut expected: Vec<&'static str> = Vec::new();
            for (position, step) in steps.iter().enumerate() {
                if position < fail_at {
                    expected.push(*step);
                }
            }
            expected.reverse();
            assert_eq!(
                undone, expected,
                "exactly the finished steps are undone, in reverse order, once"
            );
        } else {
            assert_eq!(
                outcome.map_err(|error| format!("{error}"))?,
                "ticket",
                "a run with no failing step succeeds"
            );
            assert!(undone.is_empty(), "success undoes nothing");
        }
        sim.record(&format!("fail_at={fail_at} undone={undone:?}"));
        Ok(())
    })
}

/// A seeded live feed through a bounded gate: every event is handled in
/// order, bursts past the cap follow the drawn policy, and the report counts
/// what the policy shed.
fn stream_gate_bounds_the_feed(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let cap = usize::try_from(sim.rng().between(1, 4))?;
        let policy = match sim.rng().below(3) {
            0 => Overflow::DropOldest,
            1 => Overflow::DropNewest,
            _ => Overflow::Refuse,
        };
        let batches: Vec<Vec<u32>> = (0..sim.rng().between(1, 4))
            .map(|_| {
                (0..sim.rng().between(0, 5))
                    .map(|_| sim.rng().below(100))
                    .collect()
            })
            .collect::<Vec<Vec<u32>>>();
        let scope = tenant_scope("acme", CancellationToken::new())?;
        let mut gate = StreamGate::new(cap, policy)?;
        let pending: RefCell<Vec<Option<Vec<u32>>>> = RefCell::new(
            std::iter::once(None)
                .chain(batches.iter().rev().cloned().map(Some))
                .collect(),
        );
        let seen: RefCell<Vec<u32>> = RefCell::new(Vec::new());
        let report: PumpReport = drive(pump(
            &scope,
            "prices",
            &mut gate,
            || {
                let batch = pending.borrow_mut().pop().flatten();
                async move { batch }
            },
            |_, event| {
                if let Ok(mut held) = seen.try_borrow_mut() {
                    held.push(event);
                }
                async move { Ok::<(), FlowError>(()) }
            },
        ))?;
        let seen = seen
            .try_borrow()
            .map_err(|error| format!("the seen order was poisoned: {error}"))?;
        // The independent model: the same gate, kept by hand. The pump drains
        // the gate after every batch, so each burst starts empty and the
        // policy decides within the burst.
        let mut kept: Vec<u32> = Vec::new();
        let mut dropped: u64 = 0;
        for batch in &batches {
            let mut burst: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
            for event in batch {
                if burst.len() < cap {
                    burst.push_back(*event);
                    continue;
                }
                match policy {
                    Overflow::DropOldest => {
                        burst.pop_front();
                        burst.push_back(*event);
                        dropped = dropped.saturating_add(1);
                    }
                    Overflow::DropNewest => {
                        dropped = dropped.saturating_add(1);
                    }
                    Overflow::Refuse => {
                        burst.push_back(*event);
                    }
                    _ => {
                        let refusal: TestResult =
                            Err(String::from("an overflow policy this sim does not model").into());
                        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "sweep: returning an error to the caller");
                        return refusal;
                    }
                }
            }
            kept.extend(burst);
        }
        assert_eq!(
            *seen, kept,
            "every retained event is handled once, in arrival order"
        );
        assert_eq!(
            report.handled,
            u64::try_from(seen.len()).map_err(|error| format!("the feed is too long: {error}"))?,
            "the report counts every handled event"
        );
        match policy {
            Overflow::DropOldest | Overflow::DropNewest => {
                assert_eq!(
                    report.dropped, dropped,
                    "the report counts every shed event"
                );
                assert_eq!(report.refused, 0, "a shedding policy refuses nothing");
            }
            Overflow::Refuse => {
                assert_eq!(report.dropped, 0, "backpressure sheds nothing");
            }
            _ => {
                let refusal: TestResult =
                    Err(String::from("an overflow policy this sim does not assert").into());
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "sweep: returning an error to the caller");
                return refusal;
            }
        }
        sim.record(&format!(
            "cap={cap} policy={policy:?} batches={batches:?} seen={seen:?}"
        ));
        Ok(())
    })
}

band_family::band_family! {
    justification_sequence_band_00 => justification_sequence_replays, 0;
    justification_sequence_band_01 => justification_sequence_replays, 1;
    justification_sequence_band_02 => justification_sequence_replays, 2;
    justification_sequence_band_03 => justification_sequence_replays, 3;
    justification_sequence_band_04 => justification_sequence_replays, 4;
    justification_sequence_band_05 => justification_sequence_replays, 5;
    justification_sequence_band_06 => justification_sequence_replays, 6;
    justification_sequence_band_07 => justification_sequence_replays, 7;
    graph_schedule_band_00 => graph_schedule_respects_needs_capacity_and_bound, 0;
    graph_schedule_band_01 => graph_schedule_respects_needs_capacity_and_bound, 1;
    graph_schedule_band_02 => graph_schedule_respects_needs_capacity_and_bound, 2;
    graph_schedule_band_03 => graph_schedule_respects_needs_capacity_and_bound, 3;
    graph_schedule_band_04 => graph_schedule_respects_needs_capacity_and_bound, 4;
    graph_schedule_band_05 => graph_schedule_respects_needs_capacity_and_bound, 5;
    graph_schedule_band_06 => graph_schedule_respects_needs_capacity_and_bound, 6;
    graph_schedule_band_07 => graph_schedule_respects_needs_capacity_and_bound, 7;
    compensation_reverse_band_00 => compensation_replays_in_reverse, 0;
    compensation_reverse_band_01 => compensation_replays_in_reverse, 1;
    compensation_reverse_band_02 => compensation_replays_in_reverse, 2;
    compensation_reverse_band_03 => compensation_replays_in_reverse, 3;
    compensation_reverse_band_04 => compensation_replays_in_reverse, 4;
    compensation_reverse_band_05 => compensation_replays_in_reverse, 5;
    compensation_reverse_band_06 => compensation_replays_in_reverse, 6;
    compensation_reverse_band_07 => compensation_replays_in_reverse, 7;
    stream_gate_band_00 => stream_gate_bounds_the_feed, 0;
    stream_gate_band_01 => stream_gate_bounds_the_feed, 1;
    stream_gate_band_02 => stream_gate_bounds_the_feed, 2;
    stream_gate_band_03 => stream_gate_bounds_the_feed, 3;
    stream_gate_band_04 => stream_gate_bounds_the_feed, 4;
    stream_gate_band_05 => stream_gate_bounds_the_feed, 5;
    stream_gate_band_06 => stream_gate_bounds_the_feed, 6;
    stream_gate_band_07 => stream_gate_bounds_the_feed, 7;
}
