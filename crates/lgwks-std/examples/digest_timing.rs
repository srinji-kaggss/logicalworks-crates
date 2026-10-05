//! A dudect-style timing test of `Digest` equality (#275).
//!
//! Two input classes are compared, interleaved in a fixed pseudo-random
//! order: a digest against an equal copy of itself, and a digest against one
//! that differs in its first byte. An early-exit comparison is fastest in the
//! second class, so a leak shows as a difference in the two classes' mean
//! time. Welch's t statistic measures that difference; dudect's threshold is
//! |t| < 4.5 for "no leak detected".
//!
//! The same harness runs on a deliberately early-exit byte loop, the negative
//! control. It must report |t| far above the threshold, or the harness could
//! not see a leak and its verdict on `Digest` would mean nothing. The run fails
//! when either side is wrong.
//!
//! Run it on the release build: `cargo run --release -p lgwks_std --features
//! hash --example digest_timing`, optionally `-- --samples N` (per class,
//! default 1,000,000) and `--json`.

use std::error::Error;
use std::hint::black_box;
use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Instant;

use lgwks_std::hash::{Digest, Hasher, blake3};

/// dudect's threshold: |t| below it is "no leak detected".
const THRESHOLD: f64 = 4.5;
/// Comparisons per timed sample, so one sample spans many timer ticks.
const BATCH: u32 = 512;
/// Samples above this percentile of the pooled distribution are interrupts
/// and context switches, not the comparison; dudect crops them the same way.
const CROP_PERCENT: usize = 95;

/// Running mean and variance of one class (Welford).
#[derive(Default)]
struct Moments {
    /// Samples seen.
    count: u32,
    /// Their running mean.
    mean: f64,
    /// Sum of squared deviations from the running mean.
    m2: f64,
}

impl Moments {
    /// Fold one sample into the moments.
    fn push(&mut self, sample: f64) {
        self.count = self.count.saturating_add(1);
        let delta = sample - self.mean;
        self.mean += delta / f64::from(self.count);
        self.m2 += delta * (sample - self.mean);
    }

    /// Unbiased sample variance; zero below two samples.
    fn variance(&self) -> f64 {
        if self.count < 2 {
            return 0.0;
        }
        self.m2 / f64::from(self.count.saturating_sub(1))
    }
}

/// Welch's t between two classes.
fn welch_t(equal: &Moments, differ: &Moments) -> f64 {
    let spread =
        equal.variance() / f64::from(equal.count) + differ.variance() / f64::from(differ.count);
    if spread <= 0.0 {
        return 0.0;
    }
    (equal.mean - differ.mean) / spread.sqrt()
}

/// The comparison under test, `Digest`'s own equality.
#[inline(never)]
fn digest_eq(left: &Digest, right: &Digest) -> bool {
    left == right
}

/// The hand-written XOR/OR fold `Digest` used before #275, measured for the
/// record rather than gated: it shows what the delegation costs and whether
/// the optimiser turned the fold into an early exit on this target.
#[inline(never)]
fn xor_fold_eq(left: &Digest, right: &Digest) -> bool {
    let mut acc = 0_u8;
    for (left_byte, right_byte) in left.as_bytes().iter().zip(right.as_bytes()) {
        acc |= left_byte ^ right_byte;
    }
    acc == 0
}

/// The negative control: a byte loop that returns at the first difference.
#[inline(never)]
fn early_exit_eq(left: &Digest, right: &Digest) -> bool {
    for (left_byte, right_byte) in left.as_bytes().iter().zip(right.as_bytes()) {
        if black_box(*left_byte) != black_box(*right_byte) {
            return false;
        }
    }
    true
}

/// The class of each sample, fixed by a seed so a run replays exactly.
fn schedule(samples_per_class: u32) -> Vec<bool> {
    let total = usize::try_from(samples_per_class)
        .unwrap_or(usize::MAX)
        .saturating_mul(2);
    let mut classes = Vec::with_capacity(total);
    let mut block: u64 = 0;
    while classes.len() < total {
        let mut hasher = Hasher::new();
        hasher.update(b"digest_timing schedule").update(&block.to_le_bytes());
        for byte in hasher.finalize().as_bytes() {
            for bit in 0..8_u32 {
                classes.push(byte.checked_shr(bit).is_some_and(|shifted| shifted & 1 == 1));
            }
        }
        block = block.saturating_add(1);
    }
    classes.truncate(total);
    classes
}

/// Time `compare` over the two classes and return Welch's t on the cropped
/// samples, with the class means in nanoseconds per comparison.
fn measure(compare: fn(&Digest, &Digest) -> bool, classes: &[bool]) -> (f64, f64, f64) {
    let base = blake3(b"digest_timing base");
    let equal = Digest::from_bytes(*base.as_bytes());
    let mut differing_bytes = *base.as_bytes();
    if let Some(first) = differing_bytes.first_mut() {
        *first ^= 0x01;
    }
    let differ = Digest::from_bytes(differing_bytes);

    let mut timed: Vec<(bool, u32)> = Vec::with_capacity(classes.len());
    for &is_differ in classes {
        let right = if is_differ { &differ } else { &equal };
        let started = Instant::now();
        for _ in 0..BATCH {
            black_box(compare(black_box(&base), black_box(right)));
        }
        let nanos = u32::try_from(started.elapsed().as_nanos()).unwrap_or(u32::MAX);
        timed.push((is_differ, nanos));
    }

    let mut sorted: Vec<u32> = timed.iter().map(|&(_, nanos)| nanos).collect();
    sorted.sort_unstable();
    let cut = sorted
        .get(
            sorted
                .len()
                .saturating_mul(CROP_PERCENT)
                .checked_div(100)
                .unwrap_or(0),
        )
        .copied()
        .unwrap_or(u32::MAX);

    let (mut equal_moments, mut differ_moments) = (Moments::default(), Moments::default());
    for &(is_differ, nanos) in &timed {
        if nanos > cut {
            continue;
        }
        let per_compare = f64::from(nanos) / f64::from(BATCH);
        if is_differ {
            differ_moments.push(per_compare);
        } else {
            equal_moments.push(per_compare);
        }
    }
    (
        welch_t(&equal_moments, &differ_moments),
        equal_moments.mean,
        differ_moments.mean,
    )
}

fn main() -> Result<ExitCode, Box<dyn Error>> {
    let mut samples: u32 = 1_000_000;
    let mut json = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--samples" => {
                let value = args.next().ok_or("--samples needs a count")?;
                samples = value
                    .parse()
                    .map_err(|err| format!("--samples {value:?}: {err}"))?;
            }
            "--json" => json = true,
            other => return Err(format!("unknown argument {other:?}").into()),
        }
    }
    if samples < 2 {
        return Err("--samples must be at least 2".into());
    }

    let classes = schedule(samples);
    let (digest_t, digest_equal_ns, digest_differ_ns) = measure(digest_eq, &classes);
    let (control_t, control_equal_ns, control_differ_ns) = measure(early_exit_eq, &classes);
    let (fold_t, fold_equal_ns, fold_differ_ns) = measure(xor_fold_eq, &classes);
    let digest_ok = digest_t.abs() < THRESHOLD;
    let control_ok = control_t.abs() >= THRESHOLD;

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let target = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
    if json {
        writeln!(
            out,
            "{{\"target\":\"{target}\",\"samples_per_class\":{samples},\"batch\":{BATCH},\"threshold\":{THRESHOLD},\
             \"digest_eq\":{{\"t\":{digest_t:.3},\"equal_ns\":{digest_equal_ns:.3},\"differ_ns\":{digest_differ_ns:.3},\"pass\":{digest_ok}}},\
             \"early_exit_control\":{{\"t\":{control_t:.3},\"equal_ns\":{control_equal_ns:.3},\"differ_ns\":{control_differ_ns:.3},\"detected\":{control_ok}}},\
             \"xor_fold_pre_275\":{{\"t\":{fold_t:.3},\"equal_ns\":{fold_equal_ns:.3},\"differ_ns\":{fold_differ_ns:.3}}}}}"
        )?;
    } else {
        writeln!(
            out,
            "target {target}: {samples} samples per class, {BATCH} comparisons per sample, threshold |t| < {THRESHOLD}"
        )?;
        writeln!(
            out,
            "digest_eq          t = {digest_t:>9.3}  equal {digest_equal_ns:.3} ns  differ {digest_differ_ns:.3} ns  {}",
            if digest_ok { "no leak detected" } else { "LEAK" }
        )?;
        writeln!(
            out,
            "early_exit control t = {control_t:>9.3}  equal {control_equal_ns:.3} ns  differ {control_differ_ns:.3} ns  {}",
            if control_ok {
                "leak detected, as it must be"
            } else {
                "NOT DETECTED: the harness cannot see a leak"
            }
        )?;
        writeln!(
            out,
            "xor_fold (pre-#275) t = {fold_t:>9.3}  equal {fold_equal_ns:.3} ns  differ {fold_differ_ns:.3} ns  (recorded, not gated)"
        )?;
    }
    Ok(if digest_ok && control_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
