//! The concurrency ladder: both sides at every tier the contract names, under a
//! fixed in-flight ceiling, with the process high-water mark reported once.
//!
//! Moved from `main.rs` for #639; the behaviour it exercised is unchanged.

use lgwks_bot::rt::runtime::Runtime;

use lgwks_std::trace::info;

use crate::async_stats;
use crate::gate::fair;
use crate::sides::{baseline_side, facade_side};
// ── The concurrency ladder: both sides, at every tier the contract names ─────

/// The tiers, each a power of ten the issue names.
const TIERS: [usize; 4] = [100, 1_000, 10_000, 100_000];

/// The in-flight ceiling every tier ran at.
///
/// Fixed rather than scaled: the point of a tier is to change the *number of
/// tasks*, and a ceiling that moved with it would measure two variables at once
/// and neither one cleanly.
const TIER_BOUND: usize = 64;

/// Paired rounds per tier. Five is enough for a p99 that means something on a
/// distribution this short while keeping the whole ladder inside a gate lane;
/// the paired ratio's interval, not the raw p99, is what the table leans on.
const TIER_ROUNDS: usize = 5;

/// The process's peak resident set size in bytes, or `None` where the host does
/// not expose it.
///
/// Linux reads its own `/proc/self/status`, whose `VmHWM` is the kernel's
/// high-water mark. macOS exposes no in-process high-water mark without a
/// `getrusage` edge, so it reads the same documented `/bin/ps` sample the
/// open-loop paths use ([`resident_bytes`]): one reader per host, and the field
/// means a sample taken when the call runs rather than a high-water mark. The
/// distinction is in the record's `rss_source`, so a reader is never told the
/// two platforms measured the same thing.
///
/// It is *optional* rather than required, and the reason is stated rather than
/// hidden: a peak-RSS column a host cannot fill is worse than none, because an
/// empty cell in a results file reads as "small". Where it is unavailable the
/// field is `null` and the report says so, and no figure is carried over from
/// another platform.
fn peak_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(value) = line.strip_prefix("VmHWM:") {
                let kib: u64 = value.split_whitespace().next()?.parse().ok()?;
                return Some(kib.saturating_mul(1024));
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        crate::driver::resident_bytes().0
    }
}

/// One tier's measured numbers, for the report and the JSON record.
pub(crate) struct TierResult {
    pub(crate) tasks: usize,
    pub(crate) facade: Vec<f64>,
    pub(crate) baseline: Vec<f64>,
    pub(crate) facade_work_units: u64,
    pub(crate) baseline_work_units: u64,
}

/// Measure both sides at one tier and return the paired samples.
///
/// Fairness is gated here too, for the same reason it is gated in `measure`: a
/// timing ratio computed on unequal work is not a slower engine, it is a
/// different program. The gate runs per round rather than once at the end, so a
/// tier that drifts mid-run is caught at the round it drifted rather than at the
/// end.
async fn measure_tier(tasks: usize) -> Result<TierResult, String> {
    let mut facade = Vec::with_capacity(TIER_ROUNDS);
    let mut baseline = Vec::with_capacity(TIER_ROUNDS);
    let mut facade_work_units = 0;
    let mut baseline_work_units = 0;

    // A warm-up per side, discarded: the first-touch page faults and thread
    // start are one-time costs a measured round would otherwise charge to the
    // code under test.
    let _ = facade_side(tasks, TIER_BOUND).await;
    let _ = baseline_side(tasks, TIER_BOUND).await;

    for round in 0..TIER_ROUNDS {
        let (facade_time, facade_tally) = facade_side(tasks, TIER_BOUND).await;
        let (base_time, base_tally) = baseline_side(tasks, TIER_BOUND).await;
        fair(&facade_tally, &base_tally)
            .map_err(|reason| format!("{tasks} tasks round {round}: {reason}"))?;
        facade.push(facade_time);
        baseline.push(base_time);
        facade_work_units = facade_tally.work_units;
        baseline_work_units = base_tally.work_units;
    }
    Ok(TierResult {
        tasks,
        facade,
        baseline,
        facade_work_units,
        baseline_work_units,
    })
}

/// The whole ladder, reported per tier with both sides' distribution.
pub(crate) fn tier_ladder(
    runtime: &Runtime,
    json: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("concurrency ladder: both sides at every tier the contract names");
    info!("bound {TIER_BOUND} on every tier, {TIER_ROUNDS} paired rounds each");
    info!("peak RSS is the process high-water mark, read once at the end\n");

    let mut rows = Vec::new();
    for tasks in TIERS {
        let result = runtime.block_on(measure_tier(tasks))?;
        rows.push(result);
    }
    let peak = peak_rss_bytes();

    info!(
        "{:>9} {:>11} {:>11} {:>11} {:>11} {:>11} {:>11}",
        "tasks", "f p50", "f p95", "f p99", "b p50", "b p95", "b p99"
    );
    let mut json_rows = String::new();
    for row in &rows {
        let mut f = row.facade.clone();
        let mut b = row.baseline.clone();
        let percentiles = [
            async_stats::quantile(&mut f, 0.50),
            async_stats::quantile(&mut f, 0.95),
            async_stats::quantile(&mut f, 0.99),
            async_stats::quantile(&mut b, 0.50),
            async_stats::quantile(&mut b, 0.95),
            async_stats::quantile(&mut b, 0.99),
        ];
        info!(
            "{:>9} {:>11.6} {:>11.6} {:>11.6} {:>11.6} {:>11.6} {:>11.6}",
            row.tasks,
            percentiles[0],
            percentiles[1],
            percentiles[2],
            percentiles[3],
            percentiles[4],
            percentiles[5]
        );
        if !json_rows.is_empty() {
            json_rows.push('\n');
        }
        json_rows.push_str(&format!(
            "{{\"tier\":{},\"bound\":{},\"facade_p50\":{},\"facade_p95\":{},\
             \"facade_p99\":{},\"baseline_p50\":{},\"baseline_p95\":{},\"baseline_p99\":{},\
             \"facade_work_units\":{},\"baseline_work_units\":{}}}",
            row.tasks,
            TIER_BOUND,
            percentiles[0],
            percentiles[1],
            percentiles[2],
            percentiles[3],
            percentiles[4],
            percentiles[5],
            row.facade_work_units,
            row.baseline_work_units,
        ));
    }
    match peak {
        Some(bytes) => info!("\npeak RSS for the whole process: {bytes} bytes"),
        None => info!(
            "\npeak RSS: NOT AVAILABLE on this host — the field below is `null`, and no \
             value is extrapolated from another platform"
        ),
    }
    info!(
        "the ladder measures this host at this profile; the numbers are not a \
         cross-platform claim"
    );

    if let Some(path) = json {
        let peak_field = match peak {
            Some(bytes) => format!("\"peak_rss_bytes\":{bytes}"),
            None => String::from("\"peak_rss_bytes\":null"),
        };
        let body = format!(
            "{{\"tool\":\"lgwks-bench-async-ladder\",\"bound\":{TIER_BOUND},\
              \"rounds\":{TIER_ROUNDS},{peak_field},\"tiers\":[\n{json_rows}\n]}}"
        );
        std::fs::write(path, body)?;
        info!("\nwrote {path}");
    }
    Ok(())
}
