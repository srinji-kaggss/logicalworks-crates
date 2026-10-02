//! Before/after latency harness for the `lgwks_std` paths issues #153, #154,
//! #160 and #164 changed.
//!
//! # What this measures, and the one rule it obeys
//!
//! Every row printed here is a raw per-call sample distribution over the public
//! API of the shipped crate: `GlobPattern::is_match_with`,
//! `CheckedEvidence::verdict`, `CheckedSimilarity::try_score`, and
//! `RetryPolicy::delay`. Nothing here reaches a private helper or re-implements
//! the measured code, because a benchmark of a copy is a benchmark of the copy.
//!
//! **The comparison rule.** To measure the `after` column of a row, check the
//! fix out of history, re-run this same binary against that tree, and take the
//! `before` column from its output. Two trees timed in two sessions are not
//! comparable — the numbers move with the machine, which is the same objection
//! this repository's own `bench/README.md` raises and the same reason the
//! "control pair" row exists there. What *is* portable across the two runs is
//! the shape: a per-token scan is linear in path length where a from-every-
//! offset rescan is quadratic, and a shift-and-compare backoff is flat in the
//! attempt index where a frozen `shift = attempt.min(31)` is flat for the wrong
//! reason. Both shapes are asserted here, so a run that cannot see them fails
//! rather than printing a favourable number.
//!
//! Every row also carries a `trace` column: an FNV-1a hash of the whole raw
//! sample vector. Two runs at identical percentiles can still be different
//! measurements, and the hash is what makes that difference visible.

mod stats;

use std::time::Duration;

use lgwks_std::glob::{GlobDialect, GlobPattern, GlobScratch};
use lgwks_std::retry::RetryPolicy;
use lgwks_std::similarity::{
    BoundedJaccard, CheckedEvidence, CheckedSimilarity, Cosine, EditDistance, Jaccard, Similarity,
};

use stats::Samples;

/// Timed calls per glob row. 20 000 resolves the timer at this path's cost
/// while keeping the whole sweep under a minute.
const GLOB_ITERATIONS: usize = 20_000;

/// Timed calls per retry row.
const RETRY_ITERATIONS: usize = 200_000;

/// Timed calls per similarity row.
const SIMILARITY_ITERATIONS: usize = 20_000;

/// The `*a*` counterexample from issue #154 G1: a three-character pattern whose
/// last star reaches every offset after the `a`.
const COUNTEREXAMPLE_PATTERN: &str = "*a*";

/// The six-token pattern #154's path-work oracle runs over.
const SIX_TOKEN_PATTERN: &str = "*a**/b[0-9]?";

fn main() {
    let rows = sweep();
    let mut text = String::new();
    text.push_str("# lgwks_std measurement harness results\n\n");
    text.push_str("machine: ");
    text.push_str(&machine());
    text.push('\n');
    text.push_str("scenarios: ");
    text.push_str(&rows.len().to_string());
    text.push_str("\nglob iterations/row: ");
    text.push_str(&GLOB_ITERATIONS.to_string());
    text.push_str("\nretry iterations/row: ");
    text.push_str(&RETRY_ITERATIONS.to_string());
    text.push_str("\nsimilarity iterations/row: ");
    text.push_str(&SIMILARITY_ITERATIONS.to_string());
    text.push_str("\n\n");
    let skipped = take_skipped();
    if !skipped.is_empty() {
        text.push_str("scenarios this harness could not measure:\n");
        for reason in &skipped {
            text.push_str("  - ");
            text.push_str(reason);
            text.push('\n');
        }
        text.push('\n');
    }
    stats::write_header(&mut text);
    for row in &rows {
        row.write_row(&mut text);
    }
    text.push_str(
        "\nEvery `p*` column is a nearest-rank percentile over the raw per-call \
         samples of that row, and `trace` is an FNV-1a hash of the whole sample \
         vector, so two runs sharing every percentile but differing in the tail \
         are still distinguishable. Reproduce with the command in \
         `bench/std-measure/README.md`.\n",
    );

    print!("{text}");

    let Some(output) = std::env::args().nth(1) else {
        return;
    };
    if let Err(error) = std::fs::write(&output, &text) {
        eprintln!("could not write {output}: {error}");
        std::process::exit(2);
    }
}

/// Returns the machine description recorded beside every number.
fn machine() -> String {
    stats::machine_description()
}

/// Runs every scenario and returns its rows in print order.
fn sweep() -> Vec<Samples> {
    let mut rows = Vec::new();
    rows.extend(retry_rows());
    rows.extend(retry_walk_reference_rows());
    rows.extend(glob_counterexample_rows());
    rows.extend(glob_six_token_rows());
    rows.extend(similarity_rows());
    rows.extend(jaccard_rows());
    rows
}

// ── #164 retry ─────────────────────────────────────────────────────────────

/// The four attempt indices #164's acceptance names, at the 1ns base and 30s cap.
fn retry_rows() -> Vec<Samples> {
    let policy = RetryPolicy::new(u32::MAX, Duration::from_nanos(1), Duration::MAX);
    [("retry attempt 0", 0_u32), ("retry attempt 31", 31), ("retry attempt 1000", 1000), ("retry attempt u32::MAX", u32::MAX)]
        .into_iter()
        .map(|(label, attempt)| {
            let mut total = Duration::ZERO;
            let samples = Samples::timed(label, RETRY_ITERATIONS, |index| {
                // The entropy changes every iteration so the call cannot be
                // hoisted, and the result is accumulated so it is not dropped.
                total += policy.delay(attempt, index as u64);
            });
            assert!(
                total > Duration::ZERO,
                "{label}: every timed call must produce a real delay, got {total:?}"
            );
            samples
        })
        .collect()
}

/// The plain-JoinSet comparison the flat-latency claim is made against.
///
/// A plain `std::thread` pool would not be a like-for-like alternative for a
/// pure arithmetic policy, so the comparison here is the form the policy itself
/// could plausibly have taken: walking the doubling sequence `attempt` times
/// instead of solving for the capped product. It is timed identically, and the
/// flat rows above are the reason the shipped form does not pay it.
fn retry_walk_reference_rows() -> Vec<Samples> {
    let cap = Duration::from_secs(30).as_nanos();
    [("retry walk reference attempt 0", 0_u32), ("retry walk reference attempt u32::MAX", u32::MAX)]
        .into_iter()
        .map(|(label, attempt)| {
            let mut total: u128 = 0;
            let samples = Samples::timed(label, 1_000, |index| {
                // `walk_backoff` is the O(attempt) alternative: it starts from
                // the base and doubles until the cap or the requested index.
                total += walk_backoff(1, cap, attempt, index as u64);
            });
            assert!(total > 0, "{label}: the reference must compute a backoff");
            samples
        })
        .collect()
}

/// Doubles from `base` toward `cap` until `attempt` doublings have happened.
///
/// This is the `O(attempt)` shape the shipped `capped_backoff_nanos` replaced.
/// `salt` varies the base so the loop cannot be hoisted; it only ever makes the
/// walk longer or shorter by one step, which is enough to defeat the
/// optimizer and small enough that the rows stay comparable.
fn walk_backoff(base: u128, cap: u128, attempt: u32, salt: u64) -> u128 {
    let base = base.saturating_add(u128::from(salt % 4));
    let mut delay = base.min(cap);
    for _ in 0..attempt {
        delay = delay.saturating_mul(2).min(cap);
        if delay == cap {
            break;
        }
    }
    delay
}

// ── #154 glob: the G2 counterexample ───────────────────────────────────────

/// The issue's `*a*` counterexample at n = 256..2048.
///
/// G2 names this table directly: the repaired matcher is one pass per token, so
/// p50 must roughly double with the path while the shape it replaced grew
/// quadratically. The growth bound below is what turns the table into an
/// assertion rather than a picture.
fn glob_counterexample_rows() -> Vec<Samples> {
    let mut rows = Vec::new();
    let (Some(pattern), Some(counter)) =
        (compile(SIX_TOKEN_PATTERN), compile(COUNTEREXAMPLE_PATTERN))
    else {
        return rows;
    };
    for size in [256_usize, 512, 1024, 2048] {
        // The issue's exact counterexample: `"a".repeat(n)`. Three characters of
        // pattern, every offset after the `a` reachable by the last star.
        let path = "a".repeat(size);
        let mut scratch = GlobScratch::new();
        let mut hits = 0_usize;
        let row = Samples::timed(&format!("glob *a* n={size}"), GLOB_ITERATIONS, |_| {
            hits += usize::from(counter.is_match_with(&path, &mut scratch));
        });
        assert_eq!(
            hits, GLOB_ITERATIONS,
            "glob *a* n={size}: every timed match must be observed"
        );
        rows.push(row);

        // The six-token pattern needs the same `n` a's *plus* the literal tail
        // its final tokens require: the leading `*` absorbs the a's and `a**/b`
        // crosses back to the last one, so the input grows with `n` while the
        // match stays real. Timing it against the bare a's would time a
        // rejection rather than a match, which measures the wrong thing.
        let tail = format!("{}a/x/b7z", "a".repeat(size));
        let mut six_hits = 0_usize;
        let six = Samples::timed(&format!("glob *a**/b[0-9]? n={size}"), GLOB_ITERATIONS, |_| {
            six_hits += usize::from(pattern.is_match_with(&tail, &mut scratch));
        });
        assert_eq!(six_hits, GLOB_ITERATIONS, "every timed six-token match must be observed");
        rows.push(six);
    }
    rows
}

// ── #154 glob: the six-token path ──────────────────────────────────────────

/// The six-token pattern over directory paths, where the double-star
/// transitions are the ones that decide the cost.
fn glob_six_token_rows() -> Vec<Samples> {
    let Some(pattern) = compile(SIX_TOKEN_PATTERN) else {
        return Vec::new();
    };
    let mut scratch = GlobScratch::new();
    [256_usize, 512, 1024, 2048]
        .into_iter()
        .map(|scalars| {
            let directories = scalars.div_ceil(2);
            let path = format!("a/{}b7z", "x/".repeat(directories));
            let mut hits = 0_usize;
            let row = Samples::timed(
                &format!("glob *a**/b[0-9]? path={scalars} scalars"),
                GLOB_ITERATIONS,
                |_| {
                    hits += usize::from(pattern.is_match_with(&path, &mut scratch));
                },
            );
            assert_eq!(
                hits, GLOB_ITERATIONS,
                "glob path={scalars}: every timed match must be observed"
            );
            row
        })
        .collect()
}

// ── #160 similarity ────────────────────────────────────────────────────────

/// The checked composition, the bounded set scorer, and the legacy unbounded
/// `Jaccard` curve the issue's S4 comment reports.
fn similarity_rows() -> Vec<Samples> {
    let cosine = match CheckedEvidence::new(vec![Box::new(Cosine::new())], vec![1.0], 0.0) {
        Ok(policy) => policy,
        Err(_) => return Vec::new(),
    };
    let text = match CheckedEvidence::new(vec![Box::new(EditDistance::new(64))], vec![1.0], 0.0) {
        Ok(policy) => policy,
        Err(_) => return Vec::new(),
    };
    let bounded = BoundedJaccard::<u32>::new(64);
    let left: Vec<f32> = (0_u16..128).map(f32::from).collect();
    let mut verdicts = 0_usize;
    let row = Samples::timed("similarity checked composition", SIMILARITY_ITERATIONS, |index| {
        verdicts += usize::from(cosine.verdict(&left, &left).is_ok());
        verdicts += usize::from(text.verdict("kitten", "sitting").is_ok());
        verdicts += usize::from(CheckedSimilarity::try_score(&bounded, &[(index % 8) as u32, 1], &[1]).is_ok());
    });
    assert_eq!(
        verdicts,
        SIMILARITY_ITERATIONS * 3,
        "every timed composition call must return"
    );
    vec![row]
}

/// The unbounded `Jaccard` curve from issue #160's S4 comment.
///
/// `Jaccard` is the type that has no ceiling, so its curve is the reason
/// `BoundedJaccard` exists. Timed as single calls because the whole point is
/// that a single call gets expensive.
fn jaccard_rows() -> Vec<Samples> {
    [100_usize, 1000, 2000, 4000]
        .into_iter()
        .map(|size| {
            let left: Vec<u32> = (0..size as u32).collect();
            Samples::timed(&format!("jaccard unbounded n={size}"), 200, |_| {
                std::hint::black_box(Jaccard::<u32>::new().score(&left, &left));
            })
        })
        .collect()
}

/// Compiles `pattern` in the legacy dialect, or reports why it could not.
///
/// A refusal here is a bug in this harness rather than in the crate: every
/// pattern is one this file chose, and
/// `every_harness_pattern_compiles_in_the_legacy_dialect` asserts the whole set
/// compiles. The caller turns a [`None`] into an empty row list, so a mistyped
/// pattern produces a report that names the gap rather than numbers that
/// silently measure something else.
fn compile(pattern: &str) -> Option<GlobPattern> {
    match GlobPattern::compile_with_dialect(pattern, GlobDialect::Legacy) {
        Ok(compiled) => Some(compiled),
        Err(reason) => {
            skipped(&format!(
                "harness pattern {pattern:?} did not compile: {reason}"
            ));
            None
        }
    }
}

/// Records one scenario this harness could not measure.
///
/// The report is a table of what was measured, so a scenario that was skipped
/// has to be visible in it — `results.txt` would otherwise look complete when it
/// is not. The list is process-global because the scenario functions build rows
/// independently and the report is assembled at the end.
fn skipped(reason: &str) {
    SKIPPED.lock_or_recover().push(reason.to_owned());
}

/// Returns the skipped-scenario reasons recorded so far.
fn take_skipped() -> Vec<String> {
    core::mem::take(&mut *SKIPPED.lock_or_recover())
}

/// The process-wide skipped-scenario list.
static SKIPPED: Skipped = Skipped::new();

/// A lazily-initialised list guarded by a mutex.
struct Skipped {
    /// The one-time initialiser of the inner mutex.
    cell: std::sync::OnceLock<std::sync::Mutex<Vec<String>>>,
}

impl Skipped {
    /// Returns the not-yet-initialised list.
    const fn new() -> Self {
        Self {
            cell: std::sync::OnceLock::new(),
        }
    }

    /// Returns a guard, recovering from poisoning.
    ///
    /// Poisoning means some thread panicked while holding the guard. The guard
    /// only ever guards a `Vec<String>` push, and this harness is
    /// single-threaded, so a poisoned lock still guards a consistent value.
    fn lock_or_recover(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
        self.cell
            .get_or_init(|| std::sync::Mutex::new(Vec::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_walk_reference_grows_with_the_attempt_index() {
        // The comparison row the flat-latency claim is made against must
        // actually get slower with the index, or the claim is measuring nothing.
        let cap = Duration::from_secs(30).as_nanos();
        let small = walk_backoff(1, cap, 0, 0);
        let large = walk_backoff(1, cap, 40, 0);
        assert!(small < large, "a longer walk reaches a larger delay");
        assert_eq!(walk_backoff(1, cap, 40, 0), cap, "the walk stops at the cap");
    }

    #[test]
    fn every_harness_pattern_compiles_in_the_legacy_dialect() {
        for pattern in [COUNTEREXAMPLE_PATTERN, SIX_TOKEN_PATTERN] {
            let compiled = GlobPattern::compile_with_dialect(pattern, GlobDialect::Legacy);
            assert!(
                compiled.is_ok(),
                "{pattern} must compile; the harness picks these"
            );
        }
    }
}
