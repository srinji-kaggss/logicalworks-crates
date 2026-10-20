//! The fairness gate and the open-loop conservation gate: the tally comparison
//! both sides must pass, and the checks that stand in for it once the two sides
//! legitimately diverge past the knee.
//!
//! Moved from `main.rs` for #639: the behaviour, printed lines, numbers and
//! bounds are unchanged.

use super::*;

use std::time::Duration;

use crate::driver::DepthTrace;
use crate::engine::{Admission, Side};
use crate::openloop::Histogram;
// ── The fairness gate ────────────────────────────────────────────────────────

/// Whether two tallies describe the same work, with a reason when they do not.
///
/// A `bool` would let a caller print a ratio on unequal work; a `Result` with a
/// message makes the refusal carry the exact field that diverged, which is what a
/// reader needs to know whether the run measured anything.
pub(crate) fn fair(left: &Tally, right: &Tally) -> Result<(), String> {
    if left.placed != right.placed {
        let refusal = Err(format!(
            "placed: facade {} vs baseline {}",
            left.placed, right.placed
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fair: returning an error to the caller");
        return refusal;
    }
    if left.completed != right.completed {
        let refusal = Err(format!(
            "completed: facade {} vs baseline {}",
            left.completed, right.completed
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fair: returning an error to the caller");
        return refusal;
    }
    if left.cancelled != right.cancelled {
        let refusal = Err(format!(
            "cancelled: facade {} vs baseline {}",
            left.cancelled, right.cancelled
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fair: returning an error to the caller");
        return refusal;
    }
    if left.aborted != right.aborted {
        let refusal = Err(format!(
            "aborted: facade {} vs baseline {}",
            left.aborted, right.aborted
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fair: returning an error to the caller");
        return refusal;
    }
    if left.refused != right.refused {
        let refusal = Err(format!(
            "refused: facade {} vs baseline {}",
            left.refused, right.refused
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fair: returning an error to the caller");
        return refusal;
    }
    if left.work_units != right.work_units {
        let refusal = Err(format!(
            "work units: facade {} vs baseline {}",
            left.work_units, right.work_units
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "fair: returning an error to the caller");
        return refusal;
    }
    Ok(())
}

/// The floor of the p99 budget a knee is declared against.
///
/// A declared number rather than a discovered one, because "the knee" is only meaningful
/// against a service level: the same curve read against a 10 ms budget and a 1 s budget has
/// two different knees. 50 ms is the estate's standing budget for a request-shaped
/// interaction, and it is named on every table that uses it, so a reader can re-declare the
/// knee against another budget from the same rows.
///
/// It is a **floor**, not the whole rule: [`SideRun::p99_budget_nanos`] also allows one
/// further body's worth of queueing, because the sweep scales the body cost per bound and a
/// 50 ms ceiling would otherwise be smaller than one service time at the wide bounds.
pub(crate) const SLO_P99_NANOS: u64 = 50_000_000;

/// The same floor in whole milliseconds, which is the unit every table prints it in.
///
/// Derived from [`SLO_P99_NANOS`] rather than typed a second time: the table read
/// `50000000 ms` for a 50 ms budget when a message interpolated the nanosecond
/// constant beside a `ms` label, and a second literal would be the same defect
/// waiting for the next edit.
pub(crate) const SLO_P99_MILLIS: u64 = SLO_P99_NANOS / 1_000_000;

/// Extra service times of queueing the p99 budget allows on top of the floor.
///
/// One, and it is the standard queueing reading: below the knee an arrival waits less than
/// one body's time, above it the wait grows without bound. Stated as a multiple rather than
/// folded into the floor because it has to be re-readable at a different body cost — the
/// whole point of scaling the body per bound is that a budget in milliseconds alone cannot
/// mean the same thing at a 3.2 ms body and a 6.55 s one.
pub(crate) const BUDGET_EXTRA_BODIES: u64 = 1;

/// What one side did under one offered rate.
#[derive(Debug)]
pub(crate) struct SideRun {
    pub(crate) side: Side,
    pub(crate) bound: usize,
    pub(crate) offered_rate: u64,
    /// This run's declared service cost per body, in microseconds.
    ///
    /// Carried on the run rather than passed to the reporter because the capacity a
    /// bound's knee is read against is `bound / body_micros`: a record that printed
    /// p99s without the body cost would leave a reader unable to tell which curve the
    /// latency belonged to.
    pub(crate) body_micros: u64,
    pub(crate) admission: Admission,
    /// Arrivals the generator offered.
    pub(crate) offered: u64,
    /// Arrivals admitted to a slot.
    pub(crate) admitted: u64,
    /// Arrivals the ceiling refused.
    pub(crate) refused: u64,
    /// Arrivals whose body ran to completion.
    pub(crate) completed: u64,
    /// Arrivals that ended by observing cancellation.
    pub(crate) cancelled: u64,
    /// Arrivals the runtime dropped.
    pub(crate) aborted: u64,
    /// Work units the bodies recorded.
    pub(crate) work_units: u64,
    /// Tasks still in flight after the drain.
    pub(crate) in_flight_at_end: u64,
    /// The queue-depth trace; its peak is the peak queue.
    pub(crate) depth: DepthTrace,
    /// The generator's worst lag behind its own schedule.
    pub(crate) max_lag: Duration,
    /// Latencies from the intended start.
    pub(crate) histogram: Histogram,
    /// Wall time over which arrivals were offered.
    pub(crate) wall: Duration,
    /// Time spent draining after the window closed.
    pub(crate) drain: Duration,
    /// Resident set size sampled at the run's peak.
    pub(crate) rss_bytes: Option<u64>,
    /// How the RSS figure was read.
    pub(crate) rss_source: &'static str,
}

impl SideRun {
    /// Fraction of offered arrivals the ceiling refused.
    pub(crate) fn refusal_rate(&self) -> f64 {
        if self.offered == 0 {
            return 0.0;
        }
        (self.refused as f64) / (self.offered as f64)
    }

    /// Completions per second over the window and the drain together.
    pub(crate) fn throughput_per_second(&self) -> f64 {
        let seconds = self.wall.as_secs_f64() + self.drain.as_secs_f64();
        if seconds <= 0.0 {
            return 0.0;
        }
        (self.completed as f64) / seconds
    }

    /// The p99 budget this run's knee is read against, in nanoseconds.
    ///
    /// `max(SLO_P99_NANOS, (1 + BUDGET_EXTRA_BODIES) x body)`. Both halves are named on
    /// every run that prints one, because a knee declared against a budget the reader cannot
    /// see is a knee against an unpublished rule.
    pub(crate) fn p99_budget_nanos(&self) -> u64 {
        let by_bodies = self
            .body_micros
            .saturating_mul(1_000)
            .saturating_mul(1 + BUDGET_EXTRA_BODIES);
        SLO_P99_NANOS.max(by_bodies)
    }

    /// Whether this run refused nothing and stayed inside its declared p99 budget.
    ///
    /// One expression for both doors, and the refusal door is the interesting half: its
    /// declared overflow past the knee *is* the answer, so what bounds the knee there is the
    /// rate at which the ceiling first refuses rather than a queueing blowup. A run that
    /// refused anything is out of budget on either door, because the refusal is the last
    /// rung at which the engine was still inside its own declared envelope.
    pub(crate) fn within_slo(&self) -> bool {
        self.refused == 0 && self.histogram.quantile_nanos(0.99) <= self.p99_budget_nanos()
    }
}

/// The one accounting check both open-loop gates share: every completed body recorded
/// exactly its own work unit.
///
/// The same rule at both levels — an open-loop run and an in-flight tier — because it
/// is the same rule. Two copies of it is one place where the two gates can disagree
/// about what "the bodies and the accounting agree" means, and a gate that disagrees
/// with itself is a gate whose verdict depends on which level asked.
pub(crate) fn work_units_match(
    label: &str,
    completed: u64,
    work_units: u64,
    noun: &str,
) -> Result<(), String> {
    if work_units == completed {
        return Ok(());
    }
    Err(format!(
        "{label}: {work_units} work units for {completed} completed {noun} — the bodies and \
         the accounting disagree about how much ran"
    ))
}

/// The conservation gate: what every open-loop run must satisfy, whatever it measured.
///
/// This is the fairness gate's job in a regime where the two sides are *expected* to
/// diverge: past the knee one side admits what the other refuses, and that difference
/// is the result. What may never differ is whether a side did the work it reported —
/// every arrival offered was admitted or refused, every admitted body completed, every
/// completed body recorded its work, and nothing was left in flight. A percentile
/// computed on a run that violates one of these is a number about a different program,
/// so the run aborts and names the field.
pub(crate) fn open_loop_conserved(label: &str, run: &SideRun) -> Result<(), String> {
    let accounted = run.admitted.saturating_add(run.refused);
    if accounted != run.offered {
        let refusal = Err(format!(
            "{label}: offered {}, admitted {} plus refused {} is {accounted} — an arrival was \
             lost or counted twice",
            run.offered, run.admitted, run.refused
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "open_loop_conserved: returning an error to the caller");
        return refusal;
    }
    if run.completed != run.admitted {
        let refusal = Err(format!(
            "{label}: {} of {} admitted bodies completed — work was lost or duplicated",
            run.completed, run.admitted
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "open_loop_conserved: returning an error to the caller");
        return refusal;
    }
    let terminal = run
        .completed
        .saturating_add(run.cancelled)
        .saturating_add(run.aborted);
    if terminal != run.admitted {
        let refusal = Err(format!(
            "{label}: {terminal} of {} admitted arrivals reached a terminal state",
            run.admitted
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "open_loop_conserved: returning an error to the caller");
        return refusal;
    }
    work_units_match(label, run.completed, run.work_units, "bodies")?;
    if run.in_flight_at_end != 0 {
        let refusal = Err(format!(
            "{label}: {} tasks still in flight after the drain",
            run.in_flight_at_end
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "open_loop_conserved: returning an error to the caller");
        return refusal;
    }
    Ok(())
}

/// The cross-side gate for a pair that admitted everything it was offered.
///
/// Below both knees the two sides did the same work, so the closed-loop gate applies in
/// full. Past a knee the sides legitimately differ — one refused what the other
/// admitted — and refusing the run for that would refuse the measurement, so the pair
/// is reported as a saturation point, with each side's own conservation gate already
/// enforced.
///
/// Returns whether the pair was gated as equal work, so a reader can tell a gated pair
/// from a saturation point without re-deriving it.
pub(crate) fn open_loop_pair_fair(
    label: &str,
    facade: &SideRun,
    baseline: &SideRun,
) -> Result<bool, String> {
    if facade.refused != 0 || baseline.refused != 0 {
        return Ok(false);
    }
    let tally = |run: &SideRun| Tally {
        placed: run.admitted,
        completed: run.completed,
        cancelled: run.cancelled,
        aborted: run.aborted,
        refused: run.refused,
        retained: 0,
        dropped_detail: 0,
        work_units: run.work_units,
    };
    fair(&tally(facade), &tally(baseline)).map_err(|reason| format!("{label}: {reason}"))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{SLO_P99_MILLIS, SLO_P99_NANOS};

    /// The millisecond spelling of the floor is the nanosecond spelling, not a
    /// second number: a table printing the nanoseconds beside a `ms` label read
    /// `50000000 ms` for a 50 ms budget.
    #[test]
    fn the_printed_floor_is_the_floor() {
        assert_eq!(SLO_P99_MILLIS, 50, "50 ms is the estate's budget");
        assert_eq!(
            SLO_P99_MILLIS.saturating_mul(1_000_000),
            SLO_P99_NANOS,
            "the millisecond spelling must be the same budget"
        );
    }
}
