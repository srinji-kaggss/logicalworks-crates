//! Overload and recovery: a baseline phase, twice-the-knee overload, then back
//! down, measuring the drain and the return to the baseline p99.
//!
//! Moved from `main.rs` for #639: the behaviour, printed lines, numbers and
//! bounds are unchanged.

use super::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::driver::{Arrival, DepthTrace, count_arrival, intended_at, sample_queue, wait_until};
use crate::engine::{Admission, BaselineEngine, EngineBox, FacadeEngine, Side};
use crate::gate::work_units_match;
use crate::openloop::{Recorder, RecorderSet, period_nanos};
use crate::support::print_row;

/// The length of an overload run's recovery phase: what the caller asked for, clamped.
fn recovery_window_of(shape: OverloadShape) -> Duration {
    shape.recovery_seconds.min(RECOVERY_LIMIT)
}

// ── Overload and recovery ────────────────────────────────────────────────────

/// Which phase of the overload run is offering load.
///
/// An enum rather than a name compared at each use: a phase whose identity is a
/// string is a phase whose identity a typo can change, and the recovery reading
/// depends on telling the recovery phase from the other two.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PhaseLabel {
    /// Steady load below the knee: the p99 the run must come back to.
    Baseline,
    /// Twice the knee: the load that builds the queue.
    Overload,
    /// Back down below the knee: the drain and the return are measured here.
    Recovery,
}

impl PhaseLabel {
    /// The name the record carries.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Overload => "overload",
            Self::Recovery => "recovery",
        }
    }
}

/// One phase of the overload run: an offered rate and how long it holds.
#[derive(Clone, Copy)]
pub(crate) struct Phase {
    /// What this phase is.
    pub(crate) label: PhaseLabel,
    pub(crate) offered_rate: u64,
    pub(crate) window: Duration,
}

/// What one overload run measured.
#[derive(Debug)]
pub(crate) struct RecoveryRun {
    pub(crate) side: Side,
    pub(crate) bound: usize,
    /// The p99 the side served at 0.5× the knee, before any overload.
    pub(crate) baseline_p99_nanos: u64,
    /// The offered rate the overload ran at.
    pub(crate) overload_rate: u64,
    /// Deepest queue the overload reached, in arrivals.
    pub(crate) overload_peak_queue: usize,
    /// Wall time from the overload's last arrival until nothing was in flight.
    pub(crate) drain: Duration,
    /// Wall time from the overload's last arrival until a whole window's p99 was back
    /// inside the baseline's.
    pub(crate) recovered: Duration,
    /// The p99 of the first window that was back inside the baseline.
    pub(crate) recovered_p99_nanos: u64,
    /// The conservation counters of the whole run.
    pub(crate) offered: u64,
    pub(crate) admitted: u64,
    pub(crate) refused: u64,
    pub(crate) completed: u64,
    pub(crate) work_units: u64,
}

/// How long one recovery window offers load before its p99 is read.
///
/// A window has to be long enough to hold enough samples for a p99 to mean anything
/// and short enough that "back to baseline" is a time a reader can act on. 500 ms at
/// the recovery rate is several thousand samples on this host and is bounded above by
/// the run itself rather than by a timer.
const RECOVERY_WINDOW: Duration = Duration::from_millis(500);

/// The fewest samples a recovery window must hold before its p99 is read.
///
/// Below this a window's p99 is the maximum of a handful of samples, and a run that
/// reported it as a recovery would be reporting noise as a measurement.
const RECOVERY_MIN_SAMPLES: u64 = 64;

/// How long the recovery phase may run before the run reports it did not recover.
///
/// The overload left a queue behind; draining it is bounded work, and an unbounded
/// wait for a p99 that never returns would turn a slow host into a hung rig. It is
/// applied as a **clamp on the caller's recovery window** rather than as a timer of its
/// own, because a second timer beside the phase's own window would be a second place for
/// the two to disagree about when the phase ends.
const RECOVERY_LIMIT: Duration = Duration::from_secs(120);

/// The shape of one overload-and-recovery run, shared by both of its functions.
///
/// One struct rather than six parameters repeated across two signatures: the two
/// functions that take it differ only in whether they drive one side or both, and a
/// six-name list duplicated in two signatures is two places for a phase window to drift.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OverloadShape {
    /// The in-flight ceiling, which is a parameter because the recovery time is a property
    /// of the ceiling a reader chose and not of the runtime.
    pub(crate) bound: usize,
    /// The rate the run treats as the knee: the recovery phase offers half of it and the
    /// overload phase twice it.
    pub(crate) knee_rate: u64,
    /// The service cost each body runs, resolved once by `body_for_bound`.
    pub(crate) body_micros: u64,
    /// How long the baseline phase offers 0.5x the knee.
    pub(crate) baseline_window: Duration,
    /// How long the overload phase offers 2x the knee.
    pub(crate) overload_window: Duration,
    /// How long the recovery phase offers 0.5x the knee again. Its own length, because the
    /// two phases measure different things: the baseline only has to hold enough samples
    /// for one p99, while the recovery has to outlast the queue the overload built. A 30 s
    /// overload at 2x the knee, dropped to 0.5x, drains its backlog at (capacity - 0.5x
    /// knee) a second — about 51 s at bound 64 — so a recovery window shorter than that
    /// reports a recovery that had not happened yet.
    pub(crate) recovery_seconds: Duration,
}

/// Drive one side through a baseline phase, an overload phase and a recovery phase,
/// and report what it took to come back.
///
/// The phases are the issue's: a steady baseline at 0.5× the knee, then 2× the knee
/// long enough to build a queue, then back down to 0.5×. Two readings come out: how
/// long the drain took, and how long until a whole window's p99 was back inside the
/// baseline's. The conservation counters come from the same gate as every other run,
/// so "nothing was lost or duplicated while it recovered" is checked rather than
/// assumed.
async fn overload_run(
    side: Side,
    shape: OverloadShape,
) -> Result<RecoveryRun, Box<dyn std::error::Error>> {
    let OverloadShape {
        bound,
        knee_rate,
        body_micros,
        baseline_window,
        overload_window,
        recovery_seconds: _,
    } = shape;
    let half = (knee_rate / 2).max(1);
    let twice = knee_rate.saturating_mul(2);
    let recovery_window = recovery_window_of(shape);
    let phases = [
        Phase {
            label: PhaseLabel::Baseline,
            offered_rate: half,
            window: baseline_window,
        },
        Phase {
            label: PhaseLabel::Overload,
            offered_rate: twice,
            window: overload_window,
        },
        Phase {
            label: PhaseLabel::Recovery,
            offered_rate: half,
            window: recovery_window,
        },
    ];

    info!(
        "\n{} at bound {bound}: baseline {} s at {half}/s, overload {} s at {twice}/s, \
         recovery {} s at {half}/s",
        side.as_str(),
        baseline_window.as_secs(),
        overload_window.as_secs(),
        recovery_window.as_secs()
    );

    let mut engine = match side {
        Side::Facade => EngineBox::Facade(FacadeEngine::new(bound, Admission::Backpressure)),
        Side::Baseline => EngineBox::Baseline(BaselineEngine::new(bound, Admission::Backpressure)),
    };
    let work = Arc::new(AtomicU64::new(0));
    let recorders = RecorderSet::new();
    let mut depth = DepthTrace::new();
    let mut offered = 0_u64;
    let mut admitted = 0_u64;
    let mut refused = 0_u64;
    let mut baseline_p99 = 0_u64;
    // The instant the overload stopped offering: every recovery reading is measured
    // from here, so "back to baseline" includes the drain rather than starting after
    // it.
    let mut overload_stopped = Instant::now();
    let mut overload_peak_queue = 0_usize;
    let mut drain = Duration::ZERO;
    let mut recovered = Duration::ZERO;
    let mut recovered_p99 = 0_u64;

    for phase in phases {
        let period = period_nanos(phase.offered_rate);
        let t0 = Instant::now();
        // **The arrival index restarts at every phase**, while the tallies do not.
        //
        // The schedule is `phase_t0 + index x period` and `phase_t0` is this phase's
        // start, so a *cumulative* index would place every arrival in a later phase that
        // far into its future: after a 30 s overload at 20,000/s the cumulative index was
        // 357,726, and the recovery phase — which offers at 5,000/s, so one arrival every
        // 200 us — scheduled its first arrival 71.5 s ahead. The phase offered exactly one
        // arrival in its five seconds, its window never reached `RECOVERY_MIN_SAMPLES`,
        // and the run reported "the p99 did not return to the baseline". The engine had
        // recovered; the rig had stopped offering.
        let mut phase_offered = 0_u64;
        while t0.elapsed() < phase.window {
            engine.reap();
            let intended = intended_at(t0, phase_offered, period);
            wait_until(intended).await;
            phase_offered = phase_offered.saturating_add(1);
            offered = offered.saturating_add(1);
            let arrival = Arrival::mint(&work, &recorders, intended, body_micros);
            let placed = engine.admit(arrival).await;
            count_arrival(placed, &mut admitted, &mut refused);
            let next_intended = intended_at(t0, phase_offered, period);
            sample_queue(
                Instant::now(),
                next_intended,
                period,
                engine.in_flight(),
                &mut depth,
            );
            let queue = depth.peak;
            match phase.label {
                PhaseLabel::Overload => {
                    overload_peak_queue = overload_peak_queue.max(queue);
                }
                // The baseline phase has nothing to watch for: it is the reading the
                // other two are compared against.
                PhaseLabel::Baseline => {}
                // The recovery phase watches for the one fact it is measuring: a whole
                // window has served a p99 inside the baseline's. The window is bounded by
                // the phase's own end, which `RECOVERY_LIMIT` clamps, so a host that never
                // recovers reports that rather than hanging.
                //
                // **It does not wait for an empty engine.** That condition was here and it
                // is unsatisfiable by construction: the phase keeps offering load at
                // `0.5x` the knee, so the engine is never idle and the check could never
                // fire. The drain is measured separately and timed, immediately after the
                // overload stops, and *including* the drain in this reading is the whole
                // point of measuring recovery from `overload_stopped` — so requiring the
                // engine to be empty as well made the one reading that should include the
                // drain the one reading that excluded it.
                PhaseLabel::Recovery => {
                    if recovered_p99 == 0 && t0.elapsed() >= RECOVERY_WINDOW {
                        let window = Recorder::merged(recorders.shards());
                        // A window below `RECOVERY_MIN_SAMPLES` has a p99 that is the
                        // maximum of a handful of samples; reading it as "recovered"
                        // would report noise as a measurement, so the window is
                        // discarded and the next one is read instead.
                        let window_p99 = if window.count() >= RECOVERY_MIN_SAMPLES {
                            window.quantile_nanos(0.99)
                        } else {
                            0
                        };
                        if window_p99 != 0 && window_p99 <= baseline_p99 {
                            recovered_p99 = window_p99;
                            recovered = Instant::now().saturating_duration_since(overload_stopped);
                        }
                        recorders.reset();
                    }
                }
            }
        }

        let histogram = Recorder::merged(recorders.shards());
        let p99 = histogram.quantile_nanos(0.99);
        match phase.label {
            PhaseLabel::Baseline => {
                baseline_p99 = p99;
                info!(
                    "  baseline  {} offered  {} admitted  p99 {:.3} ms",
                    phase_offered,
                    admitted,
                    p99 as f64 / 1_000_000.0
                );
            }
            PhaseLabel::Overload => {
                info!(
                    "  overload  {} offered  {} admitted  {} refused  p99 {:.3} ms  peak \
                     queue {overload_peak_queue}",
                    phase_offered,
                    admitted,
                    refused,
                    p99 as f64 / 1_000_000.0
                );
                // Drain from here, timed: the overload's queue is what has to go.
                overload_stopped = Instant::now();
                let drain_started = overload_stopped;
                engine.drain().await;
                drain = drain_started.elapsed();
            }
            PhaseLabel::Recovery => {
                if recovered_p99 == 0 {
                    info!(
                        "  recovery  drained in {:.3} ms  p99 did NOT return to the baseline \
                         {:.3} ms within the {} s phase — reported as not recovered rather \
                         than as a number",
                        drain.as_secs_f64() * 1_000.0,
                        baseline_p99 as f64 / 1_000_000.0,
                        phase.window.as_secs()
                    );
                } else {
                    info!(
                        "  recovery  drained in {:.3} ms  p99 back to baseline after {:.3} ms \
                         ({} ms inside)",
                        drain.as_secs_f64() * 1_000.0,
                        recovered.as_secs_f64() * 1_000.0,
                        recovered_p99 as f64 / 1_000_000.0
                    );
                }
            }
        }
        if phase.label != PhaseLabel::Recovery {
            recorders.reset();
        }
    }

    engine.drain().await;
    let (completed, cancelled, aborted) = engine.terminals();
    if cancelled != 0 || aborted != 0 {
        let refusal = Err(format!(
            "{} overload run ended with {cancelled} cancelled and {aborted} aborted tasks: a \
             recovery that drops work is not a recovery",
            side.as_str()
        )
        .into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "overload_run: returning an error to the caller");
        return refusal;
    }
    let work_units = work.load(Ordering::SeqCst);
    let run = RecoveryRun {
        side,
        bound,
        baseline_p99_nanos: baseline_p99,
        overload_rate: twice,
        overload_peak_queue,
        drain,
        recovered,
        recovered_p99_nanos: recovered_p99,
        offered,
        admitted,
        refused,
        completed,
        work_units,
    };
    // The same conservation gate as every other run, on the same counters.
    let conserved = RecoveryRun::conserved(&run);
    conserved.map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;
    Ok(run)
}

impl RecoveryRun {
    /// Every arrival was admitted or refused, every admitted body completed, and the
    /// bodies' own work counter agrees — the property "nothing was lost or duplicated
    /// while it recovered" is a check on, not a claim about, the run.
    fn conserved(run: &RecoveryRun) -> Result<(), String> {
        let accounted = run.admitted.saturating_add(run.refused);
        if accounted != run.offered {
            let refusal = Err(format!(
                "{}: offered {}, admitted {} plus refused {} is {accounted} — an arrival was \
                 lost or counted twice",
                run.side.as_str(),
                run.offered,
                run.admitted,
                run.refused
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "conserved: returning an error to the caller");
            return refusal;
        }
        if run.completed != run.admitted {
            let refusal = Err(format!(
                "{}: {} of {} admitted bodies completed — work was lost or duplicated",
                run.side.as_str(),
                run.completed,
                run.admitted
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "conserved: returning an error to the caller");
            return refusal;
        }
        work_units_match(
            "overload and recovery",
            run.completed,
            run.work_units,
            "bodies",
        )
    }
}

/// Drive the overload-and-recovery run on both sides at one bound.
pub(crate) async fn overload_and_recovery(
    shape: OverloadShape,
    json: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let OverloadShape {
        bound,
        knee_rate,
        body_micros: _,
        baseline_window,
        overload_window,
        recovery_seconds: _,
    } = shape;
    info!(
        "overload and recovery — {baseline_window:?} baseline, {overload_window:?} at 2x the knee, \
         then {:?} back down",
        recovery_window_of(shape)
    );
    info!("drain time and time-to-baseline-p99 are both measured, and the conservation gate");
    info!("checks that nothing was lost or duplicated on either side of the transition\n");

    let mut rows = Vec::new();
    for side in Side::ALL {
        let run = overload_run(side, shape).await?;
        print_row(&[
            side.as_str().to_string(),
            bound.to_string(),
            format!("{:.3} ms", run.baseline_p99_nanos as f64 / 1_000_000.0),
            run.overload_rate.to_string(),
            run.overload_peak_queue.to_string(),
            format!("{:.1} ms", run.drain.as_secs_f64() * 1_000.0),
            format!("{:.1} ms", run.recovered.as_secs_f64() * 1_000.0),
            run.offered.to_string(),
            run.admitted.to_string(),
            run.completed.to_string(),
        ]);
        rows.push(run);
    }

    if let Some(path) = json {
        let body = format!(
            "{{\"tool\":\"lgwks-bench-async-overload\",\"bound\":{bound},\
              \"knee_offered_rate\":{knee_rate},\"baseline_window_ms\":{},\
              \"overload_window_ms\":{},\"recovery_window_ms\":{},\
              \"runs\":[\n{}\n]}}",
            baseline_window.as_millis(),
            overload_window.as_millis(),
            recovery_window_of(shape).as_millis(),
            rows.iter()
                .map(recovery_json)
                .collect::<Vec<_>>()
                .join(",\n")
        );
        std::fs::write(path, body)?;
        info!("wrote {path}");
    }
    Ok(())
}

/// One overload run as a results-file row.
fn recovery_json(run: &RecoveryRun) -> String {
    format!(
        "{{\"side\":\"{}\",\"bound\":{},\"baseline_p99_nanos\":{},\"overload_offered_rate\":{},\
          \"overload_peak_queue\":{},\"drain_millis\":{},\"recovered_millis\":{},\"phase\":\"{}\",\
          \"recovered_p99_nanos\":{},\"offered\":{},\"admitted\":{},\"refused\":{},\
          \"completed\":{},\"work_units\":{}}}",
        run.side.as_str(),
        run.bound,
        run.baseline_p99_nanos,
        run.overload_rate,
        run.overload_peak_queue,
        run.drain.as_millis(),
        run.recovered.as_millis(),
        PhaseLabel::Recovery.as_str(),
        run.recovered_p99_nanos,
        run.offered,
        run.admitted,
        run.refused,
        run.completed,
        run.work_units
    )
}
