//! Seeded simulations of wakes and deadlines (ADR 0006 R5).
//!
//! Source: the R5 falsifiers, "idle runs 0 ticks over a stated virtual span;
//! one wake polls exactly one source; same seed, same trace hash", and the
//! lost-wakeup class of scheduler bug (a wake that lands between the check and
//! the park is never seen). One seed draws the sources' cadences, the wake
//! schedule and the span; the world runs on a virtual clock driven by the same
//! loop shape the HTTP edge's driver runs, so no real time passes.

use std::cell::Cell;
use std::future::{Future, ready};
use std::rc::Rc;
use std::time::Duration;

use lgwks_std::seeded::Seeded;

use crate::cap::{Auth, Cap};
use crate::clock::Clock;
use crate::error::BotError;
use crate::gate::GrantSet;
use crate::verb::{Cadence, Observe};

use super::super::bot::EcsBot;
use super::super::tests::test_effects;
use super::{NextTick, WakeHandle};

/// Seeds each simulation runs.
const SEEDS: u64 = 256;

/// The stated virtual span an idle world is driven over: one day.
const DAY: Duration = Duration::from_secs(86_400);

type SimResult = Result<(), Box<dyn std::error::Error>>;

/// A source that counts its own polls and reads as that count.
struct Probe {
    /// When the world polls it.
    cadence: Cadence,
    /// Its polls so far, shared with the simulation.
    polls: Rc<Cell<u64>>,
}

impl Observe for Probe {
    type Output = u64;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn poll(&self, _call: (Auth, ())) -> impl Future<Output = Result<u64, BotError>> {
        self.polls.set(self.polls.get().saturating_add(1));
        ready(Ok(self.polls.get()))
    }

    fn cadence(&self) -> Cadence {
        self.cadence
    }

    fn domain_id(&self) -> &str {
        "sim::probe"
    }
}

/// Each source's poll count, shared with the simulation, in chain order.
type Counts = Vec<Rc<Cell<u64>>>;

/// A world over `cadences` on `clock`, and each source's poll count.
fn world(
    cadences: &[Cadence],
    clock: &Clock,
) -> Result<(EcsBot, Counts), Box<dyn std::error::Error>> {
    let counts: Counts = cadences.iter().map(|_| Rc::default()).collect();
    let mut probes = cadences.iter().zip(&counts).map(|(cadence, polls)| Probe {
        cadence: *cadence,
        polls: Rc::clone(polls),
    });
    let first = probes.next().ok_or("a world needs a source")?;
    let mut builder = EcsBot::builder("sim-wake")
        .with_clock(clock.clone())
        .with_effects(test_effects()?)
        .observe(first);
    for probe in probes {
        builder = builder.observe(probe);
    }
    Ok((builder.build(&GrantSet::empty())?, counts))
}

/// Each source's poll count, in chain order.
fn polls(counts: &[Rc<Cell<u64>>]) -> Vec<u64> {
    counts.iter().map(|count| count.get()).collect()
}

/// FNV-1a folded over `bytes` into `hash`.
fn fold(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Drive `bot` on `clock` until `end`, waking `wakes` (virtual instant, chain)
/// as their instants pass, the way the edge's driver does: tick when a tick is
/// due, otherwise advance to the nearer of the next deadline and the next
/// wake. Returns the ticks run and the trace hash of every tick's instant and
/// poll counts.
fn drive(
    bot: &mut EcsBot,
    clock: &Clock,
    counts: &[Rc<Cell<u64>>],
    wakes: &[(Duration, usize)],
    end: Duration,
) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    let (mut ticks, mut hash) = (0_u64, 0xcbf2_9ce4_8422_2325_u64);
    let mut next_wake = wakes.iter().peekable();
    loop {
        let now = clock.now();
        let deadline = match bot.next_tick() {
            NextTick::Now => {
                bot.tick()?;
                ticks = ticks.saturating_add(1);
                hash = fold(hash, &now.as_nanos().to_le_bytes());
                for count in polls(counts) {
                    hash = fold(hash, &count.to_le_bytes());
                }
                continue;
            }
            NextTick::In(wait) => Some(now.saturating_add(wait)),
            NextTick::Idle => None,
        };
        let wake_at = next_wake.peek().map(|&&(at, _)| at.max(now));
        let target = match (deadline, wake_at) {
            (Some(deadline), Some(wake)) => deadline.min(wake),
            (one, other) => match one.or(other) {
                Some(target) => target,
                None => return Ok((ticks, hash)),
            },
        };
        if target > end {
            return Ok((ticks, hash));
        }
        clock.advance(target.saturating_sub(now))?;
        while let Some(&&(at, chain)) = next_wake.peek()
            && at <= target
        {
            WakeHandle::for_source(bot, chain)
                .ok_or("a wake for a source not declared Wake")?
                .wake();
            next_wake.next();
        }
    }
}

#[test]
fn sim_an_idle_world_runs_no_tick_over_a_virtual_day() -> SimResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let woken = stream.below(8)?.saturating_add(1);
        let ticked = stream.below(4)?;
        let mut cadences = vec![Cadence::Wake; usize::try_from(woken)?];
        cadences.extend((0..ticked).map(|_| Cadence::Tick));
        let clock = Clock::virtual_at(Duration::ZERO);
        let (mut bot, counts) = world(&cadences, &clock)?;
        // The first tick reads every baseline; after it, nothing is due.
        let (ticks, _) = drive(&mut bot, &clock, &counts, &[], DAY)?;
        assert_eq!(ticks, 1, "seed {seed}: only the baseline tick ran");
        assert_eq!(bot.next_tick(), NextTick::Idle, "seed {seed}");
        assert!(
            polls(&counts).iter().all(|&count| count == 1),
            "seed {seed}"
        );
    }
    Ok(())
}

#[test]
fn sim_one_wake_polls_exactly_one_source() -> SimResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let sources = usize::try_from(stream.below(16)?.saturating_add(2))?;
        let clock = Clock::virtual_at(Duration::ZERO);
        let (mut bot, counts) = world(&vec![Cadence::Wake; sources], &clock)?;
        bot.tick()?;
        for _ in 0..32 {
            let chosen = stream.index(sources)?;
            let before = polls(&counts);
            let handle =
                WakeHandle::for_source(&bot, chosen).ok_or("a Wake source has a handle")?;
            // Woken one to three times before the tick: the wakes coalesce.
            for _ in 0..=stream.below(3)? {
                handle.wake();
            }
            assert_eq!(bot.next_tick(), NextTick::Now, "seed {seed}");
            bot.tick()?;
            for (chain, (after, before)) in polls(&counts).iter().zip(&before).enumerate() {
                let expected = before.saturating_add(u64::from(chain == chosen));
                assert_eq!(*after, expected, "seed {seed}: chain {chain}");
            }
            assert_eq!(bot.next_tick(), NextTick::Idle, "seed {seed}");
        }
    }
    Ok(())
}

#[test]
fn sim_every_source_ticks_at_its_deadlines_and_replays_by_seed() -> SimResult {
    for seed in 0..SEEDS {
        let mut stream = Seeded::from_seed(seed);
        let periods: Vec<Duration> = (0..stream.below(4)?.saturating_add(1))
            .map(|_| {
                stream
                    .below(600)
                    .map(|secs| Duration::from_secs(secs.saturating_add(1)))
            })
            .collect::<Result<_, _>>()?;
        let span = Duration::from_secs(stream.below(7_200)?.saturating_add(1));
        let mut cadences: Vec<Cadence> = periods.iter().copied().map(Cadence::Every).collect();
        let woken = cadences.len();
        cadences.push(Cadence::Wake);
        let mut wakes: Vec<(Duration, usize)> = (0..stream.below(16)?)
            .map(|_| {
                stream
                    .below(span.as_secs())
                    .map(|at| (Duration::from_secs(at), woken))
            })
            .collect::<Result<_, _>>()?;
        wakes.sort_unstable();
        let mut traces = Vec::new();
        for _replay in 0..2 {
            let clock = Clock::virtual_at(Duration::ZERO);
            let (mut bot, counts) = world(&cadences, &clock)?;
            let (_, hash) = drive(&mut bot, &clock, &counts, &wakes, span)?;
            // An `Every(p)` source is polled at 0, p, 2p, ... up to the span.
            for (chain, period) in periods.iter().enumerate() {
                let expected = span
                    .as_secs()
                    .div_euclid(period.as_secs())
                    .saturating_add(1);
                let polled = polls(&counts).get(chain).copied();
                assert_eq!(polled, Some(expected), "seed {seed}: chain {chain}");
            }
            traces.push(hash);
        }
        assert_eq!(
            traces.first(),
            traces.last(),
            "seed {seed}: the trace replays"
        );
    }
    Ok(())
}

#[test]
fn a_zero_every_period_is_refused_at_build() -> SimResult {
    let clock = Clock::virtual_at(Duration::ZERO);
    let refused = world(&[Cadence::Every(Duration::ZERO)], &clock);
    assert!(
        refused.is_err(),
        "Every(0) would make every instant a deadline"
    );
    Ok(())
}
