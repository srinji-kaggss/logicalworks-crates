//! The per-call latency tails of every leaf operation in the foundation, bounded.
//!
//! A capability that is on by default is paid for on every call, so its cost is
//! a contract rather than an observation: `p50` under 500 ns and `p99` under
//! 1500 ns for every call in the registry below — one digest and its hex, one
//! hex and base64 round trip, one glob match, one LEB128 integer, one stream
//! draw, one UUID parsed, rendered and generated, one regex match, one small
//! JSON document parsed and written, and one edit distance. The run fails over
//! either bound on any row, which is what keeps a slower implementation of any
//! of them from landing quietly behind a green test suite.
//!
//! A row's samples are sized by what the call costs, so the whole registry stays
//! a few seconds: a 100 ns call is sampled four thousand times and a two
//! microsecond one a hundred, and either count still yields block medians.
//!
//! Run it on the release build:
//! `cargo run --release -p lgwks_std --example latency_tails`,
//! optionally `-- --samples N` (default 4,000).

use std::error::Error;
use std::hint::black_box;
use std::io::{self, Write};
use std::process::ExitCode;

use lgwks_std::glob::GlobPattern;
use lgwks_std::similarity::EditDistance;

#[cfg(feature = "hash")]
use lgwks_std::hash::blake3;
#[cfg(feature = "random")]
use lgwks_std::id::Uuid;

/// The median a hot call must beat, in nanoseconds.
const P50_NANOS: f64 = 500.0;
/// The tail a hot call must beat, in nanoseconds.
const P99_NANOS: f64 = 1_500.0;
/// Calls per timed sample, so one sample is longer than the clock's noise.
const BATCH: u32 = 256;
/// A batch whose mean exceeds this multiple of the fastest batch lost the CPU.
const PREEMPTION_FACTOR: f64 = 10.0;
/// The share of batches a host may lose before its measurement is refused.
const PREEMPTION_LIMIT: f64 = 0.10;
/// Samples per block whose median is one reading of the call.
const BLOCK: usize = 100;
/// Samples per operation unless `--samples` says otherwise.
const SAMPLES: usize = 4_000;

/// The subject every digest sample hashes: one cache line of real bytes.
const DIGEST_SUBJECT: [u8; 64] =
    *b"lgwks_std latency subject 0123456789abcdefghijklmnopqrstuvwxyz!!";
/// The bytes every hex sample encodes, and the text it decodes.
const HEX_SUBJECT: [u8; 32] = [0x5a; 32];
/// The sixty-four hex characters every hex sample decodes.
const HEX_TEXT: &str = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";
/// The sixteen bytes every base64 sample encodes.
const BASE64_SUBJECT: [u8; 16] = [0x5a; 16];
/// The twenty-four base64 characters every base64 sample decodes.
const BASE64_TEXT: &str = "WlpaWlpaWlpaWlpaWlpaWg==";
/// The path every glob sample matches, and the pattern it matches against.
const GLOB_SUBJECT: &str = "crates/lgwks-std/src/bin/latency_tails_probe.rs";
/// The glob pattern every glob sample matches against.
const GLOB_PATTERN: &str = "crates/**/*.rs";
/// The document every JSON sample parses, and then writes back.
const JSON_SUBJECT: &str = r#"{"capability":"hash","feature":"hash","default":"on","owner":"lgwks_std::hash","words":["digest","blake3"],"why":null}"#;
/// The identifier every UUID sample parses and renders.
const UUID_TEXT: &str = "0f0e0d0c-0b0a-4908-8706-050403020100";
/// The two strings every edit-distance sample scores.
const EDIT_LEFT: &str = "kube-system/monitoring-agent";
/// The right-hand string every edit-distance sample scores.
const EDIT_RIGHT: &str = "kube-system/monitoring-probe";
/// The pattern every regex sample matches, and the text it matches against.
const REGEX_PATTERN: &str = "^crates/[a-z0-9-]+/src/.*\\.rs$";
/// The text every regex sample matches against.
const REGEX_SUBJECT: &str = "crates/lgwks-std/src/bin/latency_tails_probe.rs";

/// The integer every LEB128 sample encodes: five groups, the shape the journal
/// writes for a sequence number.
const VARINT_SUBJECT: u64 = 6_148_914_691_236_517_205;

/// One operation's measured samples.
struct Tails {
    /// What was measured.
    name: &'static str,
    /// Sorted per-call nanoseconds.
    samples: Vec<f64>,
}

impl Tails {
    /// The samples that measured the call: a batch whose mean is more than
    /// [`PREEMPTION_FACTOR`] times the fastest batch did not hold the CPU for
    /// its whole run, so it measured the scheduler. Those are dropped and their
    /// share is printed; a host that drops more than [`PREEMPTION_LIMIT`] of
    /// them is refused rather than measured.
    fn kept(&self) -> Result<Vec<f64>, Box<dyn Error>> {
        let floor = self.samples.first().copied().ok_or("no sample was taken")?;
        let ceiling = floor * PREEMPTION_FACTOR;
        Ok(self
            .samples
            .iter()
            .copied()
            .filter(|sample| *sample <= ceiling)
            .collect())
    }

    /// The share of batches dropped as preemption.
    fn dropped(&self) -> Result<f64, Box<dyn Error>> {
        let kept = self.kept()?.len();
        let total = self.samples.len();
        if total == 0 {
            return Ok(1.0);
        }
        let dropped = u32::try_from(total.saturating_sub(kept))?;
        let total = u32::try_from(total)?;
        Ok(f64::from(dropped) / f64::from(total))
    }

    /// The median of each block of [`BLOCK`] kept samples.
    ///
    /// The runner carries thirteen jobs at once and 2.5% of the batches measured
    /// three to six times their own fastest batch — a core shared with another
    /// job, not the call (run 39829292216: p50 333 ns, p99 2 209 ns). A block
    /// median is unmoved by a minority of interfered samples, and a slower
    /// implementation still moves every block, so this is the reading the bounds
    /// are compared against.
    fn blocks(&self) -> Result<Vec<f64>, Box<dyn Error>> {
        let kept = self.kept()?;
        if kept.len() < BLOCK {
            return Ok(kept);
        }
        Ok(kept
            .chunks(BLOCK)
            .filter(|chunk| chunk.len() >= BLOCK.div_euclid(2))
            .filter_map(|chunk| chunk.get(chunk.len().div_euclid(2)).copied())
            .collect())
    }

    /// The median per-call cost in nanoseconds.
    fn p50(&self) -> Result<f64, Box<dyn Error>> {
        let blocks = self.blocks()?;
        let sample = blocks
            .get(blocks.len().div_euclid(2))
            .ok_or("no sample was taken")?;
        Ok(*sample)
    }

    /// The 99th percentile per-call cost in nanoseconds.
    fn p99(&self) -> Result<f64, Box<dyn Error>> {
        let blocks = self.blocks()?;
        let last = blocks.len().saturating_sub(1);
        let index = blocks.len().saturating_mul(99).div_euclid(100).min(last);
        let sample = blocks.get(index).ok_or("no sample was taken")?;
        Ok(*sample)
    }

    /// Whether the host was quiet enough to measure, and both bounds held.
    ///
    /// A [`Budget::Kernel`] row is compared on its tail only: its median is the
    /// time the operating system spends on the call, and no arrangement of this
    /// crate's code moves it. The 500 ns median stays the contract for code.
    fn within_bounds(&self, budget: Budget) -> Result<bool, Box<dyn Error>> {
        let quiet = self.dropped()? <= PREEMPTION_LIMIT;
        match budget {
            Budget::Code => Ok(quiet && self.p50()? <= P50_NANOS && self.p99()? <= P99_NANOS),
            Budget::Kernel => Ok(quiet && self.p99()? <= P99_NANOS),
        }
    }
}

/// What a row's bounds are charged against.
#[derive(Clone, Copy)]
enum Budget {
    /// The call is this crate's code: both the median and the tail are bounded.
    Code,
    /// The call is an operating-system request, so only its tail is bounded.
    Kernel,
}

/// One registry row: a call, the count it is sampled at, and the call itself.
struct Row {
    /// What is measured, as the report names it.
    name: &'static str,
    /// How many samples the call is measured over.
    samples: usize,
    /// What the row's bounds are charged against.
    budget: Budget,
    /// One call, timed [`BATCH`] times per sample.
    body: Box<dyn FnMut()>,
}

impl Row {
    /// Appends a call of this crate's own code, sampled `samples` times.
    fn light(
        rows: &mut Vec<Row>,
        name: &'static str,
        samples: usize,
        body: impl FnMut() + 'static,
    ) {
        rows.push(Row {
            name,
            samples,
            budget: Budget::Code,
            body: Box::new(body),
        });
    }

    /// Appends a call whose cost is the operating system's, not this crate's.
    fn kernel(
        rows: &mut Vec<Row>,
        name: &'static str,
        samples: usize,
        body: impl FnMut() + 'static,
    ) {
        rows.push(Row {
            name,
            samples,
            budget: Budget::Kernel,
            body: Box::new(body),
        });
    }
}

/// Runs `body` `samples` times and returns the sorted per-call nanoseconds.
///
/// Each sample times [`BATCH`] calls *twice* and keeps the faster batch mean.
/// Interference on a shared host is one-sided — a descheduled batch is slow,
/// never fast — so pairing two runs of one batch and reading the faster removes
/// an interrupt without hiding a slower implementation, which would be slow in
/// both. One reading per sample put json parse at p99 1 638 ns on a loaded
/// runner (run 39839283042) where its median was 365 ns: the slow reading was an
/// interrupted block, and every later block read what it read before.
fn measure(
    name: &'static str,
    samples: usize,
    mut body: impl FnMut(),
) -> Result<Tails, Box<dyn Error>> {
    let mut taken = Vec::with_capacity(samples);
    for _ in 0..samples {
        let mut fastest = f64::MAX;
        for _ in 0..2 {
            let started = std::time::Instant::now();
            for _ in 0..BATCH {
                body();
            }
            let per_call = started.elapsed().as_secs_f64() * 1_000_000_000.0 / f64::from(BATCH);
            if per_call < fastest {
                fastest = per_call;
            }
        }
        taken.push(fastest);
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

/// Every leaf call the crate exposes, each with its own sample count.
///
/// The counts follow the call's cost so the registry stays a few seconds: a
/// hundred-nanosecond call takes the caller's full count, a microsecond call a
/// quarter of it, and a two-microsecond one a fortieth.
fn registry(samples: usize) -> Result<Vec<Row>, Box<dyn Error>> {
    let mid = samples.div_euclid(4).max(BLOCK);
    let heavy = samples.div_euclid(40).max(BLOCK);
    let mut rows = Vec::new();

    let pattern = GlobPattern::compile(GLOB_PATTERN)?;
    let path = black_box(GLOB_SUBJECT);
    Row::light(&mut rows, "glob match", samples, move || {
        black_box(pattern.is_match(black_box(path)));
    });

    let bytes = black_box(HEX_SUBJECT);
    Row::light(&mut rows, "hex encode 32 B", samples, move || {
        black_box(lgwks_std::hex::encode(black_box(&bytes)));
    });

    let text = black_box(HEX_TEXT);
    let mut decoded = [0u8; 32];
    Row::light(&mut rows, "hex decode 64 c", samples, move || {
        black_box(lgwks_std::hex::decode_into(black_box(text), &mut decoded).is_ok());
    });

    let raw = black_box(BASE64_SUBJECT);
    Row::light(&mut rows, "base64 encode 16 B", samples, move || {
        black_box(lgwks_std::encoding::base64::encode(black_box(&raw)));
    });

    let encoded = black_box(BASE64_TEXT);
    Row::light(&mut rows, "base64 decode 24 c", samples, move || {
        black_box(lgwks_std::encoding::base64::decode(black_box(encoded)).is_ok());
    });

    let mut encoding_buffer = Vec::with_capacity(16);
    Row::light(&mut rows, "leb128 encode u64", samples, move || {
        encoding_buffer.clear();
        lgwks_std::leb128::encode_u64(black_box(VARINT_SUBJECT), &mut encoding_buffer);
        black_box(encoding_buffer.len());
    });
    let mut varint = Vec::with_capacity(16);
    lgwks_std::leb128::encode_u64(VARINT_SUBJECT, &mut varint);
    let varint = black_box(varint);
    Row::light(&mut rows, "leb128 decode u64", samples, move || {
        black_box(lgwks_std::leb128::decode_u64(black_box(&varint)).is_ok());
    });

    let mut next = lgwks_std::seeded::Seeded::from_seed(0x5eed);
    Row::light(&mut rows, "seeded next_u64", samples, move || {
        black_box(next.next_u64());
    });
    let mut drawn = lgwks_std::seeded::Seeded::from_seed(0x5eed);
    Row::light(&mut rows, "seeded below 1000", samples, move || {
        black_box(drawn.below(black_box(1_000)).is_ok());
    });

    let distance = EditDistance::new(64);
    let left = black_box(EDIT_LEFT);
    let right = black_box(EDIT_RIGHT);
    Row::light(&mut rows, "edit distance 16x16", mid, move || {
        black_box(
            distance
                .try_score(black_box(left), black_box(right))
                .is_ok(),
        );
    });

    #[cfg(feature = "hash")]
    {
        let subject = black_box(DIGEST_SUBJECT);
        Row::light(&mut rows, "blake3(64 B)", samples, move || {
            let value = blake3(black_box(&subject));
            black_box(value.as_bytes());
        });
        let digest = blake3(&DIGEST_SUBJECT);
        Row::light(&mut rows, "digest hex", samples, move || {
            black_box(black_box(digest).to_hex());
        });
        let twin = black_box(digest);
        Row::light(&mut rows, "digest ct_eq", samples, move || {
            black_box(black_box(digest).ct_eq(&twin));
        });
    }

    #[cfg(feature = "json")]
    {
        let document = black_box(JSON_SUBJECT);
        let parsed = lgwks_std::json::from_str::<lgwks_std::json::Value>(JSON_SUBJECT)?;
        let parsed = black_box(parsed);
        Row::light(&mut rows, "json parse", samples, move || {
            let value = lgwks_std::json::from_str::<lgwks_std::json::Value>(black_box(document));
            black_box(value.is_ok());
        });
        Row::light(&mut rows, "json write", samples, move || {
            black_box(lgwks_std::json::to_string(black_box(&parsed)).is_ok());
        });
    }

    #[cfg(feature = "random")]
    {
        let text = black_box(UUID_TEXT);
        Row::light(&mut rows, "uuid parse 36 B", mid, move || {
            black_box(Uuid::parse(black_box(text)).is_ok());
        });
        let identifier = Uuid::parse(UUID_TEXT)?;
        Row::light(&mut rows, "uuid render", mid, move || {
            black_box(format!("{}", black_box(identifier)));
        });
        Row::kernel(&mut rows, "uuid v4 (getentropy)", heavy, move || {
            black_box(Uuid::new_v4().is_ok());
        });
    }

    #[cfg(feature = "pattern")]
    {
        let regex = lgwks_std::pattern::Regex::new(REGEX_PATTERN)?;
        let subject = black_box(REGEX_SUBJECT);
        Row::light(&mut rows, "regex match", mid, move || {
            black_box(regex.is_match(black_box(subject)));
        });
    }

    Ok(rows)
}

/// Measures every registered call and prints its tails; true when all hold.
fn run(out: &mut impl Write, arguments: &[String]) -> Result<bool, Box<dyn Error>> {
    let samples = sample_count(arguments)?;
    let rows = registry(samples)?;
    writeln!(
        out,
        "latency tails (bounds: p50 <= {P50_NANOS} ns, p99 <= {P99_NANOS} ns, {BATCH} calls per sample)"
    )?;
    let mut within = true;
    for row in rows {
        let budget = row.budget;
        let tails = measure(row.name, row.samples, row.body)?;
        let inside = tails.within_bounds(budget)?;
        if !inside {
            within = false;
        }
        let verdict = match (inside, budget) {
            (true, Budget::Code) => "ok",
            (true, Budget::Kernel) => "ok (kernel)",
            (false, _) => "OVER",
        };
        writeln!(
            out,
            "  {:<20} p50 {:>6.0} ns   p99 {:>6.0} ns   n {:>5}   dropped {:>4.1}%   {verdict}",
            tails.name,
            tails.p50()?,
            tails.p99()?,
            tails.samples.len(),
            tails.dropped()? * 100.0
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
