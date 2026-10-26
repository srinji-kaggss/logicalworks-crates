//! The snapshot beside the store: the folded index and the length it accounts for.
//!
//! An open loads this file and drains only what was appended after the length
//! it names, so its cost is bounded by live history rather than by every run
//! the store ever held. The store file stays the truth and the snapshot only
//! the shortcut: a missing or damaged snapshot falls back to the full replay
//! it replaces, because every frame the snapshot folds is still on the disk
//! behind it.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;

use lgwks_std::json::{Deserialize, Serialize};

use super::{
    HEADER_LEN, Index, RunEntry, SNAPSHOT_FILE, SNAPSHOT_FORMAT, SNAPSHOT_TMP, StoredPolicy,
    TenantStoreError,
};
use crate::journal::frame::SaturatingFrom;

/// The snapshot as it is held: the folded maps, the file length they account
/// for, and the chain head the next frame follows.
#[derive(Debug, Clone)]
pub(super) struct Snapshot {
    /// Per tenant: the immutable policy.
    pub(super) policies: BTreeMap<String, StoredPolicy>,
    /// Per run id: the run and its lanes.
    pub(super) runs: BTreeMap<String, RunEntry>,
    /// How many frames the maps fold.
    pub(super) frames: u64,
    /// The file length every folded frame accounts for.
    pub(super) committed: u64,
    /// The chain head the next frame follows, as raw bytes.
    pub(super) tail: [u8; 32],
}

/// The snapshot file's shape as JSON: the format it was written in beside the
/// values it carries, so a file from a build that means something else by
/// them is refused rather than misread.
#[derive(Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct SnapshotFile {
    /// The snapshot format this file was written in.
    format: u8,
    /// The file length the maps account for.
    committed: u64,
    /// How many frames the maps fold.
    frames: u64,
    /// The chain head the next frame follows, as raw bytes.
    tail: Vec<u8>,
    /// Per tenant: the immutable policy.
    policies: BTreeMap<String, StoredPolicy>,
    /// Per run id: the run and its lanes.
    runs: BTreeMap<String, RunEntry>,
}

/// Read the snapshot beside the store in `dir`, or `None` where there is no
/// usable one.
///
/// A missing file is no snapshot yet — the first opens of a store, and every
/// open of one written before snapshots existed. A damaged file is the same
/// answer, not a refusal: the frames it would have folded are still on the
/// disk behind it, so falling back to the full replay loses time and nothing
/// else. What is refused is only what the device itself refuses, because a
/// device that cannot be read cannot be replayed either.
pub(super) fn read(dir: &Path) -> Result<Option<Snapshot>, TenantStoreError> {
    let path = dir.join(SNAPSHOT_FILE);
    let mut file = match OpenOptions::new().read(true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(cause) => {
            lgwks_std::trace::warn!(
                path = %path.display(),
                %cause,
                "tenant_store: snapshot unreadable; falling back to the full replay"
            );
            return Err(TenantStoreError::storage(cause));
        }
    };
    let mut bytes = Vec::new();
    if let Err(cause) = file.read_to_end(&mut bytes) {
        lgwks_std::trace::warn!(
            path = %path.display(),
            %cause,
            "tenant_store: snapshot unreadable; falling back to the full replay"
        );
        return Err(TenantStoreError::storage(cause));
    }
    let stored: SnapshotFile = match crate::json::from_slice(&bytes) {
        Ok(stored) => stored,
        Err(error) => {
            lgwks_std::trace::warn!(
                path = %path.display(),
                %error,
                "tenant_store: snapshot damaged; falling back to the full replay"
            );
            return Ok(None);
        }
    };
    if stored.format != SNAPSHOT_FORMAT {
        lgwks_std::trace::warn!(
            path = %path.display(),
            found = stored.format,
            "tenant_store: foreign snapshot format; falling back to the full replay"
        );
        return Ok(None);
    }
    let tail: [u8; 32] = match stored.tail.as_slice().try_into() {
        Ok(tail) => tail,
        Err(_) => {
            lgwks_std::trace::warn!(
                path = %path.display(),
                "tenant_store: snapshot head is not 32 bytes; falling back to the full replay"
            );
            return Ok(None);
        }
    };
    if stored.committed < u64::saturating_from(HEADER_LEN) {
        lgwks_std::trace::warn!(
            path = %path.display(),
            committed = stored.committed,
            "tenant_store: snapshot shorter than a header; falling back to the full replay"
        );
        return Ok(None);
    }
    Ok(Some(Snapshot {
        policies: stored.policies,
        runs: stored.runs,
        frames: stored.frames,
        committed: stored.committed,
        tail,
    }))
}

/// Write the snapshot the `index` implies, beside the store in `dir`.
///
/// Written atomically — to a temporary file, synced, then renamed — so a
/// crash leaves the previous snapshot or none, never a torn one. A torn one
/// would only cost the next open its shortcut, but a shortcut that cannot be
/// trusted is not one, and the rename is what keeps it trustworthy. The write
/// is synced before the rename is, so the renamed file names bytes the device
/// already holds.
///
/// Serializing these maps is infallible in practice — string keys, no floats,
/// no borrowed halves — so the one error arm below names the codec through
/// the device refusal rather than inventing a variant for it, and the store
/// file it snapshots stays the truth either way.
pub(super) fn write(dir: &Path, index: &Index) -> Result<(), TenantStoreError> {
    let stored = SnapshotFile {
        format: SNAPSHOT_FORMAT,
        committed: index.committed,
        frames: index.frames,
        tail: index.tail.as_bytes().to_vec(),
        policies: index.policies.clone(),
        runs: index.runs.clone(),
    };
    let bytes = crate::json::to_vec(&stored).map_err(|cause| {
        lgwks_std::trace::warn!(
            %cause,
            "tenant_store: snapshot unarchivable; the store file stays the truth"
        );
        TenantStoreError::storage(std::io::Error::other(cause))
    })?;
    let path = dir.join(SNAPSHOT_FILE);
    let tmp = dir.join(SNAPSHOT_TMP);
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .map_err(TenantStoreError::storage)?;
    file.write_all(&bytes).map_err(TenantStoreError::storage)?;
    file.sync_all().map_err(TenantStoreError::storage)?;
    drop(file);
    std::fs::rename(&tmp, &path).map_err(TenantStoreError::storage)?;
    let dir_file = OpenOptions::new()
        .read(true)
        .open(dir)
        .map_err(TenantStoreError::storage)?;
    dir_file.sync_all().map_err(TenantStoreError::storage)?;
    Ok(())
}
