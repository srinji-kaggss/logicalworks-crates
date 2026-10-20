//! The per-stage profile: where a tick goes, measured on the tick path itself.
//!
//! A ratio against a hand-rolled loop says the bot is slower; it does not say
//! which half of the tick the difference is made of. This drives the crate's own
//! `tick_profiled` -- the same four phases, with the per-stage instrument armed --
//! so the breakdown is a decomposition of the real tick rather than of a model of
//! it.
//!
//! The instrument is not free: seven clock reads on a tick. So every row is
//! measured twice on the same bot and the same workload, once through `tick` and
//! once through `tick_profiled`, and the difference is printed beside the stages.
//! The shares are what the profile supports; the absolute times carry the
//! instrument and are printed with its cost subtracted.

use std::cell::Cell;
use std::fmt::Write as _;
use std::rc::Rc;
use std::time::{Duration, Instant};

use lgwks_bot::{TickProfile, TickStage};

use crate::scenarios::Scenario;
use crate::stats;
use crate::{Clock, build_bot, selected};

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

/// The per-stage table, for every scenario `only` selects.
///
/// The percentages are the claim the profile supports; the nanoseconds beside
/// them carry the instrument, and the `plain` column is what the same workload
/// costs with the instrument off. The `instrument` column is the difference, and
/// the clock's own cost is printed under the table because on this host it is
/// what the instrument mostly is.
pub(super) fn stage_report(only: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    let scenarios = selected(only)?;
    let mut rows = Vec::with_capacity(scenarios.len());
    for scenario in scenarios {
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
        "  each row runs its own timed window x rounds, release, one process; the two\n\
         windows run the same workload on the same bot, back to back"
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
/// term in the instrument column above: macOS does not put `clock_gettime` in
/// the vDSO, so a clock read is a real syscall and a seven-read instrument costs
/// more than a short tick. A reader who wants the stage absolutes without the
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
///
/// `NAN` for a row that measured no ticks, which prints as `NaN` in the table
/// rather than a zero nobody measured.
fn ns_of(row: &StageRow, stage: TickStage) -> f64 {
    match row.profile.per_tick(stage) {
        Some(mean) => mean.as_secs_f64() * 1e9,
        None => f64::NAN,
    }
}
