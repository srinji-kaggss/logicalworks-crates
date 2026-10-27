//! The saturation sweep: offered rate past the knee at every declared bound, on
//! one admission door, with the knee read from the measured rows.
//!
//! Moved from `main.rs` for #639; the behaviour it exercised is unchanged.

use std::time::Duration;

use crate::widen;
use lgwks_std::trace::info;

use crate::driver::{OpenLoopSpec, open_loop_side};
use crate::engine::{Admission, Side};
use crate::gate::{
    BUDGET_EXTRA_BODIES, SLO_P99_NANOS, SideRun, floor_label, open_loop_conserved,
    open_loop_pair_fair,
};
use crate::support::{BodyKind, calibrate_cpu_ns_per_round, cpu_body_micros};
use crate::support::{micros, print_columns, print_row, worker_and_core_cells};
// ── The saturation sweep ─────────────────────────────────────────────────────

/// The in-flight bounds the saturation sweep runs at.
///
/// Six, because the issue's sweep list (`64`, `1,024`, `16,384`, `131,072`) and the
/// delivery brief's list (`64`, `1,024`, `10,000`, `100,000`) name different decades
/// and neither is a subset of the other. Both are here rather than one chosen for the
/// reader's convenience, because a sweep that quietly drops a tier reads as coverage
/// of the tiers it kept.
const SWEEP_BOUNDS: [usize; 6] = [64, 1_024, 10_000, 16_384, 100_000, 131_072];

/// The offered-rate ladder, as multiples of a quarter of the ceiling's capacity.
///
/// Expressed against **capacity** rather than against the bound, because the knee is
/// where the offered rate reaches what the ceiling can sustain, and a ladder of
/// `bound x k` walks straight past that point at a wide bound and never reaches it at a
/// narrow one. Capacity is `bound / body_seconds`, so a ceiling of 64 admitting a 3.2 ms
/// body sustains 20,000 arrivals a second and its ladder is 5,000 / 20,000 / 80,000 /
/// 320,000 — the knee at 20,000 is the second rung rather than beyond the ladder's end.
pub(crate) const SWEEP_MULTIPLIERS: [u64; 4] = [1, 4, 16, 64];

/// The declared capacity every swept ceiling is scaled to, in arrivals per second.
///
/// **One target for every bound, with the body cost derived per bound**, because a single
/// constant body cost cannot produce a curve at more than one ceiling width. A 5 ms body
/// puts a ceiling of 64 at 12,800 arrivals a second and a ceiling of 131,072 at 26
/// million; an in-process generator offers about 450,000 a second on this host, so at the
/// wide ceiling every rung above the first measures the *generator's* backlog — its own
/// `max lag` reached 3.4 s against a ceiling that had admitted every arrival immediately —
/// and the p99 on those rows is a property of the instrument rather than of the runtime.
/// Scaling the body instead (`bound * 1e6 / target`) puts every ceiling under exactly this
/// load at the same four offered rates, so the knee is comparable across bounds and is a
/// property of the engine.
///
/// 20,000 arrivals a second is a declared figure and is printed on every run: it is well
/// inside the generator's own placement ceiling (measured at 450,000–700,000 a second on
/// the reference host, and printed beside it as `achieved/s`), so a knee declared against
/// it is a knee of the engine rather than of the loop that offers the work.
pub(crate) const DEFAULT_CAPACITY_TARGET: u64 = 20_000;

/// The floor this generator was measured placing arrivals at, in arrivals per second.
///
/// A measured number, not a specification of this host: a calibration sweep on an Apple
/// M5 Pro with 15 cores placed 449,482 arrivals a second on the facade and 701,013 on the
/// baseline at a bound wide enough (100,000) that the ceiling was never the limit, and the
/// lower of the two is what a paired point can rely on. It is rounded down so a run whose
/// own `achieved/s` sits above it is not contradicted, and every sweep row prints its
/// achieved rate beside the offered one so a reader sees the margin rather than taking this
/// line's word for it.
///
/// A host that places slower than this gets a smaller usable range, and the rig says so
/// rather than quietly reporting the generator's backlog as the ceiling's knee: the
/// `max lag ms` column is the tell, and a row whose lag is comparable to its p99 is
/// measuring the generator.
pub(crate) const GENERATOR_PLACEMENT_CEILING: u64 = 400_000;

/// The service cost every body runs at one bound, in microseconds.
///
/// `bound * 1e6 / capacity_target` when no override is given, which is the scaling
/// [`DEFAULT_CAPACITY_TARGET`] exists for. An override pins the cost at every bound
/// instead, which is the other question a reader may want answered — "at a fixed 5 ms
/// service time, where does each ceiling's knee sit" — and the two are reported separately
/// because they answer different questions and neither answers the other.
pub(crate) fn body_for_bound(bound: usize, capacity_target: u64, override_micros: u64) -> u64 {
    if override_micros > 0 {
        return override_micros;
    }
    let permits = widen(bound);
    // Rounded up, so a ceiling is never declared to sustain more than the target by
    // truncation: `63 * 1e6 / 20_000` is 3,150 us exactly and `bound * 50` is 3,150 us,
    // while a bound the division rounds down would silently get a body cheaper than the
    // target asks for.
    permits
        .saturating_mul(1_000_000)
        .div_ceil(capacity_target.max(1))
        .max(1)
}

/// The highest offered rate this rig will generate, in arrivals per second.
///
/// A cap on the *generator*, not on either engine, and declared because it is what
/// ends the ladder at a wide ceiling: an in-process generator places arrivals one at a
/// time through an engine's own spawn door, so a ceiling of 131,072 admitting a 5 ms body
/// could sustain 26 million a second and this rig cannot reach it. A ladder whose top
/// rung was clamped by the generator is reported as clamped, because a reader who read
/// the top row as the knee would be reading the instrument's ceiling as the runtime's.
pub(crate) const MAX_OFFERED_RATE: u64 = 2_000_000;

/// The offered rates this rig will actually drive at one bound, deduplicated.
///
/// [`SWEEP_MULTIPLIERS`] walked upward and every rung above [`MAX_OFFERED_RATE`] landed
/// on the same number, so a wide bound printed four identical rows and a reader counting
/// them would read four measurements where there was one. The ladder is deduplicated
/// instead, and [`ladder_clamped`] is what says whether the last rung was the
/// instrument's ceiling rather than the runtime's.
///
/// The deduplication is load-bearing at a zero body cost rather than cosmetic: at zero the
/// body is a counter bump and a yield — about a microsecond — so a ceiling of 64 declares
/// a capacity of 64 million arrivals a second and *every* rung of *every* ladder clamps to
/// [`MAX_OFFERED_RATE`]. Without it the sweep would print one rate six times per bound and
/// a reader would count six measurements where there was one.
pub(crate) fn sweep_ladder(bound: usize, body_micros: u64) -> Vec<u64> {
    let mut ladder: Vec<u64> = SWEEP_MULTIPLIERS
        .iter()
        .map(|rung| sweep_rate(bound, body_micros, *rung))
        .collect();
    // The rates rise with the rung, so every duplicate is adjacent and `dedup` removes
    // all of them rather than the runs of them.
    ladder.dedup();
    ladder
}

/// The arrival count one sweep point offers on each side.
///
/// A **count**, derived from the rate and the window, because the window alone let the
/// faster side place more arrivals than the slower one and the fairness gate refused
/// exactly that, naming `placed`: offering "for one second" makes the offered work a
/// property of each engine's speed. Both sides are therefore offered this many arrivals
/// and the difference between what they placed is a difference in the engines.
///
/// The window is floored at one second: a zero-second window would offer no arrivals at
/// all, which is a measurement of nothing rather than a short one.
pub(crate) fn sweep_arrivals(rate: u64, window: Duration) -> u64 {
    rate.saturating_mul(window.as_secs().max(1))
}

/// What one ceiling can sustain at one body cost, in arrivals per second.
///
/// Saturating throughout: a body of zero microseconds would claim an unbounded capacity,
/// and an unbounded figure is exactly the sort of number this rig exists not to publish.
pub(crate) fn capacity_per_second(bound: usize, body_micros: u64) -> u64 {
    let permits = widen(bound);
    permits.saturating_mul(1_000_000) / body_micros.max(1)
}

/// The offered rate for the `rung`-th multiple of a quarter of the ceiling's capacity.
fn sweep_rate(bound: usize, body_micros: u64, rung: u64) -> u64 {
    let capacity = capacity_per_second(bound, body_micros);
    capacity
        .saturating_mul(rung)
        .saturating_div(4)
        .clamp(1, MAX_OFFERED_RATE)
}

/// Whether the ladder's top rung was clamped by [`MAX_OFFERED_RATE`] rather than by the
/// ceiling, which is the fact a reader needs to read the knee table correctly.
pub(crate) fn ladder_clamped(bound: usize, body_micros: u64) -> bool {
    let declared = capacity_per_second(bound, body_micros)
        .saturating_mul(SWEEP_MULTIPLIERS[SWEEP_MULTIPLIERS.len() - 1])
        .saturating_div(4);
    declared > MAX_OFFERED_RATE
}

/// A rate rendered as a whole number of arrivals per second.
///
/// One renderer for the offered and the achieved columns, so the two are comparable by
/// eye — the pair is how a reader sees that an offered rate above this rig's own
/// placement ceiling was a rate the *generator* could not express.
fn rate_cell(per_second: f64) -> String {
    format!("{per_second:.0}")
}

/// The columns of the saturation table.
///
/// `offered/s` beside `achieved/s` is the pair that makes the table readable: past the
/// generator's own placement ceiling the two diverge, and the divergence is the fact a
/// reader needs before reading any latency on that row.
const SWEEP_COLUMNS: [&str; 12] = [
    "offered/s",
    "side",
    "offered",
    "admitted",
    "refused",
    "achieved/s",
    "p50 us",
    "p95 us",
    "p99 us",
    "peak queue",
    "mean queue",
    "max lag ms",
];

/// The columns of the knee table.
const KNEE_COLUMNS: [&str; 6] = [
    "bound",
    "side",
    "knee offered/s",
    "achieved/s",
    "p99 at knee",
    "refused at top",
];

/// Both sides' runs at one offered rate, gathered so the knee table can read either.
///
/// A `Vec` of two rather than a named pair, because the sweep walks both sides in the
/// same loop and the only thing that differs is the engine — so the ladder is a list of
/// points and each point holds the runs it measured.
pub(crate) struct SweepPair {
    pub(crate) bound: usize,
    pub(crate) offered_rate: u64,
    pub(crate) facade: SideRun,
    pub(crate) baseline: SideRun,
}

impl SweepPair {
    /// This pair's run on `side`.
    pub(crate) fn run(&self, side: Side) -> &SideRun {
        match side {
            Side::Facade => &self.facade,
            Side::Baseline => &self.baseline,
        }
    }
}

/// Measure one side at one offered rate and gate it, for the table and the record.
async fn measure_point(
    side: Side,
    admission: Admission,
    spec: &OpenLoopSpec,
) -> Result<SideRun, Box<dyn std::error::Error>> {
    let run = open_loop_side(side, admission, spec)
        .await
        .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;
    let label = format!(
        "bound {} at {}/s on the {}",
        spec.bound,
        spec.offered_rate,
        side.as_str()
    );
    open_loop_conserved(&label, &run)
        .map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;
    Ok(run)
}

/// Print one sweep row.
fn print_sweep_row(run: &SideRun) {
    let (p50, p95, p99) = run.histogram.percentiles_nanos();
    print_row(&[
        run.offered_rate.to_string(),
        run.side.as_str().to_string(),
        run.offered.to_string(),
        run.admitted.to_string(),
        run.refused.to_string(),
        rate_cell(run.throughput_per_second()),
        micros(p50),
        micros(p95),
        micros(p99),
        run.depth.peak.to_string(),
        format!("{:.1}", run.depth.mean()),
        run.max_lag.as_millis().to_string(),
    ]);
}

/// Sweep offered rate past the knee at every bound, on one admission door.
///
/// For each bound the ladder is walked on both sides, paired within the point, and
/// gated. The knee for a row is the highest ladder rate that refused nothing and
/// stayed inside [`SLO_P99_NANOS`]: a rate past it either refused arrivals or blew the
/// budget, and either is a refusal rather than a served request.
pub(crate) async fn saturation_sweep(
    sweep: SweepRun,
    json: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let SweepRun {
        admission,
        window,
        capacity_target,
        body_override,
        body_kind,
        cpu_rounds_override,
        workers,
        ..
    } = sweep;
    info!("saturation sweep — offered rate against p50/p95/p99 and the ceiling");
    info!(
        "door: {}   window: {} s per point   SLO: p99 <= {} ms",
        admission.as_str(),
        window.as_secs(),
        SLO_P99_NANOS / 1_000_000
    );
    if body_override > 0 {
        info!(
            "body cost pinned at {body_override} us at every bound, so each ceiling's declared \
             capacity is bound / body and the wide bounds are generator-capped by \
             construction; the capacity target {capacity_target}/s does not apply to this run"
        );
    } else {
        info!(
            "body cost derived per bound so every ceiling's declared capacity is \
             {capacity_target}/s, which is inside this generator's placement ceiling \
             (declared {}); a constant body cost would make the wide bounds measure the \
             generator",
            GENERATOR_PLACEMENT_CEILING
        );
    }
    info!("latency is measured from each arrival's INTENDED start\n");

    // One calibration per sweep, not per bound or per side: the cost of a hash
    // round is a property of this host, and both sides of every point burn the
    // same rounds whatever it reads.
    let cpu_ns_per_round = match body_kind {
        BodyKind::Timer => 0.0,
        BodyKind::Cpu => calibrate_cpu_ns_per_round(),
    };
    if body_kind == BodyKind::Cpu {
        if cpu_rounds_override == 0 {
            let refusal: Box<dyn std::error::Error> =
                "--body-kind=cpu needs --cpu-rounds=N: the CPU door burns a fixed, seeded \
                 number of hash rounds per task, and a sweep that derived them per bound \
                 would burn seconds per body at the wide bounds"
                    .to_owned()
                    .into();
            lgwks_std::trace::debug!(error = ?refusal, "saturation_sweep: refusing a CPU door with no pinned rounds");
            return Err(refusal);
        }
        if body_override > 0 {
            let refusal: Box<dyn std::error::Error> =
                "--cpu-rounds pins the body cost and so does --body-micros: name one"
                    .to_owned()
                    .into();
            lgwks_std::trace::debug!(error = ?refusal, "saturation_sweep: refusing two pinned body costs");
            return Err(refusal);
        }
        info!(
            "body kind: cpu — every body burns {cpu_rounds_override} seeded hash rounds, one \
             work unit recorded; calibration {cpu_ns_per_round:.2} ns/round, the ladder is \
             derived from the workers' capacity rather than the ceiling's"
        );
    }
    // What the ladder's capacity is derived from: the ceiling for timer
    // bodies, which park, and the scheduler's workers for CPU bodies, which
    // burn. A CPU ceiling wider than the worker count never binds — four
    // workers run four bodies at once however high the ceiling — so deriving
    // the ladder from the bound would offer past the cores' knee at every
    // rung and measure the queue, not the contention.
    // Named workers, else the cores the runtime would discover; a host that
    // will not say runs the one worker every runtime has.
    let workers_available = match (workers, std::thread::available_parallelism()) {
        (Some(named), _) => named,
        (None, Ok(cores)) => cores.get(),
        (None, Err(_)) => 1,
    };

    let mut points: Vec<SweepPair> = Vec::new();
    let mut json_rows: Vec<String> = Vec::new();
    let mut json_bodies: Vec<String> = Vec::new();
    let mut bound_bodies: Vec<(usize, usize, u64)> = Vec::new();

    for bound in SWEEP_BOUNDS {
        let basis = match body_kind {
            BodyKind::Timer => bound,
            BodyKind::Cpu => workers_available,
        };
        // The declared cost is what the knee budget is read against: derived
        // per bound for timer bodies, the pinned rounds' calibrated cost for
        // CPU bodies, the same at every bound because the rounds are fixed.
        let (body_micros, cpu_rounds) = match body_kind {
            BodyKind::Timer => (body_for_bound(bound, capacity_target, body_override), 0),
            BodyKind::Cpu => (
                cpu_body_micros(cpu_rounds_override, cpu_ns_per_round),
                cpu_rounds_override,
            ),
        };
        bound_bodies.push((bound, basis, body_micros));
        // One warm-up per side per bound, discarded: the first-touch page faults and
        // the runtime's worker start are one-time costs a measured point would
        // otherwise charge to the code under test.
        let warm = OpenLoopSpec {
            body_micros,
            cpu_rounds,
            offered_rate: sweep_rate(basis, body_micros, 1),
            window: Duration::from_millis(300),
            arrivals: None,
            bound,
            drop_every: 0,
        };
        for side in Side::ALL {
            open_loop_side(side, admission, &warm).await?;
        }

        let header = sweep_header(bound, basis, body_micros, cpu_rounds);
        info!("{header}");
        print_columns(&SWEEP_COLUMNS);
        for rate in sweep_ladder(basis, body_micros) {
            let spec = OpenLoopSpec {
                body_micros,
                cpu_rounds,
                offered_rate: rate,
                window,
                // The count the rate and the window define: both sides are offered the
                // same work, so a difference in what they placed is a difference in the
                // engines rather than in how long each had.
                arrivals: Some(sweep_arrivals(rate, window)),
                bound,
                drop_every: 0,
            };
            let mut runs = Vec::new();
            for side in Side::ALL {
                let run = measure_point(side, admission, &spec).await?;
                print_sweep_row(&run);
                if let Some(rss) = run.rss_bytes {
                    info!(
                        "           peak RSS {rss} bytes at the run's peak ({})",
                        run.rss_source
                    );
                }
                json_rows.push(sweep_json(&run));
                runs.push(run);
            }
            let label = format!("bound {bound} at {}/s", spec.offered_rate);
            if !open_loop_pair_fair(&label, &runs[0], &runs[1])? {
                info!(
                    "           a saturation point: facade refused {}, baseline refused {} — \
                     each side's own conservation gate is what gates it",
                    runs[0].refused, runs[1].refused
                );
            }
            points.push(SweepPair {
                bound,
                offered_rate: spec.offered_rate,
                facade: runs.swap_remove(0),
                baseline: runs.swap_remove(0),
            });
        }
        json_bodies.push(format!(
            "{{\"bound\":{bound},\"ladder_basis\":{basis},\"body_micros\":{body_micros},\"cpu_rounds\":{cpu_rounds},\"declared_capacity_per_second\":{}}}",
            capacity_per_second(basis, body_micros)
        ));
        info!("");
    }

    declare_knees(&points, &bound_bodies);
    let record = SweepRun {
        cpu_ns_per_round,
        ..sweep
    };
    write_sweep_json(json, &record, &json_bodies, &json_rows);
    Ok(())
}

/// One bound's header: its capacity, its body, and the budget its knee is read
/// against.
///
/// A function rather than an inline `info!` because the line states the SLO
/// every row below is judged by, and a test must be able to call the exact
/// string that is printed: the defect this replaced printed the nanosecond
/// constant beside a `ms` label.
fn sweep_header(bound: usize, basis: usize, body_micros: u64, cpu_rounds: u64) -> String {
    let budget = SLO_P99_NANOS.max(
        body_micros
            .saturating_mul(1_000)
            .saturating_mul(1 + BUDGET_EXTRA_BODIES),
    );
    let body = if cpu_rounds > 0 {
        format!("a {body_micros} us CPU body of {cpu_rounds} hash rounds")
    } else {
        format!("a {body_micros} us body")
    };
    format!(
        "bound {bound}  (declared capacity {} arrivals/s at {body}; knee budget \
         {:.1} ms = the {} floor or {BUDGET_EXTRA_BODIES} further service time{}, whichever is \
         larger)",
        capacity_per_second(basis, body_micros),
        budget as f64 / 1_000_000.0,
        floor_label(),
        if ladder_clamped(basis, body_micros) {
            "; ladder top clamped by the generator's own rate"
        } else {
            ""
        }
    )
}

/// The knee per bound and per side, declared against [`SLO_P99_NANOS`].
///
/// The knee is the highest ladder rate that refused nothing and stayed inside the
/// budget. It is declared rather than eyeballed because a curve has no knee of its
/// own: the same table read against a 10 ms SLO has a different one, and the SLO is
/// named on the same line so the two cannot be separated.
///
/// `bodies` is the ceiling, the ladder basis and the declared body cost per
/// bound the sweep ran: the basis is the ceiling itself for timer bodies and
/// the worker count for CPU bodies, so the ladder is rebuilt from what ran
/// rather than from what a ceiling-derived arithmetic would have said.
fn declare_knees(points: &[SweepPair], bodies: &[(usize, usize, u64)]) {
    info!(
        "knee — the highest offered rate with no refusal and p99 inside the bound's budget \
         ({} or {BUDGET_EXTRA_BODIES} further service time, whichever is larger)",
        floor_label()
    );
    print_columns(&KNEE_COLUMNS);
    for (bound, basis, body_micros) in bodies {
        let bound = *bound;
        let basis = *basis;
        let body_micros = *body_micros;
        let ladder = sweep_ladder(basis, body_micros);
        // `sweep_ladder` maps every multiplier to a rate, so the ladder is never empty;
        // a bound with no rungs has no knee to report and is skipped rather than given
        // an invented top rate.
        let Some(&top_rate) = ladder.last() else {
            continue;
        };
        for side in Side::ALL {
            let mut knee_rate = 0_u64;
            let mut knee_achieved = 0.0;
            let mut knee_p99 = 0_u64;
            let mut refused_at_top = 0_u64;
            for point in points.iter().filter(|point| point.bound == bound) {
                let run = point.run(side);
                if run.within_slo() {
                    knee_rate = point.offered_rate;
                    knee_achieved = run.throughput_per_second();
                    knee_p99 = run.histogram.quantile_nanos(0.99);
                }
                if point.offered_rate == top_rate {
                    refused_at_top = run.refused;
                }
            }
            let clamped = if ladder_clamped(basis, body_micros) && knee_rate == top_rate {
                " (generator-capped)"
            } else {
                ""
            };
            // A knee of zero is not a knee of nothing: it is a ladder on which every rung
            // was past the budget, and saying so is the fact a reader needs. A bare `0`
            // in this column reads as "not measured".
            let knee = if knee_rate == 0 {
                format!("none on this ladder{clamped}")
            } else {
                format!("{knee_rate}{clamped}")
            };
            print_row(&[
                bound.to_string(),
                side.as_str().to_string(),
                knee,
                rate_cell(knee_achieved),
                if knee_rate == 0 {
                    "-".to_string()
                } else {
                    format!("{:.1} ms", knee_p99 as f64 / 1_000_000.0)
                },
                refused_at_top.to_string(),
            ]);
        }
    }
    info!("");
}

/// The settings one saturation sweep ran under: the provenance its record carries.
#[derive(Clone, Copy)]
pub(crate) struct SweepRun {
    /// How an arrival at a full bound was admitted.
    pub(crate) admission: Admission,
    /// How long each point offered load.
    pub(crate) window: Duration,
    /// The arrivals per second each bound's body cost was derived to reach.
    pub(crate) capacity_target: u64,
    /// A fixed body cost in microseconds, or `0` to derive it per bound.
    pub(crate) body_override: u64,
    /// Whether bodies parked on timers or burned hash rounds.
    pub(crate) body_kind: BodyKind,
    /// Nanoseconds one hash round calibrated at on this host: filled in by the
    /// sweep, `0` as the caller passes it.
    pub(crate) cpu_ns_per_round: f64,
    /// Pinned hash rounds per body, or `0` to derive them per bound.
    pub(crate) cpu_rounds_override: u64,
    /// The runtime's worker count, when one was set.
    pub(crate) workers: Option<usize>,
}

/// Write the sweep's results rows, if a path was given.
///
/// A write failure is reported rather than propagated: the measurement has already
/// been made and printed, and refusing the whole run because the record could not be
/// filed would throw away the numbers in order to complain about the filing.
fn write_sweep_json(json: Option<&str>, sweep: &SweepRun, bounds: &[String], rows: &[String]) {
    let Some(path) = json else {
        return;
    };
    let SweepRun {
        admission,
        window,
        capacity_target,
        body_override,
        body_kind,
        cpu_ns_per_round,
        cpu_rounds_override,
        workers,
    } = *sweep;
    let (workers, cores) = worker_and_core_cells(workers);
    let kind = match body_kind {
        BodyKind::Timer => "timer",
        BodyKind::Cpu => "cpu",
    };
    let body = format!(
        "{{\"tool\":\"lgwks-bench-async-saturation\",\"door\":\"{}\",\
          \"window_seconds\":{},\"capacity_target_per_second\":{capacity_target},\
          \"body_micros_override\":{body_override},\"body_kind\":\"{kind}\",\
          \"cpu_ns_per_round\":{cpu_ns_per_round:.3},\"cpu_rounds_override\":{cpu_rounds_override},\
          \"max_offered_rate\":{MAX_OFFERED_RATE},\
          \"runtime_workers\":{},\"host_cores_visible\":{},\
          \"declared_capacity_note\":\"a point's declared capacity is bound * 1000000 / \
          body_micros arrivals per second, and the body cost is derived per bound to reach \
          capacity_target_per_second; the measured knee is read from the points, not from \
          this arithmetic\",\"slo_p99_floor_nanos\":{SLO_P99_NANOS},\
          \"slo_extra_bodies\":{BUDGET_EXTRA_BODIES},\
          \"bounds\":[\n{}\n],\"points\":[\n{}\n]}}",
        admission.as_str(),
        window.as_secs(),
        workers,
        cores,
        bounds.join(",\n"),
        rows.join(",\n")
    );
    match std::fs::write(path, body) {
        Ok(()) => info!("wrote {path}"),
        Err(error) => info!("could not write {path}: {error}"),
    }
}

/// One sweep point as a results-file row.
fn sweep_json(run: &SideRun) -> String {
    let (p50, p95, p99) = run.histogram.percentiles_nanos();
    let rss = match run.rss_bytes {
        Some(bytes) => bytes.to_string(),
        None => "null".to_string(),
    };
    format!(
        "{{\"bound\":{},\"side\":\"{}\",\"door\":\"{}\",\"offered_rate\":{},\"body_micros\":{},\
          \"offered\":{},\
          \"admitted\":{},\"refused\":{},\"completed\":{},\"cancelled\":{},\"aborted\":{},\
          \"work_units\":{},\"refusal_rate\":{},\"p50_nanos\":{p50},\"p95_nanos\":{p95},\
          \"p99_nanos\":{p99},\"max_nanos\":{},\"mean_nanos\":{},\"peak_queue\":{},\
          \"mean_queue\":{},\"max_lag_millis\":{},\"drain_millis\":{},\
          \"throughput_per_second\":{},\"p99_budget_nanos\":{},\"within_slo\":{},\"min_nanos\":{},\
          \"clamped_samples\":{},\"rss_bytes\":{rss},\"rss_source\":\"{}\"}}",
        run.bound,
        run.side.as_str(),
        run.admission.as_str(),
        run.offered_rate,
        run.body_micros,
        run.offered,
        run.admitted,
        run.refused,
        run.completed,
        run.cancelled,
        run.aborted,
        run.work_units,
        run.refusal_rate(),
        run.histogram.max_nanos(),
        run.histogram.mean_nanos(),
        run.depth.peak,
        run.depth.mean(),
        run.max_lag.as_millis(),
        run.drain.as_millis(),
        run.throughput_per_second(),
        run.p99_budget_nanos(),
        run.within_slo(),
        run.histogram.min_nanos(),
        run.histogram.clamped(),
        run.rss_source
    )
}

#[cfg(test)]
mod tests {
    use super::sweep_header;

    /// The header a bound prints names the floor in the unit its label says: the
    /// defect this guards printed `50000000 ms` for a 50 ms floor.
    #[test]
    fn the_sweep_header_names_the_floor_in_milliseconds() {
        for cpu_rounds in [0_u64, 1_600_000] {
            let header = sweep_header(64, 64, 3_200, cpu_rounds);
            assert!(
                header.contains("the 50 ms floor"),
                "the printed header must name the 50 ms floor: {header}"
            );
            assert!(
                !header.contains("50000000"),
                "the nanosecond constant beside a ms label was the defect: {header}"
            );
        }
        let cpu = sweep_header(64, 2, 950, 300_000);
        assert!(
            cpu.contains("300000"),
            "the CPU header must name its rounds: {cpu}"
        );
        // Two workers at a 950 us body sustain 2,105 arrivals/s; the ceiling's
        // 64 would say 67,368. The header must read the workers'.
        assert!(
            cpu.contains("2105"),
            "the CPU header must derive its capacity from the workers: {cpu}"
        );
        assert!(
            !cpu.contains("67368"),
            "the CPU header must not derive its capacity from the ceiling: {cpu}"
        );
    }
}
