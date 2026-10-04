//! The measurement harness for the PR-review journey.
//!
//! Not a pass/fail gate: it prints percentiles and peak RSS so the adapter's
//! cost on the real path is a number rather than an adjective. Every run forks
//! a real `gh` through the same supervisor the adapter uses, so what is timed
//! is the process boundary itself — fork, capture, reap — not a mock.
//!
//! ```text
//! cargo run -p lgwks_bot --features full --example review_pr_bench -- <tier>
//! ```
//!
//! `<tier>` is the number of reviews to publish. The report names the tier it
//! ran and the one it was asked for, so a measurement that had to stop short
//! says so rather than reporting the smaller number as the result.

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lgwks_bot::domain::gh::{Gh, PullRequest, Repository};
use lgwks_bot::review::{ReviewOutcome, ReviewRequest, ScriptFailure};
use lgwks_bot::script::Scope;
use lgwks_bot::task::{Host, task};

/// The pull request the measurements review.
const REPO: &str = "acme/widgets";
/// Its number.
const NUMBER: u64 = 7;
/// The body every measured review publishes.
const BODY: &str = "measured review body";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut out = std::io::stdout().lock();
    let requested: usize = std::env::args()
        .nth(1)
        .unwrap_or_else(|| String::from("32"))
        .parse()?;

    let program = std::env::var("LWCK_GH").or_else(|error| match error {
        std::env::VarError::NotPresent => Ok(String::from("gh")),
        unreadable @ std::env::VarError::NotUnicode(_) => Err(unreadable),
    })?;
    let gh = Gh::new(Repository::new(REPO)?)
        .program(&program)
        .capture_limit(std::num::NonZeroUsize::new(64 * 1024).ok_or("a non-zero limit")?)
        .deadline(Some(Duration::from_secs(20)));

    // The admission ceiling is the tier: this measures what the host admits,
    // not what the machine happens to absorb.
    let host = Host::builder("bench")?
        .max_concurrent_tasks(
            std::num::NonZeroUsize::new(requested.max(1)).ok_or("a non-zero ceiling")?,
        )
        .default_deadline(Duration::from_secs(120))
        .build()?;

    let completed = AtomicUsize::new(0);
    let started = Instant::now();
    let mut samples = Vec::with_capacity(requested);

    for _ in 0..requested {
        let review = task("bench-review", |scope: Scope, gh: Gh| async move {
            one(scope, gh).await
        })?;
        let at = Instant::now();
        let report = host.block_on(&review, gh.clone())?;
        samples.push(at.elapsed());
        completed.fetch_add(1, Ordering::Relaxed);
        note_outcome(&mut out, requested, &report)?;
    }

    let wall = started.elapsed();
    samples.sort_unstable();
    writeln!(
        out,
        "tier-requested={requested} tier-reached={} tier-ceiling={}",
        completed.load(Ordering::Relaxed),
        host.limits().max_concurrent_tasks()
    )?;
    writeln!(
        out,
        "reviews={} wall={:?} p50={:?} p95={:?} p99={:?} peak_rss_kib={}",
        samples.len(),
        wall,
        percentile(&samples, 50),
        percentile(&samples, 95),
        percentile(&samples, 99),
        peak_rss_kib()
    )?;
    Ok(())
}

/// Print anything about one measured run other than a published review.
fn note_outcome(
    out: &mut impl Write,
    requested: usize,
    report: &lgwks_bot::task::Report<ReviewOutcome>,
) -> std::io::Result<()> {
    match report.output() {
        Some(&ReviewOutcome::Published { .. }) => Ok(()),
        Some(other) => writeln!(out, "tier {requested}: outcome {other:?}"),
        None => writeln!(
            out,
            "tier {requested}: run failed: {}",
            report
                .error()
                .map_or_else(|| String::from("no error reported"), ToString::to_string)
        ),
    }
}

/// One review, as the measured task body: the same sequence the journey runs.
async fn one(scope: Scope, gh: Gh) -> Result<ReviewOutcome, lgwks_bot::script::FlowError> {
    let pull = PullRequest::new(
        Repository::new(REPO).map_err(|source| ScriptFailure::new(source.to_string()))?,
        NUMBER,
    );
    let request = ReviewRequest::new(pull, "COMMENT", BODY).with_marker("bench");
    lgwks_bot::review::review_pr(scope, gh, request, |_snapshot, _scope| {
        Ok(String::from(BODY))
    })
    .await
}

/// The `p`-th percentile of a sorted sample, nearest-rank.
fn percentile(sorted: &[Duration], rank: usize) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let chosen = rank.saturating_mul(sorted.len()).div_ceil(100);
    let index = chosen.saturating_sub(1).min(sorted.len().saturating_sub(1));
    sorted.get(index).copied().unwrap_or(Duration::ZERO)
}

/// This process's peak resident set size in KiB, or `0` where the platform
/// does not expose it.
///
/// Zero is reported rather than guessed. `getrusage` would need an FFI edge this
/// crate does not have, so on macOS and the BSDs this reads `0` and the real
/// figure comes from the platform's own tool (`/usr/bin/time -l`) wrapping the
/// run. A number printed next to a `0` would look like a measurement; a `0`
/// that means "not available here" does not.
fn peak_rss_kib() -> u64 {
    #[cfg(unix)]
    {
        // `/proc` is Linux; on the BSDs and macOS the same number comes from
        // `getrusage`, which this crate cannot reach without an FFI edge, so it
        // reports 0 rather than a guess.
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find(|line| line.starts_with("VmHWM:"))
                    .and_then(|line| {
                        line.split_whitespace()
                            .nth(1)
                            .and_then(|value| value.parse().ok())
                    })
            })
            .unwrap_or(0)
    }
    #[cfg(not(unix))]
    {
        0
    }
}
