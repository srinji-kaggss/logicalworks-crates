//! Continue-as-new on disk: how a handle was opened, the ladder hold, writing and syncing the successor, and the seal step.

use super::{
    Continuation, ContinuationPolicy, EventKind, File, HEAD_BYTES, HashMap, LENGTH_BYTES,
    MAX_FRAME_BYTES, OpenOptions, Path, Prefix, SealOutcome, SealPause, Write, fence_and_write,
};

/// Which constructor a handle came in through.
///
/// Three shapes rather than three booleans, because the two properties are not
/// independent: a fault injector and a continuing journal are both things a
/// caller *declares*, and a struct of two flags would let a caller open a stalled
/// journal that also claims to continue without either being what it meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OpenKind {
    /// An ordinary journal, opened with [`FileJournal::open`](crate::journal::FileJournal::open).
    Plain,
    /// An ordinary journal whose device answers only when released.
    Stalled,
    /// A journal that continues at the policy's trigger points.
    Continuing(ContinuationPolicy),
}

impl OpenKind {
    /// Whether the device holds its flush until released.
    pub(super) const fn stalled(self) -> bool {
        matches!(self, Self::Stalled)
    }

    /// Whether this handle continues at the watermark.
    pub(super) const fn continues(self) -> bool {
        matches!(self, Self::Continuing(_))
    }

    /// The trigger points a continuing handle was opened with.
    pub(super) const fn policy(self) -> ContinuationPolicy {
        match self {
            Self::Continuing(policy) => policy,
            Self::Plain | Self::Stalled => ContinuationPolicy::declared(),
        }
    }
}

/// Record one carried key's rung in an append fence's ladder index.
///
/// The carried rung and the rung an event recorded are the same fact about the
/// same key, so they land in the same index: a successor's ladder check is
/// literally the predecessor's, with the checkpoint as where the index was
/// seeded.
pub(super) fn ladder_hold(
    ladder: &mut HashMap<crate::effect::EffectKey, EventKind>,
    key: crate::effect::EffectKey,
    rung: EventKind,
) {
    ladder.insert(key, rung);
}

/// Whether `successor` is a complete, authenticated successor of `seal`'s
/// predecessor.
///
/// The one fact that decides who is authoritative, asked of the bytes rather than
/// of anything a running process remembers: a successor exists, its first frame
/// is whole, and that frame chains from exactly the predecessor's tail the seal
/// names. Anything short of all three — absent, torn, or naming some other
/// position — is a continuation that never committed, and the predecessor stays
/// live.
///
/// Read through its own descriptor with the shared grammar's classification, so a
/// torn first frame reads as *not complete* rather than as a decode failure: this
/// question is "did the continuation commit?", and a half-written file is the
/// answer "no", not an error.
pub(super) fn successor_is_complete(successor: &Path, seal: &Continuation) -> bool {
    let Ok(mut file) = OpenOptions::new().read(true).open(successor) else {
        return false;
    };
    let mut prefix = [0u8; LENGTH_BYTES];
    let Ok(Prefix::Full) = super::super::frame::read_prefix(&mut file, &mut prefix) else {
        return false;
    };
    let declared = super::super::frame::declared_length(&prefix);
    if !super::super::frame::is_possible_length(declared, MAX_FRAME_BYTES) {
        return false;
    }
    let mut payload = vec![0u8; declared];
    let mut head = [0u8; HEAD_BYTES];
    if super::super::frame::read_piece(&mut file, &mut payload).is_err()
        || super::super::frame::read_piece(&mut file, &mut head).is_err()
    {
        return false;
    }
    match Continuation::from_payload(&payload) {
        Some(Ok(carried)) => {
            carried.predecessor() == seal.predecessor()
                && super::super::chain_over_bytes(&seal.predecessor().head(), &payload)
                    == lgwks_std::hash::Digest::from_bytes(head)
        }
        Some(Err(_)) | None => false,
    }
}

/// Make the successor's name itself durable, where the platform has such a call.
///
/// A directory entry that has not reached the disk is a successor that a power
/// loss would not find, and the whole authority rule asks whether that file is
/// there. `#[cfg(unix)]` rather than best-effort: on a platform with no directory
/// sync the call cannot be made at all, and a swallowed error would be a bound
/// nothing observes. What the journal promises is process-crash survival, which
/// does not depend on this call — the name is in the running system's page cache
/// across any process death — so its absence costs power-loss durability the
/// journal never claimed.
#[cfg(unix)]
fn sync_directory(path: &Path, created_here: bool) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    File::open(parent)?.sync_all()?;
    // Only a directory this seal *created* still has an unflushed name of its own.
    // Every later generation writes into a directory that already exists, and a
    // directory fsync is the most expensive call this seal makes, so paying for the
    // grandparent on every continuation would cost a long run far more than the
    // durability it buys.
    if created_here && let Some(grandparent) = parent.parent() {
        File::open(grandparent)?.sync_all()?;
    }
    Ok(())
}

/// [`sync_directory`] where the platform has no directory sync to make.
#[cfg(not(unix))]
fn sync_directory(_path: &Path, _created_here: bool) -> std::io::Result<()> {
    Ok(())
}

/// Write the seal frame into the successor, creating it or completing it.
///
/// The two cases are one operation because one is the other's recovery: a
/// successor that exists here is a continuation that did not commit — this
/// journal's authority rule already decided that, or the open would have refused
/// — so its bytes are an interrupted write and taking them back is the same repair
/// the open performs on a torn tail. Its frame is rewritten byte for byte, so the
/// result is the same file either way.
pub(super) fn write_successor(successor: &Path, frame: &[u8]) -> std::io::Result<bool> {
    // The generation directory is a name this seal creates, so it is made here
    // rather than by an operator: a host that asked for a lifecycle asked for the
    // directories that lifecycle needs. Its own durability is `sync_parent`'s.
    //
    // Whether this call created it is read *before* creating it and reported,
    // because only a name that has just been created still needs its own parent
    // entry flushed, and a directory fsync is the most expensive call this seal
    // makes. Asked after the create, the answer is always "it exists", and a new
    // generation directory's entry would never reach the disk.
    let created = successor.parent().is_some_and(|parent| !parent.exists());
    if let Some(parent) = successor.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let written = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(successor)
    {
        Ok(mut file) => file.write_all(frame),
        Err(ref error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let file = OpenOptions::new().read(true).write(true).open(successor)?;
            file.set_len(0)?;
            let mut handle = file;
            handle.write_all(frame)
        }
        Err(error) => {
            let refusal = Err(error);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "write_successor: the successor could not be created");
            return refusal;
        }
    };
    written.map(|()| created)
}

/// The seal, as the one ordered step the journal's storage owner runs.
///
/// Every byte this journal ever writes goes through this owner thread
/// (INV-BOT-50), and a continuation is not an exception to that: it is the case
/// where the fence, the write and the flush guard **two** files, so running it
/// anywhere else would make the length check and the write it guards overtakable
/// by the very appends they exist to order against.
///
/// The four pauses sit between the four durable steps, in the order the steps
/// happen, so a kill at any of them leaves a different file to reopen and a
/// harness can sweep the whole set from [`SealPause::all`].
pub(super) fn seal_on_owner(
    file: &mut File,
    expected_len: u64,
    frame: &[u8],
    successor: &Path,
    pause: Option<SealPause>,
) -> std::io::Result<super::super::owner::Stage<SealOutcome, ()>> {
    fence_and_write(file, expected_len, frame)?;
    let Some(paused) = paused_at(pause, SealPause::AfterSealWrite) else {
        let created_directory = write_successor(successor, frame)?;
        let Some(paused) = paused_at(pause, SealPause::AfterSuccessorWrite) else {
            OpenOptions::new().read(true).open(successor)?.sync_all()?;
            let Some(paused) = paused_at(pause, SealPause::AfterSuccessorSync) else {
                sync_directory(successor, created_directory)?;
                let Some(paused) = paused_at(pause, SealPause::AfterDirectorySync) else {
                    return Ok(super::super::owner::Stage::Unsynced {
                        answer: SealOutcome::Sealed { bytes: frame.len() },
                        bytes: frame.len(),
                        settle: Box::new(|_: &mut ()| {}),
                    });
                };
                return Ok(super::super::owner::Stage::Settled(Ok(paused)));
            };
            return Ok(super::super::owner::Stage::Settled(Ok(paused)));
        };
        return Ok(super::super::owner::Stage::Settled(Ok(paused)));
    };
    Ok(super::super::owner::Stage::Settled(Ok(paused)))
}

/// The pause answer for `boundary`, when this seal is armed to stop there.
fn paused_at(pause: Option<SealPause>, boundary: SealPause) -> Option<SealOutcome> {
    match pause {
        Some(armed) if armed == boundary => Some(SealOutcome::Paused(boundary)),
        _ => None,
    }
}
