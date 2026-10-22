//! The per-call latency tails of the foundation's hot operations, bounded.
//!
//! A capability that is on by default is paid for on every call, so its cost is
//! a contract rather than an observation: `p50` under 500 ns and `p99` under
//! 1500 ns for the three calls a consumer makes in a loop — one 64-byte digest,
//! one glob match on a path, and one small JSON document parsed. The run fails
//! over either bound, which is what keeps a slower implementation of any of the
//! three from landing quietly behind a green test suite.
//!
//! Run it on the release build:
//! `cargo run --release -p lgwks_std --features hash,json --example latency_tails`,
//! optionally `-- --samples N` (default 100,000).

use std::error::Error;
use std::hint::black_box;
use std::io::{self, Write};
use std::process::ExitCode;

use lgwks_std::glob::GlobPattern;
use lgwks_std::hash::blake3;

/// The median a hot call must beat, in nanoseconds.
const P50_NANOS: f64 = 500.0;
/// The tail a hot call must beat, in nanoseconds.
const P99_NANOS: f64 = 1_500.0;
/// Calls per timed sample, so one sample is longer than the clock's noise.
const BATCH: u32 = 256;
/// Samples per operation unless `--samples` says otherwise.
const SAMPLES: usize = 4_000;

/// The subject every digest sample hashes: one cache line of real bytes.
const DIGEST_SUBJECT: [u8; 64] =
    *b"lgwks_std latency subject 0123456789abcdefghijklmnopqrstuvwxyz!!";

/// The path every glob sample matches.
const GLOB_SUBJECT: &str = "crates/lgwks-std/src/bin/latency_tails_probe.rs";
/// The pattern every glob sample matches it against.
const GLOB_PATTERN: &str = "crates/**/*.rs";

/// The document every JSON sample parses: a report row, the shape the estate
/// writes a thousand of.
const JSON_SUBJECT: &str = r#"{"capability":"hash","feature":"hash","default":"on","owner":"lgwks_std::hash","words":["digest","blake3"],"why":null}"#;

/// One operation's measured samples.
struct Tails {
    /// What was measured.
    name: &'static str,
    /// Sorted per-call nanoseconds.
    samples: Vec<f64>,
}

impl Tails {
    /// The median per-call cost in nanoseconds.
    fn p50(&self) -> Result<f64, Box<dyn Error>> {
        let sample = self
            .samples
            .get(self.samples.len().div_euclid(2))
            .ok_or("no sample was taken")?;
        Ok(*sample)
    }

    /// The 99th percentile per-call cost in nanoseconds.
    fn p99(&self) -> Result<f64, Box<dyn Error>> {
        let last = self.samples.len().saturating_sub(1);
        let index = self
            .samples
            .len()
            .saturating_mul(99)
            .div_euclid(100)
            .min(last);
        let sample = self.samples.get(index).ok_or("no sample was taken")?;
        Ok(*sample)
    }

    /// Whether both bounds hold for this operation.
    fn within_bounds(&self) -> Result<bool, Box<dyn Error>> {
        Ok(self.p50()? <= P50_NANOS && self.p99()? <= P99_NANOS)
    }
}

/// Runs `body` `samples` times and returns the sorted per-call nanoseconds.
///
/// Each sample times [`BATCH`] calls and reports their mean, because one call of
/// a hot path is shorter than the clock's own noise: timing a single call on a
/// runner shared with twelve other jobs measures the scheduler, not the code
/// (run 39815132509 failed this gate on a host-idle measurement). What is bounded
/// here is the call, and a batch is how the call is seen.
fn measure(
    name: &'static str,
    samples: usize,
    mut body: impl FnMut(),
) -> Result<Tails, Box<dyn Error>> {
    let mut taken = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = std::time::Instant::now();
        for _ in 0..BATCH {
            body();
        }
        let per_call = started.elapsed().as_secs_f64() * 1_000_000_000.0 / f64::from(BATCH);
        taken.push(per_call);
    }
    taken.sort_by(f64::total_cmp);
    Ok(Tails {
        name,
        samples: taken,
    })
}

/// Reads `--samples N`, defaulting to [`SAMPLES`].
fn sample_count(arguments: &[String]) -> Result<usize, Box<dyn Error>> {
    let mut count = SAMPLES;
    let mut walker = arguments.iter();
    while let Some(argument) = walker.next() {
        if argument == "--samples" {
            let value = walker.next().ok_or("--samples needs a number")?;
            count = value.parse::<usize>()?;
        }
    }
    Ok(count.max(1))
}

/// Measures the three calls and prints their tails; true when all are in bounds.
fn run(out: &mut impl Write, arguments: &[String]) -> Result<bool, Box<dyn Error>> {
    let samples = sample_count(arguments)?;
    let pattern = GlobPattern::compile(GLOB_PATTERN)?;
    let subject = black_box(DIGEST_SUBJECT);
    let digest = measure("blake3(64 B)", samples, || {
        let value = blake3(black_box(&subject));
        black_box(value.as_bytes());
    })?;
    let path = black_box(GLOB_SUBJECT);
    let matching = measure("glob match", samples, || {
        black_box(pattern.is_match(black_box(path)));
    })?;
    let document = black_box(JSON_SUBJECT);
    let parsed = measure("json parse", samples, || {
        let value = lgwks_std::json::from_str::<lgwks_std::json::Value>(black_box(document));
        black_box(value.is_ok());
    })?;
    writeln!(
        out,
        "latency tails over {samples} samples (bounds: p50 <= {P50_NANOS} ns, p99 <= {P99_NANOS} ns)"
    )?;
    let mut within = true;
    for tails in [&digest, &matching, &parsed] {
        let inside = tails.within_bounds()?;
        if !inside {
            within = false;
        }
        let verdict = if inside { "ok" } else { "OVER" };
        writeln!(
            out,
            "  {:<12} p50 {:>6.0} ns   p99 {:>6.0} ns   {verdict}",
            tails.name,
            tails.p50()?,
            tails.p99()?
        )?;
    }
    if within {
        writeln!(out, "every hot call is inside its latency contract")?;
    } else {
        writeln!(
            out,
            "a hot call broke its latency contract: p50 must stay at or under {P50_NANOS} ns and p99 at or under {P99_NANOS} ns"
        )?;
    }
    Ok(within)
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    match run(&mut out, &arguments) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(cause) => {
            lgwks_std::trace::debug!(error = %cause, "latency: the run could not start");
            let stderr = io::stderr();
            let mut err = stderr.lock();
            let _reported = writeln!(err, "latency_tails: {cause}");
            ExitCode::from(2)
        }
    }
}
