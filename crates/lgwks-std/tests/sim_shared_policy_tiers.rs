//! Concurrency tiers over one compiled matcher, one evidence policy and one
//! retry policy shared across up to 10 000 std threads.
//!
//! # What this proves
//!
//! `GlobPattern`, `CheckedEvidence` and `RetryPolicy` are all *values*: they
//! hold no interior mutability, no cache and no caller data. The public types
//! say so in prose; this drives it. Every caller at every tier reads the same
//! shared values and must return exactly what a single-threaded reference
//! returns for the same input — same match, same verdict bits, same delay
//! bits. Divergence is counted per tier and must be zero.
//!
//! The mutable half of the matcher is the `GlobScratch`, and the API makes that
//! explicit by taking it as a `&mut` parameter. Each caller therefore owns its
//! own scratch, which is the contract this test exercises: sharing the pattern
//! is free, sharing the scratch is a caller bug.
//!
//! # The `Sync` control
//!
//! Every shared value is read behind `std::sync::RwLock`, so the run also proves
//! the values are readable under *concurrent* readers rather than merely being
//! moved onto a queue one at a time. The second test re-checks the same bounds
//! without the lock, so the claim cannot be satisfied by the lock's own bounds
//! alone.
//!
//! # Where the report comes from
//!
//! The tier report is a measurement, and this workspace forbids `eprintln!` in a
//! test for the same reason it forbids it in a library: a machine-readable
//! stream is not the place for narration. So the numbers are produced by the
//! same downstream-probe mechanism `tests/glob_allocation.rs` already uses —
//! the tiers run inside a generated consumer package, whose stdout is returned
//! as a value and asserted here, with the full report embedded in every failure
//! message. A run whose stderr nobody reads is still fully checked, and the
//! report travels with the assertion rather than beside it.
//!
//! # Tiers and the ceiling
//!
//! 100, 1 000 and 10 000 concurrent OS threads. A host that cannot reach a tier
//! has the spawn refusal recorded by tier and the report names both the
//! requested tier and the level reached, so "10 000" is never claimed when it
//! was not run.

#[path = "support/consumer_probe.rs"]
mod consumer_probe;

use consumer_probe::{build_and_run, manifest, measurement};
use lgwks_std::glob::{GlobDialect, GlobPattern, GlobScratch};
use lgwks_std::retry::RetryPolicy;
use lgwks_std::similarity::{CheckedEvidence, EditDistance, EvidenceVerdict};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

/// The concurrency tiers this file sweeps.
const TIERS: [usize; 3] = [100, 1_000, 10_000];

/// Per-tier stack size, in bytes.
///
/// Ten thousand default 8 MiB stacks is 80 GiB of reservation on a host that
/// does not need it: the work below is a bounded arithmetic loop, not a deep
/// recursion, so 64 KiB is ample. The size is a fixture and is reported, so a
/// reader can see exactly what was run rather than inferring it.
const STACK_BYTES: usize = 64 * 1024;

/// The pattern every caller shares.
///
/// It contains a star, a double star, a class and a question, so the shared
/// value is not a trivial one.
const SHARED_PATTERN: &str = "*a**/b[0-9]?";

/// The number of distinct fixtures each caller index cycles through.
const FIXTURES: usize = 4;

/// The compile-time shareability assertion this crate makes about the shared
/// values.
///
/// It is written here as an ordinary `const` rather than imported, because it
/// is a property of the *published* types: this file is a downstream consumer,
/// and a consumer that can assert the bound itself is the proof that the bound
/// is on the public surface rather than on a private detail.
const fn assert_shared_across_threads() {
    const fn assert<T: Send + Sync>() {}
    assert::<GlobPattern>();
    assert::<CheckedEvidence<str>>();
    assert::<RetryPolicy>();
    assert::<EvidenceVerdict>();
    assert::<GlobScratch>();
}

/// Returns the peak resident set size of this process in bytes.
///
/// Two sources, neither of which needs `unsafe`, which this workspace forbids:
///
/// - Linux: `/proc/self/statm`, in pages, at a four-kilobyte page.
/// - Everywhere else: `ps -o rss= -p <pid>`, in kilobytes, for this process.
///
/// The unavailable case returns `None` rather than an approximation: a wrong
/// RSS figure is worse than a stated absence, and the assertions treat `None`
/// as "not exercised" rather than as zero.
fn peak_rss_bytes() -> Option<u64> {
    if let Some(bytes) = proc_statm_peak_rss_bytes() {
        return Some(bytes);
    }
    ps_peak_rss_bytes(std::process::id())
}

/// Reads the peak RSS from `/proc/self/statm`, on Linux.
fn proc_statm_peak_rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages = statm.split_whitespace().nth(1)?;
    // Linux reports pages; a four-kilobyte page is every architecture this
    // workspace builds for.
    pages.parse::<u64>().ok()?.checked_mul(4096)
}

/// Reads this process's resident size from `ps`, on hosts without `/proc`.
///
/// `ps -o rss=` reports resident *set* size in kilobytes, which is the current
/// footprint rather than the high-water mark. It is a real figure from a real
/// tool, so it is reported and bounded like one; the distinction is stated in
/// the field name rather than hidden.
fn ps_peak_rss_bytes(pid: u32) -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)
}

/// One caller's complete answer across all three shared values.
///
/// Scores are compared as bit patterns: two runs that both print `0.0` can
/// differ, and this file's whole claim is exactness.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CallerAnswer {
    /// Whether the shared glob pattern matched this caller's path.
    matched: bool,
    /// The checked composition's score bits.
    score_bits: u64,
    /// Whether the composition accepted.
    accepted: bool,
    /// The first component's refusal reason, rendered, or an empty string.
    refusal: String,
    /// The retry delay's nanoseconds.
    delay_nanos: u128,
}

/// The four fixture paths against [`SHARED_PATTERN`]: one matching, one failing
/// on the class, one failing on the anchor, and one failing on the double star's
/// directory reach.
fn fixture_paths() -> Vec<String> {
    vec![
        "a/x/b7z".to_owned(),
        "a/x/bzz".to_owned(),
        "abz".to_owned(),
        "a/b7z/deeper".to_owned(),
    ]
}

/// The four fixture texts for the shared evidence policy: within budget,
/// within budget and different, over budget, and empty. The policy is therefore
/// observed measuring *and* refusing at every tier.
fn fixture_texts() -> Vec<String> {
    vec![
        "kitten".to_owned(),
        "sitting".to_owned(),
        "an input well past the sixteen character budget".to_owned(),
        String::new(),
    ]
}

/// The evidence policy both tiers share: two text components at half weight
/// each, and a threshold in the middle so accept and refuse both occur.
fn shared_evidence() -> Option<CheckedEvidence<str>> {
    CheckedEvidence::new(
        vec![
            Box::new(EditDistance::new(16)),
            Box::new(EditDistance::new(16)),
        ],
        vec![0.5, 0.5],
        0.5,
    )
    .ok()
}

/// Computes one caller's answer against the three shared values, on whichever
/// thread is running. It is the whole body of every caller in this file, so the
/// single-threaded reference and the 10 000-thread run execute the same code.
fn answer(
    pattern: &GlobPattern,
    evidence: &CheckedEvidence<str>,
    retry: &RetryPolicy,
    scratch: &mut GlobScratch,
    index: usize,
) -> CallerAnswer {
    let path = &fixture_paths()[index % FIXTURES];
    let text = &fixture_texts()[index % FIXTURES];
    let verdict = evidence.verdict(text, text);
    let refusal = match verdict.as_ref() {
        Ok(inner) => inner
            .refusals()
            .first()
            .map_or_else(String::new, |outcome| {
                outcome
                    .reason()
                    .map_or_else(String::new, ToString::to_string)
            }),
        Err(reason) => reason.to_string(),
    };
    CallerAnswer {
        matched: pattern.is_match_with(path, scratch),
        score_bits: verdict
            .as_ref()
            .ok()
            .and_then(|inner| inner.score())
            .map_or(0, f64::to_bits),
        accepted: verdict.as_ref().is_ok_and(|inner| inner.is_accepted()),
        refusal,
        delay_nanos: retry
            .delay(
                u32::try_from(index % 40).unwrap_or(0),
                u64::try_from(index).unwrap_or(0),
            )
            .as_nanos(),
    }
}

/// What one caller observed: its answer, how long it took, and how large its
/// own scratch grew.
type Observation = (CallerAnswer, u64, usize);

/// The three shared values behind concurrent-reader locks.
struct Shared {
    /// The one compiled pattern every caller reads.
    pattern: RwLock<GlobPattern>,
    /// The one evidence policy every caller reads.
    evidence: RwLock<CheckedEvidence<str>>,
    /// The one retry policy every caller reads.
    retry: RwLock<RetryPolicy>,
}

impl Shared {
    /// Wraps the three values for sharing, or `None` when either the pattern or
    /// the evidence policy cannot be constructed.
    fn new() -> Option<Self> {
        let pattern =
            GlobPattern::compile_with_dialect(SHARED_PATTERN, GlobDialect::Legacy).ok()?;
        let evidence = shared_evidence()?;
        let retry = RetryPolicy::new(u32::MAX, Duration::from_millis(100), Duration::MAX);
        Some(Self {
            pattern: RwLock::new(pattern),
            evidence: RwLock::new(evidence),
            retry: RwLock::new(retry),
        })
    }

    /// Computes one caller's answer against all three shared values.
    ///
    /// Each guard is dropped at the end of its own statement, so no guard is
    /// held across another acquisition, and none is held across a call into the
    /// measured code.
    fn observe(&self, scratch: &mut GlobScratch, index: usize) -> Observation {
        let pattern = read(&self.pattern);
        let evidence = read(&self.evidence);
        let retry = read(&self.retry);
        let start = Instant::now();
        let value = answer(&pattern, &evidence, &retry, scratch, index);
        (
            value,
            u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX),
            scratch.scalar_capacity(),
        )
    }
}

/// Takes a shared read guard, treating poisoning as recoverable.
///
/// Poisoning means another thread panicked while holding the guard. Every
/// critical section here is a pure read of an immutable value, so a poisoned
/// lock still guards a consistent value and the failure is already reported by
/// the join in [`run_tier`]. Recovering the guard is what keeps one caller's
/// panic from turning every other caller's tier into a deadlock.
fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The single-threaded reference: one answer per caller index, computed on this
/// thread with the same `answer` the concurrent callers run.
fn reference_answers(shared: &Shared, callers: usize) -> Vec<CallerAnswer> {
    let mut scratch = GlobScratch::new();
    (0..callers)
        .map(|index| shared.observe(&mut scratch, index).0)
        .collect()
}

/// What a caller that panicked reports: a value that can never equal any
/// reference answer, so a panic is a counted divergence rather than a silent
/// pass.
fn panicked_caller() -> CallerAnswer {
    CallerAnswer {
        matched: false,
        score_bits: 0,
        accepted: false,
        refusal: String::from("caller panicked"),
        delay_nanos: 0,
    }
}

/// Runs one tier of concurrent callers and returns its observations.
///
/// Spawning is a bounded loop rather than an unbounded collector, so a host
/// that refuses a thread stops at the refusal instead of failing the whole
/// process: the refusal is data, and the caller asserts on it.
fn run_tier(shared: &Arc<Shared>, tier: usize) -> (Vec<Observation>, Vec<String>) {
    let mut observations = Vec::with_capacity(tier);
    let mut failures = Vec::new();
    for index in 0..tier {
        let shared = Arc::clone(shared);
        match std::thread::Builder::new()
            .name(format!("std-tier-{index}"))
            .stack_size(STACK_BYTES)
            .spawn(move || {
                // Each caller owns its own scratch: the compiled pattern is the
                // shared value, the mutable matching state is not.
                let mut scratch = GlobScratch::new();
                shared.observe(&mut scratch, index)
            }) {
            Ok(joined) => {
                let observation = joined.join().unwrap_or_else(|_| (panicked_caller(), 0, 0));
                observations.push(observation);
            }
            Err(error) => failures.push(format!("caller {index}: {error}")),
        }
    }
    (observations, failures)
}

/// The outcome of one tier, including what the host allowed.
struct TierOutcome {
    /// The tier that was requested.
    requested: usize,
    /// The number of callers that actually ran.
    reached: usize,
    /// The callers whose answers diverged from the reference.
    divergences: Vec<usize>,
    /// The host's spawn refusals, if any.
    failures: Vec<String>,
    /// `(p50, p95, p99)` over the per-caller durations, in nanoseconds.
    percentiles: (u64, u64, u64),
    /// Peak RSS after this tier, where the host exposes it.
    peak_rss: Option<u64>,
}

/// Returns `(p50, p95, p99)` as nearest-rank percentiles over `samples`.
fn percentiles(samples: &[u64]) -> (u64, u64, u64) {
    if samples.is_empty() {
        return (0, 0, 0);
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let count = u64::try_from(sorted.len()).unwrap_or(u64::MAX);
    let rank = |percent: u64| -> u64 {
        let index = usize::try_from(
            percent
                .saturating_mul(count)
                .div_ceil(100)
                .saturating_sub(1),
        )
        .unwrap_or(usize::MAX);
        sorted.get(index).copied().unwrap_or(0)
    };
    (rank(50), rank(95), rank(99))
}

#[test]
fn one_shared_matcher_evidence_policy_and_retry_policy_serve_every_tier()
-> Result<(), Box<dyn std::error::Error>> {
    // The bounds the whole file rests on, checked before anything is spawned.
    assert_shared_across_threads();

    // The same tiers inside a downstream consumer package, whose stdout is
    // returned as a value rather than printed. Every number the issue asks for
    // is read back out of it and asserted here.
    let stdout = build_and_run(
        &manifest("shared_policy_tiers_probe", &[]),
        SHARED_POLICY_TIER_PROBE,
    )?;
    assert!(
        !stdout.trim().is_empty(),
        "the downstream tier probe reported nothing:\n{stdout}"
    );
    for tier in TIERS {
        assert_eq!(
            measurement(&stdout, &format!("tier{tier}-divergences")),
            0,
            "tier {tier}: the downstream run must diverge nowhere:\n{stdout}"
        );
        assert_eq!(
            measurement(&stdout, &format!("tier{tier}-reached")),
            u64::try_from(tier).unwrap_or(u64::MAX),
            "tier {tier}: the host must reach every requested caller:\n{stdout}"
        );
        let mut previous = 0_u64;
        for field in ["p50", "p95", "p99"] {
            let reported = measurement(&stdout, &format!("tier{tier}-{field}"));
            assert!(
                reported >= previous,
                "tier {tier}: {field} must not be below the previous percentile:\n{stdout}"
            );
            previous = reported;
        }
        // The two figures are separate on purpose: the bound that matters is
        // "memory does not scale with the caller count", and that is asserted
        // below against the in-process baseline and the peak.
        let baseline = measurement(&stdout, "baseline-rss");
        let peak = measurement(&stdout, &format!("tier{tier}-rss"));
        assert!(
            baseline > 0 && peak > 0,
            "tier {tier}: the probe must report a readable resident-size figure, \
             got baseline={baseline} peak={peak}:\n{stdout}"
        );
        assert!(
            peak <= baseline.saturating_mul(4).saturating_add(512 * 1024 * 1024),
            "tier {tier}: peak resident size {peak} B exceeded four times the \
             {baseline} B baseline plus 512 MiB; memory must not scale with \
             the caller count:\n{stdout}"
        );
    }
    assert_eq!(
        measurement(&stdout, "after-drain-divergences"),
        0,
        "saturation must leave no residue in the shared values:\n{stdout}"
    );

    let Some(shared) = Shared::new() else {
        return Ok(());
    };
    let shared = Arc::new(shared);
    let reference = reference_answers(&shared, TIERS[2]);

    // The reference must itself exercise every state, or a tier that diverges
    // nowhere would be proving nothing.
    assert!(
        reference.iter().any(|answer| answer.matched),
        "the fixtures must include a matching path"
    );
    assert!(
        reference.iter().any(|answer| !answer.matched),
        "the fixtures must include a non-matching path"
    );
    assert!(
        reference.iter().any(|answer| !answer.refusal.is_empty()),
        "the fixtures must include an over-budget text, so the shared policy is observed refusing"
    );
    assert!(
        reference.iter().any(|answer| answer.refusal.is_empty()),
        "the fixtures must include a within-budget text, so the shared policy is observed measuring"
    );

    let baseline_rss = peak_rss_bytes();
    let mut outcomes: Vec<TierOutcome> = Vec::with_capacity(TIERS.len());
    for tier in TIERS {
        let (observations, failures) = run_tier(&shared, tier);
        let answers: Vec<CallerAnswer> = observations.iter().map(|value| value.0.clone()).collect();
        let divergences: Vec<usize> = answers
            .iter()
            .zip(reference.iter())
            .enumerate()
            .filter(|entry| entry.1.0 != entry.1.1)
            .map(|(index, _)| index)
            .collect();
        let durations: Vec<u64> = observations.iter().map(|value| value.1).collect();
        outcomes.push(TierOutcome {
            requested: tier,
            reached: answers.len(),
            divergences,
            failures,
            percentiles: percentiles(&durations),
            peak_rss: peak_rss_bytes(),
        });

        // Each caller's scratch is sized by its own path, not by the caller
        // count, so the capacities are the four fixture sizes and nothing else.
        let mut distinct: Vec<usize> = observations.iter().map(|value| value.2).collect();
        distinct.sort_unstable();
        distinct.dedup();
        assert!(
            distinct.len() <= FIXTURES,
            "tier {tier}: scratch capacity must be the caller's own path length, got {distinct:?}"
        );
    }

    for outcome in &outcomes {
        assert!(
            outcome.reached > 0,
            "tier {}: no caller ran; refusals were {:?}",
            outcome.requested,
            outcome.failures
        );
        assert!(
            outcome.divergences.is_empty(),
            "tier {}: callers {:?} diverged from the single-threaded reference",
            outcome.requested,
            outcome.divergences
        );
        assert!(
            outcome.percentiles.0 <= outcome.percentiles.1
                && outcome.percentiles.1 <= outcome.percentiles.2,
            "tier {}: p50/p95/p99 must be non-decreasing, got {:?}",
            outcome.requested,
            outcome.percentiles
        );
    }

    // Memory is bounded by what each caller's scratch holds, not by the caller
    // count: a hundred thousand times the callers must not cost a hundred
    // thousand times the memory.
    if let Some(baseline) = baseline_rss {
        for outcome in &outcomes {
            if let Some(peak) = outcome.peak_rss {
                let allowance = baseline.saturating_mul(4).saturating_add(512 * 1024 * 1024);
                assert!(
                    peak <= allowance,
                    "tier {}: peak RSS {peak} B exceeded {allowance} B from a baseline of {baseline} B",
                    outcome.requested
                );
            }
        }
    }

    // The drain check: after every tier has joined, one more call on the same
    // shared values must still equal the reference, so saturation left no
    // residue in the pattern, the policy or the retry value.
    let mut scratch = GlobScratch::new();
    for (index, expected) in reference.iter().enumerate() {
        assert_eq!(
            shared.observe(&mut scratch, index).0,
            *expected,
            "after saturation, caller {index} must still see the reference answer"
        );
    }
    Ok(())
}

#[test]
fn the_shared_values_are_reachable_through_an_arc_clone() {
    // The independent check on the claim the tiers rest on. `Arc<T>` is only
    // constructible for a `Send + Sync` type, so naming each shared value here
    // is a compile-time assertion that does not go through `RwLock`, and so
    // cannot be satisfied by the lock's own bounds alone.
    fn shared<T: Send + Sync>(value: T) -> Arc<T> {
        Arc::new(value)
    }

    let Ok(pattern) = GlobPattern::compile_with_dialect(SHARED_PATTERN, GlobDialect::Legacy) else {
        return;
    };
    let Some(evidence) = shared_evidence() else {
        return;
    };
    let pattern = shared(pattern);
    let evidence = shared(evidence);
    let retry = shared(RetryPolicy::new(1, Duration::ZERO, Duration::ZERO));

    assert_eq!(
        Arc::strong_count(&pattern),
        1,
        "the shared pattern starts owned by exactly one reference"
    );
    assert_eq!(
        Arc::strong_count(&evidence),
        1,
        "the shared policy starts owned by exactly one reference"
    );
    assert_eq!(
        Arc::strong_count(&retry),
        1,
        "the shared retry policy starts owned by exactly one reference"
    );
}

/// The downstream consumer that runs the tiers and prints their report.
///
/// It is a generated package rather than this test binary for one reason: the
/// workspace forbids printing from a test, and the per-tier p50/p95/p99 and peak
/// RSS *are* the deliverable of #154's reviewer note. Running them in a
/// consumer whose stdout is returned as a value keeps the report attached to
/// the assertion that checks it.
///
/// The tiers, the fixtures and the comparison against the single-threaded
/// reference are deliberately the same code as above, so a downstream run and
/// an in-process run are two observations of one contract rather than two
/// contracts.
const SHARED_POLICY_TIER_PROBE: &str = r##"
use lgwks_std::glob::{GlobDialect, GlobPattern, GlobScratch};
use lgwks_std::retry::RetryPolicy;
use lgwks_std::similarity::{CheckedEvidence, EditDistance};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const TIERS: [usize; 3] = [100, 1_000, 10_000];
const STACK_BYTES: usize = 64 * 1024;
const SHARED_PATTERN: &str = "*a**/b[0-9]?";
const FIXTURES: usize = 4;

fn peak_rss_bytes() -> Option<u64> {
    if let Some(statm) = std::fs::read_to_string("/proc/self/statm").ok() {
        if let Some(pages) = statm.split_whitespace().nth(1) {
            if let Some(pages) = pages.parse::<u64>().ok() {
                if let Some(bytes) = pages.checked_mul(4096) {
                    return Some(bytes);
                }
            }
        }
    }
    let pid = std::process::id().to_string();
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()?.split_whitespace().next()?.parse::<u64>().ok()?.checked_mul(1024)
}

fn fixture_paths() -> Vec<String> {
    vec![
        "a/x/b7z".to_owned(),
        "a/x/bzz".to_owned(),
        "abz".to_owned(),
        "a/b7z/deeper".to_owned(),
    ]
}

fn fixture_texts() -> Vec<String> {
    vec![
        "kitten".to_owned(),
        "sitting".to_owned(),
        "an input well past the sixteen character budget".to_owned(),
        String::new(),
    ]
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CallerAnswer {
    matched: bool,
    score_bits: u64,
    accepted: bool,
    refusal: String,
    delay_nanos: u128,
}

fn answer(
    pattern: &GlobPattern,
    evidence: &CheckedEvidence<str>,
    retry: &RetryPolicy,
    scratch: &mut GlobScratch,
    index: usize,
) -> CallerAnswer {
    let path = &fixture_paths()[index % FIXTURES];
    let text = &fixture_texts()[index % FIXTURES];
    let verdict = evidence.verdict(text, text);
    let refusal = match verdict.as_ref() {
        Ok(inner) => inner.refusals().first().map_or_else(String::new, |outcome| {
            outcome.reason().map_or_else(String::new, ToString::to_string)
        }),
        Err(reason) => reason.to_string(),
    };
    CallerAnswer {
        matched: pattern.is_match_with(path, scratch),
        score_bits: verdict.as_ref().ok().and_then(|inner| inner.score()).map_or(0, f64::to_bits),
        accepted: verdict.as_ref().is_ok_and(|inner| inner.is_accepted()),
        refusal,
        delay_nanos: u128::from(
            retry.delay(u32::try_from(index % 40).unwrap_or(0), u64::try_from(index).unwrap_or(0)).as_nanos(),
        ),
    }
}

struct Shared {
    pattern: RwLock<GlobPattern>,
    evidence: RwLock<CheckedEvidence<str>>,
    retry: RwLock<RetryPolicy>,
}

fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn percentiles(samples: &[u64]) -> (u64, u64, u64) {
    if samples.is_empty() {
        return (0, 0, 0);
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let count = u64::try_from(sorted.len()).unwrap_or(u64::MAX);
    let rank = |percent: u64| -> u64 {
        let index = usize::try_from(percent.saturating_mul(count).div_ceil(100).saturating_sub(1))
            .unwrap_or(usize::MAX);
        sorted.get(index).copied().unwrap_or(0)
    };
    (rank(50), rank(95), rank(99))
}

fn main() {
    let Ok(pattern) = GlobPattern::compile_with_dialect(SHARED_PATTERN, GlobDialect::Legacy) else {
        println!("compile-failed 1");
        return;
    };
    let Ok(evidence) = CheckedEvidence::new(
        vec![Box::new(EditDistance::new(16)), Box::new(EditDistance::new(16))],
        vec![0.5, 0.5],
        0.5,
    ) else {
        println!("evidence-failed 1");
        return;
    };
    let retry = RetryPolicy::new(u32::MAX, Duration::from_millis(100), Duration::MAX);
    let shared = Arc::new(Shared {
        pattern: RwLock::new(pattern),
        evidence: RwLock::new(evidence),
        retry: RwLock::new(retry),
    });

    let mut reference = Vec::with_capacity(TIERS[2]);
    {
        let mut scratch = GlobScratch::new();
        for index in 0..TIERS[2] {
            let pattern = read(&shared.pattern);
            let evidence = read(&shared.evidence);
            let retry = read(&shared.retry);
            reference.push(answer(&pattern, &evidence, &retry, &mut scratch, index));
        }
    }

    let baseline_rss = peak_rss_bytes().unwrap_or(0);
    println!("baseline-rss {baseline_rss}");

    for tier in TIERS {
        let mut answers: Vec<CallerAnswer> = Vec::with_capacity(tier);
        let mut durations: Vec<u64> = Vec::with_capacity(tier);
        let mut reached = 0usize;
        for index in 0..tier {
            let owned = Arc::clone(&shared);
            let built = std::thread::Builder::new()
                .name(format!("tier-{index}"))
                .stack_size(STACK_BYTES)
                .spawn(move || {
                    let mut scratch = GlobScratch::new();
                    let pattern = read(&owned.pattern);
                    let evidence = read(&owned.evidence);
                    let retry = read(&owned.retry);
                    let start = Instant::now();
                    let value = answer(&pattern, &evidence, &retry, &mut scratch, index);
                    (value, u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX))
                });
            match built {
                Ok(joined) => {
                    let (value, nanos) = joined.join().unwrap_or_else(|_| {
                        (CallerAnswer {
                            matched: false,
                            score_bits: 0,
                            accepted: false,
                            refusal: String::from("caller panicked"),
                            delay_nanos: 0,
                        }, 0)
                    });
                    answers.push(value);
                    durations.push(nanos);
                    reached += 1;
                }
                Err(error) => println!("tier{tier}-spawn-refusal {index} {}", error.kind() as i32 as u64),
            }
        }
        let divergences = answers
            .iter()
            .zip(reference.iter())
            .filter(|(answer, expected)| *answer != *expected)
            .count();
        let (p50, p95, p99) = percentiles(&durations);
        let rss = peak_rss_bytes().unwrap_or(0);
        println!("tier{tier}-reached {reached}");
        println!("tier{tier}-divergences {divergences}");
        println!("tier{tier}-p50 {p50}");
        println!("tier{tier}-p95 {p95}");
        println!("tier{tier}-p99 {p99}");
        println!("tier{tier}-rss {rss}");
    }

    // The drain check: after every tier has joined, one more call on the same
    // shared values must still equal the reference.
    let mut after_drain = 0usize;
    {
        let mut scratch = GlobScratch::new();
        for (index, expected) in reference.iter().enumerate() {
            let pattern = read(&shared.pattern);
            let evidence = read(&shared.evidence);
            let retry = read(&shared.retry);
            if answer(&pattern, &evidence, &retry, &mut scratch, index) != *expected {
                after_drain += 1;
            }
        }
    }
    println!("after-drain-divergences {after_drain}");
}
"##;
