//! The record a run store archives, its chain head and frame, and the replay that rebuilds the index from the file.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use lgwks_std::hash::{Digest, Hasher};
use lgwks_std::wire::{WireError, from_bytes, to_bytes};

use crate::effect::RunId;
use crate::journal::frame::{self, Cursor, HEAD_BYTES, SaturatingFrom};

use super::{
    DefinitionIdentity, Held, Index, MAX_RECORD_BYTES, MAX_RECORDS_PER_RUN, MAX_STORE_BYTES,
    RunIndex, STORE_HEADER, StoreError, StoreLimitKind, check_format_version, genesis_head,
};

/// The record as it is archived and as it is checked.
#[derive(
    Debug, Clone, lgwks_std::wire::Archive, lgwks_std::wire::Serialize, lgwks_std::wire::Deserialize,
)]
#[rkyv(
    attr(non_exhaustive),
    crate = lgwks_std::wire::rkyv,
    compare(PartialEq),
    derive(Debug)
)]
pub(super) struct Stored {
    /// The run this record belongs to.
    #[rkyv(attr(doc = "The run this record belongs to."))]
    pub(super) run: RunId,
    /// The step key's digest.
    #[rkyv(attr(doc = "The step key's digest."))]
    pub(super) key: Vec<u8>,
    /// The tenant that minted the run.
    #[rkyv(attr(doc = "The tenant that minted the run."))]
    pub(super) tenant: String,
    /// The step's path, for attribution.
    #[rkyv(attr(doc = "The step's path, for attribution."))]
    pub(super) path: String,
    /// The definition the record was written under, as its fields.
    ///
    /// Archived as fields rather than as one digest because a drift refusal has
    /// to *name* the two sides: a head over the digest could detect the
    /// disagreement but could not report which axis it was, and the fields are
    /// what a caller reads to decide whether it needs a new run or a migration.
    #[rkyv(attr(doc = "The definition the record was written under, as its fields."))]
    pub(super) definition: StoredDefinition,
    /// The archived value the step returned.
    #[rkyv(attr(doc = "The archived value the step returned."))]
    pub(super) value: Vec<u8>,
}

/// A [`DefinitionIdentity`] in the shape a record archives.
///
/// Named rather than archived directly so the record's on-disk shape is stated
/// once, in the store that writes it, and a future revision of the definition
/// vocabulary is a new struct here rather than a change to a type every consumer
/// of [`Drift`] is already matching.
#[derive(
    Debug, Clone, lgwks_std::wire::Archive, lgwks_std::wire::Serialize, lgwks_std::wire::Deserialize,
)]
#[rkyv(
    attr(non_exhaustive),
    crate = lgwks_std::wire::rkyv,
    compare(PartialEq),
    derive(Debug)
)]
pub(super) struct StoredDefinition {
    /// The task name.
    #[rkyv(attr(doc = "The task name."))]
    pub(super) name: String,
    /// The declared definition revision.
    #[rkyv(attr(doc = "The declared definition revision."))]
    pub(super) revision: u64,
    /// The input digest.
    #[rkyv(attr(doc = "The input digest."))]
    pub(super) input: Vec<u8>,
    /// How many durable steps the definition declares.
    #[rkyv(attr(doc = "How many durable steps the definition declares."))]
    pub(super) steps: u64,
    /// The declared durable-value schema id.
    #[rkyv(attr(doc = "The declared durable-value schema id."))]
    pub(super) codec: String,
}

/// The 32 bytes a record archived, as the digest the identity carries back.
///
/// A fixed width rather than a slice, because a record whose input field is not
/// 32 bytes is a corrupt frame and the checked decoder is what says so; the
/// conversion cannot silently truncate, because `as` is forbidden in this
/// workspace and this is the honest alternative to it.
fn digest_from_record(bytes: &[u8]) -> Digest {
    let mut digest = [0u8; 32];
    let copied = bytes.len().min(digest.len());
    digest[..copied].copy_from_slice(&bytes[..copied]);
    Digest::from_bytes(digest)
}

impl StoredDefinition {
    /// The record's archived form of a live identity.
    pub(super) fn of(identity: &DefinitionIdentity) -> Self {
        Self {
            name: identity.name().to_owned(),
            revision: identity.revision(),
            input: identity.input().as_bytes().to_vec(),
            steps: u64::saturating_from(identity.steps()),
            codec: identity.codec().to_owned(),
        }
    }

    /// The live identity this record was written under.
    pub(super) fn to_identity(&self) -> DefinitionIdentity {
        DefinitionIdentity::new(
            &self.name,
            self.revision,
            digest_from_record(&self.input),
            usize::saturating_from(self.steps),
        )
        .with_codec(&self.codec)
    }
}

impl Stored {
    /// This record's chain head over the head before it.
    ///
    /// Framed exactly as the step key is framed, so a record's head cannot be
    /// confused with the digest of the bytes it holds: the run, the key, the
    /// tenant, the path and every definition field are length-framed, and the
    /// value is framed last so no two different records hash the same byte
    /// string. The definition's own bytes go in through the identity's framing
    /// so a head over the record commits to the identity the same way the
    /// identity's digest commits to it.
    fn head_from(&self, previous: &Digest, _archived: &[u8]) -> Digest {
        let mut hasher = Hasher::new();
        hasher.write_framed(previous.as_bytes());
        hasher.write_framed(&self.run_key_bytes());
        hasher.write_framed(&self.key);
        hasher.write_framed(self.tenant.as_bytes());
        hasher.write_framed(self.path.as_bytes());
        hasher.write_framed(self.definition.name.as_bytes());
        hasher.write_framed(&self.definition.revision.to_le_bytes());
        hasher.write_framed(&self.definition.input);
        hasher.write_framed(&self.definition.steps.to_le_bytes());
        hasher.write_framed(self.definition.codec.as_bytes());
        hasher.write_framed(&self.value);
        hasher.finalize()
    }

    /// The run id's 16 bytes, as a field the framing can hold.
    ///
    /// The hex spelling rather than the big-endian integer, because
    /// `RunId::to_hex` is the one conversion this crate defines for the role
    /// and a second one here would be a second definition to keep in step.
    fn run_key_bytes(&self) -> Vec<u8> {
        self.run.id().to_hex().into_bytes()
    }
}

/// The frame for one record: length, payload, head.
///
/// The head is stored rather than recomputed on faith. That is what separates a
/// torn tail (dropped, because a write leaves a prefix and a prefix has no head)
/// from a frame that does not follow (refused, because those bytes were
/// acknowledged).
pub(super) fn frame(stored: &Stored, previous: &Digest) -> Result<(Vec<u8>, Digest), StoreError> {
    // Archive, bound, chain and lay out — the same step the effect journal takes.
    // Only the record's own archiving and its step-chaining head are this store's.
    frame::frame_record(
        stored,
        previous,
        MAX_RECORD_BYTES,
        |stored| {
            to_bytes::<WireError>(stored)
                .map(|bytes| bytes.as_ref().to_vec())
                .map_err(|cause| StoreError::Encoding { cause })
        },
        Stored::head_from,
        record_too_large,
    )
}

/// The refusal for a store that has no room left, named by what it would have
/// needed.
///
/// One constructor because both call sites discovered the same fact by different
/// routes — an addition that overflowed and an addition that came out past the
/// ceiling — and a literal per site would be two chances to name a different bound
/// for the same ceiling.
pub(super) fn store_full(requested: u64) -> StoreError {
    StoreError::Limit {
        kind: StoreLimitKind::StoreBytes,
        requested,
        limit: MAX_STORE_BYTES,
    }
}

/// The refusal for a record whose archived bytes are past this store's ceiling.
///
/// One constructor, because the message and the two numbers are the same fact
/// whichever call site discovers it, and a literal per site would be two chances to
/// name a different bound for the same ceiling.
fn record_too_large(len: usize) -> StoreError {
    StoreError::Limit {
        kind: StoreLimitKind::RecordBytes,
        requested: u64::saturating_from(len),
        limit: u64::saturating_from(MAX_RECORD_BYTES),
    }
}

/// Read `buf` in full, reporting whether all of it arrived.
fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> Result<bool, StoreError> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => return Ok(false),
            Ok(read) => filled = filled.saturating_add(read),
            Err(ref error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(cause) => {
                let refusal = Err(StoreError::storage(cause));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "read_full: returning an error to the caller");
                return refusal;
            }
        }
    }
    Ok(true)
}

/// One complete, decoded frame read from the store.
struct Framed {
    /// The record the frame's payload decoded to.
    stored: Stored,
    /// The payload bytes, which the chain head is computed over.
    payload: Vec<u8>,
    /// The chain head the frame recorded for itself.
    head: [u8; HEAD_BYTES],
    /// The payload length the frame declared.
    declared: usize,
}

/// Read the frame at ordinal `at`, or `None` where the file stops holding a
/// whole frame.
///
/// A partial length prefix, or a payload or head cut short, is an append that
/// never finished: it was never anyone's answer, so the scan stops and the
/// caller trims to `committed`. A complete prefix naming a frame this store
/// never writes (a write leaves a prefix of a length it did finish computing,
/// and that length was always legal) or a payload that does not decode cannot
/// be an interrupted append and is refused. So is a cut whose bytes hold a frame
/// this store acknowledged under another length: `cursor` is where the frame
/// starts and the head it chains from, which is what the stored head is checked
/// against (#262).
fn next_frame(file: &mut File, cursor: &Cursor<'_>) -> Result<Option<Framed>, StoreError> {
    let at = cursor.at;
    let corrupt = || StoreError::Corrupt { at };
    let Some(raw) = frame::read_raw(
        file,
        cursor,
        MAX_RECORD_BYTES,
        StoreError::storage,
        corrupt,
        |previous, payload| {
            let aligned = frame::decodable(payload);
            from_bytes::<Stored, WireError>(aligned.as_slice())
                .ok()
                .map(|stored| stored.head_from(previous, payload))
        },
    )?
    else {
        return Ok(None);
    };
    let stored = from_bytes::<Stored, WireError>(&raw.payload).map_err(|error| {
        lgwks_std::trace::debug!(?error, at, "next_frame: the payload did not decode");
        StoreError::Corrupt { at }
    })?;
    Ok(Some(Framed {
        stored,
        declared: raw.payload.len(),
        payload: raw.payload,
        head: raw.head,
    }))
}

/// Read every committed frame, returning the index it implies.
///
/// The scan is bounded by the file's own length: every iteration consumes at
/// least one byte, and a file that grows underneath the scan cannot extend it,
/// because the reader stops at the length it saw when it began. A frame whose
/// stored head does not follow is [`StoreError::Corrupt`] and is not truncated.
pub(super) fn replay(file: &mut File) -> Result<Index, StoreError> {
    let total = file.metadata().map_err(StoreError::storage)?.len();
    file.seek(SeekFrom::Start(0)).map_err(StoreError::storage)?;

    let mut header = [0u8; STORE_HEADER.len()];
    if !read_full(file, &mut header)? {
        let refusal = Err(StoreError::NotAStore);
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "replay: returning an error to the caller");
        return refusal;
    }
    // The two refusals are separated, and the version one is checked first,
    // because they are different facts and only one of them is a version the
    // writer could have meant. Every version of this format shares the whole
    // magic *except* its last byte, which is that format's version — so a file
    // whose first fifteen bytes match is this store's, at some version, and a
    // file whose first fifteen do not match was never a run store at all. That
    // is the whole reason `STORE_MAGIC` is 15 bytes of constant plus a version
    // byte rather than one 16-byte constant.
    check_format_version(header)?;
    let mut index = Index {
        runs: HashMap::new(),
        committed: u64::saturating_from(STORE_HEADER.len()),
        tail: genesis_head(),
    };
    let mut previous = genesis_head();
    let mut at = 0u64;
    while let Some(Framed {
        stored,
        payload,
        head,
        declared,
    }) = next_frame(file, &Cursor::new(at, index.committed, &previous))?
    {
        if stored.head_from(&previous, &payload) != Digest::from_bytes(head) {
            let refusal = Err(StoreError::Corrupt { at });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "replay: returning an error to the caller");
            return refusal;
        }
        let frame_len = frame::framed_len(declared);
        // A frame that runs past the end of the file is an interrupted append.
        // Readable as a whole above, so this only catches a header that lied
        // about a frame already acknowledged, which is rot rather than a crash.
        if index
            .committed
            .checked_add(frame_len)
            .is_none_or(|end| end > total)
        {
            break;
        }
        index.committed = index.committed.saturating_add(frame_len);
        index.tail = stored.head_from(&previous, &payload);
        previous = stored.head_from(&previous, &payload);
        at = at.saturating_add(1);
        if at > MAX_RECORDS_PER_RUN {
            let refusal = Err(StoreError::Limit {
                kind: StoreLimitKind::Records,
                requested: at,
                limit: MAX_RECORDS_PER_RUN,
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "replay: returning an error to the caller");
            return refusal;
        }
        let run = index.runs.entry(stored.run).or_insert_with(|| RunIndex {
            tenant: stored.tenant.clone(),
            definition: stored.definition.to_identity(),
            steps: HashMap::new(),
        });
        run.steps.insert(
            hex_of(&stored.key),
            Held {
                path: stored.path.clone(),
                bytes: stored.value.clone(),
            },
        );
    }
    Ok(index)
}

/// Lowercase hex, for a digest used as a map key.
///
/// `lgwks_std::hash::Digest::to_hex` takes a `Digest`, and this is the same 32
/// bytes before the head has been computed; formatting them here rather than
/// reaching into the digest type keeps the key exactly the bytes on the disk.
pub(super) fn hex_of(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = Vec::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes.iter().copied() {
        hex.push(DIGITS[usize::from(byte >> 4) & 0x0f]);
        hex.push(DIGITS[usize::from(byte) & 0x0f]);
    }
    // Every byte pushed above is an ASCII digit, so this is well-formed UTF-8
    // and the conversion cannot fail; `String::from_utf8` reports the invariant
    // rather than trusting it.
    hex.into_iter().map(char::from).collect()
}
