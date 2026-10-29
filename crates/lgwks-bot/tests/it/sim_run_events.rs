//! Simulation family: runs into the world under seeded deaths (ADR 0006 R3).
//!
//! One seed draws how many hosts share one run feed, how many runs each makes,
//! which of them fail, where the world ticks and where the process "dies".
//! A death drops every host, the feed and the world, and the next life reopens
//! the feed over the same journal, the way a restarted process reopens its file.
//! The journal is the real in-memory adapter behind a shared handle, so the
//! restart reads exactly the rungs the last life acknowledged, and nothing pays
//! an fsync per rung.
//!
//! The model is a count per fact. Across every life: the feed's admitted and
//! settled counts equal the runs started and ended, the bound system fires each
//! settled run exactly once whatever the deaths, and no run fires twice (the
//! exactly-once half of the ADR row; the kill row in `run_events` is the real
//! `SIGKILL`).
//!
//! | Family | Property it pins |
//! |---|---|
//! | `run_band_00..03` | 512 seeds, each band swept twice for one trace hash |
//! | `a_burst_past_one_tick_fires_over_successive_ticks` | a tick fires at most 1,024 runs; the rest fire on the next |
//! | `a_tick_with_nothing_settled_writes_no_rung` | an idle world appends nothing to the journal |
//! | `two_feeds_fire_only_their_own_runs` | two tenants' worlds never fire each other's runs |
//! | `outcomes_fire_in_settlement_order` | the world reads each run as it ended, in the order it settled |
//! | `run_ids_never_repeat_across_reopens` | a reopen after every run never reissues a run id |
//! | `distinct_seeds_draw_distinct_traces` | the seed reaches the schedule, so a replay hash is not a constant |

use crate::run_world::{Fired, bound_world, fallible, fire};
use crate::sim;

use std::collections::BTreeSet;
use std::error::Error;
use std::sync::{Arc, Mutex, MutexGuard};

use lgwks_bot::journal::{
    DurabilityPromise, DurableAck, EffectEvent, EffectJournal, JournalError, JournalPosition,
    MemoryJournal,
};
use lgwks_bot::task::{Disposition, Host, task};
use lgwks_bot::{Bot, RunFeed, RunOutcome, SettledRun};

use sim::Band;

type TestResult = Result<(), Box<dyn Error>>;

/// The seed space this family sweeps, and how many bands it is cut into.
const SEEDS: u64 = 512;
const PARTS: u64 = 4;

/// One journal every life of a seed shares: the durable store a restart reopens.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<MemoryJournal>>);

impl Shared {
    fn lock(&self) -> MutexGuard<'_, MemoryJournal> {
        match self.0.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl EffectJournal for Shared {
    fn durability(&self) -> DurabilityPromise {
        self.lock().durability()
    }

    fn tail(&self) -> JournalPosition {
        self.lock().tail()
    }

    fn committed(&self) -> Result<Vec<EffectEvent>, JournalError> {
        EffectJournal::committed(&*self.lock())
    }

    fn compare_and_append(
        &mut self,
        expected_tail: JournalPosition,
        event: &EffectEvent,
    ) -> Result<DurableAck, JournalError> {
        self.lock().compare_and_append(expected_tail, event)
    }

    fn reserve_handoff_capacity(&self, rungs: u64) -> Result<(), JournalError> {
        self.lock().reserve_handoff_capacity(rungs)
    }
}

/// One life of a seed: the feed, the hosts that write it, and the world that
/// binds it.
struct Life {
    feed: RunFeed,
    hosts: Vec<Host>,
    bot: Bot,
    fired: Fired,
}

impl Life {
    fn open(journal: &Shared, tenants: u32) -> Result<Self, Box<dyn Error>> {
        let feed = RunFeed::open(Box::new(journal.clone()))?;
        let mut hosts = Vec::new();
        for tenant in 0..tenants {
            hosts.push(
                Host::builder(&format!("tenant-{tenant}"))?
                    .runs(feed.clone())
                    .build()?,
            );
        }
        let (bot, fired) = bound_world(&feed)?;
        Ok(Self {
            feed,
            hosts,
            bot,
            fired,
        })
    }
}

/// One seed: runs, ticks and deaths in a seeded order, then a last tick that
/// drains whatever is due.
fn run_case(sim: &mut sim::Sim) -> TestResult {
    let journal = Shared::default();
    let tenants = sim.rng().between(1, 5);
    let steps = sim.rng().between(1, 40);
    let probe = task("probe", fallible)?;
    let mut life = Life::open(&journal, tenants)?;
    let mut started: u64 = 0;
    let mut failed: u64 = 0;
    let mut fired: Vec<SettledRun> = Vec::new();
    let mut deaths: u64 = 0;
    for _ in 0..steps {
        match sim.rng().below(10) {
            0..=5 => {
                let pick = usize::try_from(sim.rng().below(tenants))?;
                let fail = sim.rng().chance(250);
                let host = life.hosts.get(pick).ok_or("the drawn host exists")?;
                let report = lgwks_bot::block_on(host.run(&probe, fail));
                let expected = if fail {
                    Disposition::Failed
                } else {
                    Disposition::Succeeded
                };
                assert_eq!(report.disposition(), expected, "a run ends as seeded");
                started = started.saturating_add(1);
                failed = failed.saturating_add(u64::from(fail));
            }
            6..=7 => {
                life.bot.tick()?;
            }
            _ => {
                fired.extend(life.fired.take());
                drop(life);
                deaths = deaths.saturating_add(1);
                life = Life::open(&journal, tenants)?;
            }
        }
    }
    life.bot.tick()?;
    fired.extend(life.fired.take());
    let counts = life.feed.counts();
    assert_eq!(counts.admitted, started, "every run started was admitted");
    assert_eq!(counts.settled, started, "every run ended was settled");
    assert_eq!(counts.due, 0, "the last tick drained the world");
    assert_eq!(
        u64::try_from(fired.len())?,
        started,
        "every settled run fired, across every death"
    );
    let distinct: BTreeSet<_> = fired.iter().map(SettledRun::run).collect();
    assert_eq!(distinct.len(), fired.len(), "no run fired twice");
    let fired_failed = fired
        .iter()
        .filter(|run| run.outcome() == RunOutcome::Failed)
        .count();
    assert_eq!(
        u64::try_from(fired_failed)?,
        failed,
        "each run fired as it ended"
    );
    sim.trace.record_number("tenants", tenants);
    sim.trace.record_number("runs", started);
    sim.trace.record_number("failed", failed);
    sim.trace.record_number("deaths", deaths);
    Ok(())
}

/// The one band of seeds a declared test sweeps.
fn band(index: usize) -> Band {
    sim::bands(SEEDS, PARTS).swap_remove(index)
}

macro_rules! run_family {
    ($($name:ident => $index:expr);+ $(;)?) => {
        $(
            /// A seeded band of runs, ticks and deaths, swept twice for one trace hash.
            #[test]
            fn $name() -> TestResult {
                sim::assert_replays(band($index), run_case)
            }
        )+
    };
}

run_family!(run_band_00 => 0; run_band_01 => 1; run_band_02 => 2; run_band_03 => 3);

/// A narrow band for the properties whose single seed already does real work.
fn narrow() -> Band {
    sim::bands(16, 1).swap_remove(0)
}

/// `count` succeeding runs on one host bound to `feed`.
fn settle_runs(feed: &RunFeed, count: u64) -> TestResult {
    let host = Host::builder("burst")?.runs(feed.clone()).build()?;
    let probe = task("probe", fallible)?;
    for _ in 0..count {
        let report = lgwks_bot::block_on(host.run(&probe, false));
        assert_eq!(report.disposition(), Disposition::Succeeded, "a run ends");
    }
    Ok(())
}

#[test]
fn a_burst_past_one_tick_fires_over_successive_ticks() -> TestResult {
    sim::assert_replays(narrow(), |sim| {
        // One tick fires at most 1,024; the burst is always past it.
        let runs = u64::from(sim.rng().between(1_025, 1_600));
        let feed = RunFeed::open(Box::new(MemoryJournal::new()))?;
        settle_runs(&feed, runs)?;
        let (mut bot, fired) = bound_world(&feed)?;
        bot.tick()?;
        let first = u64::try_from(fired.take().len())?;
        bot.tick()?;
        let second = u64::try_from(fired.take().len())?;
        assert_eq!(first, 1_024, "the first tick fires one bounded batch");
        assert_eq!(
            first.saturating_add(second),
            runs,
            "the next tick fires the rest"
        );
        assert_eq!(feed.counts().due, 0, "and nothing is left due");
        sim.trace.record_number("runs", runs);
        sim.trace.record_number("second", second);
        Ok(())
    })
}

#[test]
fn a_tick_with_nothing_settled_writes_no_rung() -> TestResult {
    sim::assert_replays(narrow(), |sim| {
        let journal = Shared::default();
        let settled = u64::from(sim.rng().below(4));
        let feed = RunFeed::open(Box::new(journal.clone()))?;
        settle_runs(&feed, settled)?;
        let (mut bot, _fired) = bound_world(&feed)?;
        bot.tick()?;
        let held = EffectJournal::committed(&journal)?.len();
        let idle = sim.rng().between(1, 20);
        for _ in 0..idle {
            bot.tick()?;
        }
        assert_eq!(
            EffectJournal::committed(&journal)?.len(),
            held,
            "an idle tick appends nothing"
        );
        sim.trace.record_number("settled", settled);
        sim.trace.record_number("idle", idle);
        Ok(())
    })
}

#[test]
fn two_feeds_fire_only_their_own_runs() -> TestResult {
    sim::assert_replays(narrow(), |sim| {
        let left = RunFeed::open(Box::new(Shared::default()))?;
        let right = RunFeed::open(Box::new(Shared::default()))?;
        let on_left = u64::from(sim.rng().between(1, 30));
        let on_right = u64::from(sim.rng().between(1, 30));
        settle_runs(&left, on_left)?;
        settle_runs(&right, on_right)?;
        let fired_left = u64::try_from(fire(&left)?.len())?;
        assert_eq!(fired_left, on_left, "the left world fires the left runs");
        assert_eq!(
            right.counts().due,
            usize::try_from(on_right)?,
            "and leaves every right run due"
        );
        let fired_right = u64::try_from(fire(&right)?.len())?;
        assert_eq!(fired_right, on_right, "the right world fires its own");
        assert_eq!(
            left.counts().handled,
            on_left,
            "the left handled count never moves"
        );
        sim.trace.record_number("left", on_left);
        sim.trace.record_number("right", on_right);
        Ok(())
    })
}

#[test]
fn outcomes_fire_in_settlement_order() -> TestResult {
    sim::assert_replays(narrow(), |sim| {
        let feed = RunFeed::open(Box::new(MemoryJournal::new()))?;
        let host = Host::builder("order")?.runs(feed.clone()).build()?;
        let probe = task("probe", fallible)?;
        let mut expected = Vec::new();
        for _ in 0..sim.rng().between(1, 40) {
            let fail = sim.rng().chance(500);
            lgwks_bot::block_on(host.run(&probe, fail));
            expected.push(if fail {
                RunOutcome::Failed
            } else {
                RunOutcome::Succeeded
            });
        }
        let outcomes: Vec<_> = fire(&feed)?.iter().map(SettledRun::outcome).collect();
        assert_eq!(outcomes, expected, "each run fires as it ended, in order");
        let failed = expected
            .iter()
            .filter(|outcome| **outcome == RunOutcome::Failed)
            .count();
        sim.trace
            .record_number("runs", u64::try_from(expected.len())?);
        sim.trace.record_number("failed", u64::try_from(failed)?);
        Ok(())
    })
}

#[test]
fn run_ids_never_repeat_across_reopens() -> TestResult {
    sim::assert_replays(narrow(), |sim| {
        let journal = Shared::default();
        let lives = u64::from(sim.rng().between(1, 25));
        let mut seen = BTreeSet::new();
        for _ in 0..lives {
            let feed = RunFeed::open(Box::new(journal.clone()))?;
            settle_runs(&feed, 1)?;
            for run in fire(&feed)? {
                assert!(seen.insert(run.run()), "a reopen reissued a run id");
            }
        }
        assert_eq!(u64::try_from(seen.len())?, lives, "one fresh run per life");
        sim.trace.record_number("lives", lives);
        Ok(())
    })
}

#[test]
fn distinct_seeds_draw_distinct_traces() -> TestResult {
    let hashes = sim::sweep(narrow(), run_case)?;
    let distinct: BTreeSet<_> = hashes.iter().collect();
    assert!(
        distinct.len() > 1,
        "sixteen seeds drew one trace, so the seed never reached the schedule"
    );
    Ok(())
}
