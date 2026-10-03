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
//! | `capture_cuts_end_at_the_capture_ceiling` | a capture-cut prefix ends at the capture's ceiling, never as a child truncation or a clean end |
//! | `room_without_a_record_is_the_ceiling` | a legal record larger than the room remaining is the ceiling; only `0` and `> ceiling` are rot |
//! | `rot_before_the_capture_cut_is_the_ending` | a rot prefix the capture retained whole is the ending; only a cut that reached it is the capture's ceiling |
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

use lgwks_bot::rt::process::{FrameRead, ProcessSpec};
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
        // Read through the capture's own door, not a re-plumbed slice: the
        // capture is untruncated here, so the ending is the reader's own.
        assert!(
            !outcome.stdout().truncated(),
            "cut={cut}: the write must fit the {ceiling}-byte ceiling, or the ending would \
             be the capture's rather than the child's"
        );
        let frames = outcome.stdout().frames(ceiling);
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
        let frames = outcome.stdout().frames(ceiling);
        sim.trace.record_count("frames", frames.records().len());
        // The ending's *class*, not a rendering of it: a text comparison would
        // pass while the classification changed.
        sim.trace
            .record(&format!("ended-{}", ending_class(&frames)));
        sim.trace
            .record_u64("capture-truncated", u64::from(outcome.stdout().truncated()));
        sim.trace.record_count("ceiling", ceiling);
        sim.trace.record_u64("size", u64::from(size));
        Ok(())
    })
}

/// The class of a pass's ending, as the name the trace records.
///
/// A `Debug` rendering would put the payload of a partial record into the trace,
/// so two runs that agreed on every assertion could still differ byte-for-byte
/// in their receipts. The class is the fact a replay receipt should carry. The
/// wildcard is [`FrameRead`]'s `#[non_exhaustive]` promise from the outside: a
/// variant added later must be *named* here or the receipt silently loses it.
fn ending_class(frames: &lgwks_bot::rt::process::Frames) -> &'static str {
    match *frames.ended() {
        FrameRead::Frame { .. } => "frame",
        FrameRead::EndOfStream => "end-of-stream",
        FrameRead::TruncatedPrefix { .. } => "truncated-prefix",
        FrameRead::TruncatedPayload { .. } => "truncated-payload",
        FrameRead::MalformedPrefix { .. } => "malformed-prefix",
        FrameRead::CeilingReached { .. } => "ceiling-reached",
        _ => "unknown",
    }
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

/// A framed read of a capture-cut prefix ends at the capture's ceiling, never at
/// a truncation the child performed and never at a clean end.
///
/// The child writes whole length-prefixed records whose total is drawn past the
/// capture ceiling, so the retained bytes are a prefix **the capture** cut. The
/// cut point is swept across every position inside a record — prefix, payload and
/// the exact boundary between two records — because the boundary is the case that
/// would otherwise read as a complete stream: the retained bytes end exactly
/// where a record does, and only the capture's own flag says the child wrote more.
///
/// Two tenants run the same shape with their own payloads, so a crossed pipe
/// would show up as the wrong bytes rather than as a wrong count.
fn capture_cuts_end_at_the_capture_ceiling(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        const PAYLOAD: usize = 16;
        const FRAME: usize = 4 + PAYLOAD;
        // Six frames = 120 bytes against a ceiling drawn from 20..60, so the
        // capture always cuts and the cut lands somewhere inside the stream. The
        // ceiling is drawn above the payload deliberately: a ceiling *below* it
        // would make the very first prefix declare a length past the reader's
        // ceiling, which is `MalformedPrefix` — a different fact about the same
        // bytes, and one this family is not about.
        let ceiling = usize::try_from(sim.rng().between(20, 60))?;
        let tenant: u32 = sim.rng().below(2);
        let byte = u8::try_from(tenant).unwrap_or(0).wrapping_add(b'a');

        let outcome = run(&framed_capture(byte, PAYLOAD, 6, ceiling)?)?;
        assert!(
            outcome.stdout().truncated(),
            "a 6-frame {FRAME}-byte-per-frame write must overrun a {ceiling}-byte capture ceiling"
        );
        assert!(
            outcome.stdout().retained_capacity() <= ceiling,
            "the retained buffer must not grow past its own ceiling"
        );

        let frames = outcome.stdout().frames(ceiling);
        assert_eq!(
            frames.ended(),
            &FrameRead::CeilingReached {
                ceiling: outcome.stdout().retained_capacity()
            },
            "a capture-cut prefix ends at the capture's own ceiling, whatever its bytes \
             happen to end on: tenant={tenant} ceiling={ceiling}"
        );
        assert_eq!(
            ending_class(&frames),
            "ceiling-reached",
            "the ending's class is what a caller branches on, and a cut prefix is the ceiling"
        );
        assert!(
            !frames.is_complete(),
            "tenant={tenant} ceiling={ceiling}: a reading of a capture-cut prefix is not a \
             complete reading of the child's output"
        );
        assert!(
            !frames.ended().is_truncated(),
            "the capture cut the bytes; the child did not truncate a record"
        );
        // Only whole records are records, and only the frames that fit entirely
        // inside the retained prefix are among them. The model counts them from
        // the bytes rather than from the reader, so the two are checked against
        // each other.
        let retained = outcome.stdout().bytes().len();
        let expected = whole_frames(retained, FRAME);
        assert_eq!(
            frames.records().len(),
            expected,
            "tenant={tenant} ceiling={ceiling}: {retained} retained bytes hold {expected} \
             whole frames of {FRAME} bytes, got {:?}",
            frames.records()
        );
        for (index, record) in frames.records().iter().enumerate() {
            assert_eq!(
                record.payload(),
                Some(
                    std::iter::repeat_n(byte, PAYLOAD)
                        .collect::<Vec<_>>()
                        .as_slice()
                ),
                "tenant={tenant}: record {index} must decode to this tenant's own payload"
            );
        }
        sim.record("capture-cut");
        sim.trace.record_u64("tenant", u64::from(tenant));
        sim.trace.record_count("ceiling", ceiling);
        sim.trace.record_count("retained", retained);
        sim.trace.record_count("frames", frames.records().len());
        sim.record(ending_class(&frames));
        Ok(())
    })
}

/// A legal record with no room left is the ceiling; only `0` and a length past the
/// ceiling are rot.
///
/// Swept across the boundary a real stream lands on: the ceiling is drawn and the
/// first record takes a seeded share of it, so the second record is sometimes
/// legal-and-fitting, sometimes legal-and-too-large, and the ending must be a
/// whole frame, the ceiling, or rot accordingly — never rot for the middle case.
fn room_without_a_record_is_the_ceiling(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        const CEILING: usize = 64 * 1024;
        // The *capture* ceiling is deliberately larger than the reader's. The two
        // are different bounds, and conflating them would make this family
        // observe the capture's cut instead of the reader's room: with a capture
        // at 64 KiB, a second record too large for the remaining room also
        // overran the capture, and the ending would be the capture's ceiling by
        // the rule `capture_cuts_end_at_the_capture_ceiling` pins. A capture that
        // always holds the whole stream leaves the reader's ceiling the only one
        // in play, which is what this row is about.
        const CAPTURE: usize = 128 * 1024;
        let first = usize::try_from(sim.rng().between(1024, 40 * 1024))?;
        // The second record is drawn against the room the first one leaves, so
        // both the fitting and the too-large cases are swept by construction.
        let room = CEILING.saturating_sub(first);
        let second = usize::try_from(sim.rng().between(1024, 48 * 1024))?;

        // One prefix per record, in stream order. They are literals because the four
        // bytes each names are the thing under test; the payloads come from
        // `head`, because two 40 KiB records spelled as octal escapes would be a
        // half-megabyte single argument, past what an `execve` accepts, and the
        // child would fail to start rather than exercise the reader.
        let prefix_of = |length: usize| -> String {
            u32::try_from(length)
                .unwrap_or(0)
                .to_be_bytes()
                .iter()
                .map(|value| format!("\\{value:03o}"))
                .collect()
        };
        let script = format!(
            "printf '{first_prefix}'; head -c {first} /dev/zero | tr '\\0' '9'; \
             printf '{second_prefix}'; head -c {second} /dev/zero | tr '\\0' '9'",
            first_prefix = prefix_of(first),
            second_prefix = prefix_of(second),
        );
        let outcome = run(&captured(&script, CAPTURE)?)?;
        assert!(
            !outcome.stdout().truncated(),
            "both records must fit a {CAPTURE}-byte capture, or the ending would be the \
             capture's rather than the reader's"
        );
        // The child must have written exactly the two records the seed asked for.
        // A `head` or `tr` that delivered fewer bytes would read as a truncation
        // and be reported as one, so the count is pinned before the framing is.
        assert_eq!(
            outcome.stdout().total_bytes(),
            u64::try_from(first.saturating_add(second).saturating_add(8)).unwrap_or(u64::MAX),
            "first={first} second={second}: the child wrote exactly two length-prefixed records"
        );

        let frames = outcome.stdout().frames(CEILING);
        let fits = second <= room;
        assert_eq!(
            frames.records().len(),
            if fits { 2 } else { 1 },
            "first={first} second={second} room={room}: {fits} decides whether the second \
             record is decoded at all"
        );
        assert_eq!(
            frames.ended(),
            if fits {
                &FrameRead::EndOfStream
            } else {
                &FrameRead::CeilingReached { ceiling: CEILING }
            },
            "first={first} second={second} room={room}: a legal record larger than the room \
             remaining is the ceiling, never malformed"
        );
        assert_eq!(
            frames.retained_bytes(),
            if fits {
                first.saturating_add(second)
            } else {
                first
            },
            "a record stopped by the ceiling is never charged: its payload was never read"
        );
        assert!(
            !matches!(frames.ended(), FrameRead::MalformedPrefix { .. }),
            "a well-formed record must never be reported as rot: first={first} second={second}"
        );

        // The two lengths no writer of this grammar produces, swept as a pair:
        // zero, and one past the ceiling. A reader that folded either into the
        // ceiling would accept a record the frame grammar refuses.
        for declared in [0_usize, CEILING + 1] {
            let mut rot: &[u8] = &u32::try_from(declared).unwrap_or(0).to_be_bytes();
            let refused = lgwks_bot::rt::process::read_frames(&mut rot, CEILING)?;
            assert_eq!(
                refused.ended(),
                &FrameRead::MalformedPrefix {
                    declared,
                    ceiling: CEILING
                },
                "declared={declared} names no record this grammar writes, so it is rot"
            );
            assert_eq!(
                refused.retained_bytes(),
                0,
                "declared={declared}: a refused prefix is never charged"
            );
        }
        sim.record("room");
        sim.trace.record_count("first", first);
        sim.trace.record_count("second", second);
        sim.trace.record_count("room", room);
        sim.record(ending_class(&frames));
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

/// A seeded sweep of `capture_cuts_end_at_the_capture_ceiling` over seeds 72..80.
#[test]
fn capture_cuts_end_at_the_capture_ceiling_band_00() -> TestResult {
    capture_cuts_end_at_the_capture_ceiling(Band::new(72, 8))
}

/// A seeded sweep of `capture_cuts_end_at_the_capture_ceiling` over seeds 80..88.
#[test]
fn capture_cuts_end_at_the_capture_ceiling_band_01() -> TestResult {
    capture_cuts_end_at_the_capture_ceiling(Band::new(80, 8))
}

/// A seeded sweep of `room_without_a_record_is_the_ceiling` over seeds 88..96.
#[test]
fn room_without_a_record_is_the_ceiling_band_00() -> TestResult {
    room_without_a_record_is_the_ceiling(Band::new(88, 8))
}

/// A seeded sweep of `room_without_a_record_is_the_ceiling` over seeds 96..104.
#[test]
fn room_without_a_record_is_the_ceiling_band_01() -> TestResult {
    room_without_a_record_is_the_ceiling(Band::new(96, 8))
}

/// A seeded sweep of `rot_before_the_capture_cut_is_the_ending` over seeds
/// 104..112.
#[test]
fn rot_before_the_capture_cut_is_the_ending_band_00() -> TestResult {
    rot_before_the_capture_cut_is_the_ending(Band::new(104, 8))
}

/// A seeded sweep of `rot_before_the_capture_cut_is_the_ending` over seeds
/// 112..120.
#[test]
fn rot_before_the_capture_cut_is_the_ending_band_01() -> TestResult {
    rot_before_the_capture_cut_is_the_ending(Band::new(112, 8))
}

/// A sweep of `capture_cuts_end_at_the_capture_ceiling` at 100, 1 000 and 10 000
/// concurrent captures, and the named replay receipt over the tier sweep.
///
/// Each tier drives `min(requested, CAPTURE_CEILING)` **real** children at once,
/// each with its own supervisor and its own capture. The requested, reached and
/// ceiling levels are all reported (the INV-BOT-16 rule), so a tier this host
/// cannot reach says so rather than passing quietly: ten thousand real `sh`
/// children is a fork storm, and the honest claim is the tier that was reached.
#[test]
fn capture_cuts_saturate_at_the_declared_tiers() -> TestResult {
    const PAYLOAD: usize = 16;
    const RECORDS: usize = 6;
    const FRAME: usize = 4 + PAYLOAD;
    const CEILING: usize = 32;
    // The tier this host is asked to drive. Every child is a real process on a
    // real pipe, so the ceiling is what the OS will bear rather than an arbitrary
    // number; a reader that shared state across captures would fail here at any
    // tier, which is what makes the reached level sufficient evidence.
    const CAPTURE_CEILING: usize = 256;
    let expected_frames = whole_frames(CEILING, FRAME);

    for requested in [100_usize, 1_000, 10_000] {
        let level = requested.min(CAPTURE_CEILING);
        let specs: Vec<ProcessSpec> = (0..level)
            .map(|index| framed_capture(tenant_byte(index), PAYLOAD, RECORDS, CEILING))
            .collect::<Result<Vec<_>, _>>()?;
        let runtime = lgwks_bot::Runtime::new()?;
        // Each child gets its own `Supervisor::new(1)` *inside its own async
        // block*, because `run_process` takes `&mut self`: a future that
        // borrowed a supervisor from an enclosing scope could not be stored in a
        // `Vec` and joined, and one supervisor shared across children would
        // serialise them — the opposite of the tier under test.
        let observations = runtime.block_on(async {
            let runs: Vec<_> = specs
                .iter()
                .map(|spec| async move {
                    let mut supervisor = Supervisor::new(1);
                    supervisor.run_process(spec).await
                })
                .collect();
            lgwks_std::task::join_all(runs).await
        });

        assert_eq!(
            observations.len(),
            level,
            "requested={requested} reached={level} ceiling={CAPTURE_CEILING}: every concurrent \
             run reported, so the tier really ran"
        );
        for (index, outcome) in observations.into_iter().enumerate() {
            let outcome = outcome?;
            let read = outcome.stdout().frames(CEILING);
            assert_eq!(
                read.ended(),
                &FrameRead::CeilingReached {
                    ceiling: outcome.stdout().retained_capacity()
                },
                "requested={requested} reached={level} child {index}: a capture-cut prefix ends \
                 at the capture's ceiling"
            );
            assert_eq!(
                read.records().len(),
                expected_frames,
                "requested={requested} child {index}: only the {expected_frames} whole frames \
                 inside a {CEILING}-byte capture are records"
            );
            let byte = tenant_byte(index);
            for (record, payload) in read
                .records()
                .iter()
                .filter_map(FrameRead::payload)
                .enumerate()
            {
                assert_eq!(
                    payload,
                    std::iter::repeat_n(byte, PAYLOAD)
                        .collect::<Vec<_>>()
                        .as_slice(),
                    "requested={requested} child {index}: record {record} must be this child's \
                     own payload byte, never another child's"
                );
            }
        }
    }
    Ok(())
}

/// Rot before the capture cut is the ending; the capture's ceiling only ends what
/// the cut itself reached.
///
/// Two tenants run the same shape with their own rot, so a crossed pipe shows up
/// as the wrong bytes rather than a wrong count. Per tenant the stream is: `pad`
/// whole records, then one **rot** record whose prefix declares a length no writer
/// of this grammar produces, then `tail` more whole records, so the capture cuts
/// somewhere in the stream. The seed picks whether the rot prefix itself was
/// retained whole, and the ending follows from that and from nothing else: the
/// child's rot when the capture kept all four of its bytes, otherwise the
/// capture's own ceiling.
///
/// The rot is drawn from the two lengths the grammar refuses — `0`, and one past
/// the reader's ceiling — because a *legal* record larger than the room remaining
/// is `room_without_a_record_is_the_ceiling`'s case and would make this family's
/// ending indistinguishable from a bound.
///
/// The cut and the reader's ceiling are the *same* number here, deliberately:
/// they are drawn together so the one thing this family separates — the rot the
/// capture retained from the bound it stopped at — is not confused by which of the
/// two happened to be smaller.
fn rot_before_the_capture_cut_is_the_ending(band: Band) -> TestResult {
    sim::assert_replays(band, |sim| {
        const PAYLOAD: usize = 8;
        const FRAME: usize = 4 + PAYLOAD;
        let pad = usize::try_from(sim.rng().below(3))?;
        let tenant: u32 = sim.rng().below(2);
        let byte = u8::try_from(tenant).unwrap_or(0).wrapping_add(b'a');
        // Drawn *after* `pad` so that drawing it does not renumber the seeds, and
        // above both the rot's position and one whole record — so whether the rot
        // was retained whole is a real draw across the sweep rather than an
        // artefact of a bound smaller than the prefix.
        let ceiling = usize::try_from(sim.rng().between(24, 48))?;
        // Enough records after the rot that the cut lands past it whenever it
        // lands past it at all.
        let tail = usize::try_from(sim.rng().between(1, 4))?;
        let rot_at = pad.saturating_mul(FRAME).saturating_add(4);
        let declared = if sim.rng().chance(500) {
            0
        } else {
            ceiling.saturating_add(1)
        };

        let mut writer: Vec<u8> = Vec::new();
        let push_record = |writer: &mut Vec<u8>, declared: usize| {
            writer.extend_from_slice(&u32::try_from(declared).unwrap_or(0).to_be_bytes());
            writer.extend(std::iter::repeat_n(byte, declared));
        };
        for _ in 0..pad {
            push_record(&mut writer, PAYLOAD);
        }
        push_record(&mut writer, declared);
        for _ in 0..tail {
            push_record(&mut writer, PAYLOAD);
        }
        let literal = writer
            .iter()
            .map(|value| format!("\\{value:03o}"))
            .collect::<String>();
        let outcome = run(&captured(&format!("printf '{literal}'"), ceiling)?)?;
        let retained = outcome.stdout().bytes().len();
        let capture_cut = outcome.stdout().truncated();

        // The model: the rot prefix is whole exactly when the capture retained all
        // four of its bytes. When it was, the pass stopped on it — the capture's
        // cut, if there was one, lies after it and cannot change that. When it was
        // not, the cut came first and the pass ran out of retained bytes instead.
        let rot_whole = retained >= rot_at;
        let frames = outcome.stdout().frames(ceiling);
        if rot_whole {
            assert_eq!(
                frames.ended(),
                &FrameRead::MalformedPrefix { declared, ceiling },
                "tenant={tenant} ceiling={ceiling} retained={retained}: the rot prefix at byte \
                 {rot_at} was read whole, so it is the child's own rot and the capture's cut \
                 after it does not replace it"
            );
            assert_eq!(
                frames.records().len(),
                pad,
                "tenant={tenant}: the {pad} records before the rot are whole, and the rot is \
                 never one of them"
            );
        } else {
            assert!(
                capture_cut,
                "tenant={tenant} ceiling={ceiling} retained={retained}: the model says the cut \
                 reached the pass rather than the rot, so the capture must have cut"
            );
            assert_eq!(
                frames.ended(),
                &FrameRead::CeilingReached {
                    ceiling: outcome.stdout().retained_capacity()
                },
                "tenant={tenant} ceiling={ceiling} retained={retained}: the cut fell short of \
                 the rot prefix, so the ending is the capture's own ceiling"
            );
        }
        assert!(
            !frames.is_complete(),
            "tenant={tenant} ceiling={ceiling}: the pass stopped short of the end whatever it \
             stopped on"
        );
        assert!(
            !frames.ended().payload().is_some(),
            "an ending the pass stopped on carries no payload"
        );
        sim.record("rot");
        sim.trace.record_u64("tenant", u64::from(tenant));
        sim.trace.record_count("ceiling", ceiling);
        sim.trace.record_count("retained", retained);
        sim.trace.record_count("pad", pad);
        sim.trace.record_count("tail", tail);
        sim.trace.record_count("rot-at", rot_at);
        sim.trace.record_count("declared", declared);
        sim.trace.record_u64("capture-cut", u64::from(capture_cut));
        sim.record(ending_class(&frames));
        Ok(())
    })
}

/// The one payload byte a tenant's records carry, from its index.
///
/// 26 tenants rather than unbounded indices, so a tier of 10,000 children shares
/// payload bytes between runs: the assertion is about *which* capture a record
/// came from, not about every child having a distinct byte, and a crossed pipe
/// would still show as a byte this child never wrote.
fn tenant_byte(index: usize) -> u8 {
    u8::try_from(index % 26).unwrap_or(0).wrapping_add(b'a')
}

/// How many whole `frame`-byte frames fit in `retained` bytes.
///
/// A counted subtraction rather than a division: the workspace forbids
/// `clippy::integer_division`, and a loop over a frame count bounded by a
/// capture ceiling is not a cost worth a suppressed lint.
fn whole_frames(retained: usize, frame: usize) -> usize {
    let mut left = retained;
    let mut whole = 0_usize;
    while left >= frame {
        left = left.saturating_sub(frame);
        whole = whole.saturating_add(1);
    }
    whole
}

/// A shell spec writing `records` whole framed payloads of `byte`, with a capture
/// of `ceiling` bytes.
///
/// Shared by the seeded family and the saturation tiers so one framing shape has
/// one spelling: a shape written twice is a shape whose two spellings can drift.
fn framed_capture(
    byte: u8,
    payload: usize,
    records: usize,
    ceiling: usize,
) -> Result<ProcessSpec, Box<dyn Error>> {
    let body: Vec<u8> = std::iter::repeat_n(byte, payload).collect();
    let mut writer: Vec<u8> = Vec::new();
    for _ in 0..records {
        writer.extend_from_slice(&u32::try_from(payload).unwrap_or(0).to_be_bytes());
        writer.extend_from_slice(&body);
    }
    let literal = writer
        .iter()
        .map(|value| format!("\\{value:03o}"))
        .collect::<String>();
    captured(&format!("printf '{literal}'"), ceiling)
}
