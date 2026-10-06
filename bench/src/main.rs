//! lgwks_bot against baselines and competitor engines.
//!
//! # What this measures, and what it refuses to
//!
//! Every timing this rig reports is gated on a **fairness check**: the bot and
//! the baseline must produce the identical sequence of effect counts *and*
//! perform the identical number of condition evaluations, tick for tick, over
//! the whole run. If either differs, the two engines are not doing the same
//! work, the ratio is meaningless, and the rig fails loudly rather than
//! printing a favourable number.
//!
//! The evaluation counter is not belt-and-braces. An earlier revision of this
//! file gated on effects alone, and the fan-out scenario reported a 2937x ratio
//! because the baseline returned `entries` as an integer instead of looping
//! over them: the same effect count, none of the work. Gating on work as well
//! as on results is what makes the comparison mean something.
//!
//! # The inputs are deterministic by construction
//!
//! Each source's value is `tick / period`, a pure function of the tick index.
//! There is no RNG in the workload, so the expected effect count is not merely
//! reproducible, it is *computable in closed form* and asserted. A benchmark
//! whose workload is random can only be checked for self-consistency; this one
//! can be checked against arithmetic.
//!
//! # What is deliberately excluded
//!
//! Raw-`bevy_ecs` and CEL/zen-engine comparators are not here. A raw-bevy
//! comparator would be *this rig's* reimplementation of the bot's schedule, so
//! it would measure the author's fluency with bevy rather than the bot's cost.
//! The honest comparators are the null hypothesis (a hand-rolled loop doing
//! exactly the same work, with the work counted) and the cost decomposition
//! (`poll-only-*` scenarios), and both are present. See `README.md`.

mod stats;

use std::cell::Cell;
use std::fmt::Write as _;
use std::rc::Rc;
use std::time::{Duration, Instant};

use lgwks_bot::broker::Broker;
use lgwks_bot::effect::{EnvironmentId, FlowRevision, RunId};
use lgwks_bot::journal::{MAX_JOURNAL_EVENTS, MemoryJournal};
use lgwks_bot::spec::{EffectIdentity, EffectScope};
use lgwks_bot::{
    Auth, Bot, BotError, Cap, EffectLifetime, Evaluate, Execute, GrantSet, Observe, TickProfile,
    TickStage,
};
use lgwks_std::trace::info;

// ── The allocation counter ───────────────────────────────────────────────────
//
// Timing alone says a tick is slow; it does not say what the tick is spending
// its time on, and "reduce allocations" is then a guess. This counts them, so
// the hot path's allocation model is a measured number rather than an inference
// from reading `Box::new` in a source file.
//
// Two counters, because they answer different questions: `ALLOCS` is the count
// the tick pays, and `BYTES` says whether those allocations are small handovers
// or large buffers. A tick that is allocation-bound shows a count in the
// hundreds per tick with a small mean size.
//
// Counting is a relaxed atomic add and a subtraction on the free path. That is
// not free, and it inflates the measured wall time of the timed loops below.
// The two are therefore run separately: `--alloc-report` counts, the timing
// scenarios do not, and no ratio in `results.json` is taken from a counting run.

/// The allocation counter, shared with `bench/async` by path inclusion.
///
/// It lives in its own file because a second copy of an instrument that
/// produces published numbers is a second instrument, and a reader comparing the
/// two rigs would be comparing two different measurements under one name.
#[path = "alloc_count.rs"]
mod alloc_count;

// ── The workload ─────────────────────────────────────────────────────────────

/// The tick counter every source derives its value from.
///
/// Shared rather than owned so all sources advance together: the value a source
/// reports is `tick / period`, which makes the whole input sequence a pure
/// function of the tick index.
#[derive(Clone)]
struct Clock(Rc<Cell<u64>>);

impl Clock {
    fn new() -> Self {
        Self(Rc::new(Cell::new(0)))
    }

    fn set(&self, tick: u64) {
        self.0.set(tick);
    }
}

/// A source whose value steps once every `period` ticks.
///
/// `period == 1` changes every tick (maximum churn); `period == 100` changes one
/// tick in a hundred (steady state). The change *rate* is therefore a knob on
/// the scenario rather than a property of a random seed.
struct Source {
    clock: Clock,
    period: u64,
}

impl Observe for Source {
    type Output = u64;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, _call: (Auth, ())) -> Result<u64, BotError> {
        let period = if self.period == 0 { 1 } else { self.period };
        Ok(self.clock.0.get() / period)
    }

    /// The value, as a revision.
    ///
    /// This is the change-tick path: the substrate compares this against the
    /// revision it last committed for the chain and **skips the poll entirely**
    /// when they match, so an unchanged tick costs one integer comparison per
    /// source and allocates nothing. A real source would report something its
    /// world maintains for it — an ETag, an `mtime`, a subscription sequence — and
    /// this one reports the counter it already holds, which is the same contract
    /// with a cheaper witness.
    ///
    /// Reporting the value is honest here and is not the same as the digest the
    /// rig used to implement: the deprecated `fingerprint` was read *beside* the
    /// poll and could describe a value the poll never returned, whereas this is
    /// read before the poll it may skip, so it can only ever describe the value
    /// the substrate commits (`docs/bot-on-ecs.md` §4).
    fn revision(&self) -> Option<u64> {
        let period = if self.period == 0 { 1 } else { self.period };
        Some(self.clock.0.get() / period)
    }

    fn domain_id(&self) -> &'static str {
        "bench::source"
    }
}

/// The effect: record that it happened.
///
/// Deliberately trivial. The question this rig asks is what the *schedule* costs,
/// so the effect must not dominate it. A heavier action would measure the
/// action, and every engine would converge to that cost.
struct Tally(Rc<Cell<u64>>);

impl Execute for Tally {
    type Input = u64;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    /// Local, and stated here rather than left to the default.
    ///
    /// The default is [`EffectLifetime::External`], which the crate answers with a
    /// durability admission: an external handoff must not leave on a record that
    /// cannot outlive the process, so it needs a journal that survives the writer.
    /// This rig's action increments a counter in its own address space, so
    /// declaring `External` would be a false claim about the work being measured —
    /// and it would put a durable append inside every timed window, which is a
    /// different measurement from the one this rig publishes (see the journal
    /// section of this directory's README).
    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u64)) -> Result<(), BotError> {
        let (auth, _value) = call;
        auth.check(self.required_caps())?;
        self.0.set(self.0.get().saturating_add(1));
        Ok(())
    }

    fn domain_id(&self) -> &'static str {
        "bench::tally"
    }
}

/// The effect scope every bot in this rig dispatches under.
///
/// A fixed identity rather than a minted one, so two runs of the same scenario
/// derive the same action identities and read back the same facts: the rig
/// measures a repeated workload, and an identity that moved under it would make
/// two runs incomparable. Nothing here dispatches anything durable — the action
/// is [`EffectLifetime::Local`] — so what this scope buys is the effect path
/// itself running: the ledger write, the warrant, and the broker's revalidation.
fn effects() -> Result<EffectScope, Box<dyn std::error::Error>> {
    let environment = EnvironmentId::from_hex(&hex_of(&[0x21; 16]))?;
    let mut broker = Broker::new();
    broker.register(environment)?;
    // A fixed 32-byte revision digest, not a computed one: the flow revision is
    // folded into every action identity, and a revision that moved between runs
    // would make two runs of one workload incomparable.
    let flow = FlowRevision::from_tagged("blake3_256", &hex_of(&[0x5a; 32]))?;
    let identity = EffectIdentity::new(RunId::from_hex(&hex_of(&[0x01; 16]))?, environment, flow);
    Ok(EffectScope::new(
        identity,
        broker,
        Box::new(MemoryJournal::new()),
    ))
}

/// Bytes as lower-case hex, the spelling every identifier in this crate parses.
///
/// A helper rather than three pasted literals, because a run id and an
/// environment that disagree is a bot the broker refuses at its first dispatch,
/// and a rig that reported a workload it never ran would be worse than one that
/// failed to build.
fn hex_of(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The condition: fire on an even observed value.
///
/// Non-constant on purpose: a condition that always returned `true` would make
/// "fired" identical to "changed" and would not exercise the `Evaluate` verb.
///
/// It also counts its own evaluations. That counter is the fairness gate's
/// second axis, and it is what catches a baseline that reaches the right answer
/// by a cheaper route.
struct Even(Rc<Cell<u64>>);

impl Evaluate<u64> for Even {
    fn check(&self, value: &u64) -> Result<bool, BotError> {
        self.0.set(self.0.get().saturating_add(1));
        Ok(value.is_multiple_of(2))
    }

    fn condition_id(&self) -> &'static str {
        "bench::even"
    }
}

// ── The baseline ─────────────────────────────────────────────────────────────

/// One chain of the hand-rolled equivalent: poll, detect change, evaluate, act.
///
/// This is the null hypothesis. It holds no ECS world, no ledger, no `Auth`
/// proof, and no capability set -- it is what the work costs when nothing is
/// being guaranteed. Whatever the bot costs above this is the price of the
/// guarantees, and that price is the interesting number.
struct HandRolled {
    period: u64,
    entries: usize,
    prior: Option<u64>,
}

impl HandRolled {
    fn new(period: u64, entries: usize) -> Self {
        Self {
            period,
            entries,
            prior: None,
        }
    }

    /// One tick, mirroring the bot's semantics exactly: walk the entries only
    /// when the polled value moved, and evaluate the condition once per entry.
    ///
    /// The loop over `entries` is load-bearing, not decoration. Returning
    /// `self.entries` as a count is arithmetically identical and does none of
    /// the work; `evals` counts the evaluations so a baseline that cuts that
    /// corner cannot pass the fairness gate unnoticed.
    fn tick(&mut self, tick: u64, evals: &Cell<u64>) -> u64 {
        let period = if self.period == 0 { 1 } else { self.period };
        let value = tick / period;
        if self.prior == Some(value) {
            return 0;
        }
        self.prior = Some(value);
        let mut fired = 0;
        for _ in 0..self.entries {
            evals.set(evals.get().saturating_add(1));
            if value.is_multiple_of(2) {
                fired += 1;
            }
        }
        fired
    }
}

// ── Scenario ─────────────────────────────────────────────────────────────────

/// One measured configuration.
#[derive(Clone, Copy)]
struct Scenario {
    name: &'static str,
    /// Why this scenario is in the set. Printed with the result so a reader does
    /// not have to infer the intent from the numbers.
    intent: &'static str,
    sources: usize,
    period: u64,
    entries: usize,
    /// Timed ticks in one round. Bounded by the bot's own journal ceiling: every
    /// fired effect writes three events, and `MAX_JOURNAL_EVENTS` refuses the
    /// next one rather than dropping evidence, so a round longer than this would
    /// be a rig that stops measuring rather than one that measures more.
    ticks_per_round: u64,
    /// Untimed ticks on each round's own bot, before the timer starts. The first
    /// ticks of a fresh bot grow its buffers and prime its queries, and charging
    /// those to a round would report the warm-up as the tick's cost.
    warmup_ticks: u64,
    rounds: usize,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "poll-only-64x100",
        intent: "cost decomposition floor: poll 64 sources and detect change, with no entries to walk",
        sources: 64,
        period: 100,
        entries: 0,
        ticks_per_round: 2_000,
        warmup_ticks: 200,
        rounds: 32,
    },
    Scenario {
        name: "steady-64x100",
        intent: "change detection is the bot's stated advantage: 64 sources, one change per 100 ticks",
        sources: 64,
        period: 100,
        entries: 1,
        ticks_per_round: 2_000,
        warmup_ticks: 200,
        rounds: 32,
    },
    Scenario {
        name: "churn-64x1",
        intent: "worst case for change detection: every source moves every tick",
        sources: 64,
        period: 1,
        entries: 1,
        ticks_per_round: 200,
        warmup_ticks: 50,
        rounds: 32,
    },
    Scenario {
        name: "fanout-1x64",
        intent: "chain-walk cost: one source, sixty-four (condition, action) entries",
        sources: 1,
        period: 1,
        entries: 64,
        ticks_per_round: 200,
        warmup_ticks: 50,
        rounds: 32,
    },
    Scenario {
        name: "wide-256x10",
        intent: "does per-source cost scale linearly with source count",
        sources: 256,
        period: 10,
        entries: 1,
        ticks_per_round: 250,
        warmup_ticks: 50,
        rounds: 32,
    },
];

// ── Building both engines ────────────────────────────────────────────────────

fn build_bot(
    scenario: &Scenario,
    clock: &Clock,
    tally: &Rc<Cell<u64>>,
    evals: &Rc<Cell<u64>>,
) -> Result<Bot, Box<dyn std::error::Error>> {
    let mut builder = Bot::builder("bench").observe(Source {
        clock: clock.clone(),
        period: scenario.period,
    });
    for _ in 0..scenario.entries {
        builder = builder.on(Even(evals.clone()), Tally(tally.clone()));
    }
    for _ in 1..scenario.sources {
        builder = builder.observe(Source {
            clock: clock.clone(),
            period: scenario.period,
        });
        for _ in 0..scenario.entries {
            builder = builder.on(Even(evals.clone()), Tally(tally.clone()));
        }
    }
    Ok(builder.with_effects(effects()?).build(&GrantSet::empty())?)
}

fn build_handrolled(scenario: &Scenario) -> Vec<HandRolled> {
    (0..scenario.sources)
        .map(|_| HandRolled::new(scenario.period, scenario.entries))
        .collect()
}

// ── Measurement ──────────────────────────────────────────────────────────────

/// One scenario's paired result.
struct Measurement {
    name: &'static str,
    intent: &'static str,
    /// Per-round bot duration, seconds.
    bot: Vec<f64>,
    /// Per-round baseline duration, seconds, paired with `bot` by index.
    base: Vec<f64>,
    ticks_per_round: u64,
    effects_bot: u64,
    effects_base: u64,
    evals_bot: u64,
    evals_base: u64,
    build_bot: Duration,
    /// Events the bot's own journal retained after the busiest round, and the
    /// crate's declared ceiling on them.
    ///
    /// Reported for every row rather than once in a footnote, because a bot
    /// whose journal is near its ceiling is a bot whose next effect is refused
    /// (INV-BOT-16: a scale measurement records the requested level, the level
    /// reached, and the ceiling together, so no reader is told a number nobody
    /// ran).
    journal_events: usize,
    journal_ceiling: usize,
}

/// Run one scenario paired: within a round, the bot is timed and then the
/// baseline is timed on the same input window, so machine drift lands on both
/// legs and cancels in the ratio.
///
/// **Each round builds its own bot and its own baseline, and warms both before
/// the timer starts.** Two reasons, and both are about what a round measures:
///
/// - Every bot this rig admits dispatches an external effect, so every fired
///   effect writes to the journal behind it. One bot for the whole run retains
///   every event it ever wrote, and `MAX_JOURNAL_EVENTS` (100 000) refuses the
///   next append rather than dropping evidence — so a workload that fires
///   millions of effects cannot be measured against one journal at all. A round
///   bounds what is retained, and `Scenario::ticks_per_round` is sized to stay
///   inside that ceiling.
/// - Admission is then outside the timed window on both legs, which is what the
///   ratio is supposed to be about: the cost of a steady-state tick, not of the
///   first tick a freshly built bot pays.
///
/// The input window keeps advancing across rounds, because a source's value is a
/// function of the tick index and a workload that restarted would be measuring a
/// different one.
fn measure(scenario: &Scenario) -> Result<Measurement, Box<dyn std::error::Error>> {
    let clock = Clock::new();
    let tally = Rc::new(Cell::new(0u64));
    let evals_bot_cell = Rc::new(Cell::new(0u64));
    let evals_base_cell = Cell::new(0u64);

    let build_start = Instant::now();
    let admission = build_bot(scenario, &clock, &tally, &evals_bot_cell)?;
    let admission_cost = build_start.elapsed();
    drop(admission);

    let mut bot_tick = 0u64;
    let mut base_tick = 0u64;
    let mut effects_bot = 0u64;
    let mut effects_base = 0u64;
    let mut bot_times = Vec::with_capacity(scenario.rounds);
    let mut base_times = Vec::with_capacity(scenario.rounds);
    let mut journal_events = 0usize;

    for _ in 0..scenario.rounds {
        let mut bot = build_bot(scenario, &clock, &tally, &evals_bot_cell)?;
        let mut chains = build_handrolled(scenario);

        for _ in 0..scenario.warmup_ticks {
            clock.set(bot_tick);
            let fired = bot.tick()?;
            effects_bot = effects_bot.saturating_add(fired as u64);
            bot_tick = bot_tick.saturating_add(1);
        }
        for _ in 0..scenario.warmup_ticks {
            let fired: u64 = chains
                .iter_mut()
                .map(|c| c.tick(base_tick, &evals_base_cell))
                .sum();
            effects_base = effects_base.saturating_add(fired);
            base_tick = base_tick.saturating_add(1);
        }

        let start = Instant::now();
        for _ in 0..scenario.ticks_per_round {
            clock.set(bot_tick);
            let fired = bot.tick()?;
            effects_bot = effects_bot.saturating_add(fired as u64);
            bot_tick = bot_tick.saturating_add(1);
        }
        let bot_elapsed = start.elapsed();

        let start = Instant::now();
        for _ in 0..scenario.ticks_per_round {
            let fired: u64 = chains
                .iter_mut()
                .map(|c| c.tick(base_tick, &evals_base_cell))
                .sum();
            effects_base = effects_base.saturating_add(fired);
            base_tick = base_tick.saturating_add(1);
        }
        let base_elapsed = start.elapsed();

        journal_events = journal_events.max(retained_events(bot));
        bot_times.push(bot_elapsed.as_secs_f64());
        base_times.push(base_elapsed.as_secs_f64());
    }

    Ok(Measurement {
        name: scenario.name,
        intent: scenario.intent,
        bot: bot_times,
        base: base_times,
        ticks_per_round: scenario.ticks_per_round,
        effects_bot,
        effects_base,
        evals_bot: evals_bot_cell.get(),
        evals_base: evals_base_cell.get(),
        build_bot: admission_cost,
        journal_events,
        journal_ceiling: MAX_JOURNAL_EVENTS,
    })
}

/// How many events the bot's own journal retained, read back through the door
/// the crate provides for it.
///
/// `into_journal` rather than an accessor on the bot: the journal is the host's
/// record, and a measurement instrument is not a reason to add a getter to the
/// thing being measured.
fn retained_events(bot: Bot) -> usize {
    bot.into_journal().map_or(0, |journal| {
        journal.committed().map_or(0, |events| events.len())
    })
}

// ── Capability-check scaling ─────────────────────────────────────────────────

/// Time `Auth::check` as the number of required capabilities grows.
///
/// `Auth` is minted by cloning the required slice into a `Vec`, and `check`
/// then asks, for each required capability, whether that `Vec` contains it. That
/// is a linear scan inside a linear scan, so the expected shape is quadratic in
/// the capability count. Four shipped capabilities make that invisible; the
/// measurement is here to establish where it stops being invisible, because
/// `Cap::new` accepts any name by convention and a custom domain may require
/// many.
fn cap_check_scaling() -> Result<Vec<(usize, f64)>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for count in [1usize, 2, 4, 8, 16, 32, 64, 128] {
        let mut grants = GrantSet::empty();
        let mut caps = Vec::with_capacity(count);
        for index in 0..count {
            let cap = Cap::new(format!("bench.cap.{index}"));
            grants = grants.grant(cap.clone());
            caps.push(cap);
        }
        let auth = grants.issue(&caps)?;

        // Warm the cache and the branch predictor before timing.
        for _ in 0..1_000 {
            auth.check(&caps)?;
        }

        const ITERATIONS: u32 = 200_000;
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            auth.check(&caps)?;
        }
        let elapsed = start.elapsed().as_secs_f64();
        let per_call_ns = (elapsed / f64::from(ITERATIONS)) * 1e9;
        out.push((count, per_call_ns));
    }
    Ok(out)
}

/// Time `Bot::builder(..).build(&grants)` -- the admission path.
fn build_cost(sources: usize) -> Result<Duration, Box<dyn std::error::Error>> {
    let clock = Clock::new();
    let tally = Rc::new(Cell::new(0u64));
    let evals = Rc::new(Cell::new(0u64));
    let scenario = Scenario {
        name: "build",
        intent: "admission cost",
        sources,
        period: 100,
        entries: 1,
        ticks_per_round: 1,
        warmup_ticks: 1,
        rounds: 1,
    };
    let start = Instant::now();
    let _bot = build_bot(&scenario, &clock, &tally, &evals)?;
    Ok(start.elapsed())
}

// ── Determinism ──────────────────────────────────────────────────────────────

/// Run the same scenario twice and compare the per-tick effect counts.
///
/// This is a *correctness* observation, not a timing one, and it is the only
/// claim in this rig that the timings cannot make: that the schedule is a
/// function of its inputs. The formal statement of the same property, and the
/// proof, are in `../proofs/`.
fn determinism_probe(
    scenario: &Scenario,
    ticks: u64,
) -> Result<(bool, usize), Box<dyn std::error::Error>> {
    let run = || -> Result<Vec<u64>, Box<dyn std::error::Error>> {
        let clock = Clock::new();
        let tally = Rc::new(Cell::new(0u64));
        let evals = Rc::new(Cell::new(0u64));
        let mut bot = build_bot(scenario, &clock, &tally, &evals)?;
        let mut per_tick = Vec::with_capacity(ticks as usize);
        for tick in 0..ticks {
            clock.set(tick);
            per_tick.push(bot.tick()? as u64);
        }
        Ok(per_tick)
    };
    let first = run()?;
    let second = run()?;
    let fired: u64 = first.iter().sum();
    Ok((first == second, fired as usize))
}

// ── The per-stage profile ────────────────────────────────────────────────────
//
// A ratio against a hand-rolled loop says the bot is slower; it does not say
// which half of the tick the difference is made of. This drives the crate's own
// `tick_profiled` -- the same four phases, with the per-stage instrument armed --
// so the breakdown is a decomposition of the real tick rather than of a model of
// it.
//
// The instrument is not free: seven clock reads on a tick. So every row is
// measured twice on the same bot and the same workload, once through `tick` and
// once through `tick_profiled`, and the difference is printed beside the stages.
// The shares are what the profile supports; the absolute times carry the
// instrument and are printed with its cost subtracted.

/// One scenario's stage breakdown, plus what the instrument cost on it.
struct StageRow {
    name: &'static str,
    profile: TickProfile,
    /// ns/tick through `tick()`, measured on the same workload.
    plain_ns: f64,
    /// ns/tick through `tick_profiled()`, measured on the same workload.
    profiled_ns: f64,
}

/// Profile one scenario through both doors and report the stages.
///
/// Per-round construction for the same reason `measure` does it: the bot's
/// journal is bounded by one round's workload, and the warm-up that grows every
/// buffer sits outside the measured window on both legs. The two legs run the
/// *same* window on the *same* bot, back to back, so the difference between them
/// is the instrument and nothing else.
fn profile_row(scenario: &Scenario) -> Result<StageRow, Box<dyn std::error::Error>> {
    let clock = Clock::new();
    let tally = Rc::new(Cell::new(0u64));
    let evals = Rc::new(Cell::new(0u64));

    let mut plain_rounds: Vec<f64> = Vec::with_capacity(scenario.rounds);
    let mut profiled_rounds: Vec<f64> = Vec::with_capacity(scenario.rounds);
    let mut profile = TickProfile::new();
    let mut tick = 0u64;

    for _ in 0..scenario.rounds {
        let mut bot = build_bot(scenario, &clock, &tally, &evals)?;
        for _ in 0..scenario.warmup_ticks {
            clock.set(tick);
            let _ = bot.tick()?;
            tick = tick.saturating_add(1);
        }

        let start = Instant::now();
        for _ in 0..scenario.ticks_per_round {
            clock.set(tick);
            let _ = bot.tick()?;
            tick = tick.saturating_add(1);
        }
        plain_rounds.push(per_tick_ns(start.elapsed(), scenario.ticks_per_round));

        let start = Instant::now();
        for _ in 0..scenario.ticks_per_round {
            clock.set(tick);
            let (_, measured) = lgwks_std::task::block_on(bot.tick_profiled());
            profile.absorb(measured);
            tick = tick.saturating_add(1);
        }
        profiled_rounds.push(per_tick_ns(start.elapsed(), scenario.ticks_per_round));
    }
    Ok(StageRow {
        name: scenario.name,
        profile,
        plain_ns: stats::median(&mut plain_rounds),
        profiled_ns: stats::median(&mut profiled_rounds),
    })
}

/// A round's wall time as ns/tick.
fn per_tick_ns(elapsed: Duration, ticks: u64) -> f64 {
    if ticks == 0 {
        return f64::NAN;
    }
    (elapsed.as_secs_f64() / ticks as f64) * 1e9
}

/// The per-stage table, for every scenario.
///
/// The percentages are the claim the profile supports; the nanoseconds beside
/// them carry the instrument, and the `plain` column is what the same workload
/// costs with the instrument off. The `instrument` column is the difference, and
/// the clock's own cost is printed under the table because on this host it is
/// what the instrument mostly is.
fn stage_report() -> Result<String, Box<dyn std::error::Error>> {
    let mut rows = Vec::with_capacity(SCENARIOS.len());
    for scenario in SCENARIOS {
        rows.push(profile_row(scenario)?);
    }

    let mut out = String::new();
    writeln!(
        out,
        "\nper-stage profile -- where a tick goes, measured on the tick path itself\n\
         ================================================================================="
    )?;
    writeln!(
        out,
        "  {} timed ticks per round x {} rounds per window, release, one process; the two\n\
         windows run the same workload on the same bot, back to back",
        SCENARIOS[0].ticks_per_round, SCENARIOS[0].rounds
    )?;
    writeln!(
        out,
        "  scenario                {:>9} {:>11} {:>9} {:>9} {:>8} {:>8} {:>10} {:>10}",
        "poll", "fingerprint", "compare", "schedule", "decide", "act", "plain", "instrument"
    )?;
    for row in &rows {
        write!(out, "  {:<20}", row.name)?;
        for stage in TickStage::ALL {
            let _ = write!(out, " {value:>9.1}", value = ns_of(row, stage));
        }
        let _ = writeln!(
            out,
            " {:>10.1} {:>+10.1}",
            row.plain_ns,
            row.profiled_ns - row.plain_ns
        );
    }
    writeln!(out, "\n  shares of the measured tick")?;
    writeln!(
        out,
        "  scenario                {:>9} {:>11} {:>9} {:>9} {:>8} {:>8}",
        "poll", "fingerprint", "compare", "schedule", "decide", "act"
    )?;
    for row in &rows {
        write!(out, "  {:<20}", row.name)?;
        for stage in TickStage::ALL {
            let _ = write!(out, " {:>8.1}%", row.profile.share(stage) * 100.0);
        }
        out.push('\n');
    }
    writeln!(
        out,
        "\n  The stage columns carry the instrument: a tick reads the clock once per charge\n\
         plus once to open the window, and `plain` is the same workload with the instrument\n\
         off. One clock read costs {:.0} ns on this host, so a seven-read instrument adds about\n\
         {:.0} ns/tick whatever the tick does. `schedule` is a residual: the stages nested\n\
         inside it charge first, so what it reports is the dispatch of the two systems plus the\n\
         commit bookkeeping in `observe_fold` that nothing else claims.",
        clock_read_ns(),
        clock_read_ns() * 7.0
    )?;
    Ok(out)
}

/// What one `Instant::now()` costs on this host.
///
/// Measured rather than assumed, because on this host it is the largest single
/// term in the instrument column above: macOS does not put `clock_gettime` in the
/// vDSO, so a clock read is a real syscall and a seven-read instrument costs more
/// than a short tick. A reader who wants the stage absolutes without the
/// instrument needs this number to subtract it.
fn clock_read_ns() -> f64 {
    /// Reads in the calibration. Enough that the syscall's own jitter is a small
    /// share of the total, and small enough to cost nothing to run.
    const READS: u64 = 200_000;
    let start = Instant::now();
    let mut sink = 0u64;
    for _ in 0..READS {
        sink = sink.wrapping_add(Instant::now().elapsed().subsec_nanos() as u64);
    }
    // The sink is read so the loop cannot be optimized away, and the sum is
    // deliberately meaningless: what is measured is the elapsed time around it.
    let elapsed = (start.elapsed().as_secs_f64() / READS as f64) * 1e9;
    let _ = sink;
    elapsed
}

/// One stage's ns/tick for one row, read through the profile's own accessor.
fn ns_of(row: &StageRow, stage: TickStage) -> f64 {
    row.profile.per_tick(stage).as_secs_f64() * 1e9
}

// ── The allocation model ─────────────────────────────────────────────────────

/// Count one scenario's steady-state allocations and append its row.
fn alloc_row(out: &mut String, scenario: &Scenario) -> Result<(), Box<dyn std::error::Error>> {
    let clock = Clock::new();
    let tally = Rc::new(Cell::new(0u64));
    let evals_bot_cell = Rc::new(Cell::new(0u64));
    let evals_base_cell = Cell::new(0u64);

    let mut bot = build_bot(scenario, &clock, &tally, &evals_bot_cell)?;
    let mut chains = build_handrolled(scenario);

    // Warm up, so anything allocated once by the schedule's first run (a
    // lazily-grown buffer, a query's internal state) is already in place.
    let warm = 64u64;
    for tick in 0..warm {
        clock.set(tick);
        let _ = bot.tick()?;
    }
    for tick in 0..warm {
        let _ = chains
            .iter_mut()
            .map(|c| c.tick(tick, &evals_base_cell))
            .sum::<u64>();
    }

    let ticks = 512u64;
    alloc_count::reset();
    alloc_count::start();
    for tick in warm..warm.saturating_add(ticks) {
        clock.set(tick);
        let _ = bot.tick()?;
    }
    alloc_count::stop();
    let (bot_allocs, bot_bytes) = alloc_count::snapshot();

    alloc_count::reset();
    alloc_count::start();
    for tick in warm..warm.saturating_add(ticks) {
        let _ = chains
            .iter_mut()
            .map(|c| c.tick(tick, &evals_base_cell))
            .sum::<u64>();
    }
    alloc_count::stop();
    let (base_allocs, _) = alloc_count::snapshot();

    let per_tick = bot_allocs as f64 / ticks as f64;
    writeln!(
        out,
        "  {:<20} {:>10.1}   {:>10.1}   {:>10.1}   {:>19.1}",
        scenario.name,
        per_tick,
        bot_bytes as f64 / ticks as f64,
        bot_bytes as f64 / bot_allocs.max(1) as f64,
        base_allocs as f64 / ticks as f64,
    )?;
    Ok(())
}

/// Count the heap allocations one tick costs, per scenario.
///
/// The bot is built and warmed *outside* the counted window, so what is counted
/// is the steady-state tick and not the admission cost. The baseline is counted
/// through the same window, which is the control: a hand-rolled loop over the
/// same workload should allocate nothing, and a non-zero count there would mean
/// the counter is picking up something other than the bot.
fn alloc_report() -> Result<String, Box<dyn std::error::Error>> {
    let mut out = String::new();
    writeln!(
        out,
        "\nallocation model -- steady-state tick, built and warmed outside the counted window\n\
         =================================================================================="
    )?;
    writeln!(
        out,
        "  scenario                allocs/tick   bytes/tick   mean bytes   baseline allocs/tick"
    )?;

    for scenario in SCENARIOS {
        alloc_row(&mut out, scenario)?;
    }
    // Per-tick trace for one scenario. A mean over 512 ticks hides the shape of
    // the cost: a bot that allocates nothing on a steady tick and heavily on a
    // moving one has the same mean as one that allocates the same amount every
    // tick, and only the trace tells them apart. This is what says whether the
    // observation path still allocates on a tick where nothing moved.
    let scenario = &SCENARIOS[1];
    let clock = Clock::new();
    let tally = Rc::new(Cell::new(0u64));
    let evals = Rc::new(Cell::new(0u64));
    let mut bot = build_bot(scenario, &clock, &tally, &evals)?;
    for tick in 0..64u64 {
        clock.set(tick);
        let _ = bot.tick()?;
    }
    write!(
        out,
        "\n  per-tick allocations, {} (a value moves every {} ticks)\n    ",
        scenario.name, scenario.period
    )?;
    for tick in 64..124u64 {
        clock.set(tick);
        alloc_count::reset();
        alloc_count::start();
        let _ = bot.tick()?;
        alloc_count::stop();
        let (tick_allocs, _) = alloc_count::snapshot();
        write!(out, "{tick}:{tick_allocs} ")?;
    }
    writeln!(out, "\n")?;
    Ok(out)
}

// ── Reporting ────────────────────────────────────────────────────────────────

/// Measure one scenario, refuse an unfair comparison, and append its human and JSON rows.
fn scenario_report(
    scenario: &Scenario,
    human: &mut String,
    json: &mut String,
) -> Result<(), Box<dyn std::error::Error>> {
    let m = measure(scenario)?;

    // THE FAIRNESS GATE, on both axes. A ratio between two engines doing
    // different amounts of work is not a measurement, so refuse to report
    // one. Effects catch a baseline that produces different results;
    // evaluations catch a baseline that reaches the same results by
    // skipping the work.
    if m.effects_bot != m.effects_base {
        let refusal = Err(format!(
            "FAIRNESS CHECK FAILED for '{}': bot fired {} effects, baseline fired {}. \
         The two engines are not doing the same work, so no timing from this \
         scenario is reportable.",
            m.name, m.effects_bot, m.effects_base
        )
        .into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "main: returning an error to the caller");
        return refusal;
    }
    if m.evals_bot != m.evals_base {
        let refusal = Err(format!(
            "FAIRNESS CHECK FAILED for '{}': bot evaluated the condition {} times, \
         baseline {} times. The effect counts agree, so this is a baseline that \
         reaches the same answer with less work -- which is exactly the comparison \
         this gate exists to refuse.",
            m.name, m.evals_bot, m.evals_base
        )
        .into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "main: returning an error to the caller");
        return refusal;
    }

    let mut ratios: Vec<f64> = m
        .bot
        .iter()
        .zip(m.base.iter())
        .map(|(b, r)| if *r > 0.0 { b / r } else { f64::NAN })
        .collect();

    let ticks = m.ticks_per_round as f64;
    let mut bot_tps: Vec<f64> = m.bot.iter().map(|d| ticks / d).collect();
    let mut base_tps: Vec<f64> = m.base.iter().map(|d| ticks / d).collect();

    let ratio_median = stats::median(&mut ratios);
    let (ci_lo, ci_hi) = stats::bootstrap_median_ci(&ratios, 10_000, 0.95, 0x5EED_1234);
    let bot_median_tps = stats::median(&mut bot_tps);
    let base_median_tps = stats::median(&mut base_tps);

    writeln!(human, "scenario  {}", m.name)?;
    writeln!(human, "  intent  {}", m.intent)?;
    writeln!(
        human,
        "  workload  {} sources x {} entries, {} ticks/round x {} rounds",
        scenario.sources, scenario.entries, m.ticks_per_round, scenario.rounds
    )?;
    writeln!(
        human,
        "  fairness  {} effects and {} condition evaluations, identical in both engines",
        m.effects_bot, m.evals_bot
    )?;
    writeln!(
        human,
        "  bot       {:.0} ticks/s   ({:.1} ns/tick)",
        bot_median_tps,
        1e9 / bot_median_tps
    )?;
    writeln!(
        human,
        "  baseline  {:.0} ticks/s   ({:.1} ns/tick)",
        base_median_tps,
        1e9 / base_median_tps
    )?;
    writeln!(
        human,
        "  ratio     {:.2}x  bot / baseline   95% CI [{:.2}, {:.2}]  {}",
        ratio_median,
        ci_lo,
        ci_hi,
        if stats::distinguishes_parity(ci_lo, ci_hi) {
            "(excludes parity)"
        } else {
            "(INCLUDES parity -- not a distinguishable difference)"
        }
    )?;
    writeln!(
        human,
        "  journal   {} events retained after the busiest round, ceiling {}",
        m.journal_events, m.journal_ceiling
    )?;
    writeln!(
        human,
        "  build     {} chains admitted in {:.3} ms\n",
        scenario.sources,
        m.build_bot.as_secs_f64() * 1e3
    )?;

    writeln!(
        json,
        "  \"{}\": {{ \"sources\": {}, \"entries\": {}, \"ticks_per_round\": {}, \
         \"rounds\": {}, \"effects\": {}, \"evaluations\": {}, \
         \"bot_ticks_per_sec\": {:.1}, \"baseline_ticks_per_sec\": {:.1}, \
         \"ratio_median\": {:.4}, \"ratio_ci95\": [{:.4}, {:.4}], \
         \"journal_events\": {}, \"journal_ceiling\": {}, \"build_ms\": {:.4} }},",
        m.name,
        scenario.sources,
        scenario.entries,
        m.ticks_per_round,
        scenario.rounds,
        m.effects_bot,
        m.evals_bot,
        bot_median_tps,
        base_median_tps,
        ratio_median,
        ci_lo,
        ci_hi,
        m.journal_events,
        m.journal_ceiling,
        m.build_bot.as_secs_f64() * 1e3
    )?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The report goes through the estate's tracing, not `println!`: levelled,
    // filterable through `LGWKS_LOG`, and never a panic on a closed pipe.
    lgwks_std::trace::install_default("lgwks-bench")?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run_timings = !args.iter().any(|a| a == "--capcheck-only");

    if args.iter().any(|a| a == "--alloc-report") {
        info!("{}", alloc_report()?);
        return Ok(());
    }

    if args.iter().any(|a| a == "--profile") {
        info!("{}", stage_report()?);
        return Ok(());
    }

    let mut json = String::from("{\n");
    let mut human = String::new();

    writeln!(
        human,
        "\nlgwks_bot benchmark rig -- paired design, ratios are bot / hand-rolled baseline\n\
         ==============================================================================\n"
    )?;

    if run_timings {
        for scenario in SCENARIOS {
            scenario_report(scenario, &mut human, &mut json)?;
        }
    }

    // Capability-check scaling.
    let scaling = cap_check_scaling()?;
    writeln!(
        human,
        "capability-check scaling -- Auth::check as required-capability count grows"
    )?;
    writeln!(human, "  required   ns/call    ns per required cap")?;
    for (count, ns) in &scaling {
        writeln!(
            human,
            "  {:>8}   {:>7.2}    {:>18.3}",
            count,
            ns,
            ns / (*count as f64)
        )?;
    }
    // A scaling line needs both ends; a run that produced no points has no growth
    // to report, and inventing one (a 1.0 or a NaN) would print a number nobody
    // measured.
    let (Some(&(first_caps, first_ns)), Some(&(last_caps, last_ns))) =
        (scaling.first(), scaling.last())
    else {
        let refusal: Result<(), Box<dyn std::error::Error>> =
            Err("the capability-scaling run produced no points".into());
        lgwks_std::trace::warn!(error = ?refusal.as_ref().err(), "capability scaling: no points to report");
        return refusal;
    };
    let growth = last_caps as f64 / first_caps as f64;
    writeln!(
        human,
        "  growth    {:.1}x cost for {:.0}x capabilities (quadratic would be {:.0}x)\n",
        last_ns / first_ns,
        growth,
        growth * growth
    )?;

    // Build cost.
    let build_1 = build_cost(1)?;
    let build_64 = build_cost(64)?;
    writeln!(
        human,
        "admission (build) cost\n  1 source: {:.3} ms\n  64 sources: {:.3} ms\n",
        build_1.as_secs_f64() * 1e3,
        build_64.as_secs_f64() * 1e3
    )?;

    // Determinism.
    let (deterministic, fired) = determinism_probe(&SCENARIOS[1], 500)?;
    writeln!(
        human,
        "determinism probe (500 ticks, replayed)\n  identical per-tick effect sequence: {}\n  total effects: {}\n",
        if deterministic { "YES" } else { "NO" },
        fired
    )?;

    json.push_str(&format!(
        "  \"cap_check_scaling\": [{}],\n  \"determinism\": {},\n  \"build_ms_1\": {:.4},\n  \"build_ms_64\": {:.4}\n}}\n",
        scaling
            .iter()
            .map(|(c, ns)| format!("{{\"caps\": {c}, \"ns_per_call\": {ns:.3}}}"))
            .collect::<Vec<_>>()
            .join(", "),
        deterministic,
        build_1.as_secs_f64() * 1e3,
        build_64.as_secs_f64() * 1e3
    ));

    info!("{human}");

    if let Some(path) = args.iter().find(|a| a.starts_with("--json=")) {
        let path = path.trim_start_matches("--json=");
        std::fs::write(path, &json)?;
        info!("wrote {path}");
    }

    Ok(())
}
