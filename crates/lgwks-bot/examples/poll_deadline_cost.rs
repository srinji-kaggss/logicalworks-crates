//! What the per-poll deadline costs a tick, at every fan-out the bot declares.
//!
//! The measurement a claim about the deadline has to be able to answer: a
//! watchdog that is spawned and joined for every source poll is paid for on
//! every tick, including the ordinary one where every source answers on its
//! first poll and the watchdog had nothing to do. This harness measures the
//! ordinary tick, at 1 / 32 / 1,000 and 10,000 always-ready chains, and then
//! the saturated one: one wedged source among 1,000, where the tick must cost
//! about one deadline rather than one deadline per chain.
//!
//! Both are read through `Bot::tick` and `TickReport::watchdogs`, so the
//! number is the number an operator sees and not a figure this harness derived
//! from a private counter of its own.
//!
//! ```text
//! cargo run -p lgwks_bot --features rt,time,sync,ephemeral --release \
//!     --example poll_deadline_cost
//! ```
//!
//! This is a measurement harness, not a pass/fail gate: wall time varies with
//! the machine, the shape of the distribution does not. Every run writes its
//! own scratch directory and never removes one belonging to another.

use std::io::Write;
use std::time::{Duration, Instant};

use lgwks_bot::{Auth, Bot, BotError, Cap, EffectLifetime, Execute, GrantSet, Observe};

#[path = "support/measure.rs"]
mod measure;
#[path = "support/scratch.rs"]
mod scratch;

#[path = "../tests/support/effects.rs"]
mod effects;

use scratch::Scratch;

/// How many ticks each configuration is timed over.
///
/// Two hundred rather than ten: the tail percentiles are the point, and a
/// sample that short cannot produce a p99 that means anything.
const TICKS: usize = 200;

/// The chain counts the ordinary-tick table is drawn at.
///
/// One, the wave width, one above it and far above it. The wave width is the
/// point at which a second wave begins, so a cost that is per *wave* and a
/// cost that is per *chain* are distinguishable from it.
const TIERS: [usize; 4] = [1, 32, 1_000, 10_000];

/// The budget a wedged source is given.
///
/// One hundred milliseconds: long enough that an always-ready source is never
/// the one that is cut short, short enough that the saturated run reports a
/// tick cost a reader can compare against the budget itself.
const WEDGED_BUDGET: Duration = Duration::from_millis(100);

/// One chain's source: a constant value, or a poll that never resolves.
struct Ready {
    /// Whether this poll parks forever instead of reading.
    wedged: bool,
}

impl Observe for Ready {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(Observe::required_caps(self))?;
        if self.wedged {
            std::future::pending::<()>().await;
        }
        Ok(1)
    }

    fn domain_id(&self) -> &str {
        "bench::ready"
    }
}

/// An action that does nothing measurable, so a tick's cost is its observation
/// phase and not the work behind the chains it fired.
struct Quiet;

impl Execute for Quiet {
    type Input = u32;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn effect_lifetime(&self) -> EffectLifetime {
        EffectLifetime::Local
    }

    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(Execute::required_caps(self))?;
        Ok(())
    }

    fn domain_id(&self) -> &str {
        "bench::quiet"
    }
}

/// Write the `chains` sources one bot declares, one declaration per chain.
///
/// `Bot::observe` returns a builder whose type follows its source, so *n*
/// chains is *n* written-out declarations rather than a loop over a dynamic
/// list. A macro is that written-out declaration without `n` copies of it in
/// this file, and the repetition this crate's hook refuses to see in a
/// hand-expanded loop is exactly the repetition it would have to compare
/// against itself.
macro_rules! declare_chains {
    // One chain: `observe` leaves the builder holding that source, and `.on` adds
    // the entry to it.
    ($name:ident, $bot:expr, $budget:expr, $wedged:expr) => {
        $name
            .observe(Ready { wedged: $wedged })
            .on(|observed: &u32| *observed > 0, Quiet)
    };
    // The rest of them: `observe` also closes the chain before it, which is the
    // erasure boundary, so the same type keeps growing one entry per arm. Only
    // the first chain can be the wedged one, which is what puts the wedged
    // source in the first wave beside thirty-one ready siblings.
    ($name:ident, $bot:expr, $budget:expr, $wedged:expr, $count:expr) => {{
        let mut $name = declare_chains!($name, $bot, $budget, $wedged);
        for _ in 1..$count {
            $name = declare_chains!($name, $bot, $budget, false);
        }
        $name
    }};
}

/// Run `bot` for `ticks` ticks, timing each, and summarise.
///
/// One untimed warm-up tick first: it is the tick that commits the baseline every
/// condition is compared against, so every timed tick is the *ordinary* one — a
/// source that answered, nothing moved, nothing fired. Timing the commit that
/// causes the first fire would measure a different phase under a different name.
fn measure(
    bot: &mut Bot,
    ticks: usize,
    label: &str,
) -> Result<measure::Summary, Box<dyn std::error::Error>> {
    let _warm = bot.tick()?;
    let mut samples = Vec::with_capacity(ticks);
    let mut watchdogs = 0_u32;
    for _ in 0..ticks {
        let started = Instant::now();
        let fired = bot.tick()?;
        let elapsed = started.elapsed();
        if fired > 0 {
            let refusal = Err(
                "a measurement tick fired an effect; the cost under test is the \
                    observation phase alone"
                    .into(),
            );
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "measure: returning an error to the caller");
            return refusal;
        }
        if bot.tick_report().stalled_any() {
            let refusal = Err(
                "a measurement tick reported a stall; this table is about the \
                    ordinary tick where every source answers"
                    .into(),
            );
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "measure: returning an error to the caller");
            return refusal;
        }
        watchdogs = watchdogs.saturating_add(bot.tick_report().watchdogs());
        samples.push(elapsed.as_micros());
    }
    let summary = measure::Summary::of(&mut samples);
    // Read through the report rather than off a private counter: the number a
    // caller can see is the number this table claims.
    let mut out = std::io::stdout().lock();
    let _written = writeln!(out, "{}", summary.line(label));
    // Ten thousandths of a watchdog per tick. A zero tick count has no per-tick
    // rate at all, and a rate that does not fit a `u32` is a refusal rather than
    // a rounded number: both are reported as the absence they are.
    let window = u32::try_from(ticks).map_err(|error| {
        format!("a tick count that does not fit this host's address space: {error}")
    })?;
    let per_tick = watchdogs
        .saturating_mul(10_000)
        .checked_div(window)
        .ok_or("a run of zero ticks has no per-tick rate")?;
    let _written = writeln!(
        out,
        "  watchdogs: {watchdogs} over {ticks} ticks, ten thousandths per tick: {per_tick}"
    );
    Ok(summary)
}

/// Report what one tier of always-ready chains costs.
fn ready_table(scratch: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut out = std::io::stdout().lock();
    let _written = writeln!(out, "always-ready sources, {TICKS} ticks per tier");
    for (index, chains) in TIERS.iter().enumerate() {
        // The bot name carries the tier, so two runs of this harness leave two
        // distinguishable bots behind in whatever reads them.
        let builder = Bot::builder(format!("bench-ready-{index}-{chains}"))
            .with_poll_deadline(Duration::from_secs(30));
        let builder = declare_chains!(builder, (), (), false, *chains);
        let mut bot = builder
            .with_effects(effects::memory_scope()?)
            .build(&GrantSet::empty())?;
        let _measured = measure(&mut bot, TICKS, &format!("{chains:>6} chains"))?;
        // The bot is dropped before the next tier is built, so the measured
        // chains are released rather than carried into the next measurement.
        drop(bot);
    }
    let _written = writeln!(out, "scratch: {}", scratch.display());
    Ok(())
}

/// Report what one wedged source among many costs.
///
/// The claim is that a source which stopped answering costs the tick about one
/// deadline rather than one deadline per chain, so the number to compare is
/// the tick's against [`WEDGED_BUDGET`] itself and against the same tick with
/// nothing wedged.
fn wedged_table(scratch: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut out = std::io::stdout().lock();
    let _written = writeln!(
        out,
        "one wedged source among 1,000, budget {WEDGED_BUDGET:?}"
    );

    // The counterfactual: the same bot with nothing wedged, so the saturated
    // row is a difference against a measured baseline rather than against an
    // assumed one.
    let baseline = Bot::builder("bench-wedged-baseline").with_poll_deadline(WEDGED_BUDGET);
    let baseline = declare_chains!(baseline, (), (), false, 1_000);
    let mut baseline = baseline
        .with_effects(effects::memory_scope()?)
        .build(&GrantSet::empty())?;
    let _summary = measure(&mut baseline, TICKS, "1000 chains, none wedged")?;
    drop(baseline);

    let wedged = Bot::builder("bench-wedged").with_poll_deadline(WEDGED_BUDGET);
    let wedged = declare_chains!(wedged, (), (), true, 1_000);
    let mut wedged = wedged
        .with_effects(effects::memory_scope()?)
        .build(&GrantSet::empty())?;

    let mut samples = Vec::with_capacity(TICKS);
    let mut stalled_seen = 0_usize;
    let mut watchdogs = 0_u32;
    for _ in 0..TICKS {
        let started = Instant::now();
        let _outcome = wedged.tick();
        samples.push(started.elapsed().as_micros());
        let report = wedged.tick_report();
        stalled_seen = stalled_seen.saturating_add(report.stalled().len());
        watchdogs = watchdogs.saturating_add(report.watchdogs());
    }
    let summary = measure::Summary::of(&mut samples);
    let _written = writeln!(out, "{}", summary.line("1000 chains, one wedged"));
    let _written = writeln!(
        out,
        "  stalled reports: {stalled_seen} over {TICKS} ticks; watchdogs: {watchdogs}"
    );
    let _written = writeln!(out, "scratch: {}", scratch.display());
    Ok(())
}

/// Measure the ordinary tick at every tier, then the saturated one.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A per-run directory under the system temp dir, named by random bytes so
    // two runs on one machine never share one, and owned by a guard so a
    // refusal half way through leaves nothing for the next run to inherit.
    let scratch = Scratch::new("poll-cost")?;

    let mut out = std::io::stdout().lock();
    let _written = writeln!(
        out,
        "poll deadline cost; {} ticks per configuration; a tier that is never \
         reached is honest data, not a claim",
        TICKS
    );
    let _written = writeln!(out, "{}", "-".repeat(72));

    ready_table(scratch.path())?;
    let _written = writeln!(out, "{}", "-".repeat(72));
    wedged_table(scratch.path())?;
    Ok(())
}
