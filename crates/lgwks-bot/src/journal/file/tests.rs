//! Unit tests for the file journal, moved verbatim out of file.rs.

use super::*;
use crate::effect::{
    ActionDigest, ActionId, AttemptId, EnvironmentEpoch, EnvironmentId, FlowRevision, RunId,
};
use crate::journal::chain;
use crate::journal::frame::probe::{declared_at, frame_starts, with_prefix};
use crate::journal::{AttemptStatus, EventKind};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Removes a test's scratch path when the test ends, however it ends.
///
/// The guard is best effort: a scratch file the system refuses to remove
/// is litter, not a failed observation, so the error is dropped rather
/// than allowed to mask the test's own verdict.
struct TempGuard(std::path::PathBuf);

impl Drop for TempGuard {
    fn drop(&mut self) {
        if self.0.is_dir() {
            drop(std::fs::remove_dir_all(&self.0));
        } else {
            drop(std::fs::remove_file(&self.0));
        }
    }
}

const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const DIGEST_HEX: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";

/// A scratch path unique to one test run.
///
/// Shared with `journal::owner`'s tests, so the journal's two test suites name
/// their scratch files one way (INV-DEP-6). The construction itself is
/// `frame::probe`'s, because the two halves of a unique name are not a detail a
/// second copy may spell differently.
pub(in crate::journal) fn scratch(name: &str) -> Result<PathBuf, std::io::Error> {
    crate::journal::frame::probe::scratch_path("journal-file", name)
}

/// The one key builder: every field but the attempt is this module's fixture
/// constant, and a test that wants a second attempt differs only in that
/// field. Building it in three copies meant three places for a field to differ.
fn key_for_attempt(attempt: &str) -> Result<crate::effect::EffectKey, Box<dyn std::error::Error>> {
    let run = RunId::from_hex(RUN)?;
    let action = ActionId::from_hex(ACTION)?;
    let attempt = AttemptId::from_decimal(attempt)?;
    let flow = FlowRevision::from_tagged("blake3_256", FLOW_HEX)?;
    let digest = ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?;
    let environment = EnvironmentId::from_hex(ENV)?;
    let epoch = EnvironmentEpoch::from_decimal("1")?;
    Ok(crate::effect::EffectIdentity::new(run, environment, flow)
        .key(action, attempt, digest, epoch))
}

/// The first attempt of the run every test in this module writes.
fn key() -> Result<crate::effect::EffectKey, Box<dyn std::error::Error>> {
    key_for_attempt("1")
}

/// A scratch path and the guard that removes it when the test ends.
///
/// Both halves in one value because both are needed before anything is
/// written, and a guard returned beside its path cannot be dropped by a test
/// that only meant to keep the path.
fn subject(name: &str) -> Result<(PathBuf, TempGuard), Box<dyn std::error::Error>> {
    let path = scratch(name)?;
    let guard = TempGuard(path.clone());
    Ok((path, guard))
}

/// The successor writer reports a generation directory it made, and only one.
///
/// The answer decides whether the directory's own name is flushed into its
/// parent, so an answer of "already there" for a directory this call created
/// is a new generation whose name a power loss can take back. Read after the
/// create, it was always "already there".
#[test]
fn writing_a_successor_reports_the_directory_it_created() -> TestResult {
    let (base, _guard) = subject("successor-dir")?;
    std::fs::create_dir_all(&base)?;
    let first = base.join("run.jrnl.cont").join("000001");
    assert!(
        write_successor(&first, b"frame")?,
        "the first generation's directory is created by this write"
    );
    let second = base.join("run.jrnl.cont").join("000002");
    assert!(
        !write_successor(&second, b"frame")?,
        "a later generation writes into a directory that already exists"
    );
    assert_eq!(std::fs::read(&second)?, b"frame");
    Ok(())
}

/// Make the next append of `journal` fail the way a device fails: after
/// the fence has passed, so the refusal is genuinely about the write.
///
/// Growing the file from outside is the other way in, and these tests use
/// it too — but that is a *fence* refusal, because the handle is stale,
/// which is a different contract with a different answer. A write that
/// fails after the fence is what the poison latch exists for, and no
/// filesystem produces one on demand.
fn fail_next_write_of(journal: &FileJournal) {
    journal.storage.fail_next_commit();
}

/// A key for attempt `n`, so a frame count can be built without
/// tripping the ladder's one-climb-per-key rule.
fn attempt_key(n: u64) -> Result<crate::effect::EffectKey, Box<dyn std::error::Error>> {
    key_for_attempt(&n.to_string())
}

/// A second key, so a batch can fail on its own rung.
fn key2() -> Result<crate::effect::EffectKey, Box<dyn std::error::Error>> {
    key_for_attempt("7")
}

#[test]
fn a_failed_batch_write_latches_the_poison_like_a_single_append() -> TestResult {
    let (path, _guard) = subject("batch-poison")?;
    // Every call gets its own attempt key: the ladder refuses a second
    // rung one attempt, and that refusal must never mask the poison
    // fence these assertions are about.
    let event = |attempt: u64| -> Result<EffectEvent, Box<dyn std::error::Error>> {
        Ok(EffectEvent::IntentAdmitted {
            key: attempt_key(attempt)?,
        })
    };

    // The single append's policy, pinned for parity: a write that fails
    // reports an unknown outcome and latches the poison, and the handle
    // refuses everything after it.
    let mut journal = FileJournal::open(&path)?;
    journal.compare_and_append(journal.tail(), &event(200)?)?;
    fail_next_write_of(&journal);
    match journal.compare_and_append(journal.tail(), &event(201)?) {
        Err(JournalError::OutcomeUnknown { .. }) => {}
        Err(other) => return Err(format!("expected an unknown outcome, got {other}").into()),
        Ok(_) => return Err("a failed write must not acknowledge".into()),
    }
    match journal.compare_and_append(journal.tail(), &event(201)?) {
        Err(error) => assert!(
            error.to_string().contains("previous append failed"),
            "the poisoned handle refused with its own fence, not {error}"
        ),
        Ok(_) => return Err("a poisoned handle must refuse further appends".into()),
    }
    drop(journal);

    // The batch must answer with the same two facts: the failure is an
    // unknown outcome — a prefix or all of the batch may be on the disk —
    // and the handle is poisoned afterwards, not silently reusable.
    let mut journal = FileJournal::open(&path)?;
    journal.compare_and_append(journal.tail(), &event(300)?)?;
    fail_next_write_of(&journal);
    match journal.compare_and_append_all(&[event(301)?, event(302)?]) {
        Err(JournalError::OutcomeUnknown { .. }) => {}
        Err(other) => {
            return Err(format!(
                "a failed batch write answered {other}, not the single append's unknown outcome"
            )
            .into());
        }
        Ok(acks) => {
            let _ = acks;
            return Err("a failed batch write must not acknowledge".into());
        }
    }
    match journal.compare_and_append_all(&[event(303)?]) {
        Err(error) => assert!(
            error.to_string().contains("previous append failed"),
            "the poisoned handle refused with its own fence, not {error}"
        ),
        Ok(_) => return Err("a poisoned handle must refuse further batches".into()),
    }
    Ok(())
}

/// A storage device that has stopped answering must not stop the task that
/// is waiting on it.
///
/// This is finding R07's acceptance case, and the instrument is chosen so
/// there is no timing race anywhere in it. The journal is opened on a
/// stalled device, so the flush provably cannot complete until
/// [`FileJournal::release_storage`] is called — the append is not "slow",
/// it is *parked*, and no amount of waiting would finish it. The test then
/// drives the same current-thread runtime the finding is about, counts how
/// many times an unrelated task ran while the append was outstanding, and
/// only then releases the storage.
///
/// On a tree whose durable write runs on the awaiting thread, the append is
/// not merely slow: it never returns, so the release is never reached. That
/// is the negative control, and it is why the assertion is about the count
/// and not about a duration.
#[test]
#[cfg(feature = "rt")]
fn a_stalled_device_does_not_stop_the_task_waiting_on_it() -> TestResult {
    use std::future::poll_fn;
    use std::task::Poll;
    use std::time::{Duration, Instant};

    let (path, _guard) = subject("stalled-device")?;
    let mut journal = FileJournal::open_with_stalled_storage(&path)?;
    // Taken before the append starts: once the future holds the journal's
    // borrow, the journal itself is unreachable for the whole wait.
    let gate = journal.storage_gate();
    let event = EffectEvent::IntentAdmitted {
        key: attempt_key(900)?,
    };
    let tail = journal.tail();
    let runtime = crate::rt::runtime::Runtime::new()?;

    let mut append = Box::pin(journal.compare_and_append_async(tail, &event));
    let mut last = Instant::now();
    let mut longest = Duration::ZERO;
    let mut ticks = 0u64;

    let ack = runtime.block_on(poll_fn(|cx| {
        // The unrelated task, sampled on every wakeup. Its gap is the
        // runtime's unavailability, which is the thing under test.
        let now = Instant::now();
        longest = longest.max(now.duration_since(last));
        last = now;
        ticks = ticks.saturating_add(1);

        match append.as_mut().poll(cx) {
            Poll::Ready(answer) => Poll::Ready(answer),
            Poll::Pending => {
                // Nothing else is queued on this runtime, so the wakeup
                // that stands in for "an unrelated task got a turn" has to
                // come from here. Without it the task parks on the stalled
                // device and never reaches the release below — which is
                // the point: the device is genuinely not answering.
                if ticks >= 64 {
                    gate.release();
                }
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }))?;
    // The future holds the handle's borrow, and it is the future's drop
    // that tells the storage owner nobody is waiting any more.
    drop(append);

    assert!(
        ticks >= 64,
        "the runtime stopped after {ticks} wakeups before the parked flush \
             was released: the durable write is running on the awaiting thread"
    );
    assert_eq!(
        ack.promise(),
        DurabilityPromise::ProcessCrash,
        "the acknowledgment still has to be earned, and a released device earns it"
    );
    assert!(
        longest < Duration::from_millis(500),
        "the executor was unavailable for {longest:?} while a flush that \
             could not finish was outstanding"
    );
    assert_eq!(
        EffectJournal::committed(&journal)?.len(),
        1,
        "the frame was folded in"
    );
    drop(journal);
    let reopened = FileJournal::open(&path)?;
    assert_eq!(
        EffectJournal::committed(&reopened)?.len(),
        1,
        "the released frame must be on the disk, not only in the handle"
    );
    Ok(())
}

/// A caller that walks away from a durable append it started leaves the
/// handle unable to say whether the bytes landed, and the handle must say
/// so rather than carry on.
///
/// The finding's second half: dropping a waiter is not proof that the
/// append stopped. This drives that case and then checks the two things
/// that make it safe — the handle refuses to append again, and a reopen
/// reconciles by reading the file back, with no second frame for the same
/// attempt.
#[test]
#[cfg(feature = "rt")]
fn a_dropped_waiter_poisons_the_handle_and_a_reopen_does_not_duplicate() -> TestResult {
    use std::future::poll_fn;
    use std::task::Poll;

    let (path, _guard) = subject("dropped-waiter")?;
    let mut journal = FileJournal::open_with_stalled_storage(&path)?;
    let gate = journal.storage_gate();
    let event = EffectEvent::IntentAdmitted {
        key: attempt_key(901)?,
    };
    let tail = journal.tail();
    let runtime = crate::rt::runtime::Runtime::new()?;

    // Poll the append exactly once, so the storage owner has been handed
    // the request, then drop it: the caller is gone while the bytes are
    // still moving. This is the cancellation the finding names, and the
    // device is parked so the ordering is the one under test rather than a
    // race with a fast disk — the owner cannot finish before the drop.
    let mut append = Box::pin(journal.compare_and_append_async(tail, &event));
    let started = runtime.block_on(poll_fn(|cx| {
        let polled = append.as_mut().poll(cx);
        if polled.is_pending() {
            cx.waker().wake_by_ref();
        }
        Poll::Ready(polled)
    }));
    assert!(
        started.is_pending(),
        "the append completed before it could be abandoned"
    );
    drop(append);
    gate.release();

    // The owner finishes the write it was given and finds no waiter. The
    // handle must be poisoned once it has. Polled against a deadline
    // rather than a spin count: what is being waited for is a `sync_all`,
    // which on a real device is milliseconds, so a yield budget would be
    // racing the disk rather than the owner.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !journal.storage.poisoned() {
        if std::time::Instant::now() >= deadline {
            return Err("the storage owner never latched the poison".into());
        }
        std::thread::yield_now();
    }
    match journal.compare_and_append(journal.tail(), &event) {
        Err(error) => assert!(
            error.to_string().contains("previous append failed"),
            "an abandoned append must refuse with its own fence, not {error}"
        ),
        Ok(_) => return Err("a handle that lost its waiter must refuse".into()),
    }
    drop(journal);

    // The reopen is the reconciliation: it reads what actually landed, and
    // the ladder then refuses the same rung again, so a retry cannot write
    // a second frame for one attempt.
    let mut reopened = FileJournal::open(&path)?;
    let landed = EffectJournal::committed(&reopened)?.len();
    assert!(
        landed <= 1,
        "an abandoned append wrote {landed} frames for one attempt"
    );
    let same = EffectEvent::IntentAdmitted {
        key: attempt_key(901)?,
    };
    assert!(
        reopened.compare_and_append(reopened.tail(), &same).is_err(),
        "the replayed ladder must refuse a rung the file already records"
    );
    Ok(())
}

/// The raw bytes of one frame, built the way the writer builds them, so
/// the scan-level fault tests can serve exact prefixes without a file.
///
/// Through the shared grammar rather than by hand, because a hand-rolled frame
/// in the test would be a second definition of the thing the tests are supposed
/// to be checking — a fixture that drifted from the writer would prove nothing.
fn frame_bytes(
    position: JournalPosition,
    event: &EffectEvent,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let payload = event.to_bytes()?;
    let head = chain(position, event)?;
    let length = u32::try_from(payload.len())?;
    Ok(crate::journal::frame::encode(length, &payload, &head))
}

/// A reader that serves bytes up to an absolute offset and fails every
/// read past it, the way a storage fault mid-frame presents to the scan.
struct FaultyAfter {
    inner: std::io::Cursor<Vec<u8>>,
    serve: u64,
}

impl Read for FaultyAfter {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.inner.position() >= self.serve {
            let refusal = Err(std::io::Error::other("injected storage fault"));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "read: returning an error to the caller");
            return refusal;
        }
        let remaining = self.serve.saturating_sub(self.inner.position());
        // A `serve` this host cannot count is at least every byte the buffer
        // holds, so the ceiling below is what bounds the read either way.
        let cap = usize::saturating_from(remaining).min(buf.len());
        self.inner.read(&mut buf[..cap])
    }
}

#[test]
fn a_storage_fault_mid_frame_is_storage_not_a_torn_tail() -> TestResult {
    let event = EffectEvent::IntentAdmitted { key: key()? };
    let frame = frame_bytes(JournalPosition::genesis(), &event)?;
    // One fault inside the length prefix, one inside the payload, one
    // inside the head: whichever field the fault reaches, the scan must
    // answer with the storage error itself. Every frame read shares one
    // classification, so no fault can be mistaken for the evidence of an
    // interrupted append and repaired destructively.
    let mid_head = frame.len().saturating_sub(HEAD_BYTES).saturating_add(1);
    for serve in [1usize, LENGTH_BYTES.saturating_add(1), mid_head] {
        let serve = u64::try_from(serve)?;
        let mut faulty = FaultyAfter {
            inner: std::io::Cursor::new(frame.clone()),
            serve,
        };
        match scan(&mut faulty, MAX_JOURNAL_EVENTS) {
            Err(JournalError::Storage(_)) => {}
            Err(other) => {
                return Err(format!(
                    "a fault {serve} bytes in was classified {other}, not storage"
                )
                .into());
            }
            Ok(scanned) => {
                let stop = scanned.stop;
                return Err(format!(
                    "a fault {serve} bytes in stopped the scan as {stop:?}, not storage"
                )
                .into());
            }
        }
    }
    Ok(())
}

#[test]
fn scanning_refuses_a_complete_event_beyond_the_limit() -> TestResult {
    let first = EffectEvent::IntentAdmitted {
        key: attempt_key(1)?,
    };
    let second = EffectEvent::IntentAdmitted {
        key: attempt_key(2)?,
    };
    let genesis = JournalPosition::genesis();
    let first_position = JournalPosition {
        sequence: 1,
        head: chain(genesis, &first)?,
    };
    let mut bytes = frame_bytes(genesis, &first)?;
    bytes.extend_from_slice(&frame_bytes(first_position, &second)?);
    let mut reader = std::io::Cursor::new(bytes);

    match scan(&mut reader, 1) {
        Err(JournalError::CapacityExceeded {
            resource: JournalLimitKind::Events,
            limit,
            requested,
        }) => {
            assert_eq!(limit, 1);
            assert_eq!(requested, 2);
        }
        Err(other) => return Err(format!("expected event-limit refusal, got {other}").into()),
        Ok(scanned) => {
            let entries = scanned.entries;
            return Err(format!(
                "two frames under a one-event limit must refuse, retained {}",
                entries.len()
            )
            .into());
        }
    }
    Ok(())
}

#[test]
fn open_refuses_an_over_limit_file_without_truncating_it() -> TestResult {
    let (path, _guard) = subject("byte-limit")?;
    let file = File::create(&path)?;
    let requested = MAX_JOURNAL_BYTES.saturating_add(1);
    file.set_len(requested)?;
    drop(file);

    match FileJournal::open(&path) {
        Err(JournalError::CapacityExceeded {
            resource: JournalLimitKind::Bytes,
            limit,
            requested: actual,
        }) => {
            assert_eq!(limit, MAX_JOURNAL_BYTES);
            assert_eq!(actual, requested);
        }
        Err(other) => return Err(format!("expected byte-limit refusal, got {other}").into()),
        Ok(_) => return Err("an over-limit journal must not be opened".into()),
    }
    assert_eq!(
        std::fs::metadata(&path)?.len(),
        requested,
        "capacity refusal preserves the existing file byte-for-byte in length"
    );
    Ok(())
}

#[test]
fn batch_admission_refuses_history_over_the_event_limit_without_writing() -> TestResult {
    let (path, _guard) = subject("event-limit")?;
    let requested = MAX_JOURNAL_EVENTS.saturating_add(1);
    let mut events = Vec::new();
    for attempt in 1..=requested {
        events.push(EffectEvent::IntentAdmitted {
            key: attempt_key(u64::try_from(attempt)?)?,
        });
    }
    let mut journal = FileJournal::open(&path)?;

    match journal.compare_and_append_all(&events) {
        Err(JournalError::CapacityExceeded {
            resource: JournalLimitKind::Events,
            limit,
            requested: actual,
        }) => {
            assert_eq!(limit, u64::try_from(MAX_JOURNAL_EVENTS)?);
            assert_eq!(actual, u64::try_from(requested)?);
        }
        Err(other) => return Err(format!("expected event-limit refusal, got {other}").into()),
        Ok(acks) => {
            return Err(format!(
                "over-limit batch must refuse before write, returned {} acknowledgments",
                acks.len()
            )
            .into());
        }
    }
    assert!(
        journal.committed()?.is_empty(),
        "refused batch leaves the complete prior history unchanged"
    );
    assert_eq!(
        std::fs::metadata(&path)?.len(),
        0,
        "refused batch writes no partial prefix"
    );
    Ok(())
}

/// Writes three acknowledged frames and returns the file's bytes plus the
/// byte offset where the third frame begins.
fn three_frame_file(path: &Path) -> Result<(Vec<u8>, usize), Box<dyn std::error::Error>> {
    {
        let mut journal = FileJournal::open(path)?;
        for attempt in 1u64..=3 {
            journal.compare_and_append(
                journal.tail(),
                &EffectEvent::IntentAdmitted {
                    key: attempt_key(attempt)?,
                },
            )?;
        }
    }
    let bytes = std::fs::read(path)?;
    let first = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let first_len = crate::journal::frame::framed_len(usize::try_from(first)?);
    let next = first_len;
    let mut second_prefix = [0u8; LENGTH_BYTES];
    for (slot, offset) in second_prefix.iter_mut().zip(0u64..) {
        let index = next.saturating_add(offset);
        *slot = *bytes
            .get(usize::try_from(index)?)
            .ok_or("fixture file is shorter than its own first frame")?;
    }
    let second = u32::from_be_bytes(second_prefix);
    let second_len = crate::journal::frame::framed_len(usize::try_from(second)?);
    Ok((
        bytes,
        usize::try_from(first_len.saturating_add(second_len))?,
    ))
}

#[test]
fn an_inflated_length_over_a_complete_final_frame_is_refused_not_trimmed() -> TestResult {
    let (path, _guard) = subject("inflated-length")?;
    let (mut bytes, third_start) = three_frame_file(&path)?;
    let before = bytes.len();

    // Every length a scan can meet at the last frame's prefix: the
    // maximum the frame bound allows, one over it, and zero. None of them
    // is a torn append — a killed writer leaves a prefix of the bytes it
    // intended, and its completed prefix is always the true length — so
    // each must be refused with the file left byte-for-byte alone. The
    // inflation case is the destructive one: the declared payload runs
    // past the end of the file, and the tail looks torn to a reader that
    // trusts the prefix.
    let inflated = u32::try_from(MAX_FRAME_BYTES)?;
    let over_limit = inflated.saturating_add(1);
    for (name, replacement) in [
        ("the maximum legal length", inflated),
        ("one byte over the limit", over_limit),
        ("a zero length", 0u32),
    ] {
        for (slot, offset) in bytes[third_start..]
            .iter_mut()
            .take(LENGTH_BYTES)
            .zip(0usize..)
        {
            *slot = replacement.to_be_bytes()[offset];
        }
        std::fs::write(&path, &bytes)?;

        match FileJournal::open(&path) {
            Err(JournalError::Corrupt(corruption)) => {
                assert_eq!(corruption.at(), 2, "{name}: the lying frame is named");
                assert!(
                    matches!(corruption.kind(), CorruptionKind::Framed),
                    "{name}: the refusal names the framing, got {:?}",
                    corruption.kind()
                );
            }
            Err(other) => {
                return Err(format!("{name}: expected a corruption refusal, got {other}").into());
            }
            Ok(_) => {
                return Err(format!("{name}: a lying length must not reopen as a journal").into());
            }
        }
        assert_eq!(
            std::fs::metadata(&path)?.len(),
            u64::try_from(before)?,
            "{name}: refused bytes are never trimmed"
        );
    }
    Ok(())
}

#[test]
fn a_short_final_frame_is_still_repaired_as_a_torn_tail() -> TestResult {
    let (path, _guard) = subject("torn-tail")?;
    let (bytes, third_start) = three_frame_file(&path)?;
    let complete_len = u64::try_from(third_start)?;

    // A real interrupted append: the file ends mid-frame. This tail was
    // never acknowledged, so the reopen repairs it — and the repair must
    // survive the corruption distinction, not be swallowed by it.
    let torn = &bytes[..third_start.saturating_add(10)];
    std::fs::write(&path, torn)?;

    let reopened = FileJournal::open(&path)?;
    assert!(
        reopened.torn_tail_repaired(),
        "an interrupted append is repaired, not refused"
    );
    assert_eq!(
        reopened.committed()?.len(),
        2,
        "the two kept frames survive"
    );
    assert_eq!(
        std::fs::metadata(&path)?.len(),
        complete_len,
        "the repair restores the prefix every acknowledgment names"
    );
    Ok(())
}

/// Open `path` and require a refusal as `Corrupt` at frame `at`, leaving the
/// file byte-identical to `before`.
fn require_refused_untouched(path: &Path, before: &[u8], at: u64, why: &str) -> TestResult {
    let outcome: TestResult = match FileJournal::open(path) {
        Err(JournalError::Corrupt(corruption)) => {
            assert_eq!(corruption.at(), at, "{why}: the lying frame is named");
            assert!(
                matches!(corruption.kind(), CorruptionKind::Framed),
                "{why}: the refusal names the framing, got {:?}",
                corruption.kind()
            );
            Ok(())
        }
        Err(other) => Err(format!("{why}: expected a corruption refusal, got {other}").into()),
        Ok(opened) => Err(format!(
            "{why}: reopened as a journal of {} events, so an acknowledged frame was lost",
            opened.events().count()
        )
        .into()),
    };
    outcome?;
    assert_eq!(
        lgwks_std::hash::blake3(&std::fs::read(path)?),
        lgwks_std::hash::blake3(before),
        "{why}: refused bytes are never touched"
    );
    Ok(())
}

/// The acknowledged final frame whose length grows by `k` is the defect #262
/// reported. `k` in `1..=32` leaves the payload whole and the head short, so the
/// payload read succeeds and the head read is what ends early; `k` above `32`
/// ends inside the payload. Both are one lie told two ways, and both refuse.
#[test]
fn a_lengthened_acknowledged_final_frame_is_refused_not_trimmed() -> TestResult {
    let (path, _guard) = subject("lengthened-final")?;
    let (bytes, third) = three_frame_file(&path)?;
    let declared = declared_at(&bytes, third);
    for extra in 1u32..=1024 {
        let lied = with_prefix(&bytes, third, declared + extra);
        std::fs::write(&path, &lied)?;
        require_refused_untouched(&path, &lied, 2, &format!("final frame L+{extra}"))?;
    }
    Ok(())
}

/// The same lie on a successor whose one event follows the seal it carries.
///
/// The cut is resolved against the last whole frame, and in a successor with
/// no event before the lie that frame is the carried seal. Resolved against
/// the events alone it was the genesis, which the acknowledged event does not
/// chain from, so the lie read as a torn append and the event was trimmed.
#[test]
fn a_lengthened_event_behind_a_carried_seal_is_refused_not_trimmed() -> TestResult {
    let (dir, _guard) = subject("lengthened-successor")?;
    std::fs::create_dir_all(&dir)?;
    let successor = {
        let mut journal = FileJournal::open_continuing_with(
            dir.join("run.jrnl"),
            crate::journal::ContinuationPolicy::declared(),
        )?;
        let first = EffectEvent::IntentAdmitted {
            key: attempt_key(1)?,
        };
        journal.compare_and_append(journal.tail(), &first)?;
        let mut next = journal
            .continue_as_file()?
            .ok_or("a continuing journal hands back its successor")?;
        let second = EffectEvent::IntentAdmitted {
            key: attempt_key(2)?,
        };
        next.compare_and_append(next.tail(), &second)?;
        next.path().to_path_buf()
    };
    let bytes = std::fs::read(&successor)?;
    let event = frame_starts(&bytes, 0)?[1];
    let declared = declared_at(&bytes, event);
    for extra in 1u32..=96 {
        let lied = with_prefix(&bytes, event, declared + extra);
        std::fs::write(&successor, &lied)?;
        require_refused_untouched(&successor, &lied, 0, &format!("successor event L+{extra}"))?;
    }
    Ok(())
}

/// The second shape from the same finding: an acknowledged frame with frames
/// behind it whose length is inflated past the end of the file. The true frame
/// is still the first thing behind the prefix, so it authenticates and the two
/// frames after it are not read as one more candidate and dropped.
#[test]
fn an_inflated_non_final_length_is_refused_and_every_byte_survives() -> TestResult {
    let (path, _guard) = subject("inflated-middle")?;
    let (bytes, _) = three_frame_file(&path)?;
    let middle = frame_starts(&bytes, 0)?[1];
    let remaining = u32::try_from(bytes.len() - middle)?;
    for declared in [
        remaining,
        remaining + 1,
        remaining + 31,
        remaining + 500,
        u32::try_from(MAX_FRAME_BYTES)?,
    ] {
        let lied = with_prefix(&bytes, middle, declared);
        std::fs::write(&path, &lied)?;
        require_refused_untouched(
            &path,
            &lied,
            1,
            &format!("middle frame declared {declared}"),
        )?;
    }
    Ok(())
}

/// The control: a genuinely cut append is still repaired. Every byte offset
/// inside the final frame is a place a killed writer could have stopped, and each
/// must reopen with the two acknowledged frames, report the repair, and leave the
/// file at exactly the acknowledged prefix. This is what keeps the refusal above
/// from being bought by refusing everything.
#[test]
fn an_append_cut_at_every_byte_of_the_final_frame_is_repaired() -> TestResult {
    let (path, _guard) = subject("cut-every-byte")?;
    let (bytes, third) = three_frame_file(&path)?;
    for cut in third + 1..bytes.len() {
        std::fs::write(&path, &bytes[..cut])?;
        let reopened = FileJournal::open(&path).map_err(|error| {
            format!(
                "a final frame cut at byte {cut} of {} was refused: {error}",
                bytes.len()
            )
        })?;
        assert!(
            reopened.torn_tail_repaired(),
            "cut at {cut} was not reported as repaired"
        );
        assert_eq!(
            reopened.committed()?.len(),
            2,
            "cut at {cut} lost an acknowledged frame"
        );
        drop(reopened);
        assert_eq!(
            std::fs::metadata(&path)?.len(),
            u64::try_from(third)?,
            "cut at {cut} was not trimmed to the acknowledged prefix"
        );
    }
    Ok(())
}

/// A middle frame whose length is inflated *and* whose own payload is damaged
/// authenticates as nothing itself, but the acknowledged frame behind it still
/// does, so the open refuses rather than trimming both.
#[test]
fn a_damaged_cut_frame_with_an_acknowledged_frame_behind_it_is_refused() -> TestResult {
    let (path, _guard) = subject("damaged-middle")?;
    let (bytes, _) = three_frame_file(&path)?;
    let starts = frame_starts(&bytes, 0)?;
    let middle = starts[1];
    let remaining = u32::try_from(bytes.len() - middle)?;
    let mut lied = with_prefix(&bytes, middle, remaining + 7);
    lied[middle + LENGTH_BYTES + 3] ^= 0x55;
    std::fs::write(&path, &lied)?;
    require_refused_untouched(&path, &lied, 1, "damaged middle frame")
}

/// The stated limit, pinned so the claim is no larger than the test: a final
/// frame whose length was changed and whose head was also damaged authenticates as
/// nothing and has nothing behind it, so it is indistinguishable from an append
/// cut inside its head, and it is trimmed.
#[test]
fn a_final_frame_with_a_lying_length_and_a_damaged_head_is_the_stated_limit() -> TestResult {
    let (path, _guard) = subject("two-faults")?;
    let (bytes, third) = three_frame_file(&path)?;
    let declared = declared_at(&bytes, third);
    let mut lied = with_prefix(&bytes, third, declared + 5);
    let last = lied.len() - 1;
    lied[last] ^= 0xff;
    std::fs::write(&path, &lied)?;
    let reopened = FileJournal::open(&path)?;
    assert!(reopened.torn_tail_repaired());
    assert_eq!(reopened.committed()?.len(), 2);
    Ok(())
}

/// The streaming replay answers a lying length as `open` does. It reads from
/// its own descriptor, so a file changed after `open` is the case it can meet.
#[test]
fn a_streaming_replay_refuses_a_lengthened_final_frame_and_then_ends() -> TestResult {
    let (path, _guard) = subject("replay-lengthened")?;
    let (bytes, third) = three_frame_file(&path)?;
    let journal = FileJournal::open(&path)?;
    let declared = declared_at(&bytes, third);
    for extra in [1u32, 17, 32, 33, 400] {
        std::fs::write(&path, with_prefix(&bytes, third, declared + extra))?;
        let mut replay = journal.replay()?;
        assert!(
            matches!(replay.next(), Some(Ok(_))),
            "L+{extra}: first event"
        );
        assert!(
            matches!(replay.next(), Some(Ok(_))),
            "L+{extra}: second event"
        );
        assert!(
            matches!(replay.next(), Some(Err(JournalError::Corrupt(_)))),
            "L+{extra}: the lengthened frame must be a refusal, not the end of the stream"
        );
        assert!(
            replay.next().is_none(),
            "L+{extra}: a refusal ends the stream"
        );
    }
    std::fs::write(&path, &bytes[..third + 10])?;
    let events = journal.replay()?.collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        events.len(),
        2,
        "a cut append still ends the stream quietly"
    );
    Ok(())
}

mod refusals;

mod readback;

mod batch;
