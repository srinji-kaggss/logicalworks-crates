//! Why the store refused.

use std::fmt;
use std::time::Duration;

use lgwks_std::wire::WireError;

/// Why the store refused.
///
/// A read failure is never absence: every arm here is an error the caller must
/// see, never an empty listing that would let a finished run look unfinished.
#[derive(Debug)]
#[non_exhaustive]
pub enum TenantStoreError {
    /// The device refused an open, read, write or sync.
    Storage {
        /// What the device said.
        cause: std::io::Error,
    },
    /// The file exists but is not a tenant store, or its header is short.
    NotAStore,
    /// The file is a tenant store in a format this build does not read.
    ///
    /// Distinct from [`NotAStore`](Self::NotAStore) because the two say
    /// opposite things about the disk: one says these bytes were never this
    /// store's, the other says they were, written by a version of this crate
    /// whose records mean something this build cannot reconstruct. The file is
    /// left unchanged either way: it is never reset.
    FormatVersion {
        /// The version byte the file declares.
        found: u8,
        /// The version this build reads.
        expected: u8,
    },
    /// A committed frame does not follow from the records before it. Those
    /// bytes are acknowledged evidence and are refused, never trimmed.
    Corrupt {
        /// The frame's index, from zero.
        at: u64,
    },
    /// A record could not be archived, or a frame did not decode.
    Encoding {
        /// What the codec said.
        cause: WireError,
    },
    /// A framed record past the store's ceiling. Nothing was written.
    Limit {
        /// What would have been needed.
        requested: u64,
        /// The ceiling.
        limit: u64,
    },
    /// Another coordinator held the lock past the busy timeout. Nothing was
    /// written and nothing was read; the caller waits and retries, or reports.
    Busy {
        /// How long the caller waited before it was refused.
        waited: Duration,
    },
    /// The tenant already has a policy; it was not changed.
    AlreadyInitialised {
        /// The tenant that was already initialised.
        tenant: String,
    },
    /// A run id this store already holds; the second begin wrote nothing.
    DuplicateRun {
        /// The run id that was already begun.
        run: String,
    },
    /// No run of this tenant matches the prefix.
    NoSuchRun {
        /// The tenant that was searched. Empty when the caller named no
        /// tenant, as the row-addressed updates do.
        tenant: String,
        /// The prefix or id that matched nothing.
        run: String,
    },
    /// A prefix names more than one of the tenant's runs.
    Ambiguous {
        /// The prefix that matched more than one run.
        run: String,
    },
    /// A lane row this run wrote at its start no longer matches; nothing was
    /// changed.
    LaneLost {
        /// The run the lane belongs to.
        run_id: String,
        /// The lane position the caller named.
        lane: usize,
        /// The step whose precondition the row failed.
        step: &'static str,
    },
}

impl fmt::Display for TenantStoreError {
    /// The refusal, naming the ceiling, the device, the tenant or the row.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Storage { ref cause } => {
                write!(formatter, "the tenant store's device refused: {cause}")
            }
            Self::NotAStore => formatter
                .write_str("this file is not a tenant store; refusing to read it as run records"),
            Self::FormatVersion { found, expected } => write!(
                formatter,
                "this is a tenant store in format version {found}, and this build reads \
                 version {expected}; it was left unchanged"
            ),
            Self::Corrupt { at } => write!(
                formatter,
                "tenant store frame {at} does not follow from the records before it; \
                 the bytes are refused, not trimmed"
            ),
            Self::Encoding { ref cause } => {
                write!(formatter, "a run record could not be archived: {cause}")
            }
            Self::Limit { requested, limit } => write!(
                formatter,
                "one framed record of {requested} bytes exceeds the ceiling of {limit}"
            ),
            Self::Busy { waited } => write!(
                formatter,
                "another coordinator held the store lock past the busy timeout after \
                 {waited:?}; nothing was written"
            ),
            Self::AlreadyInitialised { ref tenant } => write!(
                formatter,
                "tenant {tenant:?} is already initialised; its policy never changes"
            ),
            Self::DuplicateRun { ref run } => write!(
                formatter,
                "run {run:?} was already begun; the second begin wrote nothing"
            ),
            Self::NoSuchRun {
                ref tenant,
                ref run,
            } => {
                write!(formatter, "tenant {tenant:?} has no run {run:?}")
            }
            Self::Ambiguous { ref run } => {
                write!(
                    formatter,
                    "{run:?} names more than one run; give more of it"
                )
            }
            Self::LaneLost {
                ref run_id,
                lane,
                step,
            } => write!(
                formatter,
                "run {run_id}: lane #{lane} is not in the state its {step} needs; \
                 it was left unchanged"
            ),
        }
    }
}

impl std::error::Error for TenantStoreError {
    /// The device's or the codec's own error.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Storage { ref cause } => Some(cause),
            Self::Encoding { ref cause } => Some(cause),
            Self::NotAStore
            | Self::FormatVersion { .. }
            | Self::Corrupt { .. }
            | Self::Limit { .. }
            | Self::Busy { .. }
            | Self::AlreadyInitialised { .. }
            | Self::DuplicateRun { .. }
            | Self::NoSuchRun { .. }
            | Self::Ambiguous { .. }
            | Self::LaneLost { .. } => None,
        }
    }
}

impl TenantStoreError {
    /// A storage refusal carrying the device's own error.
    ///
    /// One constructor rather than a literal per call site, because every
    /// `map_err` here wraps the same fact and a literal per site would be
    /// several chances to name a different variant for it.
    pub(super) fn storage(cause: std::io::Error) -> Self {
        Self::Storage { cause }
    }
}
