//! The store's own record: its archived form, the chain, the frame codec over the shared grammar, and replay into the index.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use lgwks_std::hash::{Digest, Hasher};
use lgwks_std::wire::{
    AlignedVec, Archive, Deserialize, Serialize, WireError, from_bytes, to_bytes,
};

use super::{
    DETAIL_NEVER_STARTED, DETAIL_UNKNOWN, HEADER_LEN, Index, KIND_ABANDON_RUN, KIND_BEGIN_RUN,
    KIND_FINISH_RUN, KIND_INIT_POLICY, KIND_LANE_DISPATCHED, KIND_LANE_ENDED, LANE_DISPATCHED,
    LANE_ENDED, LANE_PENDING, LaneEntry, MAX_FRAME_BYTES, OUTCOME_UNKNOWN, OUTCOME_UNMEASURED,
    RunEntry, RunStart, STORE_FORMAT, STORE_MAGIC, StoredClose, StoredPolicy, TenantStoreError,
    VERSION_BYTE,
};
use crate::journal::frame::{self, HEAD_BYTES, LENGTH_BYTES, SaturatingFrom};

/// The chain digest a store starts from: 32 zero bytes hashed through the
/// same framing as any record, so the first head is a real chain step and not
/// a constant every file shares.
pub(super) fn genesis_head() -> Digest {
    let mut hasher = Hasher::new();
    hasher.write_framed(b"lgwks-tenant-store/genesis");
    hasher.finalize()
}

/// The record as it is archived and as its chain head is checked.
#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
#[rkyv(
    attr(non_exhaustive),
    crate = lgwks_std::wire::rkyv,
    compare(PartialEq),
    derive(Debug)
)]
pub(super) struct Stored {
    /// Which of the six record kinds this is.
    #[rkyv(attr(doc = "Which of the six record kinds this is."))]
    kind: u8,
    /// The tenant the record belongs to.
    #[rkyv(attr(doc = "The tenant the record belongs to."))]
    tenant: String,
    /// The run the record belongs to.
    #[rkyv(attr(doc = "The run the record belongs to."))]
    run_id: String,
    /// The repository under test (begin only).
    #[rkyv(attr(doc = "The repository under test (begin only)."))]
    repo: String,
    /// The coordinator's host (begin only).
    #[rkyv(attr(doc = "The coordinator's host (begin only)."))]
    host: String,
    /// The coordinator's process id (begin only).
    #[rkyv(attr(doc = "The coordinator's process id (begin only)."))]
    coordinator_pid: u32,
    /// The coordinator's lock (begin only).
    #[rkyv(attr(doc = "The coordinator's lock (begin only)."))]
    coordinator_lock: String,
    /// The moment the record names: started, finished, or recorded.
    #[rkyv(attr(doc = "The moment the record names: started, finished, or recorded."))]
    moment: String,
    /// The lane ids in plan order (begin only).
    #[rkyv(attr(doc = "The lane ids in plan order (begin only)."))]
    lanes: Vec<String>,
    /// The lane position the record names (lane ops only).
    #[rkyv(attr(doc = "The lane position the record names (lane ops only)."))]
    lane_idx: u64,
    /// The verdict (finish only).
    #[rkyv(attr(doc = "The verdict (finish only)."))]
    verdict: String,
    /// The sealed record (finish only).
    #[rkyv(attr(doc = "The sealed record (finish only)."))]
    record: String,
    /// The seal (finish only).
    #[rkyv(attr(doc = "The seal (finish only)."))]
    seal: String,
    /// The outcome (lane end only).
    #[rkyv(attr(doc = "The outcome (lane end only)."))]
    outcome: String,
    /// The detail (lane end only).
    #[rkyv(attr(doc = "The detail (lane end only)."))]
    detail: String,
    /// The captured tail (lane end only).
    #[rkyv(attr(doc = "The captured tail (lane end only)."))]
    tail: Vec<u8>,
    /// The gate set, exactly as recorded (policy only).
    #[rkyv(attr(doc = "The gate set, exactly as recorded (policy only)."))]
    gates_json: String,
    /// The hardness level (policy only).
    #[rkyv(attr(doc = "The hardness level (policy only)."))]
    hardness: u8,
}

impl Stored {
    /// A policy record.
    pub(super) fn init_policy(tenant: &str, gates_json: &str, hardness: u8, now: &str) -> Self {
        Self::empty(KIND_INIT_POLICY, tenant, now, gates_json, hardness)
    }

    /// The shared skeleton: every field a kind does not use stays empty, so
    /// the head commits to the absence as well as the presence.
    pub(super) fn empty(
        kind: u8,
        tenant: &str,
        moment: &str,
        gates_json: &str,
        hardness: u8,
    ) -> Self {
        Self {
            kind,
            tenant: tenant.to_owned(),
            run_id: String::new(),
            repo: String::new(),
            host: String::new(),
            coordinator_pid: 0,
            coordinator_lock: String::new(),
            moment: moment.to_owned(),
            lanes: Vec::new(),
            lane_idx: 0,
            verdict: String::new(),
            record: String::new(),
            seal: String::new(),
            outcome: String::new(),
            detail: String::new(),
            tail: Vec::new(),
            gates_json: gates_json.to_owned(),
            hardness,
        }
    }

    /// A run-begin record: the run row and every pending lane row in one frame.
    pub(super) fn begin_run(run: &RunStart<'_>, lanes: &[&str]) -> Self {
        let mut stored = Self::empty(KIND_BEGIN_RUN, run.tenant, run.started_at, "", 0);
        run.run_id.clone_into(&mut stored.run_id);
        run.repo.clone_into(&mut stored.repo);
        run.host.clone_into(&mut stored.host);
        stored.coordinator_pid = run.coordinator_pid;
        run.coordinator_lock
            .clone_into(&mut stored.coordinator_lock);
        stored.lanes = lanes.iter().map(|id| (*id).to_owned()).collect();
        stored
    }

    /// A lane dispatch or any lane op that names only the run and the lane.
    pub(super) fn lane_op(kind: u8, run_id: &str, lane: usize) -> Self {
        let mut stored = Self::empty(kind, "", "", "", 0);
        run_id.clone_into(&mut stored.run_id);
        stored.lane_idx = u64::saturating_from(lane);
        stored
    }

    /// A lane-end record with its outcome, detail and tail.
    pub(super) fn lane_ended(
        run_id: &str,
        lane: usize,
        outcome: &str,
        detail: &str,
        tail: &[u8],
    ) -> Self {
        let mut stored = Self::lane_op(KIND_LANE_ENDED, run_id, lane);
        outcome.clone_into(&mut stored.outcome);
        detail.clone_into(&mut stored.detail);
        stored.tail = tail.to_vec();
        stored
    }

    /// A run-finish record with its verdict, sealed record and seal.
    pub(super) fn finish_run(
        run_id: &str,
        verdict: &str,
        finished_at: &str,
        record: &str,
        seal: &str,
    ) -> Self {
        let mut stored = Self::lane_op(KIND_FINISH_RUN, run_id, 0);
        finished_at.clone_into(&mut stored.moment);
        verdict.clone_into(&mut stored.verdict);
        record.clone_into(&mut stored.record);
        seal.clone_into(&mut stored.seal);
        stored
    }

    /// An abandon record: the run and the moment its coordinator was found gone.
    ///
    /// The lane closes are not separate records. They are implied by this one
    /// frame at fold time, which is what makes the close transactional: one
    /// frame, one sync, the run and every open lane closed together.
    pub(super) fn abandon_run(run_id: &str, now: &str) -> Self {
        let mut stored = Self::lane_op(KIND_ABANDON_RUN, run_id, 0);
        now.clone_into(&mut stored.moment);
        stored
    }

    /// This record's chain head over the head before it and the archived bytes.
    ///
    /// Every field length-framed, so no two different records hash the same
    /// byte string and an empty field commits to its own absence. The archived
    /// bytes arrive because the framing step hands them over rather than
    /// re-encoding inside; this head chains over the fields, and the bytes are
    /// here so the one framing step serves heads that need either.
    pub(super) fn head_from(record: &Stored, previous: &Digest, _archived: &[u8]) -> Digest {
        let mut hasher = Hasher::new();
        hasher.write_framed(previous.as_bytes());
        hasher.write_framed(&[record.kind]);
        hasher.write_framed(record.tenant.as_bytes());
        hasher.write_framed(record.run_id.as_bytes());
        hasher.write_framed(record.repo.as_bytes());
        hasher.write_framed(record.host.as_bytes());
        hasher.write_framed(&record.coordinator_pid.to_le_bytes());
        hasher.write_framed(record.coordinator_lock.as_bytes());
        hasher.write_framed(record.moment.as_bytes());
        for id in &record.lanes {
            hasher.write_framed(id.as_bytes());
        }
        hasher.write_framed(&record.lane_idx.to_le_bytes());
        hasher.write_framed(record.verdict.as_bytes());
        hasher.write_framed(record.record.as_bytes());
        hasher.write_framed(record.seal.as_bytes());
        hasher.write_framed(record.outcome.as_bytes());
        hasher.write_framed(record.detail.as_bytes());
        hasher.write_framed(&record.tail);
        hasher.write_framed(record.gates_json.as_bytes());
        hasher.write_framed(&[record.hardness]);
        hasher.finalize()
    }
}

/// One admitted lane: the id from the plan, still pending.
///
/// One constructor because the begin fold builds one per plan lane, and a
/// literal per lane would restate the pending state for every lane of every
/// run.
fn pending_lane(id: &str) -> LaneEntry {
    LaneEntry {
        id: id.to_owned(),
        state: LANE_PENDING.to_owned(),
        outcome: None,
        detail: None,
        tail: None,
    }
}

/// Close one lane with an outcome and its detail.
///
/// The one closer every end reaches — a lane's own end, a dispatched lane's
/// abandonment, a pending lane's abandonment — so "ended" is stated once and
/// no closer can end a lane without naming why. The tail is the ender's own:
/// only a lane that ran leaves bytes behind.
fn end_lane(slot: &mut LaneEntry, outcome: &str, detail: &str) {
    LANE_ENDED.clone_into(&mut slot.state);
    slot.outcome = Some(outcome.to_owned());
    slot.detail = Some(detail.to_owned());
}

/// Close one run with an already-built close.
///
/// One assignment because the finish and abandon folds close different facts —
/// a verdict the coordinator earned, a verdict its death imposed — and the
/// assignment is what they share rather than what distinguishes them.
fn close_run(entry: &mut RunEntry, close: StoredClose) {
    entry.close = Some(close);
}

/// Fold one verified record into the index.
///
/// Inserts dispatch once: the two records that create rows (policy, begin)
/// return early, and the four that mutate a run share the one lookup below.
/// A mutation for a run the file never began is impossible here — the writer
/// refused it before framing — so a missing row returns without folding rather
/// than inventing one. An unknown kind is rot, refused at read time rather
/// than folded, because skipping would hand the caller a history with a hole
/// in it. See [`next_frame`].
pub(super) fn fold(index: &mut Index, stored: &Stored) {
    if stored.kind == KIND_INIT_POLICY {
        index.policies.insert(
            stored.tenant.clone(),
            StoredPolicy {
                gates_json: stored.gates_json.clone(),
                hardness: stored.hardness,
                created_at: stored.moment.clone(),
            },
        );
        return;
    }
    if stored.kind == KIND_BEGIN_RUN {
        index.runs.insert(
            stored.run_id.clone(),
            RunEntry {
                tenant: stored.tenant.clone(),
                repo: stored.repo.clone(),
                host: stored.host.clone(),
                coordinator_pid: stored.coordinator_pid,
                coordinator_lock: stored.coordinator_lock.clone(),
                started_at: stored.moment.clone(),
                close: None,
                lanes: stored.lanes.iter().map(|id| pending_lane(id)).collect(),
            },
        );
        return;
    }
    let Some(entry) = index.runs.get_mut(&stored.run_id) else {
        return;
    };
    if stored.kind == KIND_LANE_DISPATCHED {
        if let Some(slot) = lane_slot(entry, &stored.lane_idx) {
            LANE_DISPATCHED.clone_into(&mut slot.state);
        }
    } else if stored.kind == KIND_LANE_ENDED {
        if let Some(slot) = lane_slot(entry, &stored.lane_idx) {
            end_lane(slot, &stored.outcome, &stored.detail);
            slot.tail = Some(stored.tail.clone());
        }
    } else if stored.kind == KIND_FINISH_RUN {
        close_run(
            entry,
            StoredClose::finished(
                &stored.moment,
                &stored.verdict,
                &stored.record,
                &stored.seal,
            ),
        );
    } else if stored.kind == KIND_ABANDON_RUN {
        close_run(entry, StoredClose::abandoned(&stored.moment));
        for slot in &mut entry.lanes {
            if slot.state == LANE_DISPATCHED {
                end_lane(slot, OUTCOME_UNKNOWN, DETAIL_UNKNOWN);
            } else if slot.state == LANE_PENDING {
                end_lane(slot, OUTCOME_UNMEASURED, DETAIL_NEVER_STARTED);
            }
        }
    }
}

/// The lane slot a lane op names, or `None` when the position is past the plan.
///
/// `None` here is decided by the caller as `LaneLost`: the fold itself never
/// refuses, because replay folds what the writer already decided.
fn lane_slot<'a>(entry: &'a mut RunEntry, idx: &u64) -> Option<&'a mut LaneEntry> {
    entry.lanes.get_mut(usize::saturating_from(*idx))
}

/// The frame for one record: archive it, refuse it past the ceiling, chain its
/// head, and lay it out through the estate's one frame grammar.
pub(super) fn frame_record(
    stored: &Stored,
    previous: &Digest,
) -> Result<(Vec<u8>, Digest), TenantStoreError> {
    frame::frame_record(
        stored,
        previous,
        MAX_FRAME_BYTES,
        |record| {
            to_bytes::<WireError>(record)
                .map(|bytes| bytes.as_ref().to_vec())
                .map_err(|cause| TenantStoreError::Encoding { cause })
        },
        Stored::head_from,
        |len| TenantStoreError::Limit {
            requested: u64::saturating_from(len),
            limit: u64::saturating_from(MAX_FRAME_BYTES),
        },
    )
}

/// Decode one frame payload, copying only when the archive demands it.
///
/// An archive read from a misaligned slice is refused rather than decoded,
/// which would make a frame the writer really framed look like noise — so a
/// payload that does not sit at an archive-aligned offset is copied into one
/// that does. This is the alignment half of the estate's frame reading, stated
/// here because the shared helper travels behind the `script` gate and this
/// store builds without it.
fn decode_stored(payload: &[u8], at: u64) -> Result<Stored, TenantStoreError> {
    const ARCHIVE_ALIGN: usize = 16;
    let corrupt = || TenantStoreError::Corrupt { at };
    if payload.as_ptr().align_offset(ARCHIVE_ALIGN) == 0 {
        from_bytes::<Stored, WireError>(payload).map_err(|error| {
            lgwks_std::trace::warn!(
                at,
                %error,
                "tenant_store: stored payload failed to decode; refusing frame as corrupt"
            );
            corrupt()
        })
    } else {
        let mut copy: AlignedVec = AlignedVec::with_capacity(payload.len());
        copy.extend_from_slice(payload);
        from_bytes::<Stored, WireError>(&copy).map_err(|error| {
            lgwks_std::trace::warn!(
                at,
                %error,
                "tenant_store: realigned payload failed to decode; refusing frame as corrupt"
            );
            corrupt()
        })
    }
}

/// Read `buf` in full: `Ok(true)` when every byte arrived, `Ok(false)` when
/// the file ended first. A device refusal is an error, never evidence the
/// bytes were never written.
fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> Result<bool, TenantStoreError> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => return Ok(false),
            Ok(read) => filled = filled.saturating_add(read),
            Err(ref error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(cause) => {
                lgwks_std::trace::warn!(
                    filled,
                    wanted = buf.len(),
                    %cause,
                    "tenant_store: frame read refused by the device mid-frame"
                );
                return Err(TenantStoreError::storage(cause));
            }
        }
    }
    Ok(true)
}

/// One verified frame: the decoded record and the head it committed to.
struct Framed {
    /// The record the frame's payload decoded to.
    stored: Stored,
    /// The chain head the frame stored for itself.
    head: Digest,
}

/// Whether `kind` is one of the six records this format produces.
///
/// A seventh kind is bytes no writer of this format produced: rot or a hand,
/// refused rather than folded, because the fold is total over the six and
/// skipping would hand the caller a history with a hole in it.
fn is_known_kind(kind: u8) -> bool {
    kind == KIND_INIT_POLICY
        || kind == KIND_BEGIN_RUN
        || kind == KIND_LANE_DISPATCHED
        || kind == KIND_LANE_ENDED
        || kind == KIND_FINISH_RUN
        || kind == KIND_ABANDON_RUN
}

/// Read the frame at ordinal `at` from the file's current position, or `None`
/// where the file stops holding a whole frame.
///
/// A partial prefix, payload or head is an append that never finished: the
/// scan stops and the caller trims to `committed`. A complete prefix naming a
/// frame this store never writes, a payload that does not decode, an unknown
/// kind, or a head that does not follow is rot in acknowledged bytes and is
/// refused.
fn next_frame(
    file: &mut File,
    at: u64,
    previous: &Digest,
) -> Result<Option<Framed>, TenantStoreError> {
    let corrupt = || TenantStoreError::Corrupt { at };
    let mut prefix = [0u8; LENGTH_BYTES];
    match frame::read_prefix(file, &mut prefix).map_err(TenantStoreError::storage)? {
        frame::Prefix::Eof => return Ok(None),
        frame::Prefix::Torn => return Ok(None),
        frame::Prefix::Full => {}
    }
    let declared = frame::declared_length(&prefix);
    if !frame::is_possible_length(declared, MAX_FRAME_BYTES) {
        lgwks_std::trace::warn!(
            at,
            declared,
            limit = MAX_FRAME_BYTES,
            "tenant_store: frame refused: the declared length is one this store never writes"
        );
        return Err(corrupt());
    }
    let mut payload = vec![0u8; declared];
    if !read_full(file, &mut payload)? {
        return Ok(None);
    }
    let mut head = [0u8; HEAD_BYTES];
    if !read_full(file, &mut head)? {
        return Ok(None);
    }
    let stored = decode_stored(&payload, at)?;
    if !is_known_kind(stored.kind) {
        lgwks_std::trace::warn!(
            at,
            kind = stored.kind,
            "tenant_store: frame refused: unknown record kind in acknowledged bytes"
        );
        return Err(corrupt());
    }
    if Stored::head_from(&stored, previous, &payload) != Digest::from_bytes(head) {
        lgwks_std::trace::warn!(
            at,
            "tenant_store: frame refused: the chain head does not follow the previous one"
        );
        return Err(corrupt());
    }
    Ok(Some(Framed {
        stored,
        head: Digest::from_bytes(head),
    }))
}

/// Refuse a header this build does not read: a magic mismatch was never a
/// tenant store, while a matching magic with a foreign version byte is one at
/// some version. Reporting the second as the first would tell an operator
/// their data was never theirs, which is what makes someone delete the file.
fn check_header(header: &[u8; HEADER_LEN]) -> Result<(), TenantStoreError> {
    if header[..VERSION_BYTE] != STORE_MAGIC[..VERSION_BYTE] {
        lgwks_std::trace::warn!("tenant_store: open refused: the file magic is not a tenant store");
        return Err(TenantStoreError::NotAStore);
    }
    if header[VERSION_BYTE] != STORE_FORMAT {
        lgwks_std::trace::warn!(
            found = header[VERSION_BYTE],
            expected = STORE_FORMAT,
            "tenant_store: open refused: foreign store format version"
        );
        return Err(TenantStoreError::FormatVersion {
            found: header[VERSION_BYTE],
            expected: STORE_FORMAT,
        });
    }
    Ok(())
}

/// Fold every whole frame from the file's current position until the length
/// seen when the drain began or a torn tail, and return the head the drain
/// ends at.
///
/// One definition of the fold loop for the two callers — the full replay from
/// the start and the tail refresh past `committed` — so the two cannot disagree
/// about what folding a frame means. Bounded by `total`: every iteration
/// consumes at least one byte, and a file that grows underneath cannot extend
/// the drain.
fn drain(
    file: &mut File,
    index: &mut Index,
    total: u64,
    mut previous: Digest,
) -> Result<Digest, TenantStoreError> {
    while index.committed < total {
        let at = index.frames;
        let Some(framed) = next_frame(file, at, &previous)? else {
            break;
        };
        let advance = frame_len_of(&framed.stored, at)?;
        fold(index, &framed.stored);
        index.committed = index.committed.saturating_add(advance);
        index.tail = framed.head;
        previous = framed.head;
        index.frames = index.frames.saturating_add(1);
    }
    Ok(previous)
}

/// Read every committed frame and fold the index it implies, from the start.
pub(super) fn full_replay(file: &mut File, index: &mut Index) -> Result<(), TenantStoreError> {
    let total = file.metadata().map_err(TenantStoreError::storage)?.len();
    file.seek(SeekFrom::Start(0))
        .map_err(TenantStoreError::storage)?;
    let mut header = [0u8; HEADER_LEN];
    if !read_full(file, &mut header)? {
        lgwks_std::trace::warn!("tenant_store: replay refused: the file holds no whole header");
        return Err(TenantStoreError::NotAStore);
    }
    check_header(&header)?;
    index.policies.clear();
    index.runs.clear();
    index.frames = 0;
    index.committed = u64::saturating_from(HEADER_LEN);
    index.tail = genesis_head();
    drain(file, index, total, genesis_head())?;
    Ok(())
}

/// The bytes one folded frame accounts for: prefix, payload and head.
///
/// Re-archived from the decoded record through the one grammar, so the reader
/// and the accountant cannot disagree about what a frame is.
fn frame_len_of(stored: &Stored, at: u64) -> Result<u64, TenantStoreError> {
    let payload = to_bytes::<WireError>(stored)
        .map(|bytes| bytes.as_ref().to_vec())
        .map_err(|error| {
            lgwks_std::trace::warn!(
                at,
                %error,
                "tenant_store: frame length unaccountable: the record no longer archives"
            );
            TenantStoreError::Corrupt { at }
        })?;
    Ok(frame::framed_len(payload.len()))
}

/// Fold frames appended since `committed`; trim a torn tail.
///
/// When the file shrank under the index — another handle trimmed a torn tail
/// this handle never saw — the offsets no longer line up and the only honest
/// answer is a full replay from the start.
pub(super) fn refresh(file: &mut File, index: &mut Index) -> Result<(), TenantStoreError> {
    let total = file.metadata().map_err(TenantStoreError::storage)?.len();
    if total < index.committed {
        return full_replay(file, index);
    }
    if total == index.committed {
        return Ok(());
    }
    file.seek(SeekFrom::Start(index.committed))
        .map_err(TenantStoreError::storage)?;
    drain(file, index, total, index.tail)?;
    trim_to_committed(file, index)
}

/// Truncate a torn tail past the committed length, so the next append does not
/// write past an interrupted frame and fork the file.
pub(super) fn trim_to_committed(file: &mut File, index: &Index) -> Result<(), TenantStoreError> {
    if file.metadata().map_err(TenantStoreError::storage)?.len() != index.committed {
        file.set_len(index.committed)
            .map_err(TenantStoreError::storage)?;
        file.sync_all().map_err(TenantStoreError::storage)?;
    }
    Ok(())
}
