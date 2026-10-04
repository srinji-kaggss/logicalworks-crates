//! What a resumable run was recorded under, so a resume can refuse to replay it
//! under anything else.
//!
//! A run store's records are keyed by [`StepKey`], which is a digest of the
//! tenant and the step path. That answers "which step is this", and it answers
//! nothing about the three other things a recorded value is only meaningful
//! under: the definition that produced it, the input it was given, and the codec
//! its value is in. A task edited, a caller passing a different input, and a
//! step whose return type changed all keep every step path identical — so a
//! resume under any of them finds the record, and returns a value the new
//! definition never produced.
//!
//! [`DefinitionIdentity`] is the four facts that close that gap, and
//! [`Drift`] is what a disagreement between a recorded one and a live one is
//! called. They live beside the store rather than in it because the store's job
//! is to keep the bytes and the chain; what the bytes *mean* is the caller's
//! vocabulary, and a store that named the drift kinds would be naming a task's
//! schema.
//!
//! # Order and the step path
//!
//! The step path carries order (`alpha/beta` runs `beta` after `alpha`) but not
//! depth-by-depth reordering. Two adjacent durable steps swapped in the
//! definition keep every path, every key and every recorded value — each step
//! still returns the value it returned last time — so there is no identity
//! difference for the store to detect, and a `Drift` that claimed otherwise
//! would be refusing a resume that is in fact sound. The honest form of that
//! half of T15 is that reordering two whole *blocks* changes the paths, and that
//! is caught as [`Drift::Order`]: a definition revision that declares a
//! different step count is a different shape of flow, whether it arrived by
//! renaming or by insertion. What is deliberately not claimed: a same-revision
//! edit that permutes two siblings without changing the step count, which the
//! store cannot see and which is named here rather than left to be discovered.
//!
//! # Schema drift, and where the codec id comes from
//!
//! [`DefinitionIdentity::codec`] is a declared string, never
//! `std::any::type_name`: Rust documents `type_name` as diagnostic, so a module
//! rename would silently give every step a new identity. It is taken from
//! [`RunStore::with_codec`], which is what a caller passes the schema name a
//! step's durable values are written under; the default says "unversioned", and
//! a definition that adopts a real codec name does so as an explicit schema
//! change. `lgwks.bot.schema.v1.*` is the convention this crate already uses
//! for [`crate::effect::InputIdentity`].
//!
//! # Migration
//!
//! These fields are appended to the stored record, and the store's header
//! version is bumped with them: [`STORE_FORMAT`] is `\x02` where `\x01` named the
//! record without a definition identity. There is no migration path and there is
//! none planned, deliberately. An old file is refused at open as
//! [`StoreError::FormatVersion`], naming the
//! version it declares and the version this build reads, rather than half-read:
//! the only alternative is to read a record with no definition identity as one
//! that had the default identity — which would make every pre-version resume look
//! like an exactly-compatible one. It is a typed refusal of its own rather than
//! [`StoreError::NotAStore`], because that arm says
//! the bytes were never this store's and these were written by an earlier
//! version of this crate: telling an operator their data is not their own is
//! what makes someone delete a file a system is still relying on. The format has
//! never shipped a version that could lose a record, so there is nothing to
//! convert; a deployment that needs its records keeps its own copy and re-runs.

use std::fmt;

use lgwks_std::hash::{Digest, Hasher};

/// What part of a replay's identity does not match the recorded one.
///
/// One typed arm per axis, because each names a different repair: a changed
/// definition wants a new run, a changed input wants a decision about whether
/// the same work applies, a changed codec wants a migration and a changed shape
/// wants an author. A single "incompatible" would leave the caller to guess
/// which of those it was looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Drift {
    /// The task's declared definition revision is not the recorded one.
    Definition {
        /// The revision the caller declared now.
        declared: u64,
        /// The revision the records were written under.
        recorded: u64,
    },
    /// The input digest is not the recorded one.
    Input {
        /// The caller's input now.
        named: Digest,
        /// The input the records were written under.
        recorded: Digest,
    },
    /// The number of declared durable steps is not the recorded one.
    ///
    /// The shape of the flow, which is what an inserted or removed step is.
    Order {
        /// How many durable steps the definition declares now.
        declared: usize,
        /// How many the records were written under.
        recorded: usize,
    },
    /// A step's durable-value schema id is not the recorded one.
    Schema {
        /// The schema id the caller declared now.
        named: String,
        /// The schema id the records were written under.
        recorded: String,
    },
}

impl Drift {
    /// Which axis disagreed, in a stable order a trace can record.
    ///
    /// The tag rather than the [`fmt::Debug`] spelling, because a debug string
    /// changes when a field is added and a replay trace that changed with it
    /// would compare unequal for a run that behaved identically.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match *self {
            Self::Definition { .. } => "definition",
            Self::Input { .. } => "input",
            Self::Order { .. } => "order",
            Self::Schema { .. } => "schema",
        }
    }
}

impl fmt::Display for Drift {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Definition { declared, recorded } => write!(
                formatter,
                "the definition revision is {declared}, the records were written under \
                 {recorded}"
            ),
            Self::Input { named, recorded } => write!(
                formatter,
                "the input is {} and the records were written under {}",
                named.to_hex(),
                recorded.to_hex()
            ),
            Self::Order { declared, recorded } => write!(
                formatter,
                "the definition declares {declared} durable steps and the records were \
                 written under {recorded}"
            ),
            Self::Schema {
                ref named,
                ref recorded,
            } => write!(
                formatter,
                "the durable-value schema is {named:?} and the records were written under \
                 {recorded:?}"
            ),
        }
    }
}

/// The schema id a record carries when its caller declared none.
///
/// "Unversioned" rather than the empty string, so a caller that reads it back
/// cannot mistake it for a schema it forgot to declare.
pub const UNVERSIONED_CODEC: &str = "lgwks.bot.schema.unversioned";

/// The four facts a recorded step value is only meaningful under.
///
/// Carried in every record, checked before anything new runs, and never derived
/// from a value that could change without the author meaning it to: the task
/// name is validated at declaration, the revision is declared by the caller,
/// the input digest is content-addressed, and the codec is a declared string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionIdentity {
    /// The task this run was recorded under.
    name: String,
    /// The definition revision the caller declared for that task.
    revision: u64,
    /// The content digest of the input the run was given.
    input: Digest,
    /// How many durable steps that definition declares.
    ///
    /// The count rather than the ordered list, because the list is what the
    /// step paths already are: a step whose path changed is a different step key,
    /// and a definition that adds or removes a durable step changes the count
    /// even when every surviving path is byte-identical.
    steps: usize,
    /// The declared schema id of the step values.
    codec: String,
}

impl DefinitionIdentity {
    /// The identity of a task's first revision, before any drift has been seen.
    ///
    /// The starting point rather than a default a caller may fall back to: a
    /// store holding records for an unknown definition has to be told which
    /// revision it is being asked about, because "unknown" and "revision zero"
    /// are different facts and only one of them can be compared.
    #[must_use]
    pub fn new(name: &str, revision: u64, input: Digest, steps: usize) -> Self {
        Self {
            name: name.to_owned(),
            revision,
            input,
            steps,
            codec: UNVERSIONED_CODEC.to_owned(),
        }
    }

    /// The same identity under a declared durable-value schema.
    ///
    /// The door a caller changes the schema through. It is a change of the
    /// declared id, never of the bytes: a definition whose durable values were
    /// written under one schema and read back as another is exactly the drift
    /// this type exists to refuse.
    #[must_use]
    pub fn with_codec(mut self, codec: &str) -> Self {
        codec.clone_into(&mut self.codec);
        self
    }

    /// The task this run was recorded under.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The definition revision the caller declared.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The content digest of the input the run was given.
    #[must_use]
    pub const fn input(&self) -> Digest {
        self.input
    }

    /// How many durable steps the definition declares.
    #[must_use]
    pub const fn steps(&self) -> usize {
        self.steps
    }

    /// The declared schema id of the step values.
    #[must_use]
    pub fn codec(&self) -> &str {
        &self.codec
    }

    /// The digest that names this identity, and what a record stores.
    ///
    /// Domain-separated and length-framed per field, so no two distinct
    /// identities can hash the same bytes and the digest cannot be confused with
    /// the [`StepKey`](crate::script::StepKey) of any step.
    #[must_use]
    pub fn digest(&self) -> Digest {
        let mut hasher = Hasher::new();
        hasher.write_framed(b"lgwks.bot.definition.v1");
        hasher.write_framed(self.name.as_bytes());
        hasher.write_framed(&self.revision.to_le_bytes());
        hasher.write_framed(self.input.as_bytes());
        hasher.write_framed(&u64::try_from(self.steps).unwrap_or(u64::MAX).to_le_bytes());
        hasher.write_framed(self.codec.as_bytes());
        hasher.finalize()
    }

    /// Which axis of `recorded` this identity does not agree with, if any.
    ///
    /// The first disagreeing axis in a fixed order, so two callers comparing the
    /// same pair get the same answer: definition, then input, then order, then
    /// schema. Order matters because it is the most explanatory answer when two
    /// of them disagree at once — a task that was both revised and re-run with a
    /// different input should be told about the revision first.
    #[must_use]
    pub fn drift_from(&self, recorded: &Self) -> Option<Drift> {
        if self.revision != recorded.revision {
            return Some(Drift::Definition {
                declared: self.revision,
                recorded: recorded.revision,
            });
        }
        if self.input != recorded.input {
            return Some(Drift::Input {
                named: self.input,
                recorded: recorded.input,
            });
        }
        if self.steps != recorded.steps {
            return Some(Drift::Order {
                declared: self.steps,
                recorded: recorded.steps,
            });
        }
        if self.codec != recorded.codec {
            return Some(Drift::Schema {
                named: self.codec.clone(),
                recorded: recorded.codec.clone(),
            });
        }
        None
    }
}

/// The current on-disk format of a run store.
///
/// `\x02` is the record *with* a definition identity. `\x01` named a record that
/// carried the run, key, tenant, path and value only, and a file in that format
/// is refused at open rather than read as a record whose definition identity was
/// [`UNVERSIONED_CODEC`]: there is no migration, and inventing one would make
/// every pre-version resume look compatible rather than unprovable.
pub(crate) const STORE_FORMAT: u8 = 2;
