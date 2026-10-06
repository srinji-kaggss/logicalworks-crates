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

/// Reviews published when the command line names no tier.
const DEFAULT_TIER: &str = "32";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut out = std::io::stdout().lock();
    let tier = std::env::args().nth(1);
    let requested: usize = match tier {
        Some(text) => text.parse()?,
        None => DEFAULT_TIER.parse()?,
    };
    // A tier of zero publishes nothing, so every percentile below would be a
    // reading of an empty sample. The measurement is refused instead.
    if requested == 0 {
        let refusal = Err("a tier of zero reviews measures nothing".into());
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "main: returning an error to the caller");
        return refusal;
    }

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
        "reviews={} wall={wall:?} p50={} p95={} p99={} peak_rss_kib={}",
        samples.len(),
        field(percentile(&samples, 50)),
        field(percentile(&samples, 95)),
        field(percentile(&samples, 99)),
        field(peak_rss_kib())
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

/// A reading as a report field, or `null` where there is none.
///
/// `null` is the estate's spelling of *not measured* (INV-BOT-142) and never
/// *measured as zero*: a printed `0` beside a peak-RSS field would read as a
/// process that used no memory at all.
fn field(reading: Option<impl std::fmt::Display>) -> String {
    match reading {
        Some(value) => value.to_string(),
        None => String::from("null"),
    }
}

/// The `p`-th percentile of a sorted sample in microseconds, nearest rank.
///
/// `None` when the sample is empty: an empty sample has no percentile, and the
/// caller prints the absence rather than a duration nobody measured. The caller
/// refuses a tier of zero before it gets here, so an empty sample would be a
/// refusal and not a result.
fn percentile(sorted: &[Duration], rank: usize) -> Option<u64> {
    let last = sorted.len().checked_sub(1)?;
    let chosen = rank.saturating_mul(sorted.len()).div_ceil(100);
    let micros = sorted.get(chosen.saturating_sub(1).min(last))?.as_micros();
    u64::try_from(micros).ok()
}

/// This process's peak resident set size in KiB, or `None` where the platform
/// does not expose it.
///
/// The absence is reported rather than guessed. `getrusage` would need an FFI
/// edge this crate does not have, so on macOS and the BSDs there is no reading
/// here and the real figure comes from the platform's own tool
/// (`/usr/bin/time -l`) wrapping the run.
fn peak_rss_kib() -> Option<u64> {
    #[cfg(unix)]
    {
        // `/proc` is Linux; on the BSDs and macOS the same number comes from
        // `getrusage`, which this crate cannot reach without an FFI edge, so
        // there is no reading rather than a guess.
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
        let value = line.split_whitespace().nth(1)?;
        value.parse().ok()
    }
    #[cfg(not(unix))]
    {
        None
    }
}
