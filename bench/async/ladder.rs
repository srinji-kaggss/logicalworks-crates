//! The concurrency ladder: both sides at every tier the contract names, under a
//! fixed in-flight ceiling, with the process's resident set reported once and
//! labelled with how it was read.
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

/// One resident-set reading taken after a tier, and how it was read.
///
/// The open-loop paths' reader ([`resident_bytes`]) is the one parser. On Linux
/// its figure is `VmHWM`, the kernel's high-water mark, so the reading after the
/// last tier is the peak. macOS has no in-process high-water mark without a
/// `getrusage` edge, and its figure is a `/bin/ps` sample of the current RSS,
/// taken here once a tier's tasks have finished, not while they were alive: the
/// largest of those samples is a lower bound on the peak, and the label says so
/// rather than borrowing the open-loop label, which a sample taken mid-run earns.
///
/// Where the host cannot report RSS the figure is `None` and the label says why:
/// an empty cell in a results file reads as "small", and no figure is carried
/// over from another platform.
///
/// [`resident_bytes`]: crate::driver::resident_bytes
fn rss_after_tier() -> (Option<u64>, &'static str) {
    let (bytes, source) = crate::driver::resident_bytes();
    #[cfg(not(target_os = "linux"))]
    let source = if bytes.is_some() {
        "/bin/ps -o rss= (current, largest of one sample after each tier; a lower bound on the peak)"
    } else {
        source
    };
    (bytes, source)
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
    info!("RSS: VmHWM on Linux, a /bin/ps sample on macOS, read after every tier\n");

    let mut rows = Vec::new();
    let mut rss: (Option<u64>, &'static str) = (None, "not read");
    for tasks in TIERS {
        let result = runtime.block_on(measure_tier(tasks))?;
        rows.push(result);
        let read = rss_after_tier();
        if read.0 >= rss.0 {
            rss = read;
        }
    }
    let (peak, rss_source) = rss;

    info!(
        "{:>9} {:>11} {:>11} {:>11} {:>11} {:>11} {:>11}",
        "tasks", "f p50", "f p95", "f p99", "b p50", "b p95", "b p99"
    );
    let mut json_rows = Vec::with_capacity(rows.len());
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
        json_rows.push(format!(
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
        Some(bytes) => info!("\nRSS for the whole process: {bytes} bytes ({rss_source})"),
        None => info!(
            "\npeak RSS: NOT AVAILABLE on this host ({rss_source}) — the field below is `null`, \
             and no value is extrapolated from another platform"
        ),
    }
    info!(
        "the ladder measures this host at this profile; the numbers are not a \
         cross-platform claim"
    );

    if let Some(path) = json {
        std::fs::write(path, ladder_record(peak, rss_source, &json_rows))?;
        info!("\nwrote {path}");
    }
    Ok(())
}

/// The ladder's JSON record: the run's header and one object per tier.
///
/// The rows are joined with `join`, not by pushing a separator per row: the
/// hand-pushed form wrote a newline and no comma, so every record with more
/// than one tier was not JSON (#639).
fn ladder_record(peak: Option<u64>, rss_source: &str, rows: &[String]) -> String {
    let peak_field = match peak {
        Some(bytes) => format!("\"peak_rss_bytes\":{bytes}"),
        None => String::from("\"peak_rss_bytes\":null"),
    };
    let tiers = rows.join(",\n");
    format!(
        "{{\"tool\":\"lgwks-bench-async-ladder\",\"bound\":{TIER_BOUND},\
          \"rounds\":{TIER_ROUNDS},{peak_field},\"rss_source\":\"{rss_source}\",\
          \"tiers\":[\n{tiers}\n]}}"
    )
}

#[cfg(test)]
mod tests {
    use super::ladder_record;

    /// Two tiers are separated by a comma, so the array is JSON.
    #[test]
    fn tier_rows_are_comma_separated() {
        let rows = [
            String::from("{\"tier\":100}"),
            String::from("{\"tier\":1000}"),
        ];
        let record = ladder_record(Some(1), "probe", &rows);
        assert!(
            record.contains("\"tiers\":[\n{\"tier\":100},\n{\"tier\":1000}\n]}"),
            "{record}"
        );
    }

    /// An unreadable RSS is `null`, never a number.
    #[test]
    fn an_unread_rss_is_null() {
        let record = ladder_record(None, "unreadable", &[String::from("{}")]);
        assert!(record.contains("\"peak_rss_bytes\":null"), "{record}");
    }
}
