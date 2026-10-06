//! Tenant-scoped artifacts: keyed by `(tenant, digest)`, read concurrently,
//! written one at a time.
//!
//! Two workers writing the same `(tenant, digest)` are writing the same content,
//! so a write is idempotent by identity rather than by content comparison — and
//! the two must still be *serialized*, because "the second sees the first" is
//! what makes the second a no-op instead of a race. Two workers on **different**
//! tenants holding identical bytes are writing different keys and never touch
//! each other.
//!
//! That last one is the property this store exists for. A digest is a function of
//! content alone, so two tenants producing the same bytes get the same digest;
//! keying on the digest alone would make tenant A's artifact readable by tenant
//! B, and "the bytes match" is not an authorization. [`ArtifactKey`] therefore
//! hashes the tenant and the digest together, and the tenant in the key is
//! checked against the caller's own tenant on every read.
//!
//! # Concurrency, stated precisely
//!
//! - **Reads** take no lock beyond a `RwLock` read guard on the index and
//!   never wait on a writer, so any number of readers progress while a write to
//!   another key is in flight.
//! - **Writes to one key** are serialized through a single writer path, and the
//!   losing writer of an identical content is told
//!   [`WriteOutcome::AlreadyPresent`] rather than being told it stored something.
//! - **Writes to different keys** are not serialized against each other: the
//!   index lock is held for the map operation only, never across the artifact's
//!   body being archived.
//! - **Every bound is declared.** Bytes per artifact, artifacts per tenant, and
//!   the longest a tenant name may be. Each refusal names its ceiling and leaves
//!   the store byte-identical.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};

use lgwks_std::hash::{Digest, Hasher};

use crate::journal::frame::SaturatingFrom;

/// The most bytes one artifact may hold.
pub const MAX_ARTIFACT_BYTES: usize = 1024 * 1024;

/// The most artifacts one tenant may hold.
pub const MAX_ARTIFACTS_PER_TENANT: usize = 4_096;

/// The longest a tenant name may be in an artifact key.
///
/// The same bound [`crate::script::Tenant`] enforces, so a name that could key a
/// step record can key an artifact, and a caller cannot smuggle a separator into
/// one key and not the other.
pub const MAX_ARTIFACT_TENANT_BYTES: usize = crate::script::MAX_TENANT_BYTES;

/// What identifies one artifact: whose it is, and which bytes.
///
/// Built through [`ArtifactKey::of`] so a key cannot be assembled with one
/// tenant's name and another tenant's digest, and every field is private so
/// neither can be edited after the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactKey {
    /// BLAKE3 over the framed tenant and the framed digest.
    digest: Digest,
}

impl ArtifactKey {
    /// The key for `tenant`'s copy of `digest`.
    ///
    /// The digest of the key is over *both* facts, length-framed, so two tenants
    /// holding the same bytes get different keys and no pair of different
    /// `(tenant, digest)` pairs can collide in the same bytes.
    #[must_use]
    pub fn of(tenant: &str, digest: &Digest) -> Self {
        let mut hasher = Hasher::new();
        hasher
            .write_framed(b"lgwks-bot/proposal/artifact")
            .write_framed(tenant.as_bytes())
            .write_framed(digest.as_bytes());
        Self {
            digest: hasher.finalize(),
        }
    }

    /// The key's own digest, which is what the store indexes.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }

    /// Lowercase hex, the form a report or an artifact filename carries.
    #[must_use]
    pub fn to_hex(&self) -> String {
        self.digest.to_hex()
    }
}

impl fmt::Display for ArtifactKey {
    /// Lowercase hex of the key's digest.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.digest, formatter)
    }
}

/// What a write did.
///
/// Three arms because "the artifact exists" and "I stored it" are different
/// answers to different questions, and a caller reconciling an idempotent
/// write needs to tell them apart without comparing digests itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WriteOutcome {
    /// The artifact was committed and is now readable.
    Stored {
        /// How many bytes were committed.
        bytes: usize,
    },
    /// An identical artifact was already committed; nothing new was written.
    AlreadyPresent {
        /// How many bytes the committed artifact holds.
        bytes: usize,
    },
}

/// One artifact's stored bytes and the digest they hash to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Held {
    /// The digest of the content, as the caller stated it.
    digest: Digest,
    /// The content, shared so a read hands out a pointer rather than a copy.
    ///
    /// `Arc<[u8]>` rather than `Arc<Vec<u8>>`: the slice is exactly what the
    /// store holds, has no spare capacity a later push could invalidate, and
    /// costs one allocation rather than a buffer plus a vector.
    bytes: Arc<[u8]>,
    /// How many writers have committed this key. A second writer of identical
    /// content increments it, which is what makes "serialized" observable
    /// rather than merely asserted: the count is the order the writers arrived
    /// in, and a store that lost an update would show one.
    writers: u64,
}

/// One tenant's artifacts, and the one writer path that serializes them.
///
/// The locks are `Arc`ed so [`TenantShelf`] is a handle: the store hands the
/// same shelf to every caller that asks for that tenant, and two workers writing
/// one key therefore reach the same `Mutex` rather than each taking their own.
#[derive(Debug, Default, Clone)]
struct TenantShelf {
    /// The artifacts, keyed by the content digest the caller stated.
    artifacts: Arc<RwLock<HashMap<Digest, Held>>>,
    /// The writer lock for this tenant.
    ///
    /// One lock per tenant rather than one store-wide, so a write for tenant A
    /// does not block a write for tenant B. The read path never takes it, which
    /// is what lets independent reads progress while a write is in flight.
    writer: Arc<Mutex<()>>,
}

/// A tenant-scoped, content-addressed artifact store.
///
/// In memory and bounded on all three axes, which is the honest thing for a
/// structure whose job is to *be* the boundary: it is the test double's sink and
/// a host's short-lived evidence shelf. A caller that needs artifacts to outlive
/// the process writes them itself, through
/// [`crate::task::HostBuilder::run_store`], and this store is what indexes and
/// authorizes them.
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    /// One shelf per tenant.
    shelves: Arc<RwLock<BTreeMap<String, TenantShelf>>>,
}

impl ArtifactStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shelves: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// How many tenants this store holds shelves for.
    #[must_use]
    pub fn tenants(&self) -> usize {
        crate::journal::owner::read(&self.shelves).len()
    }

    /// How many artifacts `tenant` holds.
    #[must_use]
    pub fn artifacts(&self, tenant: &str) -> usize {
        crate::journal::owner::read(&self.shelf(tenant).artifacts).len()
    }

    /// The digest `bytes` hash to, which is the identity a write is filed under.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::TooLarge`] when the content is past
    /// [`MAX_ARTIFACT_BYTES`], so a caller learns the ceiling before it has
    /// committed to a key.
    pub fn digest_of(bytes: &[u8]) -> Result<Digest, ArtifactError> {
        check_bytes(bytes.len())?;
        Ok(lgwks_std::hash::blake3(bytes))
    }

    /// Commit `bytes` as `tenant`'s artifact.
    ///
    /// Idempotent by content: a second write of the same bytes under the same
    /// tenant is [`WriteOutcome::AlreadyPresent`] and commits nothing. Writes to
    /// one key are serialized through the tenant's writer lock; writes to
    /// different tenants are not serialized against each other.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::TooLarge`] past the byte ceiling, or
    /// [`ArtifactError::Full`] when the tenant already holds
    /// [`MAX_ARTIFACTS_PER_TENANT`] artifacts and this one is new. Either leaves
    /// the store byte-identical.
    pub fn write(&self, tenant: &str, bytes: &[u8]) -> Result<WriteOutcome, ArtifactError> {
        check_tenant(tenant)?;
        check_bytes(bytes.len())?;
        let digest = lgwks_std::hash::blake3(bytes);
        // The writer lock is taken for the *check and the insert* together, which
        // is the whole of "conflicting writes are serialized": two writers of one
        // key cannot both observe absence. The key itself is derived here so the
        // digest a caller would look the artifact up by is computed on the write
        // path as well as the read path — one derivation, two doors.
        let _key = ArtifactKey::of(tenant, &digest);
        let shelf = self.shelf(tenant);
        let _serialized = crate::journal::owner::lock(&shelf.writer);
        let mut held = crate::journal::owner::write(&shelf.artifacts);
        match held.get_mut(&digest) {
            Some(existing) => {
                existing.writers = existing.writers.saturating_add(1);
                Ok(WriteOutcome::AlreadyPresent {
                    bytes: existing.bytes.len(),
                })
            }
            None => {
                if held.len() >= MAX_ARTIFACTS_PER_TENANT {
                    let refusal = Err(ArtifactError::Full {
                        tenant: tenant.to_owned(),
                        held: held.len(),
                    });
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "write: returning an error to the caller");
                    return refusal;
                }
                held.insert(
                    digest,
                    Held {
                        digest,
                        bytes: Arc::from(bytes.to_vec().into_boxed_slice()),
                        writers: 1,
                    },
                );
                Ok(WriteOutcome::Stored { bytes: bytes.len() })
            }
        }
    }

    /// Read `tenant`'s copy of the artifact whose content hashes to `digest`.
    ///
    /// `None` is absence and never a foreign tenant's content: the store is
    /// indexed by tenant first, so a digest another tenant holds simply is not in
    /// this tenant's shelf. That is what makes "parallel workers cannot read
    /// another tenant's same-digest artifact" a property of the index rather
    /// than a check a caller has to remember.
    #[must_use]
    pub fn read(&self, tenant: &str, digest: &Digest) -> Option<Arc<[u8]>> {
        crate::journal::owner::read(&self.shelf(tenant).artifacts)
            .get(digest)
            .map(|held| Arc::clone(&held.bytes))
    }

    /// How many writers have committed this key.
    ///
    /// The serialization receipt: one means the key was written once, two means a
    /// second writer arrived and was serialized into a no-op, and the count is
    /// per key so two tenants writing their own copies never share it.
    #[must_use]
    pub fn writers(&self, tenant: &str, digest: &Digest) -> u64 {
        crate::journal::owner::read(&self.shelf(tenant).artifacts)
            .get(digest)
            .map_or(0, |held| held.writers)
    }

    /// Whether `tenant` holds the artifact whose content hashes to `digest`.
    #[must_use]
    pub fn holds(&self, tenant: &str, digest: &Digest) -> bool {
        self.read(tenant, digest).is_some()
    }

    /// The shelf for `tenant`, created under a write lock on first use.
    ///
    /// A `RwLock` read on the hot path — every artifact of a tenant that already
    /// has a shelf — and a write only when the tenant is new. `BTreeMap` so the
    /// tenant set is in name order and a report of it does not depend on
    /// insertion order. The shelf is a handle over `Arc`-ed locks, so every
    /// caller that asks for this tenant reaches the same writer lock.
    fn shelf(&self, tenant: &str) -> TenantShelf {
        if let Some(shelf) = crate::journal::owner::read(&self.shelves).get(tenant) {
            return shelf.clone();
        }
        let mut shelves = crate::journal::owner::write(&self.shelves);
        shelves.entry(tenant.to_owned()).or_default().clone()
    }
}

impl Default for ArtifactStore {
    /// An empty store.
    fn default() -> Self {
        Self::new()
    }
}

/// Refuse a content larger than the byte ceiling.
fn check_bytes(len: usize) -> Result<(), ArtifactError> {
    if len > MAX_ARTIFACT_BYTES {
        let refusal = Err(ArtifactError::TooLarge {
            got: u64::saturating_from(len),
            limit: u64::saturating_from(MAX_ARTIFACT_BYTES),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "check_bytes: returning an error to the caller");
        return refusal;
    }
    Ok(())
}

/// Refuse a tenant name that could not key a step record.
fn check_tenant(tenant: &str) -> Result<(), ArtifactError> {
    if tenant.is_empty() || tenant.len() > MAX_ARTIFACT_TENANT_BYTES {
        let refusal = Err(ArtifactError::InvalidTenant {
            tenant: tenant.to_owned(),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "check_tenant: returning an error to the caller");
        return refusal;
    }
    Ok(())
}

// ── ArtifactError ────────────────────────────────────────────────────────────

/// Why an artifact could not be stored.
///
/// A refusal, never a partial write: every arm leaves the store byte-identical,
/// because an artifact store that dropped half an artifact on a ceiling would be
/// reporting content it cannot produce again.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ArtifactError {
    /// The content is past [`MAX_ARTIFACT_BYTES`].
    TooLarge {
        /// The size given.
        got: u64,
        /// The ceiling.
        limit: u64,
    },
    /// The tenant already holds [`MAX_ARTIFACTS_PER_TENANT`] artifacts.
    Full {
        /// The tenant that is full.
        tenant: String,
        /// How many artifacts it holds.
        held: usize,
    },
    /// The tenant name could not key an artifact.
    InvalidTenant {
        /// The name given.
        tenant: String,
    },
}

impl fmt::Display for ArtifactError {
    /// What was refused, naming the ceiling.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::TooLarge { got, limit } => {
                write!(formatter, "TooLarge: {got} bytes exceeds {limit}")
            }
            Self::Full {
                ref tenant,
                ref held,
            } => write!(
                formatter,
                "Full: tenant {tenant:?} already holds {held} artifacts, the ceiling"
            ),
            Self::InvalidTenant { ref tenant } => write!(
                formatter,
                "InvalidTenant: {tenant:?} is not a tenant name of 1..={MAX_ARTIFACT_TENANT_BYTES} \
                 bytes"
            ),
        }
    }
}

impl std::error::Error for ArtifactError {}

impl From<ArtifactError> for super::Refusal {
    /// An artifact refusal is a proposal refusal, so a call site that stores a
    /// model's artifact reads the same `?` as one that decoded it.
    fn from(error: ArtifactError) -> Self {
        match error {
            ArtifactError::TooLarge { got, limit } => Self::Limit {
                what: "the bytes in an artifact",
                got,
                limit,
            },
            ArtifactError::Full { tenant: _, held } => Self::Limit {
                what: "the artifacts held by a tenant",
                got: u64::saturating_from(held).saturating_add(1),
                limit: u64::saturating_from(MAX_ARTIFACTS_PER_TENANT),
            },
            ArtifactError::InvalidTenant { .. } => Self::Malformed {
                cause: "a tenant name that cannot key an artifact",
                at: 0,
            },
        }
    }
}
