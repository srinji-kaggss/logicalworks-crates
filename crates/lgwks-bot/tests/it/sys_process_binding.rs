//! Black-box acceptance for `Supervisor::run_process` and the `domain::sys`
//! process binding.
//!
//! `run_process` is the result-bearing counterpart of `spawn_process`: it runs
//! one child to completion under the same in-flight ceiling, process-group
//! ownership and deadline machinery, and reports what it observed. These tests
//! pin the properties that make its report usable:
//!
//! - the exit code and the signal are reported distinctly, and neither is a
//!   claim about the work the command was asked to do (T35);
//! - stdout and stderr are captured exactly up to their ceiling, a saturating
//!   child is drained and truncated rather than blocked or retained without
//!   bound, and the exact total is still reported (T05);
//! - a deadline stops the whole group and is reported as a deadline, not as a
//!   normal exit;
//! - a parent's zero exit does not fabricate tree cleanup: the descendant is
//!   gone when the report says cleanup is confirmed (T19);
//! - dropping the run future after the fork leaves no orphan (T20);
//! - the sanctioned path stays bounded under many concurrent processes;
//! - a refusal before the fork and a failure after it are different types.
//!
//! The `domain::sys::Process` half drives the same machinery through the verb
//! traits and checks the dispatch certainty it reports.
#![cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]

use crate::scratch::Scratch;

use std::num::NonZeroUsize;
use std::time::Duration;

use lgwks_bot::domain::sys::{DEFAULT_CAPTURE_LIMIT, DEFAULT_DEADLINE, Process};
use lgwks_bot::rt::process::{
    FrameRead, ProcessRun, ProcessRunError, ProcessSpec, StdioPolicy, read_frames,
};
use lgwks_bot::rt::runtime::Builder;
use lgwks_bot::rt::supervise::{CleanupReceipt, Supervisor};
use lgwks_bot::rt::time::sleep;
use lgwks_bot::{Auth, BotError, Cap, DispatchCertainty, Execute, GrantSet, Observe, Query};

// The pid-file scratch directory and the group-absence wait are shared with the
// other process test targets, so there is one copy of each.
use crate::process_probe;

use process_probe::{drop_after_pid, read_pid, wait_group_gone, wait_group_stopped};

/// What a test reports when a precondition did not hold.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// How long a liveness wait may take before the test calls it a failure.
const BUDGET: Duration = Duration::from_secs(10);

// ── Spec helpers ────────────────────────────────────────────────────────────

/// A shell spec running `script`, with stderr on stdout's stream when asked.
fn shell(script: &str) -> ProcessSpec {
    let mut spec = ProcessSpec::new("sh");
    spec.arg("-c").arg(script);
    spec
}

/// A shell spec with the given capture ceilings and no deadline.
fn captured_shell(script: &str, limit: usize) -> ProcessSpec {
    let mut spec = shell(script);
    // A zero limit captures nothing, so the streams are left as they are rather
    // than captured at a one-byte ceiling nobody asked for.
    if let Some(limit) = NonZeroUsize::new(limit) {
        spec.capture_stdout(limit);
        spec.capture_stderr(limit);
    }
    spec
}

/// The capture ceiling the capture-cut tests share: smaller than any of their
/// streams, so the capture always cuts.
const SMALL_CAPTURE: usize = 8;

/// Run `script` once on a one-slot supervisor, capturing at most `capture` bytes
/// of each stream.
fn run_captured(script: &str, capture: usize) -> Result<ProcessRun, Box<dyn std::error::Error>> {
    let runtime = lgwks_bot::Runtime::new()?;
    Ok(runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        supervisor
            .run_process(&captured_shell(script, capture))
            .await
    })?)
}

/// A script printing `count` whole framed records, each the four-byte prefix
/// `[0, 0, 0, 2]` and the payload `ok`.
fn ok_records(count: usize) -> String {
    format!("printf '{}'", "\\000\\000\\000\\002ok".repeat(count))
}

/// A `domain::sys::Process` for `script`, at the documented defaults.
fn process_for(script: &str) -> Process {
    let mut spec = shell(script);
    spec.capture_stdout(DEFAULT_CAPTURE_LIMIT);
    spec.capture_stderr(DEFAULT_CAPTURE_LIMIT);
    spec.deadline(DEFAULT_DEADLINE);
    Process::from_spec(spec)
}

/// An `Auth` covering `bot.sys`.
fn sys_auth() -> Result<Auth, BotError> {
    GrantSet::empty().grant(Cap::sys()).issue(&[Cap::sys()])
}

/// An `Auth` covering nothing, for the missing-capability case.
fn empty_auth() -> Result<Auth, BotError> {
    GrantSet::empty().issue(&[])
}

// ── run_process: exit status, streams, deadline ─────────────────────────────

#[test]
fn run_process_reports_an_exit_code_and_a_signal_distinctly() -> TestResult {
    let runtime = lgwks_bot::Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);

        let exited_zero = supervisor.run_process(&shell("exit 0")).await?;
        assert_eq!(
            exited_zero.exit_code(),
            Some(0),
            "exit zero is reported as exit zero, not as a completed task (T35)"
        );
        assert_eq!(exited_zero.signal(), None, "a clean exit is not a signal");
        assert!(!exited_zero.deadline_fired(), "no deadline was set");

        let exited_seven = supervisor.run_process(&shell("exit 7")).await?;
        assert_eq!(exited_seven.exit_code(), Some(7), "the code survives");

        let signalled = supervisor.run_process(&shell("kill -TERM $$")).await?;
        assert_eq!(
            signalled.exit_code(),
            None,
            "a signal death has no exit code"
        );
        assert_eq!(
            signalled.signal(),
            Some(15),
            "the terminating signal is reported, not folded into a code"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}

#[test]
fn run_process_captures_stdout_and_stderr_exactly_under_the_limit() -> TestResult {
    let runtime = lgwks_bot::Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        let run = supervisor
            .run_process(&captured_shell("printf abc; printf def >&2", 4096))
            .await?;
        assert_eq!(run.stdout().bytes(), b"abc", "stdout is captured verbatim");
        assert_eq!(run.stderr().bytes(), b"def", "stderr is captured verbatim");
        assert!(!run.stdout().truncated() && !run.stderr().truncated());
        assert_eq!(run.stdout().total_bytes(), 3, "the exact total is reported");
        assert_eq!(run.stderr().total_bytes(), 3, "the exact total is reported");
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}

/// T05: a child that writes far more than the ceiling is drained and truncated,
/// not blocked, and its retained buffer never exceeds the ceiling.
#[test]
fn a_chatty_child_is_drained_and_truncated_within_its_ceiling() -> TestResult {
    const TOTAL: u64 = 50 * 1024 * 1024;
    const LIMIT: usize = 4096;
    // Two independent 50 MiB writes, one to each pipe. `/dev/zero` gives a
    // stream of one byte (`0`), so the retained head is exactly that byte and
    // the assertion can name what it kept. Fifty mebibytes through each pipe
    // proves the child is drained while it runs rather than blocking on a full
    // pipe.
    let script = "head -c 52428800 /dev/zero; head -c 52428800 /dev/zero 1>&2";
    let runtime = lgwks_bot::Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        let run = supervisor
            .run_process(&captured_shell(script, LIMIT))
            .await?;
        for (name, stream) in [("stdout", run.stdout()), ("stderr", run.stderr())] {
            assert_eq!(
                stream.bytes().len(),
                LIMIT,
                "{name} must retain exactly the first {LIMIT} bytes"
            );
            assert!(
                stream.bytes().iter().all(|byte| *byte == 0),
                "{name} must retain the head of the stream"
            );
            assert!(
                stream.truncated(),
                "{name} wrote {TOTAL} bytes, over its {LIMIT}-byte ceiling"
            );
            assert_eq!(
                stream.total_bytes(),
                TOTAL,
                "{name} must report the exact total it saw"
            );
            assert!(
                stream.retained_capacity() <= LIMIT,
                "{name} retained {} bytes of capacity, over the {LIMIT}-byte ceiling",
                stream.retained_capacity()
            );
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}

/// T05, slow-consumer half: a flooding child against a deliberately slow reader
/// stays inside its ceiling, and the exact total is still reported.
///
/// A single worker is the point rather than a detail. The three driver futures —
/// the two pipe reads and the exit observation — share one task, so on one worker
/// the reader's own cost is the only thing deciding when the child's next write
/// is serviced. That is the tightest reader/writer ratio available without
/// touching the production loop, and it is what makes the assertion meaningful:
/// a ceiling that leaked would grow here, and a reader that stopped draining at
/// the ceiling would deadlock this test rather than pass it.
#[test]
fn a_flooding_child_against_a_slow_reader_stays_within_its_ceiling_t05() -> TestResult {
    // Eight mebibytes on each stream. Far past the ceiling (so truncation is
    // certain rather than incidental) and far past any pipe buffer (so the child
    // blocks unless the reader keeps draining while it runs).
    const TOTAL: u64 = 8 * 1024 * 1024;
    const LIMIT: usize = 4096;
    let script = format!("head -c {TOTAL} /dev/zero; head -c {TOTAL} /dev/zero 1>&2");
    let runtime = Builder::new()
        .worker_threads(NonZeroUsize::new(1))
        .build()?;
    let run = runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        supervisor
            .run_process(&captured_shell(&script, LIMIT))
            .await
            .map_err(Box::<dyn std::error::Error>::from)
    })?;

    for (name, stream) in [("stdout", run.stdout()), ("stderr", run.stderr())] {
        process_probe::assert_capture_within_ceiling(name, stream, LIMIT, TOTAL);
    }
    Ok(())
}

/// The same flood with a deadline, to prove the drain is concurrent with the
/// child's writing rather than sequenced after its exit.
///
/// Without this the flood test above passes on an implementation that waits for
/// the child to exit *before* reading, which is correct for a child under its
/// ceiling and a deadlock for one over it. Here the child writes far more than a
/// pipe holds and the deadline is far shorter than the write takes, so the only
/// way the run settles promptly is if the pipe is being drained while the child
/// is still running.
#[test]
fn a_flooding_child_is_drained_while_it_runs_not_after_it_exits() -> TestResult {
    const LIMIT: usize = 4096;
    // A child's write to a full pipe blocks, so under a reader that waits for the
    // exit this child never exits and the run only settles by the deadline.
    // A deadline long enough for a real drain, and far too short for a
    // write-then-exit child of this size on a loaded machine.
    const DEADLINE_MS: u64 = 5_000;
    const TOTAL: u64 = 32 * 1024 * 1024;
    let script = format!("head -c {TOTAL} /dev/zero");
    let mut spec = shell(&script);
    spec.capture_stdout(NonZeroUsize::new(LIMIT).ok_or("a ceiling of at least one")?);
    spec.capture_stderr(NonZeroUsize::new(LIMIT).ok_or("a ceiling of at least one")?);
    spec.deadline(Duration::from_millis(DEADLINE_MS));
    let runtime = lgwks_bot::Runtime::new()?;
    let run = runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        supervisor.run_process(&spec).await
    })?;

    assert!(
        !run.deadline_fired(),
        "a child drained while it runs finishes on its own; the deadline fired, so the \
         reader was not draining concurrently with the write (retained {} of {} bytes)",
        run.stdout().bytes().len(),
        run.stdout().total_bytes()
    );
    assert_eq!(
        run.exit_code(),
        Some(0),
        "the flood child must exit cleanly once its pipe is drained"
    );
    process_probe::assert_capture_within_ceiling("stdout", run.stdout(), LIMIT, TOTAL);
    Ok(())
}

/// T05, framed half: a record cut off mid-frame is a typed refusal, never a
/// decoded success.
///
/// The child emits one whole length-prefixed record and then a second whose
/// payload stops half-way. A reader that trusted the prefix would report the
/// short payload as the record; this one refuses it, and says how much arrived.
///
/// Read through `ProcessRun::stdout().frames(..)`, the door a caller reads its
/// own child's output through, rather than over a hand-plumbed slice — so what is
/// under test is the path a real caller takes.
#[test]
fn a_framed_record_cut_off_mid_frame_is_a_typed_refusal() -> TestResult {
    // `[0,0,0,4]` then `done`, then `[0,0,0,8]` and only `part`.
    let script = "printf '\\000\\000\\000\\004done\\000\\000\\000\\010part'";
    let runtime = lgwks_bot::Runtime::new()?;
    let run = runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        supervisor.run_process(&captured_shell(script, 4096)).await
    })?;
    assert_eq!(
        run.exit_code(),
        Some(0),
        "the child completed its own work; the framing is what is under test"
    );
    assert!(
        !run.stdout().truncated(),
        "a {}-byte write under a 4096-byte ceiling must retain the whole stream",
        run.stdout().total_bytes()
    );

    let frames = run.stdout().frames(4096);
    assert_eq!(
        frames.records().len(),
        1,
        "only the record that arrived whole is a frame: {:?}",
        frames.records()
    );
    assert_eq!(
        frames.records()[0].payload(),
        Some(&b"done"[..]),
        "the whole record decodes to exactly what the child wrote"
    );
    assert_eq!(
        frames.ended(),
        &FrameRead::TruncatedPayload {
            declared: 8,
            partial: b"part".to_vec(),
        },
        "a prefix naming eight bytes that delivered four is a truncation, and it names \
         both the declared length and what arrived"
    );
    assert!(
        !frames.ended().is_frame(),
        "the cut record must never read as a decoded frame"
    );
    assert!(
        frames.ended().payload().is_none(),
        "a refusal carries no payload, so there is no path from a cut record to bytes"
    );
    assert!(
        !frames.is_complete(),
        "a stream whose last record was cut off has no complete reading"
    );
    Ok(())
}

/// The same stream read whole is a complete reading, so the refusal above is
/// about the bytes rather than about the reader.
///
/// The control the truncation test needs: without a positive case, "always
/// refuse" would satisfy it. Here every record arrives, the capture held the
/// whole output, and the pass reports the clean end, the whole count, and no
/// truncation.
#[test]
fn a_framed_stream_that_ends_cleanly_is_complete() -> TestResult {
    let script = "printf '\\000\\000\\000\\004done\\000\\000\\000\\002ok'";
    let runtime = lgwks_bot::Runtime::new()?;
    let run = runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        supervisor.run_process(&captured_shell(script, 4096)).await
    })?;

    let frames = run.stdout().frames(4096);
    assert_eq!(
        frames.ended(),
        &FrameRead::EndOfStream,
        "a stream that ended between records is a complete read, not a failure"
    );
    assert!(frames.is_complete(), "the whole stream was decoded");
    assert!(!frames.ended().is_refusal(), "a clean end is not a refusal");
    let payloads: Vec<&[u8]> = frames
        .records()
        .iter()
        .filter_map(FrameRead::payload)
        .collect();
    assert_eq!(
        payloads,
        vec![&b"done"[..], &b"ok"[..]],
        "both records decode to exactly what the child wrote"
    );
    assert_eq!(
        frames.retained_bytes(),
        6,
        "every payload byte is accounted for"
    );
    Ok(())
}

/// D2: the capture's own cut is reported as the ceiling, never as the child's
/// truncation and never as a clean end.
///
/// The child writes three whole records and the capture ceiling holds only enough
/// for the first, so the retained bytes are a prefix **the capture** cut. A
/// framed read of them cannot see the child's whole output however those bytes
/// happen to end — here they end exactly on a record boundary, which is the case
/// that would otherwise read as a complete stream. The ending must therefore be
/// `CeilingReached` carrying the capture's retained capacity, and the pass must
/// not claim completeness.
#[test]
fn a_capture_ceiling_ends_the_framed_read_rather_than_the_child() -> TestResult {
    // Three whole two-byte records: 4 + 3 + 3 = 10 bytes, and a capture of 8
    // stops inside the second record's prefix. The two complete records before
    // the cut are decoded; the third never arrived within the ceiling.
    let script = ok_records(3);
    let run = run_captured(&script, SMALL_CAPTURE)?;
    assert!(
        run.stdout().truncated(),
        "the child wrote more than the {SMALL_CAPTURE}-byte ceiling, so the capture cut it"
    );

    let frames = run.stdout().frames(SMALL_CAPTURE);
    assert_eq!(
        frames.ended(),
        &FrameRead::CeilingReached {
            ceiling: run.stdout().retained_capacity(),
        },
        "the retained bytes are a prefix the capture cut, so the ending is the capture's \
         ceiling and never a child truncation"
    );
    assert_ne!(
        frames.ended(),
        &FrameRead::EndOfStream,
        "a pass over a truncated capture must never claim a clean end"
    );
    assert!(
        !frames.is_complete(),
        "a reading of a capture-cut prefix is not a complete reading of the child"
    );
    assert!(
        !frames.ended().is_truncated(),
        "the capture cut the bytes; the child did not truncate a record"
    );
    let whole = frames
        .records()
        .iter()
        .filter(|record| record.is_frame())
        .count();
    assert_eq!(
        whole,
        1,
        "only whole frames are records, even when the capture cut between them: {:?}",
        frames.records()
    );
    assert!(
        frames
            .records()
            .iter()
            .all(|record| record.payload().is_some()),
        "every retained record is a whole one, so every one has its payload"
    );
    Ok(())
}

/// Rot the capture retained stands: a capture's cut never launders a child's
/// malformed prefix into the capture's own ceiling.
///
/// The child writes a **zero-length prefix** — a length no writer of this grammar
/// produces — and then 32 bytes, against an 8-byte capture. Only eight of those
/// bytes were retained, so *some* of what a reader sees is a prefix the capture
/// cut; the first four are not. They are a whole prefix that named a zero-length
/// record, and that is rot in the child's output, decided entirely from bytes the
/// capture did retain. An implementation that reports every ending as the
/// capture's ceiling replaces that rot with a bound the caller did not hit, which
/// is the fail-open direction: a caller looking for corruption would see a
/// ceiling and go looking for a large record instead.
#[test]
fn a_rot_prefix_before_the_capture_cut_stays_refused() -> TestResult {
    // `\000\000\000\000` is the prefix `[0, 0, 0, 0]` — a declared length of zero.
    // 32 more bytes follow, so the 8-byte capture certainly cut.
    let script = "printf '\\000\\000\\000\\000'; head -c 32 /dev/zero | tr '\\0' 'x'";
    let run = run_captured(script, SMALL_CAPTURE)?;
    assert!(
        run.stdout().truncated(),
        "36 bytes into an 8-byte capture, so the retained bytes are a prefix the capture cut"
    );
    assert_eq!(
        run.stdout().bytes().len(),
        SMALL_CAPTURE,
        "the whole 8-byte window is retained, so every prefix inside it was read whole"
    );

    let frames = run.stdout().frames(4096);
    assert_eq!(
        frames.ended(),
        &FrameRead::MalformedPrefix {
            declared: 0,
            ceiling: 4096,
        },
        "the capture cut *later* bytes; the zero-length prefix before the cut was read whole \
         and is rot in the child's output, so it stands"
    );
    assert!(
        !frames.is_complete(),
        "a reading of a capture-cut prefix is never a complete reading of the child"
    );
    Ok(())
}

/// The same cut, with a reader ceiling the reader itself reached first: the
/// reader's own `CeilingReached` stands rather than the capture's.
///
/// The stream is four whole records of a two-byte payload, so three of them fit
/// inside the capture and charge the reader exactly its 6-byte ceiling — the
/// ceiling counts payload bytes, not stream bytes — while the fourth is cut. The
/// reader therefore stops on its own bound, having read only bytes the capture
/// retained in full, and never reaches the prefix the cut fell in the middle of.
/// Which ceiling stopped the pass is the fact a caller acts on, and a capture cut
/// that happened to truncate as well must not replace the reader's answer with
/// the capture's capacity.
#[test]
fn a_reader_ceiling_over_a_truncated_capture_is_the_readers_own() -> TestResult {
    const CAPTURE: usize = 20;
    const READER: usize = 6;
    // `[0,0,0,2] ok` six bytes each, four of them: 24 bytes against a 20-byte
    // capture. The three whole records inside the capture are exactly the reader's
    // ceiling of six payload bytes, and the fourth is the cut.
    let script = ok_records(4);
    let run = run_captured(&script, CAPTURE)?;
    assert!(
        run.stdout().truncated(),
        "24 bytes into a 20-byte capture, so the capture did cut its retained prefix"
    );
    assert!(
        run.stdout().retained_capacity() > READER,
        "the capture's own capacity must exceed the reader's, or the two ceilings could \
         never be told apart"
    );

    let frames = run.stdout().frames(READER);
    assert_eq!(
        frames.ended(),
        &FrameRead::CeilingReached { ceiling: READER },
        "the reader charged its own ceiling and stopped before the capture's cut could matter"
    );
    assert_eq!(
        frames.retained_bytes(),
        READER,
        "every byte the reader retained is accounted for at its own ceiling"
    );
    Ok(())
}

/// The control for the case above: a capture that retained the child's whole
/// output reports the child's own truncation, not the capture's ceiling.
///
/// The two tests differ in one fact only — whether the capture was truncated —
/// and the readings must differ accordingly. Without this, an implementation that
/// always answered `CeilingReached` would satisfy the test above.
#[test]
fn an_untruncated_capture_reports_the_child_own_truncation() -> TestResult {
    let script = "printf '\\000\\000\\000\\004done\\000\\000\\000\\010part'";
    let runtime = lgwks_bot::Runtime::new()?;
    let run = runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        supervisor.run_process(&captured_shell(script, 4096)).await
    })?;
    assert!(
        !run.stdout().truncated(),
        "the whole 16-byte write fits a 4096-byte ceiling, so the capture did not cut it"
    );

    let frames = run.stdout().frames(4096);
    assert_eq!(
        frames.ended(),
        &FrameRead::TruncatedPayload {
            declared: 8,
            partial: b"part".to_vec(),
        },
        "with the whole stream retained, the ending is the truncation the child performed"
    );
    assert_ne!(
        frames.ended(),
        &FrameRead::CeilingReached {
            ceiling: run.stdout().retained_capacity()
        },
        "an untruncated capture must not report the capture's ceiling"
    );
    Ok(())
}

/// D3: a legal record that does not fit the remaining room is the ceiling, not
/// rot, while a length past the ceiling in force is still refused.
///
/// Both halves in one test because they are the same decision seen from two
/// sides: a well-formed 40 KiB record is a record, and only once 40 of the 64 KiB
/// are spent does the second one have nowhere to go. Reading that as
/// `MalformedPrefix` would report a child's legal record as corruption.
#[test]
fn a_legal_record_without_room_is_the_ceiling_and_rot_is_still_refused() -> TestResult {
    const CEILING: usize = 64 * 1024;
    const RECORD: usize = 40 * 1024;
    // Two valid records of 40 KiB each: the first fits, the second has 24 KiB of
    // room left and declares 40, so it is a well-formed record with nowhere to go.
    let mut bytes: Vec<u8> = Vec::new();
    for _ in 0..2 {
        bytes.extend_from_slice(&u32::try_from(RECORD)?.to_be_bytes());
        bytes.extend(std::iter::repeat_n(7_u8, RECORD));
    }
    let mut stream: &[u8] = &bytes;
    let frames = read_frames(&mut stream, CEILING)?;
    assert_eq!(
        frames.records().len(),
        1,
        "the first whole record is decoded; the second is refused before its payload"
    );
    assert_eq!(
        frames.records()[0].payload().map(<[u8]>::len),
        Some(RECORD),
        "the decoded record is exactly the length its prefix declared"
    );
    assert_eq!(
        frames.ended(),
        &FrameRead::CeilingReached { ceiling: CEILING },
        "a legal record larger than the room remaining is the ceiling, not malformed"
    );
    assert_eq!(
        frames.retained_bytes(),
        RECORD,
        "a record stopped by the ceiling is never charged: its payload was never read"
    );

    // The two lengths that name no record this grammar writes: zero, and one past
    // the ceiling in force.
    for (declared, why) in [
        (0_usize, "a zero-length record is never written"),
        (
            CEILING + 1,
            "a length past the ceiling names no record this reader writes",
        ),
    ] {
        let mut rot: &[u8] = &u32::try_from(declared)?.to_be_bytes();
        let refused = read_frames(&mut rot, CEILING)?;
        assert_eq!(
            refused.ended(),
            &FrameRead::MalformedPrefix {
                declared,
                ceiling: CEILING
            },
            "{why}, so it is refused before a payload byte is read"
        );
        assert_eq!(
            refused.retained_bytes(),
            0,
            "{why}: a refused prefix is never charged"
        );
    }
    Ok(())
}

/// A prefix declaring a length past the reader's ceiling is refused, and never
/// allocated for.
///
/// The half of the framing contract that is about this crate rather than the
/// child: a complete prefix naming more than the caller declared is refused
/// before a byte of it is read, so a stream cannot ask for an allocation by
/// claiming a large record.
#[test]
fn a_prefix_past_the_ceiling_is_refused_before_it_is_allocated() -> TestResult {
    let mut stream: &[u8] = &[0, 0, 0, 8, 1, 2, 3, 4, 5, 6, 7, 8];
    let frames = read_frames(&mut stream, 4)?;
    assert_eq!(
        frames.ended(),
        &FrameRead::MalformedPrefix {
            declared: 8,
            ceiling: 4,
        },
        "a complete prefix naming more than the ceiling is refused, with both numbers"
    );
    assert_eq!(
        frames.retained_bytes(),
        0,
        "a refused record is never charged, because it was never read"
    );
    Ok(())
}

/// A deadline stops the whole group and says so.
#[test]
fn a_deadline_kill_is_reported_as_a_deadline_and_reaps_the_group() -> TestResult {
    let dir = Scratch::new("deadline")?;
    let pid_file = dir.path().join("shell.pid");
    let script = format!("echo $$ > {}; sleep 60", pid_file.display());
    let mut spec = shell(&script);
    // The deadline is the input under test, and any value short of the 60 s
    // sleep tests the same property. It must also outlast the shell's first
    // command, because the group the test checks is named by the pid that
    // command writes: at 200 ms a loaded host killed the shell before it ran
    // (`the shell never recorded its pid`, under seven concurrent builds).
    spec.deadline(Duration::from_secs(2));
    let runtime = lgwks_bot::Runtime::new()?;
    let (deadline_fired, cleanup, leader) = runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        let run = supervisor.run_process(&spec).await?;
        let leader = read_pid(&pid_file)
            .ok_or_else(|| std::io::Error::other("the shell never recorded its pid"))?;
        Ok::<_, Box<dyn std::error::Error>>((run.deadline_fired(), run.cleanup().clone(), leader))
    })?;
    assert!(
        deadline_fired,
        "a stop this supervisor ordered must be reported as a deadline, not as an exit"
    );
    assert_ne!(
        cleanup,
        CleanupReceipt::CleanupFailed,
        "the deadline kill is delivered and the receipt says so, not that it was refused"
    );
    assert!(
        wait_group_gone(leader, BUDGET),
        "the deadline kill must reach the whole group"
    );
    Ok(())
}

/// T19, zero-exit half: a parent that exits zero does not fabricate tree
/// cleanup while a descendant lives.
#[test]
fn a_zero_exit_reaps_a_living_descendant_before_claiming_cleanup() -> TestResult {
    let dir = Scratch::new("zero-exit")?;
    let child_file = dir.path().join("child.pid");
    let shell_file = dir.path().join("shell.pid");
    let script = format!(
        "sleep 60 & echo $! > {}; echo $$ > {}; exit 0",
        child_file.display(),
        shell_file.display()
    );
    let runtime = lgwks_bot::Runtime::new()?;
    let (exit_code, cleanup, leader, descendant) = runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        let run = supervisor.run_process(&shell(&script)).await?;
        let descendant = read_pid(&child_file)
            .ok_or_else(|| std::io::Error::other("the descendant pid was not recorded"))?;
        let leader = read_pid(&shell_file)
            .ok_or_else(|| std::io::Error::other("the shell pid was not recorded"))?;
        Ok::<_, Box<dyn std::error::Error>>((
            run.exit_code(),
            run.cleanup().clone(),
            leader,
            descendant,
        ))
    })?;
    assert_eq!(exit_code, Some(0), "the parent exited zero");
    assert_ne!(
        cleanup,
        CleanupReceipt::CleanupFailed,
        "cleanup must not have failed"
    );
    assert!(
        wait_group_gone(leader, BUDGET),
        "the descendant group must be gone once the report claims cleanup: descendant {descendant}"
    );
    Ok(())
}

// ── run_process: refusals before and after the fork ─────────────────────────

#[test]
fn a_program_that_cannot_start_is_a_typed_pre_fork_refusal() -> TestResult {
    let runtime = lgwks_bot::Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        match supervisor
            .run_process(&ProcessSpec::new("/nonexistent/lgwks-bot-probe"))
            .await
        {
            Err(ProcessRunError::NotStarted { source }) => assert_eq!(
                source.kind(),
                std::io::ErrorKind::NotFound,
                "a missing program must be reported as a failed start, not a run: {source}"
            ),
            other => {
                return Err(std::io::Error::other(format!(
                    "expected a pre-fork refusal, got {other:?}"
                ))
                .into());
            }
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}

#[test]
fn a_cancelled_supervisor_refuses_before_starting_anything() -> TestResult {
    let runtime = lgwks_bot::Runtime::new()?;
    runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        supervisor.cancel();
        match supervisor.run_process(&shell("exit 1")).await {
            Err(ProcessRunError::Refused) => {}
            other => {
                return Err(std::io::Error::other(format!(
                    "a cancelled supervisor must refuse with Refused, got {other:?}"
                ))
                .into());
            }
        }
        assert_eq!(
            supervisor.stats().spawned,
            0,
            "a refusal before the fork must start nothing"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}

// ── run_process: bounded concurrency and drop safety ────────────────────────

/// Two hundred processes through one supervisor with a bound of sixteen: all
/// complete, and the live count never exceeds the bound.
#[test]
fn many_processes_stay_within_the_in_flight_bound() -> TestResult {
    const PROCESSES: usize = 200;
    const BOUND: usize = 16;
    // `Stats::in_flight` counts tasks not yet reaped, which can briefly include
    // one whose permit is already released, so it is not the live count. The
    // children count themselves instead: each marks itself live, records how
    // many live markers it saw, and moves its marker out before exiting. The
    // highest record is a lower bound on the real concurrency, so it can only
    // exceed the bound if the bound was not enforced.
    let live = Scratch::new("bound-live")?;
    let seen = Scratch::new("bound-seen")?;
    let done = Scratch::new("bound-done")?;
    let script = format!(
        "touch {live}/$$; ls {live} | wc -l > {seen}/$$; sleep 0.02; mv {live}/$$ {done}/",
        live = live.path().join("").display(),
        seen = seen.path().join("").display(),
        done = done.path().join("").display(),
    );
    let runtime = lgwks_bot::Runtime::new()?;
    let (completed, succeeded) = runtime.block_on(async {
        let mut supervisor = Supervisor::new(BOUND);
        for _ in 0..PROCESSES {
            supervisor.spawn_process(&shell(&script)).await?;
        }
        let started = std::time::Instant::now();
        while supervisor.stats().in_flight() > 0 {
            supervisor.reap();
            if started.elapsed() >= BUDGET {
                return Err(std::io::Error::other(
                    "processes did not settle within the budget",
                ));
            }
            sleep(Duration::from_millis(2)).await;
        }
        let stats = supervisor.stats();
        Ok::<_, std::io::Error>((stats.completed, stats.succeeded))
    })?;
    let mut high_water = 0_usize;
    let mut records = 0_usize;
    for entry in std::fs::read_dir(seen.path().join(""))? {
        let count = std::fs::read_to_string(entry?.path())?;
        high_water = high_water.max(count.trim().parse::<usize>()?);
        records = records.saturating_add(1);
    }
    assert_eq!(records, PROCESSES, "every child records what it saw");
    assert!(
        high_water <= BOUND,
        "the supervisor ran {high_water} processes at once against a bound of {BOUND}"
    );
    assert_eq!(
        completed,
        u64::try_from(PROCESSES)?,
        "every process must complete"
    );
    assert_eq!(
        succeeded,
        u64::try_from(PROCESSES)?,
        "every zero exit is a clean run"
    );
    Ok(())
}

/// T20: dropping the run future after the fork leaves no orphan.
#[test]
fn dropping_a_run_future_after_the_fork_leaves_no_orphan() -> TestResult {
    let dir = Scratch::new("drop")?;
    let pid_file = dir.path().join("shell.pid");
    // The descendant is forked before the pid is written, so the drop lands
    // after the fork (T20), never while one is in progress.
    let script = format!("sleep 60 & echo $$ > {}; wait", pid_file.display());
    let runtime = lgwks_bot::Runtime::new()?;
    // The run future is dropped the moment its child reports its pid: the fork
    // has happened, the child is asleep, and the future goes away mid-flight.
    // The outer timeout only bounds a runner that never wakes again.
    let leader = runtime.block_on(async {
        let mut supervisor = Supervisor::new(1);
        let spec = shell(&script);
        lgwks_bot::rt::time::timeout(
            BUDGET,
            drop_after_pid(supervisor.run_process(&spec), &pid_file),
        )
        .await
        .ok()
        .flatten()
    });
    let leader =
        leader.ok_or("the run future never started its child, or ended before the drop")?;
    assert!(
        wait_group_stopped(leader, BUDGET),
        "dropping the run future must kill every process of the group it owned"
    );
    Ok(())
}

/// Dropping the run future before its first poll starts nothing at all.
#[test]
fn dropping_a_run_future_before_its_first_poll_starts_nothing() -> TestResult {
    let dir = Scratch::new("pre-poll")?;
    let marker = dir.path().join("marker");
    let script = format!("touch {}", marker.display());
    let mut supervisor = Supervisor::new(1);
    let spec = shell(&script);

    // Building a future and dropping it un-polled runs no code from it, so no
    // fork can have happened. The parked wait gives a mis-designed eager spawn
    // time to appear.
    let future = supervisor.run_process(&spec);
    drop(future);
    std::thread::park_timeout(Duration::from_millis(50));
    assert!(
        !marker.exists(),
        "a run future dropped before its first poll must start no process"
    );
    Ok(())
}

// ── domain::sys::Process ────────────────────────────────────────────────────

#[test]
fn execute_reports_the_exit_code_and_the_captured_streams() -> TestResult {
    let process = process_for("printf out; printf err >&2; exit 3");
    let state = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })?;
    assert_eq!(
        state.exit_code,
        Some(3),
        "the state carries the code; judging it is the caller's job (T35)"
    );
    assert!(!state.running, "a returned one-shot run is not running");
    assert_eq!(state.stdout(), "out", "stdout is reported");
    assert_eq!(state.stderr(), "err", "stderr is reported");
    assert!(!state.stdout_truncated && !state.stderr_truncated);
    assert_eq!(state.stdout_total_bytes, 3);
    assert_eq!(state.stderr_total_bytes, 3);
    assert!(!state.deadline_fired);
    Ok(())
}

#[test]
fn exit_zero_is_reported_as_zero_not_success() -> TestResult {
    let process = process_for("exit 0");
    let state = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })?;
    assert_eq!(
        state.exit_code,
        Some(0),
        "exit zero is an exit code, not a success verdict (T35)"
    );
    Ok(())
}

#[test]
fn a_signal_death_is_reported_in_the_state() -> TestResult {
    let process = process_for("kill -TERM $$");
    let state = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })?;
    assert_eq!(state.exit_code, None, "a signal has no exit code");
    assert_eq!(state.signal, Some(15), "the signal is reported");
    Ok(())
}

#[test]
fn a_deadline_is_reported_in_the_state() -> TestResult {
    let mut spec = shell("sleep 60");
    spec.capture_stdout(DEFAULT_CAPTURE_LIMIT);
    spec.capture_stderr(DEFAULT_CAPTURE_LIMIT);
    spec.deadline(Duration::from_millis(200));
    let process = Process::from_spec(spec);
    let state = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })?;
    assert!(state.deadline_fired, "the deadline stop is reported");
    assert_eq!(
        state.exit_code, None,
        "the deadline stopped the process; it did not exit"
    );
    Ok(())
}

#[test]
fn the_observed_and_query_forms_run_the_command() -> TestResult {
    let process = process_for("exit 0");
    let (observed, queried) = lgwks_bot::block_on(async {
        let observed = process.poll((sys_auth()?, ())).await?;
        let queried = process.query((sys_auth()?, &())).await?;
        Ok::<_, BotError>((observed, queried))
    })?;
    assert_eq!(
        observed.exit_code,
        Some(0),
        "poll is not a silent no-op: it runs and reports"
    );
    assert_eq!(
        queried.exit_code,
        Some(0),
        "query is not a silent no-op: it runs and reports"
    );
    Ok(())
}

#[test]
fn a_missing_program_is_a_refusal_with_nothing_run() -> TestResult {
    let process = Process::from_spec(ProcessSpec::new("/nonexistent/lgwks-bot-probe"));
    let error = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })
        .err()
        .ok_or_else(|| std::io::Error::other("a missing program must be refused"))?;
    assert_eq!(
        error.dispatch_certainty(),
        DispatchCertainty::Refused,
        "a program that never started establishes that nothing ran: {error}"
    );
    Ok(())
}

#[test]
fn a_missing_capability_is_refused_before_any_spawn() -> TestResult {
    let dir = Scratch::new("cap")?;
    let marker = dir.path().join("marker");
    let process = process_for(&format!("touch {}", marker.display()));
    let error = lgwks_bot::block_on(async { process.execute_action((empty_auth()?, &())).await })
        .err()
        .ok_or_else(|| std::io::Error::other("the capability check must deny"))?;
    assert!(
        matches!(error, BotError::CapabilityDenied { .. }),
        "a call without bot.sys must be refused as a capability deficit: {error}"
    );
    std::thread::park_timeout(Duration::from_millis(50));
    assert!(
        !marker.exists(),
        "the cap check must precede any spawn, so the command never ran"
    );
    Ok(())
}

/// The default constructor bounds its streams and its runtime even for a child
/// that would not bound itself.
#[test]
fn the_default_constructor_runs_a_real_child() -> TestResult {
    let process = Process::new("true");
    let state = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })?;
    assert_eq!(
        state.exit_code,
        Some(0),
        "the default constructor runs a real child"
    );
    assert!(
        !state.deadline_fired,
        "an ordinary child finishes before the default deadline"
    );
    Ok(())
}

/// Concurrent verb calls on one `Process` share its ceiling: a burst of calls
/// waits for slots instead of forking a burst of children. Each child counts
/// the live children (its own marker included) when it starts, so the highest
/// count any child printed is a lower bound on the real concurrency and can
/// only exceed the ceiling if the ceiling was not enforced.
#[test]
fn concurrent_calls_on_one_process_share_its_ceiling() -> TestResult {
    const CALLS: usize = 64;
    const CEILING: usize = 4;
    let live = Scratch::new("ceiling-live")?;
    let done = Scratch::new("ceiling-done")?;
    let live_dir = live.path().join("");
    let done_dir = done.path().join("");
    let script = format!(
        "touch {live}/$$; ls {live} | wc -l; sleep 0.2; mv {live}/$$ {done}/",
        live = live_dir.display(),
        done = done_dir.display(),
    );
    let process = process_for(&script)
        .max_concurrent(NonZeroUsize::new(CEILING).ok_or("the ceiling admits at least one call")?);
    let auth = sys_auth()?;
    let states = lgwks_bot::block_on(lgwks_std::task::join_all(
        (0..CALLS).map(|_| process.execute_action((auth.clone(), &()))),
    ));
    let mut highest = 0_usize;
    for state in states {
        let state = state?;
        assert_eq!(
            state.exit_code,
            Some(0),
            "every call runs its child to a zero exit"
        );
        highest = highest.max(state.stdout().trim().parse::<usize>()?);
    }
    assert!(
        highest <= CEILING,
        "a child saw {highest} live children against a ceiling of {CEILING}"
    );
    assert!(
        highest >= 2,
        "the calls must overlap up to the ceiling, not run one at a time"
    );
    Ok(())
}

/// The capture builders set the data policy the runner reads, and the ceiling
/// is non-zero by its type.
#[test]
fn capture_builders_set_the_declared_policy() -> TestResult {
    let limit = NonZeroUsize::new(1024).ok_or("a capture limit of 1024 is not zero")?;
    let mut spec = ProcessSpec::new("true");
    spec.capture_stdout(limit);
    spec.capture_stderr(limit);
    assert_eq!(
        spec.stdout_policy(),
        StdioPolicy::Capture(limit),
        "capture_stdout must set the Capture policy"
    );
    assert_eq!(
        spec.stderr_policy(),
        StdioPolicy::Capture(limit),
        "capture_stderr must set the Capture policy"
    );
    Ok(())
}

// ── domain::sys::Process, the production door to the frame grammar ──────────

/// A `Process` for `script` whose verbs report a framed stdout reading.
///
/// Built on `Process::from_spec` plus `frame_stdout`, the two steps a caller
/// takes, so the framed reading is reached through the verb rather than only by
/// a test that re-plumbs a captured slice.
fn framing_process(
    script: &str,
    capture: NonZeroUsize,
) -> Result<Process, Box<dyn std::error::Error>> {
    let ceiling = NonZeroUsize::new(4096).ok_or("a frame ceiling of at least one")?;
    let mut spec = shell(script);
    spec.capture_stdout(capture);
    spec.capture_stderr(capture);
    spec.deadline(DEFAULT_DEADLINE);
    Ok(Process::from_spec(spec).frame_stdout(ceiling))
}

/// T05, wired: the frames of a child's output are read through the verb a
/// caller performs work with, not only through a reader the test plumbs.
///
/// The child writes two whole records and then a third whose payload stops
/// half way. The `ProcessState` the `Execute` verb returns must carry exactly
/// the two whole records and end in the typed truncation, which is what makes
/// `CapturedStream::frames` a production path rather than a capability with no
/// caller.
#[test]
fn the_execute_verb_reports_stdout_frames_two_records_and_a_cut_third() -> TestResult {
    let script = "printf '\\000\\000\\000\\004done\\000\\000\\000\\002ok\\000\\000\\000\\010part'";
    let process = framing_process(script, DEFAULT_CAPTURE_LIMIT)?;
    let state = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })?;
    assert_eq!(
        state.exit_code,
        Some(0),
        "the child completed its own work; the framing is what is under test"
    );
    assert!(
        !state.stdout_truncated,
        "the write fits the capture ceiling, so the ending is the child's own"
    );
    let frames = state.stdout_frames().ok_or_else(|| {
        std::io::Error::other("a domain built with frame_stdout must report frames")
    })?;
    assert_eq!(
        frames.records().len(),
        2,
        "only the records that arrived whole are frames: {:?}",
        frames.records()
    );
    let payloads: Vec<&[u8]> = frames
        .records()
        .iter()
        .filter_map(FrameRead::payload)
        .collect();
    assert_eq!(
        payloads,
        vec![&b"done"[..], &b"ok"[..]],
        "both whole records decode to exactly what the child wrote"
    );
    assert_eq!(
        frames.ended(),
        &FrameRead::TruncatedPayload {
            declared: 8,
            partial: b"part".to_vec(),
        },
        "a prefix naming eight bytes that delivered four is a truncation"
    );
    assert!(
        !frames.is_complete(),
        "a stream whose last record was cut off has no complete reading"
    );
    Ok(())
}

/// D2 wired: a capture that cut the child's output ends at the capture's own
/// ceiling, and the domain reports it that way rather than as the child's
/// truncation or a clean end.
#[test]
fn the_execute_verb_reports_the_capture_ceiling_when_the_child_overruns_it() -> TestResult {
    const CAPTURE: usize = 8;
    let script = "printf '\\000\\000\\000\\002ok\\000\\000\\000\\002ok\\000\\000\\000\\002ok'";
    let process = framing_process(script, NonZeroUsize::new(CAPTURE).ok_or("a ceiling")?)?;
    let state = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })?;
    assert!(
        state.stdout_truncated,
        "ten bytes into an {CAPTURE}-byte capture must report that the capture cut the stream"
    );
    let frames = state.stdout_frames().ok_or_else(|| {
        std::io::Error::other("a domain built with frame_stdout must report frames")
    })?;
    assert_eq!(
        frames.ended(),
        &FrameRead::CeilingReached { ceiling: CAPTURE },
        "the retained bytes are a prefix the capture cut, so the ending is the capture's \
         retained capacity, never a child truncation"
    );
    assert!(
        !frames.is_complete(),
        "a reading of a capture-cut prefix is never a complete reading of the child"
    );
    Ok(())
}

/// A domain built without `frame_stdout` reports no frames and an unchanged
/// lossy stdout view, so attaching the reading is opt-in and changes nothing
/// else about the run.
#[test]
fn a_domain_without_frame_stdout_reports_no_frames_and_unchanged_stdout() -> TestResult {
    let process = process_for("printf out");
    let state = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })?;
    assert!(
        state.stdout_frames().is_none(),
        "a domain with no frame_stdout must not invent a framed reading"
    );
    assert_eq!(
        state.stdout(),
        "out",
        "the lossy stdout view is unchanged by the absence of a framed reading"
    );
    Ok(())
}

/// The framed reading is byte-exact where the lossy view cannot be.
///
/// The child writes one record whose payload is two bytes that are not valid
/// UTF-8. `stdout_frames()` returns them exactly, while `stdout()` replaces
/// them with U+FFFD, so the two readings of the same child output differ by
/// construction — the discriminating case for why the framed door exists.
#[test]
fn a_binary_record_round_trips_through_frames_while_stdout_is_lossy() -> TestResult {
    let script = "printf '\\000\\000\\000\\002\\377\\376'";
    let process = framing_process(script, DEFAULT_CAPTURE_LIMIT)?;
    let state = lgwks_bot::block_on(async { process.execute_action((sys_auth()?, &())).await })?;
    let frames = state.stdout_frames().ok_or_else(|| {
        std::io::Error::other("a domain built with frame_stdout must report frames")
    })?;
    assert_eq!(frames.records().len(), 1, "one whole record arrived");
    assert_eq!(
        frames.records()[0].payload(),
        Some(&[0xff_u8, 0xfe_u8][..]),
        "the framed reading round-trips the binary payload byte for byte"
    );
    assert_eq!(
        frames.ended(),
        &FrameRead::EndOfStream,
        "the whole record ended cleanly"
    );
    assert_ne!(
        state.stdout().as_bytes(),
        &[0xff_u8, 0xfe_u8][..],
        "the lossy view cannot round-trip bytes that are not UTF-8"
    );
    assert!(
        state.stdout().contains('\u{FFFD}'),
        "the lossy view replaces the invalid bytes: {:?}",
        state.stdout()
    );
    Ok(())
}
