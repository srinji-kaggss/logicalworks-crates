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
//! # The measured bots dispatch through a journal on disk
//!
//! An admitted bot is refused at assembly without an effect scope, and an
//! external effect is refused at the handoff unless the scope's journal promises
//! `DurabilityPromise::ProcessCrash`. `MemoryJournal` promises `Ephemeral`, so
//! the bots this rig measures -- whose action does not declare itself local --
//! journal to `FileJournal` on disk, under `$TMPDIR`, one file per bot.
//!
//! That puts a `write` and an `fsync` inside every measured window, and it is
//! the honest cost of the configuration rather than something this rig can
//! decline: there is no in-memory journal the crate will admit here. It also
//! makes a timing run impractical, because an append on a typical filesystem
//! costs milliseconds and the scenarios fire an effect every tick. The
//! allocation report still runs, and it prints what a measured window now
//! costs beside the allocations it contains. The figures committed in
//! `README.md` predate this and exclude the disk; read them for what they
//! measured, not for what the bot now costs.
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

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use lgwks_bot::broker::Broker;
use lgwks_bot::effect::{EnvironmentId, FlowRevision, RunId};
use lgwks_bot::journal::FileJournal;
use lgwks_bot::spec::{EffectIdentity, EffectScope};
use lgwks_bot::{
    Auth, Bot, BotError, Cap, DispatchCertainty, Evaluate, Execute, GrantSet, Observe,
};

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

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static COUNTING: AtomicU64 = AtomicU64::new(0);

struct Counting;

impl Counting {
    /// Whether the counters are live. Reads are relaxed: the flag is only ever
    /// flipped between measurement phases, never inside one.
    fn on() -> bool {
        COUNTING.load(Ordering::Relaxed) == 1
    }
}

// SAFETY: every method forwards to `System` unchanged, so the allocator
// contract (`alloc`/`dealloc`/`realloc` paired on the same `Layout`) is the
// system allocator's. The only addition is a counter increment on either side,
// which allocates nothing and touches no memory the caller owns.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if Self::on() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        // SAFETY: `layout` is forwarded verbatim from the caller.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr`/`layout` are forwarded verbatim from the caller, which
        // obtained them from this allocator's `alloc`/`realloc`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if Self::on() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        }
        // SAFETY: `ptr`/`layout`/`new_size` are forwarded verbatim.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

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

    /// The value itself, which is the cheapest faithful key this source has.
    ///
    /// It is what the source already computes, so it costs nothing extra and it
    /// is exactly the digest the contract asks for: equal fingerprints imply
    /// equal values, because it *is* the value. A real source would use an ETag
    /// or an `mtime` — something cheaper than the value — and the point of this
    /// one is that the rig measures the seam's overhead with no such shortcut
    /// available, so the number it reports is a floor on the gain and not a
    /// best case.
    fn fingerprint(&self) -> Option<u128> {
        let period = if self.period == 0 { 1 } else { self.period };
        Some(u128::from(self.clock.0.get() / period))
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
    ticks_per_round: u64,
    warmup_rounds: usize,
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
        warmup_rounds: 4,
        rounds: 24,
    },
    Scenario {
        name: "steady-64x100",
        intent: "change detection is the bot's stated advantage: 64 sources, one change per 100 ticks",
        sources: 64,
        period: 100,
        entries: 1,
        ticks_per_round: 2_000,
        warmup_rounds: 4,
        rounds: 24,
    },
    Scenario {
        name: "churn-64x1",
        intent: "worst case for change detection: every source moves every tick",
        sources: 64,
        period: 1,
        entries: 1,
        ticks_per_round: 1_000,
        warmup_rounds: 4,
        rounds: 24,
    },
    Scenario {
        name: "fanout-1x64",
        intent: "chain-walk cost: one source, sixty-four (condition, action) entries",
        sources: 1,
        period: 1,
        entries: 64,
        ticks_per_round: 1_000,
        warmup_rounds: 4,
        rounds: 24,
    },
    Scenario {
        name: "wide-256x10",
        intent: "does per-source cost scale linearly with source count",
        sources: 256,
        period: 10,
        entries: 1,
        ticks_per_round: 250,
        warmup_rounds: 4,
        rounds: 24,
    },
];

// ── Building both engines ────────────────────────────────────────────────────

/// Create this run's scratch directory, the only place a journal of this rig
/// is allowed to live.
///
/// Created once and handed down as a `&Path` rather than minted per call site:
/// a measured bot holds its journal for its whole life, so a per-call directory
/// would leave one directory open behind every bot and hide how many of them a
/// run actually builds.
///
/// Nothing here removes it on the way out. A journal is the record of what left
/// the process, and a measurement rig that deleted it at exit would be
/// discarding the very evidence it exists to produce; the directory is
/// `lgwks-bench-journal-<pid>`, so a run's records are under the run's own pid
/// and are the caller's to remove.
///
/// # Errors
///
/// Whatever [`std::fs::create_dir_all`] reports for the path it was given,
/// which is the case where the directory cannot be created at all.
fn open_scratch_dir() -> Result<PathBuf, std::io::Error> {
    let scratch = std::env::temp_dir().join(format!("lgwks-bench-journal-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    Ok(scratch)
}

/// The next journal file `bot_number` gets in this run's scratch directory.
///
/// One file per bot rather than one per run, and that is not tidiness: the
/// identity in [`bench_effects`] is fixed, so every scenario records the same
/// effect keys, and a journal holding more than one bot's attempts for a key
/// sees the same key climb its ladder twice and refuses the repeat at the fence.
/// A fresh file per bot is also a fresh history, so no bot starts against
/// another one's committed frames.
fn bench_journal_path(scratch: &Path, bot_number: usize) -> PathBuf {
    scratch.join(format!("bot-{bot_number}.journal"))
}

/// Admit one scenario's chains as a bot that dispatches under a durable scope.
///
/// `scratch` is this run's scratch directory and `bot_number` distinguishes this
/// bot's journal from its neighbours' within it.
fn build_bot(
    scenario: &Scenario,
    clock: &Clock,
    tally: &Rc<Cell<u64>>,
    evals: &Rc<Cell<u64>>,
    scratch: &Path,
    bot_number: usize,
) -> Result<Bot, BotError> {
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
    builder
        .with_effects(bench_effects(&bench_journal_path(scratch, bot_number))?)
        .build(&GrantSet::empty())
}

/// The effect scope every measured bot dispatches through.
///
/// A bot is refused at assembly without one: the change that made dispatch
/// durable also made it mandatory, and this rig was not updated with it, so
/// `lgwks-bench` failed at startup with `IncompleteSpec { field: "effects" }`
/// and reported nothing at all.
///
/// The values are fixed rather than minted, so a measured run is reproducible
/// and two scenarios are not dispatching under different identities. They are
/// also fixed *across* the run rather than per bot, which is why each bot gets
/// its own journal file; see [`bench_journal_path`].
///
/// The journal is on disk, and that is the honest cost of the configuration
/// rather than a choice this rig can make its way out of. These are
/// `EffectLifetime::External` effects, because [`Tally`] does not override
/// [`Execute::effect_lifetime`] and an action that does not say so is assumed to
/// leave the process. The admission gate on the handoff path then requires
/// [`lgwks_bot::journal::DurabilityPromise::ProcessCrash`], and
/// [`lgwks_bot::journal::MemoryJournal`] reports
/// [`lgwks_bot::journal::DurabilityPromise::Ephemeral`], so the in-memory
/// journal this rig used to build is refused at the first dispatch with
/// `PromiseUnmet { required: ProcessCrash, offered: Ephemeral }`.
/// [`FileJournal`] earns the promise it advertises — the frame is written and
/// the file is synced before the acknowledgment exists — and accepts the
/// dispatch.
///
/// What that buys, and what it costs, are both inside the measured window: a
/// tick that decides to fire an effect now pays a write and an `fsync` before
/// it returns. Every timing below and in `README.md` was taken against the
/// in-memory journal and does not include it, so treat those figures as a
/// statement about the schedule rather than about the schedule plus its
/// durability.
fn bench_effects(journal_path: &Path) -> Result<EffectScope, BotError> {
    // Every failure below is a refusal by the bench's own fixed identifiers, so
    // it names this rig rather than a caller's environment.
    fn refused(cause: impl std::fmt::Display) -> BotError {
        BotError::DomainError {
            domain: String::from("bench::effect_scope"),
            certainty: DispatchCertainty::NotDelivered,
            cause: cause.to_string(),
        }
    }
    let environment =
        EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30").map_err(refused)?;
    let mut broker = Broker::new();
    broker.register(environment).map_err(refused)?;
    Ok(EffectScope::new(
        EffectIdentity::new(
            RunId::from_hex("0102030405060708090a0b0c0d0e0f10").map_err(refused)?,
            environment,
            FlowRevision::from_tagged(
                "blake3_256",
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            )
            .map_err(refused)?,
        ),
        broker,
        Box::new(FileJournal::open(journal_path).map_err(refused)?),
    ))
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
}

/// Run one scenario paired: within a round, the bot is timed and then the
/// baseline is timed on the same input window, so machine drift lands on both
/// legs and cancels in the ratio.
///
/// `scratch` is this run's journal directory and `bot_number` names this
/// scenario's journal within it.
fn measure(
    scenario: &Scenario,
    scratch: &Path,
    bot_number: usize,
) -> Result<Measurement, BotError> {
    let clock = Clock::new();
    let tally = Rc::new(Cell::new(0u64));
    let evals_bot_cell = Rc::new(Cell::new(0u64));
    let evals_base_cell = Cell::new(0u64);

    let build_start = Instant::now();
    let mut bot = build_bot(
        scenario,
        &clock,
        &tally,
        &evals_bot_cell,
        scratch,
        bot_number,
    )?;
    let build_bot = build_start.elapsed();

    let mut chains = build_handrolled(scenario);

    let mut bot_tick = 0u64;
    let mut base_tick = 0u64;
    let mut effects_bot = 0u64;
    let mut effects_base = 0u64;
    let mut bot_times = Vec::with_capacity(scenario.rounds);
    let mut base_times = Vec::with_capacity(scenario.rounds);

    let total = scenario.warmup_rounds + scenario.rounds;
    for round in 0..total {
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

        if round >= scenario.warmup_rounds {
            bot_times.push(bot_elapsed.as_secs_f64());
            base_times.push(base_elapsed.as_secs_f64());
        }
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
        build_bot,
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
fn cap_check_scaling() -> Result<Vec<(usize, f64)>, BotError> {
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
///
/// The timed span includes opening the journal, because `build` is what
/// admits the chains and the journal is part of what they are admitted with.
fn build_cost(sources: usize, scratch: &Path, bot_number: usize) -> Result<Duration, BotError> {
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
        warmup_rounds: 0,
        rounds: 1,
    };
    let start = Instant::now();
    let _bot = build_bot(&scenario, &clock, &tally, &evals, scratch, bot_number)?;
    Ok(start.elapsed())
}

// ── Determinism ──────────────────────────────────────────────────────────────

/// Run the same scenario twice and compare the per-tick effect counts.
///
/// This is a *correctness* observation, not a timing one, and it is the only
/// claim in this rig that the timings cannot make: that the schedule is a
/// function of its inputs. The formal statement of the same property, and the
/// proof, are in `../proofs/`.
///
/// `bot_number` and its successor name the two journals, which is what the
/// comparison requires: a replay has to start from an empty history to be a
/// replay, and a journal that already holds the first run's attempts would
/// refuse the second's outright.
fn determinism_probe(
    scenario: &Scenario,
    ticks: u64,
    scratch: &Path,
    bot_number: usize,
) -> Result<(bool, usize), BotError> {
    let run = |bot_number: usize| -> Result<Vec<u64>, BotError> {
        let clock = Clock::new();
        let tally = Rc::new(Cell::new(0u64));
        let evals = Rc::new(Cell::new(0u64));
        let mut bot = build_bot(scenario, &clock, &tally, &evals, scratch, bot_number)?;
        let mut per_tick = Vec::with_capacity(ticks as usize);
        for tick in 0..ticks {
            clock.set(tick);
            per_tick.push(bot.tick()? as u64);
        }
        Ok(per_tick)
    };
    let first = run(bot_number)?;
    let second = run(bot_number + 1)?;
    let fired: u64 = first.iter().sum();
    Ok((first == second, fired as usize))
}

// ── The allocation model ─────────────────────────────────────────────────────

/// Count the heap allocations one tick costs, per scenario.
///
/// The bot is built and warmed *outside* the counted window, so what is counted
/// is the steady-state tick and not the admission cost. The baseline is counted
/// through the same window, which is the control: a hand-rolled loop over the
/// same workload should allocate nothing, and a non-zero count there would mean
/// the counter is picking up something other than the bot.
///
/// The disk I/O the journal now does is *inside* the counted window, on the
/// tick that fires an effect, because a tick that dispatches cannot return
/// before the frame it is recording has been written and synced. The counters
/// are unaffected by that in the way a reader might expect: the file's own
/// buffer is heap memory, so a steady tick on a quiet source still shows the
/// small number it showed under the in-memory journal.
fn measure_allocations(
    scenario: &Scenario,
    warm: u64,
    ticks: u64,
    scratch: &Path,
    bot_number: usize,
) -> Result<AllocRow, Box<dyn std::error::Error>> {
    let clock = Clock::new();
    let tally = Rc::new(Cell::new(0u64));
    let evals_bot_cell = Rc::new(Cell::new(0u64));
    let evals_base_cell = Cell::new(0u64);

    let mut bot = build_bot(
        scenario,
        &clock,
        &tally,
        &evals_bot_cell,
        scratch,
        bot_number,
    )?;
    let mut chains = build_handrolled(scenario);

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

    ALLOCS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    COUNTING.store(1, Ordering::Relaxed);
    for tick in warm..warm.saturating_add(ticks) {
        clock.set(tick);
        let _ = bot.tick()?;
    }
    COUNTING.store(0, Ordering::Relaxed);
    let bot_allocs = ALLOCS.load(Ordering::Relaxed);
    let bot_bytes = BYTES.load(Ordering::Relaxed);

    ALLOCS.store(0, Ordering::Relaxed);
    COUNTING.store(1, Ordering::Relaxed);
    for tick in warm..warm.saturating_add(ticks) {
        let _ = chains
            .iter_mut()
            .map(|c| c.tick(tick, &evals_base_cell))
            .sum::<u64>();
    }
    COUNTING.store(0, Ordering::Relaxed);
    let base_allocs = ALLOCS.load(Ordering::Relaxed);
    Ok(AllocRow {
        name: scenario.name,
        allocs_per_tick: bot_allocs as f64 / ticks as f64,
        bytes_per_tick: bot_bytes as f64 / ticks as f64,
        mean_bytes: bot_bytes as f64 / bot_allocs.max(1) as f64,
        base_allocs_per_tick: base_allocs as f64 / ticks as f64,
    })
}

/// One scenario's allocation row, every figure per tick.
struct AllocRow {
    /// The scenario's name, as the table's first column.
    name: &'static str,
    /// Allocations per counted tick for the bot.
    allocs_per_tick: f64,
    /// Bytes allocated per counted tick for the bot.
    bytes_per_tick: f64,
    /// Mean bytes per allocation; `0.0` when nothing allocated at all.
    mean_bytes: f64,
    /// Allocations per counted tick for the hand-rolled baseline.
    base_allocs_per_tick: f64,
}

/// Build the allocation table, against `scratch` and under this run's pid.
///
/// # Errors
///
/// Any [`BotError`] an admitted bot reports, any [`std::io::Error`] the scratch
/// directory reports, and the failure of the underlying write to the report.
fn alloc_report(scratch: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let mut out = String::new();
    writeln!(
        out,
        "\nallocation model -- steady-state tick, built and warmed outside the counted window\n\
         =================================================================================="
    )?;
    writeln!(
        out,
        "  scenario                allocs/tick   bytes/tick   mean bytes   baseline allocs/tick   row cost"
    )?;

    // Warm up, so anything allocated once by the schedule's first run (a
    // lazily-grown buffer, a query's internal state) is already in place.
    let warm = 64u64;
    let ticks = 512u64;
    for (index, scenario) in SCENARIOS.iter().enumerate() {
        let row_start = Instant::now();
        let row = measure_allocations(scenario, warm, ticks, scratch, index)?;
        // The table reports allocs and bytes, neither of which says what the
        // journal's disk I/O costs, and that is now the dominant term in a
        // moving tick. The per-tick wall clock beside each row says what the
        // measured window cost end to end, so a reader who takes these rows as
        // a statement about the schedule can see how much of it was the disk.
        //
        // Not a free-standing benchmark: the counter is live for the bot's half
        // of the window and for the baseline's, and off for the warm-up and
        // the `Vec`s between them, and the counted path also pays the counter.
        // It bounds the cost of the row rather than measuring the tick alone.
        let row_per_tick_ms = (row_start.elapsed().as_secs_f64() / ticks as f64) * 1e3;
        writeln!(
            out,
            "  {:<20} {:>10.1}   {:>10.1}   {:>10.1}   {:>19.1}   {:>9.1} ms/tick",
            row.name,
            row.allocs_per_tick,
            row.bytes_per_tick,
            row.mean_bytes,
            row.base_allocs_per_tick,
            row_per_tick_ms,
        )?;
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
    let trace_bot_number = SCENARIOS.len();
    let mut bot = build_bot(scenario, &clock, &tally, &evals, scratch, trace_bot_number)?;
    for tick in 0..64u64 {
        clock.set(tick);
        let _ = bot.tick()?;
    }
    // Timed from here rather than from the top of this block, because the
    // divisor below counts sixty traced ticks and the sixty warm-up ticks above
    // are a different sixty. Both windows are on a quiet source, so they differ
    // by a journal open and a buffer's first fill.
    let trace_start = Instant::now();
    write!(
        out,
        "\n  per-tick allocations, {} (a value moves every {} ticks)\n    ",
        scenario.name, scenario.period
    )?;
    for tick in 64..124u64 {
        clock.set(tick);
        ALLOCS.store(0, Ordering::Relaxed);
        COUNTING.store(1, Ordering::Relaxed);
        let _ = bot.tick()?;
        COUNTING.store(0, Ordering::Relaxed);
        write!(out, "{}:{} ", tick, ALLOCS.load(Ordering::Relaxed))?;
    }
    // The trace above shows where the allocations are and not what they cost,
    // and under a file-backed journal those are no longer the same question:
    // the allocation count is a steady few hundred either way, while the wall
    // clock on a tick that fired is the `sync` the durability promise is made
    // of. Sixty traced ticks, whatever that costs, said rather than estimated.
    //
    // This window is quiet by construction -- `steady-64x100` moves its value
    // once every hundred ticks -- so the figure is the cost of a tick that
    // decides nothing, and it is the floor rather than the headline. A tick that
    // dispatches is in the `row cost` column above.
    let trace_ticks = 60.0;
    let trace_per_tick_ms = (trace_start.elapsed().as_secs_f64() / trace_ticks) * 1e3;
    writeln!(
        out,
        "\n  those 60 quiet ticks took {:.1} ms/tick end to end, counting on",
        trace_per_tick_ms
    )?;
    writeln!(out, "\n")?;
    Ok(out)
}

// ── Reporting ────────────────────────────────────────────────────────────────

/// Refuses to report a timing between two engines that did different work.
///
/// THE FAIRNESS GATE, on both axes. A ratio between two engines doing different
/// amounts of work is not a measurement. Effects catch a baseline that produces
/// different results; evaluations catch a baseline that reaches the same results
/// by skipping the work.
///
/// A helper rather than two checks in the report loop: this is the condition
/// under which every number in this rig is meaningless, so it is one predicate
/// with one name rather than two comparisons a reader has to notice are paired.
fn check_fairness(m: &Measurement) -> Result<(), Box<dyn std::error::Error>> {
    lgwks_std::trace::warn!(
        operation = "check_fairness",
        "operation refused its request; the typed error carries the facts"
    );
    if m.effects_bot != m.effects_base {
        return Err(format!(
            "FAIRNESS CHECK FAILED for '{}': bot fired {} effects, baseline fired {}. \
             The two engines are not doing the same work, so no timing from this \
             scenario is reportable.",
            m.name, m.effects_bot, m.effects_base
        )
        .into());
    }
    if m.evals_bot != m.evals_base {
        return Err(format!(
            "FAIRNESS CHECK FAILED for '{}': bot evaluated the condition {} times, \
             baseline {} times. The effect counts agree, so this is a baseline that \
             reaches the same answer with less work -- which is exactly the comparison \
             this gate exists to refuse.",
            m.name, m.evals_bot, m.evals_base
        )
        .into());
    }
    Ok(())
}

/// Measures one scenario, checks it was fair, and writes both report forms.
///
/// The fairness gate runs before anything is written, so a scenario that did not
/// do the same work in both engines never reaches either report. That is the
/// whole reason this is one function: splitting the measure from the reporting
/// would put a `writeln!` between the check and the numbers it protects.
fn report_scenario(
    scenario: &Scenario,
    index: usize,
    scratch: &std::path::Path,
    human: &mut String,
    json: &mut String,
) -> Result<(), Box<dyn std::error::Error>> {
    lgwks_std::trace::warn!(
        operation = "report_scenario",
        "operation refused its request; the typed error carries the facts"
    );
    let m = measure(scenario, scratch, index)?;
    check_fairness(&m)?;
    let m = measure(scenario, scratch, index)?;

    check_fairness(&m)?;

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
                 \"build_ms\": {:.4} }},",
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
        m.build_bot.as_secs_f64() * 1e3
    )?;

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    lgwks_std::trace::warn!(
        operation = "main",
        "operation refused its request; the typed error carries the facts"
    );
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run_timings = !args.iter().any(|a| a == "--capcheck-only");

    // One scratch directory for the whole process, and every bot this run
    // admits writes its journal inside it under a number of its own. Both modes
    // need it, so it is made before either branch rather than inside one.
    let scratch = open_scratch_dir()?;

    if args.iter().any(|a| a == "--alloc-report") {
        print!("{}", alloc_report(&scratch)?);
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
        for (index, scenario) in SCENARIOS.iter().enumerate() {
            report_scenario(scenario, index, &scratch, &mut human, &mut json)?;
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
    let first_caps = scaling.first().map(|(c, _)| *c as f64).unwrap_or(1.0);
    let last_caps = scaling.last().map(|(c, _)| *c as f64).unwrap_or(1.0);
    let first_ns = scaling.first().map(|(_, ns)| *ns).unwrap_or(f64::NAN);
    let last_ns = scaling.last().map(|(_, ns)| *ns).unwrap_or(f64::NAN);
    let growth = last_caps / first_caps;
    writeln!(
        human,
        "  growth    {:.1}x cost for {:.0}x capabilities (quadratic would be {:.0}x)\n",
        last_ns / first_ns,
        growth,
        growth * growth
    )?;

    // Build cost. The two admission measurements and the two replay arms follow,
    // so their journal numbers are counted from here on.
    let build_cost_first = SCENARIOS.len();
    let build_1 = build_cost(1, &scratch, build_cost_first)?;
    let build_64 = build_cost(64, &scratch, build_cost_first + 1)?;
    writeln!(
        human,
        "admission (build) cost\n  1 source: {:.3} ms\n  64 sources: {:.3} ms\n",
        build_1.as_secs_f64() * 1e3,
        build_64.as_secs_f64() * 1e3
    )?;

    // Determinism.
    let (deterministic, fired) =
        determinism_probe(&SCENARIOS[1], 500, &scratch, build_cost_first + 2)?;
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

    print!("{human}");

    if let Some(path) = args.iter().find(|a| a.starts_with("--json=")) {
        let path = path.trim_start_matches("--json=");
        std::fs::write(path, &json)?;
        println!("wrote {path}");
    }

    Ok(())
}
