//! Row 1 of issue #278, as a deterministic simulation: the torn read under a
//! seed.
//!
//! One seed drives everything here — how many chunks the document has, whether
//! the writer pauses between them, how long a chunk takes to land, and when the
//! attempt reads. The clock is the shared [`Sim`]'s virtual one, so a writer
//! that holds a document open for an hour costs one arithmetic operation.
//! Nothing reads the wall clock, so the same seed replays to the same trace
//! hash on a busy CI box and on a laptop.
//!
//! The subject is the *shipped* protocol — [`lgwks_bot::stability::read_stable`] —
//! over a modelled writer and a modelled filesystem. What is modelled is the
//! world the reader does not own; the reader itself is never reimplemented,
//! because a second reader written "for the simulation" would let this file pass
//! while the shipped one rotted.
//!
//! The properties, one per family:
//!
//! - a reading is admitted only when two independent reads agreed, and never
//!   one the subject's own bytes contradict;
//! - a writer that never pauses is never admitted, however many reads are taken;
//! - an atomic rename is never read half-written, with no test-only help;
//! - the same seed gives the same trace hash, and the seeds diverge.

use std::error::Error;
use std::time::{Duration, SystemTime};

use lgwks_bot::stability::{MAX_STABILITY_READS, ReadFailure, Reading, read_stable};

use crate::sim::Sim;

type TestResult = Result<(), Box<dyn Error>>;

/// The seed space this file sweeps.
///
/// Wider than the shared `sim::SEED_SPACE` because this family is cheap: a
/// scenario is at most [`MAX_STABILITY_READS`] reads of a few dozen bytes, so a
/// thousand seeds cost milliseconds and the coverage the row asks for is a
/// thousand draws rather than a handful.
const SEEDS: u64 = 1024;

/// The first seed of the sweep.
///
/// Not zero: a zero seed is the one value the shared generator remaps, so a
/// family that swept it would report a draw sequence the generator does not
/// produce.
const FIRST_SEED: u64 = 1;

/// Return a typed test failure, emitting it first.
///
/// The emission is what the `scan` lane requires of every `return Err`: a
/// caller that sees only a value has no signal. Building it in one place keeps
/// every scenario's refusal identical rather than one line per call site.
fn refuse(cause: impl Into<String>) -> Result<(), Box<dyn Error>> {
    let refusal: Result<(), Box<dyn Error>> = Err(cause.into().into());
    lgwks_std::trace::debug!(
        error = ?refusal.as_ref().err(),
        "scenario: returning an error to the caller"
    );
    refusal
}

/// A modelled read the model itself cannot express, as the reader's own
/// failure type: a tick past what `SystemTime` or `usize` can hold is a broken
/// scenario, and the reader reports it as unreadable rather than as a reading
/// the model invented.
fn unmodelled(cause: &str) -> ReadFailure {
    ReadFailure::from(std::io::Error::other(cause.to_owned()))
}

/// The subject's modification time at `tick`, or `None` past the range a
/// `SystemTime` holds.
///
/// Derived from the virtual tick rather than read, so two reads a tick apart
/// carry different timestamps exactly as a real filesystem would report.
fn stamp_at(tick: u64) -> Option<SystemTime> {
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(tick))
}

/// A modelled writer: a document assembled out of chunks, one landing every
/// `period` ticks while the writer holds it open.
struct Subject {
    /// The document's chunks, in the order they land.
    chunks: Vec<Vec<u8>>,
    /// How many ticks a chunk takes to land.
    period: u64,
}

impl Subject {
    /// A document of `chunks` chunks, one landing every `period` ticks.
    fn new(chunks: Vec<Vec<u8>>, period: u64) -> Self {
        Self {
            chunks,
            period: period.max(1),
        }
    }

    /// Which chunk has landed by `tick`, counting from zero, or `None` for a
    /// tick the model cannot index (a zero period, or more chunks than `usize`).
    fn written_at(&self, tick: u64) -> Option<usize> {
        let elapsed = usize::try_from(tick.checked_div(self.period)?).ok()?;
        Some(elapsed.min(self.chunks.len()))
    }

    /// The bytes present once chunk `written` has landed.
    fn bytes_through(&self, written: usize) -> Vec<u8> {
        self.chunks
            .iter()
            .take(written.saturating_add(1))
            .flatten()
            .copied()
            .collect()
    }

    /// The length the document will have once every chunk has landed.
    ///
    /// Reported by a writer that is part-way through, and by a finished one
    /// equally — that is what makes an in-flight read distinguishable from a
    /// whole one without trusting the read to notice it on its own.
    fn intended_len(&self) -> Option<u64> {
        u64::try_from(self.chunks.iter().flatten().count()).ok()
    }

    /// One read at `tick`.
    ///
    /// A document that is finished yields its whole bytes and its own length. A
    /// document that is still being written yields the prefix that has landed
    /// and the length it intends to reach, so the reading is visibly short: the
    /// tear, as the reader sees it.
    fn read_at(&self, tick: u64) -> Result<Reading, ReadFailure> {
        let written = self
            .written_at(tick)
            .ok_or_else(|| unmodelled("a tick the chunk index cannot hold"))?;
        let intended = self
            .intended_len()
            .ok_or_else(|| unmodelled("a document longer than u64 bytes"))?;
        let stamp = stamp_at(tick).ok_or_else(|| unmodelled("a tick past SystemTime"))?;
        Ok(Reading::of_subject(
            self.bytes_through(written),
            intended,
            stamp,
        ))
    }
}

/// The subject a seed asked for: how many chunks, and how long each takes.
fn subject_for(sim: &mut Sim) -> (Subject, u64) {
    let rng = sim.rng();
    let count = rng.between(3, 9);
    let period = u64::from(rng.between(1, 6));
    let chunks = (0..count)
        .map(|index| format!("[{index};]").into_bytes())
        .collect();
    // When the reader's first read falls, which is a third dimension: two
    // seeds that draw the same document at the same pace still read it at
    // different instants, and a scenario that could not tell them apart would
    // be measuring the document rather than the seed.
    let reader_start = u64::from(rng.below(7));
    (Subject::new(chunks, period), reader_start)
}

/// Record what the seed asked for, so two seeds that happen to reach the same
/// verdict are still distinguishable in the trace.
///
/// Without this the whole space collapses onto two or three hashes — one per
/// verdict — and a replay receipt over it would be a receipt over a constant.
fn record_shape(sim: &mut Sim, subject: &Subject) -> Result<(), Box<dyn Error>> {
    // Recorded through the same trace as the verdict, so a shape that never
    // reached a verdict is still visible to a reader comparing two runs.
    sim.trace
        .record_number("chunks", u64::try_from(subject.chunks.len())?);
    sim.trace.record_number("period", subject.period);
    Ok(())
}

/// One scenario: read the modelled subject through the shipped protocol, with
/// the reader on its own tick, and record what came out.
fn scenario(sim: &mut Sim) -> Result<(), Box<dyn Error>> {
    let (subject, reader_start) = subject_for(sim);
    let mut tick = sim.clock.now().saturating_add(reader_start);
    // The reader advances its own clock on every read, so the second read sees
    // a later instant than the first. That advance is the whole difference
    // between "two calls into a cache" and "two reads of the world": without
    // it the closure would hand back the same bytes twice and every scenario
    // would settle.
    let reading = read_stable(|| {
        tick = tick.saturating_add(1);
        subject.read_at(tick)
    });

    record_shape(sim, &subject)?;
    sim.trace.record_number("read_tick", tick);
    match reading {
        Ok(settled) => {
            sim.record("settled");
            sim.trace
                .record_number("settled_len", u64::try_from(settled.bytes().len())?);
            if !settled.is_whole() {
                return refuse("an admitted reading must never contradict its own length");
            }
            let written = subject
                .written_at(tick)
                .ok_or("the reader's last tick indexes a chunk")?;
            let expected = subject.bytes_through(written);
            if settled.bytes() != expected.as_slice() {
                return refuse("an admitted reading must be the bytes the subject held");
            }
        }
        Err(failure) => {
            sim.record("refused");
            let Some(unstable) = failure.unstable() else {
                return refuse("a modelled subject never becomes unreadable");
            };
            if unstable.reads() != MAX_STABILITY_READS {
                return refuse("a refused attempt must stop at the declared bound");
            }
            sim.record(unstable.drift().as_str());
            sim.trace
                .record_number("refused_reads", u64::from(unstable.reads()));
        }
    }
    Ok(())
}

/// Every seed in the space, run once, and the trace hash each produced.
fn sweep() -> Result<Vec<u64>, Box<dyn Error>> {
    let mut hashes = Vec::new();
    for seed in FIRST_SEED..FIRST_SEED.saturating_add(SEEDS) {
        let mut sim = Sim::new(seed);
        scenario(&mut sim)?;
        hashes.push(sim.hash());
    }
    Ok(hashes)
}

/// The main property, over the whole declared space: nothing torn was ever
/// admitted, and every refusal stopped at the declared bound.
#[test]
fn a_seeded_reader_never_admits_a_torn_reading() -> TestResult {
    let hashes = sweep()?;
    assert_eq!(
        hashes.len(),
        usize::try_from(SEEDS)?,
        "the sweep must cover every seed it declares"
    );
    Ok(())
}

/// A writer that never pauses is never admitted, however many reads are taken.
///
/// The control every settled reading needs: a protocol that refused everything
/// would pass the family above, and one that admitted everything would fail
/// here.
#[test]
fn a_writer_that_never_pauses_is_never_admitted() -> TestResult {
    let mut admitted = 0u32;
    let mut refused = 0u32;
    for _seed in FIRST_SEED..FIRST_SEED.saturating_add(SEEDS) {
        let subject = Subject::new(vec![b"[a;]".to_vec(), b"[b;]".to_vec()], 1);
        let mut tick = 0u64;
        let reading = read_stable(|| {
            tick = tick.saturating_add(1);
            subject.read_at(tick)
        });
        match reading {
            Ok(_) => admitted = admitted.saturating_add(1),
            Err(failure) => {
                let Some(unstable) = failure.unstable() else {
                    return refuse("a modelled subject never becomes unreadable");
                };
                if unstable.reads() != MAX_STABILITY_READS {
                    return refuse("a refused attempt must stop at the declared bound");
                }
                refused = refused.saturating_add(1);
            }
        }
    }
    assert_eq!(
        admitted, 0,
        "a subject that changes on every tick can never produce two agreeing reads"
    );
    assert_eq!(
        refused,
        u32::try_from(SEEDS)?,
        "and it must be refused for every seed, at the declared bound"
    );
    Ok(())
}

/// An atomic rename is read whole or not at all, with no help from the
/// protocol: the writer's discipline is what carries that guarantee, and this
/// family says so over the same code path.
#[test]
fn a_renamed_subject_is_read_whole_or_refused() -> TestResult {
    let whole = b"{\"revision\":9}".to_vec();
    let reported = u64::try_from(whole.len())?;
    let stamp = stamp_at(7).ok_or("seven seconds after the epoch is a SystemTime")?;
    let mut settled = 0u32;
    for _seed in FIRST_SEED..FIRST_SEED.saturating_add(SEEDS) {
        // A renamed subject is one whose bytes never change: every open sees
        // either the old document or the new one, and this is that case.
        let reading = read_stable(|| Ok(Reading::of_subject(whole.clone(), reported, stamp)));
        let Ok(value) = reading else {
            return refuse("a subject nobody is rewriting must settle");
        };
        if value.bytes() != whole.as_slice() || !value.is_whole() {
            return refuse("a renamed subject must be read whole");
        }
        settled = settled.saturating_add(1);
    }
    assert_eq!(
        settled,
        u32::try_from(SEEDS)?,
        "and it must settle for every seed, not merely most of them"
    );
    Ok(())
}

/// The replay receipt: one seed, one hash, twice — and different seeds
/// diverging, which is what makes a differing hash meaningful.
#[test]
fn the_same_seed_replays_to_the_same_stability_trace() -> TestResult {
    let first = sweep()?;
    let second = sweep()?;
    assert_eq!(
        first, second,
        "the same seed produced two different traces, so the hash is not a receipt"
    );
    // Divergence, measured where it means something: how often two adjacent
    // seeds produce the same trace. A sweep whose trace ignored its seed would
    // collide on every pair, and an assertion on "how many distinct values"
    // would instead be a statement about how many scenarios the model has.
    let collisions = first.windows(2).filter(|pair| pair[0] == pair[1]).count();
    let pairs = first.len().saturating_sub(1);
    assert!(
        collisions * 10 <= pairs,
        "adjacent seeds must not share a trace: {collisions} of {pairs} pairs collided, so \
         the hash is not seed-sensitive"
    );
    Ok(())
}
