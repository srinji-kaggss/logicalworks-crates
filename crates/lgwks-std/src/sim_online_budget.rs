//! Deterministic simulation of the reachability probe's shared budget.
//!
//! One seed draws a candidate list (empty, short, or past the 64-address cap),
//! a total budget, and for every candidate how the dial goes: it answers after
//! part of its share, refuses after part of it, or is blackholed and waits out
//! all of it. A simulated clock advances by exactly what each dial spent, and
//! every dial is checked against a reference model of the budget written here
//! from the documentation: each candidate is offered an equal share of what
//! remains, the total never exceeds the budget, the first answer ends the
//! probe, and no more than 64 candidates are dialed.
//!
//! Source: RFC 8305 (Happy Eyeballs v2), whose point is that one unresponsive
//! address must not consume the whole connection attempt; the documented
//! defect here was a filtered IPv6 route spending the entire budget on the
//! first candidate. Same seed, same dial log; distinct seeds diverge.

use std::cell::Cell;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use lgwks_std::seeded::{DrawError, Seeded};

use super::{MAX_PROBE_ADDRESSES, probe_candidates_with, probe_resolved};

/// The seeds every family replays.
const SEEDS: [u64; 4] = [
    0x0711_0000_0000_0001,
    0x0711_0000_0000_0002,
    0x0711_FFFF_0000_0003,
    0xB0D6_0711_0000_0004,
];

/// How many probes each family draws per seed.
const PROBES: usize = 96;

/// A family's result: a draw the stream refused is reported, never replaced.
type Drawn<T> = Result<T, DrawError>;

/// How one simulated dial goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dial {
    /// The candidate answers after spending `per_mille` of its share.
    Answers(u32),
    /// The candidate refuses after spending `per_mille` of its share.
    Refuses(u32),
    /// The candidate never answers and waits out its whole share.
    Blackholed,
}

/// One drawn probe: its budget and every candidate's dial.
#[derive(Debug, Clone)]
struct Scenario {
    /// The total budget.
    timeout: Duration,
    /// Each candidate's dial, in order.
    dials: Vec<Dial>,
}

/// A per-mille fraction from `state`.
fn per_mille(state: &mut Seeded) -> Drawn<u32> {
    let drawn = state.below(1_001)?.to_le_bytes();
    Ok(u32::from_le_bytes([drawn[0], drawn[1], drawn[2], drawn[3]]))
}

/// One dial outcome from `state`, weighted by `answer_odds` in 1000.
fn draw_dial(state: &mut Seeded, answer_odds: u64) -> Drawn<Dial> {
    let roll = state.below(1_000)?;
    Ok(if roll < answer_odds {
        Dial::Answers(per_mille(state)?)
    } else if roll < answer_odds.saturating_add(400) {
        Dial::Refuses(per_mille(state)?)
    } else {
        Dial::Blackholed
    })
}

/// A drawn scenario: up to 80 candidates, a budget up to half a second.
fn draw(state: &mut Seeded, answer_odds: u64) -> Drawn<Scenario> {
    let count = state.below(81)?;
    let timeout = Duration::from_micros(state.below(500_001)?);
    let dials = (0..count)
        .map(|_| draw_dial(state, answer_odds))
        .collect::<Drawn<Vec<Dial>>>()?;
    Ok(Scenario { timeout, dials })
}

/// The documentation-range address (RFC 5737) for candidate `index`, so no
/// simulated dial could ever name a routable host. The index is the last
/// octet plus 256 times the port's low byte, which [`index_of`] reads back.
fn address(index: usize) -> SocketAddr {
    let bytes = index.to_le_bytes();
    SocketAddr::V4(SocketAddrV4::new(
        Ipv4Addr::new(192, 0, 2, bytes[0]),
        u16::from_le_bytes([bytes[1], 1]),
    ))
}

/// The candidate index [`address`] encoded in `candidate`.
fn index_of(candidate: &SocketAddr) -> Option<usize> {
    match *candidate {
        SocketAddr::V4(ref v4) => {
            let [low, _] = v4.port().to_le_bytes();
            let octet = v4.ip().octets()[3];
            usize::from(low)
                .checked_mul(256)
                .and_then(|high| high.checked_add(usize::from(octet)))
        }
        SocketAddr::V6(_) => None,
    }
}

/// What a dial spends of `share`.
fn spent(dial: Dial, share: Duration) -> Duration {
    let fraction = |part: u32| match share.checked_mul(part) {
        Some(scaled) => match scaled.checked_div(1_000) {
            Some(value) => value,
            None => share,
        },
        None => share,
    };
    match dial {
        Dial::Answers(part) | Dial::Refuses(part) => fraction(part),
        Dial::Blackholed => share,
    }
}

/// The outcome of one probe: what it returned, each dial's index and offered
/// share, and the clock when it returned.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    /// What the probe returned.
    reachable: bool,
    /// Each dial: the candidate's index and the share it was offered.
    log: Vec<(Option<usize>, Duration)>,
    /// The simulated clock when the probe returned.
    elapsed: Duration,
}

/// Runs the shipped probe loop over `scenario` with a simulated clock.
fn run(scenario: &Scenario) -> Outcome {
    let clock = Cell::new(Duration::ZERO);
    let mut log = Vec::new();
    let candidates = (0..scenario.dials.len()).map(address);
    let reachable = probe_candidates_with(
        candidates,
        scenario.timeout,
        || clock.get(),
        |candidate, share| {
            let index = index_of(candidate);
            log.push((index, share));
            let dial = match index.and_then(|at| scenario.dials.get(at)) {
                Some(dial) => *dial,
                None => Dial::Blackholed,
            };
            clock.set(clock.get().saturating_add(spent(dial, share)));
            matches!(dial, Dial::Answers(_))
        },
    );
    Outcome {
        reachable,
        log,
        elapsed: clock.get(),
    }
}

/// The reference model: the documented budget rule, computed independently.
fn reference(scenario: &Scenario) -> Outcome {
    let capped: Vec<Dial> = scenario.dials.iter().copied().take(64).collect();
    let mut elapsed = Duration::ZERO;
    let mut log = Vec::new();
    for (index, dial) in capped.iter().copied().enumerate() {
        let remaining = scenario.timeout.saturating_sub(elapsed);
        if remaining.is_zero() {
            break;
        }
        let share = match u32::try_from(capped.len().saturating_sub(index)) {
            Ok(left) => match remaining.checked_div(left) {
                Some(share) => share,
                None => remaining,
            },
            Err(_) => remaining,
        };
        log.push((Some(index), share));
        elapsed = elapsed.saturating_add(spent(dial, share));
        if matches!(dial, Dial::Answers(_)) {
            return Outcome {
                reachable: true,
                log,
                elapsed,
            };
        }
    }
    Outcome {
        reachable: false,
        log,
        elapsed,
    }
}

/// Every drawn probe under `answer_odds`, with its seed.
fn sweep(answer_odds: u64, mut check: impl FnMut(u64, &Scenario, &Outcome)) -> Drawn<()> {
    for seed in SEEDS {
        let mut state = Seeded::from_seed(seed);
        for _ in 0..PROBES {
            let scenario = draw(&mut state, answer_odds)?;
            check(seed, &scenario, &run(&scenario));
        }
    }
    Ok(())
}

/// Every drawn probe with each dial rewritten to `dial`, with its seed.
fn sweep_all(dial: Dial, mut check: impl FnMut(u64, &Scenario, &Outcome)) -> Drawn<()> {
    sweep(0, |seed, drawn, _| {
        let scenario = Scenario {
            timeout: drawn.timeout,
            dials: vec![dial; drawn.dials.len()],
        };
        check(seed, &scenario, &run(&scenario));
    })
}

/// Whether the dial at `index` in `scenario` answers.
fn answers(scenario: &Scenario, index: Option<usize>) -> bool {
    matches!(
        index.and_then(|at| scenario.dials.get(at)),
        Some(Dial::Answers(_))
    )
}

#[test]
/// Every dial, its offered share, the answer and the final clock are exactly
/// what the reference model of the shared budget predicts.
fn sim_every_dial_matches_the_reference_budget() -> Drawn<()> {
    sweep(150, |seed, scenario, outcome| {
        assert_eq!(
            *outcome,
            reference(scenario),
            "seed {seed:#x}: the probe must follow the documented budget rule"
        );
    })
}

#[test]
/// However the dials go, the probe never spends more than its budget.
fn sim_a_probe_never_outlasts_its_budget() -> Drawn<()> {
    sweep(100, |seed, scenario, outcome| {
        assert!(
            outcome.elapsed <= scenario.timeout,
            "seed {seed:#x}: spent {:?} of a {:?} budget",
            outcome.elapsed,
            scenario.timeout
        );
    })
}

#[test]
/// At most 64 candidates are dialed, in order, and none past the cap.
fn sim_no_more_than_the_capped_candidates_are_dialed_in_order() -> Drawn<()> {
    sweep(0, |seed, _, outcome| {
        assert!(
            outcome.log.len() <= MAX_PROBE_ADDRESSES,
            "seed {seed:#x}: dialed {} candidates",
            outcome.log.len()
        );
        for (position, &(index, _)) in outcome.log.iter().enumerate() {
            assert_eq!(
                index,
                Some(position),
                "seed {seed:#x}: candidates are dialed in order, none skipped"
            );
        }
    })
}

#[test]
/// The first answer ends the probe and reports reachable; with no answer the
/// probe reports unreachable.
fn sim_the_first_answer_ends_the_probe() -> Drawn<()> {
    sweep(300, |seed, scenario, outcome| {
        let answered = outcome
            .log
            .last()
            .is_some_and(|&(index, _)| answers(scenario, index));
        assert_eq!(
            outcome.reachable, answered,
            "seed {seed:#x}: reachable exactly when the last dial answered"
        );
        let earlier_answer = outcome
            .log
            .iter()
            .rev()
            .skip(1)
            .any(|&(index, _)| answers(scenario, index));
        assert!(
            !earlier_answer,
            "seed {seed:#x}: a dial after an answer was made"
        );
    })
}

#[test]
/// A zero budget dials nothing and reports unreachable.
fn sim_a_zero_budget_dials_nothing() -> Drawn<()> {
    sweep(500, |seed, drawn, _| {
        let scenario = Scenario {
            timeout: Duration::ZERO,
            dials: drawn.dials.clone(),
        };
        let outcome = run(&scenario);
        assert!(
            outcome.log.is_empty() && !outcome.reachable,
            "seed {seed:#x}: a zero budget dialed {:?}",
            outcome.log
        );
    })
}

#[test]
/// Candidates that refuse at once hand their unused share on: the offered
/// shares never shrink, and the last is the whole remaining budget.
fn sim_fast_refusals_hand_their_share_forward() -> Drawn<()> {
    sweep_all(Dial::Refuses(0), |seed, scenario, outcome| {
        for pair in outcome.log.windows(2) {
            if let [(_, before), (_, after)] = *pair {
                assert!(
                    after >= before,
                    "seed {seed:#x}: a later share {after:?} is below {before:?}"
                );
            }
        }
        if let Some(&(_, last)) = outcome.log.last() {
            assert_eq!(
                last, scenario.timeout,
                "seed {seed:#x}: the last candidate is offered all that remains"
            );
        }
    })
}

#[test]
/// When every candidate is blackholed, the candidates spend the budget
/// between them exactly — one budget, not one each.
fn sim_blackholed_candidates_spend_exactly_one_budget() -> Drawn<()> {
    sweep_all(Dial::Blackholed, |seed, scenario, outcome| {
        if scenario.dials.is_empty() || scenario.timeout.is_zero() {
            return;
        }
        assert_eq!(
            outcome.elapsed,
            scenario.timeout,
            "seed {seed:#x}: {} blackholed candidates spent one budget",
            scenario.dials.len()
        );
        assert!(!outcome.reachable, "seed {seed:#x}: nobody answered");
    })
}

#[test]
/// An empty address list is unreachable without any dial.
fn sim_an_empty_address_list_is_unreachable() -> Drawn<()> {
    for seed in SEEDS {
        let mut state = Seeded::from_seed(seed);
        let timeout = Duration::from_micros(state.below(500_001)?);
        assert!(
            !probe_resolved(&[], timeout),
            "seed {seed:#x}: no address is reachable"
        );
    }
    Ok(())
}

/// The replay trace of every drawn probe's dial log.
fn budget_trace(seed: u64) -> Drawn<u64> {
    let mut state = Seeded::from_seed(seed);
    let mut trace = 0xcbf2_9ce4_8422_2325_u64;
    let mut fold = |value: u64| trace = (trace ^ value).wrapping_mul(0x0000_0100_0000_01b3);
    for _ in 0..PROBES {
        let outcome = run(&draw(&mut state, 150)?);
        fold(u64::from(outcome.reachable));
        fold(outcome.elapsed.as_secs());
        fold(u64::from(outcome.elapsed.subsec_nanos()));
        for (_, share) in outcome.log {
            fold(u64::from(share.subsec_nanos()));
        }
    }
    Ok(trace)
}

#[test]
/// The replay oracle: one seed, one trace.
fn sim_the_same_seed_replays_to_the_same_budget_trace() -> Drawn<()> {
    for seed in SEEDS {
        assert_eq!(
            budget_trace(seed)?,
            budget_trace(seed)?,
            "seed {seed:#x}: the same seed must replay"
        );
    }
    Ok(())
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn sim_distinct_budget_seeds_diverge() -> Drawn<()> {
    assert_ne!(
        budget_trace(SEEDS[0])?,
        budget_trace(SEEDS[1])?,
        "two seeds drew the same probes"
    );
    Ok(())
}
