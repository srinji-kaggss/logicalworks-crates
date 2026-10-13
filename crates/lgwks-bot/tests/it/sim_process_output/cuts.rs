//! Capture cuts at the declared tiers, and rot before the capture cut as the ending.

use super::*;

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
            .map(|index| framed_capture(tenant_byte(index)?, PAYLOAD, RECORDS, CEILING))
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
            let byte = tenant_byte(index)?;
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
        let byte = u8::try_from(tenant)?.wrapping_add(b'a');
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
        let push_record =
            |writer: &mut Vec<u8>, declared: usize| -> Result<(), std::num::TryFromIntError> {
                writer.extend_from_slice(&u32::try_from(declared)?.to_be_bytes());
                writer.extend(std::iter::repeat_n(byte, declared));
                Ok(())
            };
        for _ in 0..pad {
            push_record(&mut writer, PAYLOAD)?;
        }
        push_record(&mut writer, declared)?;
        for _ in 0..tail {
            push_record(&mut writer, PAYLOAD)?;
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
        sim.trace.record_number("tenant", u64::from(tenant));
        sim.trace.record_number("ceiling", ceiling);
        sim.trace.record_number("retained", retained);
        sim.trace.record_number("pad", pad);
        sim.trace.record_number("tail", tail);
        sim.trace.record_number("rot-at", rot_at);
        sim.trace.record_number("declared", declared);
        sim.trace
            .record_number("capture-cut", u64::from(capture_cut));
        sim.record(ending_class(&frames));
        Ok(())
    })
}
