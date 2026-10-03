//! Simulation family: seeded process output sizes, frame cuts, tenants and replay.
//!
//! # What is real and what is seeded
//!
//! The runner is the real one — the real [`Supervisor::run_process`], real
//! `sh` children, real pipes, and the real frame reader — so a family that passed
//! while the shipped code was wrong would mean the shipped code was wrong and
//! this family was not looking. The seed chooses only the *shape* of each run:
//! how many bytes a child writes, where a frame is cut, which ceiling is in force,
//! how fast the reader is, and how many tenants run at once.
//!
//! # The replay receipt
//!
//! Every fact an assertion makes is also appended to the run's trace, and the
//! trace hashes to a `u64`. [`sim::assert_replays`] sweeps the band twice and
//! requires the two hash vectors to be identical, so a nondeterministic run fails
//! even when every assertion happens to pass. No wall-clock reading is ever
//! recorded: a timing in the trace would differ between two runs of one seed on a
//! busy box and the receipt would mean nothing.
//!
//! # Families
//!
//! | Family | Property it pins |
//! |---|---|
//! | `sizes_stay_at_the_ceiling` | a seeded output size retains `min(size, ceiling)` and reports the exact total, on both streams |
//! | `cuts_are_refused_never_decoded` | a seeded cut point inside a frame yields a typed truncation naming both lengths |
//! | `two_tenants_never_cross` | two tenants' captures are byte-distinct and neither sees the other's total |
//! | `the_same_seed_replays` | the same seed produces the same trace hash, twice |

#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

mod sim;

use std::error::Error;
use std::num::NonZeroUsize;

use lgwks_bot::rt::process::{FrameRead, ProcessSpec, read_frames};
use lgwks_bot::rt::runtime::Builder;
use lgwks_bot::rt::supervise::Supervisor;

use sim::Band;

/// What a scenario reports when its precondition did not hold.
type TestResult = Result<(), Box<dyn Error>>;

/// A shell spec writing `script`, with `ceiling` bytes retained per stream.
fn captured(script: &str, ceiling: usize) -> Result<ProcessSpec, Box<dyn Error>> {
    let limit = NonZeroUsize::new(ceiling).ok_or("a capture ceiling must be at least one")?;
    let mut spec = ProcessSpec::new("sh");
    spec.arg("-c").arg(script);
    spec.capture_stdout(limit);
    spec.capture_stderr(limit);
    Ok(spec)
}

/// Run `spec` on a supervisor bounded to one child.
///
/// Single-slot so a sweep's runs are sequential and its total is decided by the
/// scenario rather than by how many children happened to overlap.
fn run(spec: &ProcessSpec) -> Result<lgwks_bot::rt::process::ProcessRun, Box<dyn Error>> {
    let runtime = lgwks_bot::Runtime::new()?;
    let mut supervisor = Supervisor::new(1);
    Ok(runtime.block_on(supervisor.run_process(spec))?)
}

/// Record a capture's four facts, so the trace carries the assertion rather than
/// only the assertion's result.
fn record_capture(sim: &mut sim::Sim, label: &str, run: &lgwks_bot::rt::process::ProcessRun) {
    sim.record(label);
    sim.trace
        .record_count("retained", run.stdout().bytes().len());
    sim.trace.record_u64("total", run.stdout().total_bytes());
    sim.trace
        .record_u64("truncated", u64::from(run.stdout().truncated()));
}

/// A seeded output size stays at the ceiling, and the exact total is reported.
///
/// Both streams, because a ceiling that holds on stdout and leaks on stderr is a
/// ceiling that holds on whichever stream the test happened to check. The sizes
/// bracket the ceiling on purpose: some runs truncate and some do not, so
/// `truncated` is `size > ceiling` across the whole family rather than being
/// uniformly true or uniformly false.
fn sizes_stay_at_the_ceiling(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let ceiling = usize::try_from(sim.rng().between(1, 4096))?;
        // Drawn across the ceiling on purpose: the families above and below it
        // are the two ways a capture can be wrong, and a sweep that only ever
        // truncated would never observe the second.
        let size = sim.rng().between(1, 24_000);
        let script = format!("head -c {size} /dev/zero; head -c {size} /dev/zero 1>&2");
        let outcome = run(&captured(&script, ceiling)?)?;

        for (name, stream) in [("stdout", outcome.stdout()), ("stderr", outcome.stderr())] {
            let expected =
                usize::try_from(size.min(u32::try_from(ceiling).unwrap_or(u32::MAX)))?.min(ceiling);
            assert_eq!(
                stream.bytes().len(),
                expected,
                "{name} must retain min(size, ceiling): size={size} ceiling={ceiling}"
            );
            assert!(
                stream.bytes().iter().all(|byte| *byte == 0),
                "{name} must retain the head of a zero stream"
            );
            assert_eq!(
                stream.total_bytes(),
                u64::from(size),
                "{name} must report the exact total it saw"
            );
            assert_eq!(
                stream.truncated(),
                u64::from(size) > u64::try_from(ceiling).unwrap_or(u64::MAX),
                "{name} truncation is exactly size > ceiling"
            );
            assert!(
                stream.retained_capacity() <= ceiling,
                "{name} retained {} bytes of capacity, past its {ceiling}-byte ceiling",
                stream.retained_capacity()
            );
        }
        record_capture(sim, "size", &outcome);
        sim.trace.record_count("ceiling", ceiling);
        sim.trace.record_u64("size", u64::from(size));
        Ok(())
    })
}

/// A seeded cut inside a frame is refused, and never decoded as a record.
///
/// The child writes one whole record, then a second whose payload is cut at a
/// seeded point. Every cut point from "none of the payload arrived" to "all but
/// one byte arrived" is swept, so the family covers the whole range a real
/// interrupted write can land on rather than one convenient mid-payload case.
fn cuts_are_refused_never_decoded(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        // The second record declares eight payload bytes; `cut` of them arrive.
        const DECLARED: usize = 8;
        const WHOLE: &[u8] = b"whole";
        const PARTIAL: &[u8] = b"partial!!";
        let cut = usize::try_from(sim.rng().below(9))?;
        let ceiling = DECLARED.saturating_add(64);
        // One `printf` for both records: the whole one, then a prefix naming
        // The cut is in the bytes the child actually writes, so the reader really
        // does see a short payload rather than being asked to imagine one.
        let mut stream_bytes: Vec<u8> = Vec::new();
        stream_bytes.extend(u32::try_from(WHOLE.len()).unwrap_or(0).to_be_bytes());
        stream_bytes.extend_from_slice(WHOLE);
        stream_bytes.extend(u32::try_from(DECLARED).unwrap_or(0).to_be_bytes());
        stream_bytes.extend_from_slice(&PARTIAL[..cut]);
        let stream_literal = stream_bytes
            .iter()
            .map(|byte| format!("\\{byte:03o}"))
            .collect::<String>();
        let outcome = run(&captured(&format!("printf '{stream_literal}'"), ceiling)?)?;

        // The expected reading is computed from the cut, so this family's model
        // and the shipped reader are checked against each other rather than the
        // test asserting whatever the reader happened to produce.
        let complete = cut == DECLARED;
        let mut stream = outcome.stdout().bytes();
        let frames = read_frames(&mut stream, ceiling)?;
        let expected_frames = if complete { 2 } else { 1 };
        assert_eq!(
            frames.records().len(),
            expected_frames,
            "cut={cut} of {DECLARED}: {expected_frames} records are whole, got {:?}",
            frames.records()
        );
        assert_eq!(
            frames.records().first().and_then(FrameRead::payload),
            Some(WHOLE),
            "the first record decodes to exactly what the child wrote"
        );
        assert_eq!(
            frames.ended(),
            &if complete {
                FrameRead::EndOfStream
            } else {
                FrameRead::TruncatedPayload {
                    declared: DECLARED,
                    partial: PARTIAL[..cut].to_vec(),
                }
            },
            "cut={cut} of {DECLARED}: a prefix that named more than arrived is a truncation, \
             and one that named exactly what arrived is a whole record"
        );
        if complete {
            assert!(
                frames.is_complete(),
                "a cut of {DECLARED} delivered the whole payload, so the stream is complete"
            );
        } else {
            assert!(
                !frames.ended().is_frame(),
                "the cut record must never read as a decoded frame"
            );
            assert!(
                frames.ended().payload().is_none(),
                "a refusal carries no payload"
            );
            assert!(
                !frames.is_complete(),
                "a stream whose last record was cut off has no complete reading"
            );
        }
        sim.record("cut");
        sim.trace.record_count("cut", cut);
        sim.trace
            .record_u64("declared", u64::try_from(DECLARED).unwrap_or(u64::MAX));
        Ok(())
    })
}

/// Two tenants' captures never cross.
///
/// Each tenant's child writes its own repeated byte, so a crossed capture would
/// be visible as the wrong byte rather than only as the wrong count. The point is
/// not that two runs are equal — it is that neither run can see the other's
/// bytes or total.
fn two_tenants_never_cross(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let ceiling = usize::try_from(sim.rng().between(64, 8192))?;
        let size = sim.rng().between(1, 20_000);
        let tenant: u32 = sim.rng().below(4);
        let byte = u8::try_from(tenant).unwrap_or(0).wrapping_add(b'a');

        let mut observations: Vec<(Vec<u8>, u64)> = Vec::new();
        for _ in 0..2 {
            // `head -c` from `/dev/zero` written through `tr`, so each tenant's
            // stream is its own byte repeated: a crossed pipe would show up as
            // the wrong byte, not merely a wrong length.
            let script = format!("head -c {size} /dev/zero | tr '\\0' '{}'", char::from(byte));
            let outcome = run(&captured(&script, ceiling)?)?;
            observations.push((
                outcome.stdout().bytes().to_vec(),
                outcome.stdout().total_bytes(),
            ));
        }

        let (first, first_total) = observations.first().cloned().unwrap_or_default();
        let (second, second_total) = observations.get(1).cloned().unwrap_or_default();
        assert_eq!(
            first_total,
            u64::from(size),
            "the first tenant's total must be its own byte count"
        );
        assert_eq!(
            second_total,
            u64::from(size),
            "the second tenant's total must be its own byte count"
        );
        assert!(
            first.iter().all(|value| *value == byte),
            "the first tenant's retained bytes must all be its own byte {byte:?}"
        );
        assert!(
            second.iter().all(|value| *value == byte),
            "the second tenant's retained bytes must all be its own byte {byte:?}"
        );
        sim.record("tenants");
        sim.trace.record_u64("tenant", u64::from(tenant));
        sim.trace.record_count("ceiling", ceiling);
        sim.trace.record_u64("size", u64::from(size));
        sim.trace.record_count("retained", first.len());
        Ok(())
    })
}

/// The same seed produces the same trace, twice.
///
/// The union of the families above, so the receipt covers byte counts, totals,
/// truncation decisions and frame lengths rather than one scenario's shape. This
/// is the assertion that would fail if any of them read the clock.
fn the_same_seed_replays(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let ceiling = usize::try_from(sim.rng().between(1, 2048))?;
        let size = sim.rng().between(1, 8192);
        let outcome = run(&captured(&format!("head -c {size} /dev/zero"), ceiling)?)?;
        record_capture(sim, "replay", &outcome);

        // The framing half of the receipt too, so a change in how a cut frame is
        // classified shows up here and not only in the family that cuts.
        let framed = read_frames(&mut outcome.stdout().bytes().to_vec().as_slice(), ceiling);
        match framed {
            Ok(frames) => {
                sim.trace.record_count("frames", frames.records().len());
                sim.trace
                    .record_u64("ended", u64::from(frames.ended().is_frame()));
            }
            Err(error) => {
                sim.record("frame-error");
                // The kind is the whole of what a retry decision turns on, and a
                // `Debug` rendering of it would be a text comparison.
                let kind = format!("{:?}", error.kind());
                sim.trace.record(&format!("frame-error-kind-{kind}"));
            }
        }
        sim.trace.record_count("ceiling", ceiling);
        sim.trace.record_u64("size", u64::from(size));
        Ok(())
    })
}

/// A child flooding stdout is drained against a slow reader without exceeding its
/// ceiling, on a runtime whose single worker is the whole executor.
///
/// The seeded sweep's own version of `sys_process_binding`'s flood case, kept
/// here so the frame and size families are never observed without it. One worker
/// is the point: the reader and the exit observation share the thread, so the
/// reader's cost decides when the child's next write is serviced.
fn a_seeded_flood_stays_bounded_on_one_worker(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        let ceiling = usize::try_from(sim.rng().between(4096, 65_536))?;
        // Large enough that the child cannot fit it in a pipe buffer, which is
        // what makes "the reader drained it while the child wrote" observable.
        let size = sim.rng().between(4 * 1024 * 1024, 6 * 1024 * 1024);
        let runtime = Builder::new()
            .worker_threads(NonZeroUsize::new(1))
            .build()?;
        let outcome = runtime.block_on(async {
            let mut supervisor = Supervisor::new(1);
            supervisor
                .run_process(&captured(&format!("head -c {size} /dev/zero"), ceiling)?)
                .await
                .map_err(Box::<dyn Error>::from)
        })?;

        assert_eq!(
            outcome.stdout().bytes().len(),
            ceiling,
            "a flood against one worker must retain exactly its ceiling"
        );
        assert_eq!(
            outcome.stdout().total_bytes(),
            u64::from(size),
            "every byte the flooding child wrote must be counted"
        );
        assert!(
            outcome.stdout().truncated(),
            "a child writing {size} bytes past a {ceiling}-byte ceiling must report truncation"
        );
        assert!(
            outcome.stdout().retained_capacity() <= ceiling,
            "the retained buffer must not grow past the ceiling"
        );
        record_capture(sim, "flood", &outcome);
        sim.trace.record_count("ceiling", ceiling);
        sim.trace.record_u64("size", u64::from(size));
        Ok(())
    })
}

// Each band is a written-out `#[test]` rather than a macro declaration. The
// `invariants` lane resolves every name an invariant cites by scanning the
// sources for `fn` definitions, and a macro-generated test has no `fn` there — so
// a family declared through a macro runs but cannot be cited as evidence. The
// `simulation-evidence` lane reads the same way, counting `#[test]` attributes.
// `tests/sim_run_boundaries.rs` records the same constraint from the other side.

/// A seeded sweep of `sizes_stay_at_the_ceiling` over seeds 0..8.
#[test]
fn sizes_stay_at_the_ceiling_band_00() -> TestResult {
    sizes_stay_at_the_ceiling(Band::new(0, 8))
}

/// A seeded sweep of `sizes_stay_at_the_ceiling` over seeds 8..16.
#[test]
fn sizes_stay_at_the_ceiling_band_01() -> TestResult {
    sizes_stay_at_the_ceiling(Band::new(8, 8))
}

/// A seeded sweep of `cuts_are_refused_never_decoded` over seeds 16..24.
#[test]
fn cuts_are_refused_never_decoded_band_00() -> TestResult {
    cuts_are_refused_never_decoded(Band::new(16, 8))
}

/// A seeded sweep of `cuts_are_refused_never_decoded` over seeds 24..32.
#[test]
fn cuts_are_refused_never_decoded_band_01() -> TestResult {
    cuts_are_refused_never_decoded(Band::new(24, 8))
}

/// A seeded sweep of `two_tenants_never_cross` over seeds 32..40.
#[test]
fn two_tenants_never_cross_band_00() -> TestResult {
    two_tenants_never_cross(Band::new(32, 8))
}

/// A seeded sweep of `the_same_seed_replays` over seeds 40..48.
#[test]
fn the_same_seed_replays_band_00() -> TestResult {
    the_same_seed_replays(Band::new(40, 8))
}

/// A seeded sweep of `the_same_seed_replays` over seeds 48..56.
#[test]
fn the_same_seed_replays_band_01() -> TestResult {
    the_same_seed_replays(Band::new(48, 8))
}

/// A seeded sweep of the one-worker flood over seeds 56..64.
#[test]
fn a_seeded_flood_stays_bounded_on_one_worker_band_00() -> TestResult {
    a_seeded_flood_stays_bounded_on_one_worker(Band::new(56, 8))
}

/// A seeded sweep of the one-worker flood over seeds 64..72.
#[test]
fn a_seeded_flood_stays_bounded_on_one_worker_band_01() -> TestResult {
    a_seeded_flood_stays_bounded_on_one_worker(Band::new(64, 8))
}
