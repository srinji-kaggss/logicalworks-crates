//! Model output is an untrusted task input, not an instruction.
//!
//! A language model produces text. This module admits that text as *data*: it
//! is decoded against a declared schema under a byte ceiling, and every claim it
//! makes is checked against something the host already holds — an operation the
//! host registered, a capability the run already has, an evidence reference that
//! is actually present. Nothing a model says can install a tool, read a
//! credential, widen a grant, or turn a partial observation into a complete one.
//! Each of those attempts is a typed [`Refusal`] carrying the
//! [`Provenance`] of the payload that made it, and a refusal is a value a caller
//! can count, not an exception.
//!
//! # The rule this module implements (issue #87)
//!
//! The fix for prompt injection is not a better prompt. It is a boundary: the
//! untrusted bytes cross into the host through a decoder that only produces
//! values the host already decided are legal, and everything a payload asks for
//! beyond that boundary is a refusal. So:
//!
//! - **Validation** — [`decode`] is a hand-written bounded decoder, not a
//!   general document interpreter. There is no untyped plan, no expression
//!   evaluator and no fourth execution path; the operations a proposal may name
//!   are exactly the ones the host registered in [`Surface::register`], and the
//!   capabilities it may need are exactly the ones the run already holds.
//! - **Provenance** — every refusal and every refusal count carries where the
//!   bytes came from ([`Source::Model`]) and a digest of them, so "the model
//!   asked to install a tool" is a fact a log can be checked against.
//! - **No-progress detection** — [`RepairLedger`] counts one unchanged failure
//!   fingerprint and reaches a finite [`Intervention`] after a declared ceiling,
//!   rather than repairing forever. New evidence is recorded *beside* the root
//!   spend and never resets it, so progress on one axis cannot buy unbounded
//!   attempts on another.
//! - **Bounded repair** — [`PlanBudget`] bounds how many times a proposal may
//!   be re-admitted for one step.
//! - **Tenant-scoped artifacts** — [`ArtifactStore`] keys by `(tenant, digest)`,
//!   so two tenants holding the same content cannot read each other's copy, and
//!   concurrent writers to one key are serialized into one order.
//! - **Context that survives a reset** — [`Checkpoint`] carries completed steps,
//!   user corrections, `Unknown`-classed effects and evidence references through
//!   the run store, so a new task instance resuming the run recovers them
//!   instead of re-deriving them.
//!
//! # Where the model is
//!
//! Nowhere. This module never calls a network model and takes no client type, so
//! the untrusted input arrives as `&[u8]` from a caller that owns the transport.
//! That is deliberate: the guarantee here is about *admission*, and it is the
//! same guarantee whoever produced the bytes. The tests drive a deterministic
//! [`crate::proposal::model::StubModel`], a pure function from a seed to output
//! bytes, so "the model said something else" is a seed rather than a network.
//!
//! # Example
//!
//! ```
//! use lgwks_bot::cap::Cap;
//! use lgwks_bot::proposal::{
//!     Completion, Decoder, Intervention, LedgerLimits, Outcome, PlanBudget, PlanLimits,
//!     Provenance, RepairLedger, Refusal, Source, Surface,
//! };
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // The host declares what a proposal is allowed to say, and what the run holds.
//! let surface = Surface::builder("acme")?
//!     .operation("read-report", &[Cap::fs()])?
//!     .holding(&[Cap::fs()])
//!     .build();
//! let decoder = Decoder::new(PlanLimits::default());
//!
//! // Untrusted bytes naming an operation nobody registered.
//! let refused = decoder.decode(&surface, b"op=install-dependency", Source::Model);
//! assert!(
//!     matches!(refused.refusal(), Some(Refusal::UnknownOperation { .. })),
//!     "an unregistered operation is refused, whatever asked for it"
//! );
//!
//! // A proposal naming only registered operations is admitted, and the
//! // capability it needs is one the run already holds.
//! let admitted = decoder.decode(&surface, b"op=read-report\nnote=ok", Source::Model);
//! assert!(admitted.is_admitted(), "a registered operation is admitted");
//!
//! // The same operation for a run that does not hold `bot.fs` is refused by
//! // naming the capability, so the repair is a deliberate grant.
//! let poor = Surface::builder("acme")?
//!     .operation("read-report", &[Cap::fs()])?
//!     .build();
//! let denied = decoder.decode(&poor, b"op=read-report", Source::Model);
//! assert!(
//!     matches!(denied.refusal(), Some(Refusal::CapabilityNotHeld { .. })),
//!     "a capability the run does not hold is refused by name"
//! );
//!
//! // A completion claim is admitted only with the evidence it names present.
//! let claim = Completion::claim("done", &["report-1"]);
//! assert!(claim.admit(&[String::from("report-1")]).is_ok(), "its evidence is present");
//! assert!(
//!     matches!(claim.admit(&[]), Err(Refusal::NotEvidenced { .. })),
//!     "a claim naming absent evidence is NotEvidenced"
//! );
//!
//! // Repeated unchanged failure reaches a finite typed intervention. Three
//! // identical failures are tolerated; the fourth is the intervention.
//! let provenance = Provenance::for_model("acme", b"op=read-report");
//! let mut ledger = RepairLedger::new(LedgerLimits::new(3, 8), provenance);
//! for attempt in 1..=3 {
//!     assert!(
//!         ledger.record_failure("fingerprint-a").is_admitted(),
//!         "failure {attempt} of 3 is still within the repair ceiling"
//!     );
//! }
//! assert!(
//!     matches!(
//!         ledger.record_failure("fingerprint-a"),
//!         Outcome::Intervention(Intervention::NoProgress { repetitions: 4, .. })
//!     ),
//!     "the fourth identical failure is a typed intervention, not another repair"
//! );
//!
//! // New evidence is recorded beside the root spend and does not erase it.
//! assert!(ledger.record_evidence("fingerprint-a"), "evidence is recorded");
//! assert_eq!(ledger.spent(), 4, "the root spend is still four failures");
//! assert_eq!(
//!     ledger.repetitions("fingerprint-a"),
//!     4,
//!     "and the count the ceiling is measured against has not moved"
//! );
//!
//! // Bounded repair is a declared ceiling, not a property of the loop.
//! let mut budget = PlanBudget::new(2);
//! assert!(budget.charge().is_ok(), "the first admission is within budget");
//! assert!(budget.charge().is_ok(), "so is the second");
//! assert!(
//!     matches!(budget.charge(), Err(Refusal::Limit { .. })),
//!     "the third admission is refused with its ceiling named"
//! );
//! # Ok(())
//! # }
//! ```

mod artifact;
mod checkpoint;
mod codec;
mod completion;
mod ledger;
mod model;
mod surface;

use std::fmt;

use lgwks_std::hash::{Digest, Hasher};

pub use artifact::{
    ArtifactError, ArtifactKey, ArtifactStore, MAX_ARTIFACT_BYTES, MAX_ARTIFACTS_PER_TENANT,
    WriteOutcome,
};
pub use checkpoint::{
    Checkpoint, CheckpointError, Correction, CorrectionKind, EffectNote, EffectNoteKind,
    MAX_CHECKPOINT_EVIDENCE, MAX_CHECKPOINT_NOTES, MAX_CHECKPOINT_STEPS,
};
pub use codec::{Declared, Decoder, Plan, PlanLimits, Wanted, payload_digest};
pub use completion::{Completion, CompletionKind, CompletionOutcome, Coverage, MAX_EVIDENCE_REFS};
pub use ledger::{Intervention, LedgerLimits, RepairLedger};
pub use model::StubModel;
pub use surface::{Operation, Surface, SurfaceError};

/// The longest a field's *name* may be, in bytes.
///
/// Separate from the value ceiling because the two are charged against different
/// facts: a name is chosen by this decoder's grammar and a value by whoever
/// wrote the payload, so a reader that refused long names for being long would
/// be refusing its own vocabulary.
pub const MAX_FIELD_NAME_BYTES: usize = 32;

/// The most bytes one untrusted payload may occupy.
///
/// A ceiling rather than a policy: a model that has begun emitting megabytes is
/// already the failure this module exists to contain, and refusing at the byte
/// count costs nothing to reason about. [`Decoder::new`] takes a
/// [`PlanLimits`], and this is the value it uses when the caller names none.
pub const MAX_PLAN_BYTES: usize = 64 * 1024;

/// The most bytes one declared field may occupy.
///
/// Charged per field rather than only in total, so a single 60 KiB `note` is
/// refused at its own field rather than after the decoder has already built a
/// value for everything around it.
pub const MAX_FIELD_BYTES: usize = 8 * 1024;

// ── Source ───────────────────────────────────────────────────────────────────

/// Where an untrusted payload came from.
///
/// Load-bearing rather than decorative: a refusal is only actionable if the
/// reader can tell a model's mistake from a hostile tool's output, and those two
/// are the same bytes with a different [`Provenance`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Source {
    /// A language model's output, untrusted in full.
    Model,
    /// A tool or subprocess's output, untrusted in full — a command's stdout is
    /// attacker-influenced content, not an instruction.
    ToolOutput,
    /// A remote document, untrusted in full.
    Document,
}

impl Source {
    /// The name of this source, for a provenance record.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::ToolOutput => "tool-output",
            Self::Document => "document",
        }
    }
}

impl fmt::Display for Source {
    /// The [`Source::label`].
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

// ── Provenance ───────────────────────────────────────────────────────────────

/// Who produced an untrusted payload, and what it was, in one record.
///
/// Every [`Outcome`] a decode produces carries one, so no refusal exists in the
/// API without an attributable cause. The digest is over the bytes as received:
/// the same refusal reproduced from the same bytes produces the same digest,
/// which is what lets a caller aggregate them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// Which kind of untrusted producer this was.
    source: Source,
    /// Who ran it, as a tenant name.
    tenant: String,
    /// The digest of the exact bytes admitted.
    digest: Digest,
    /// How many bytes were admitted, before any ceiling was applied.
    bytes: usize,
}

impl Provenance {
    /// Record the provenance of `bytes` as received from `source`.
    #[must_use]
    pub fn of(source: Source, tenant: &str, bytes: &[u8]) -> Self {
        Self {
            source,
            tenant: tenant.to_owned(),
            digest: lgwks_std::hash::blake3(bytes),
            bytes: bytes.len(),
        }
    }

    /// The provenance a model payload for `tenant` has.
    #[must_use]
    pub fn for_model(tenant: &str, bytes: &[u8]) -> Self {
        Self::of(Source::Model, tenant, bytes)
    }

    /// Which kind of untrusted producer this was.
    #[must_use]
    pub const fn source(&self) -> Source {
        self.source
    }

    /// Who ran it.
    #[must_use]
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// The digest of the exact bytes admitted.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }

    /// Lowercase hex of the digest, the form a log or a report carries.
    #[must_use]
    pub fn digest_hex(&self) -> String {
        self.digest.to_hex()
    }

    /// How many bytes were admitted, before any ceiling was applied.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }
}

impl fmt::Display for Provenance {
    /// `<source> from <tenant>, <bytes> bytes, digest <hex>`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} from {}, {} bytes, digest {}",
            self.source.label(),
            self.tenant,
            self.bytes,
            self.digest.to_hex()
        )
    }
}

// ── Refusal ──────────────────────────────────────────────────────────────────

/// Why an untrusted payload did not become work.
///
/// Every arm is a fact about *this* payload, not about the model that produced
/// it, and no arm is reachable by a payload that stayed inside the surface the
/// host declared. The pairings that matter:
///
/// - [`Refusal::InstallTool`] and [`Refusal::CredentialRead`] exist as separate
///   arms because they are separate attacks, and a caller that only counts
///   refusals still sees both;
/// - [`Refusal::UnknownOperation`] is not a variant of either, because naming
///   nothing registered is a malformed proposal rather than a privilege
///   attempt;
/// - [`Refusal::CapabilityNotHeld`] names the capability, so the repair is to
///   grant it deliberately rather than to widen the decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// The payload was longer than the declared ceiling and was not decoded at
    /// all, so nothing inside it is known.
    Oversized {
        /// The bytes received.
        got: usize,
        /// The ceiling.
        limit: usize,
    },
    /// The payload was not a well-formed document for this decoder.
    Malformed {
        /// What the decoder stopped on.
        cause: &'static str,
        /// The byte offset it stopped at.
        at: usize,
    },
    /// The payload decoded, but one of its values was outside a declared
    /// ceiling.
    Limit {
        /// Which ceiling.
        what: &'static str,
        /// The value given.
        got: u64,
        /// The ceiling.
        limit: u64,
    },
    /// The payload was cut short, so the document it began is known but not
    /// complete.
    ///
    /// Distinct from [`Refusal::Malformed`] because the document itself was
    /// well-formed: this is a transport that delivered fewer bytes than it
    /// promised, and a partial read is never decoded into a claim about what
    /// the whole payload said.
    Incomplete {
        /// The byte offset the document stopped at.
        at: usize,
    },
    /// The payload named an operation the host never registered.
    UnknownOperation {
        /// The name it named.
        name: String,
    },
    /// The payload asked for a capability this run does not hold.
    CapabilityNotHeld {
        /// The operation it asked for.
        operation: String,
        /// The capability it needed.
        required: String,
    },
    /// The payload tried to install a tool.
    ///
    /// A proposal names operations the host already registered; installing one
    /// is not an operation this crate has, so the attempt is refused by
    /// construction rather than checked.
    InstallTool {
        /// The name it tried to install.
        name: String,
    },
    /// The payload tried to read a credential.
    ///
    /// Refused for the same reason: a proposal cannot reach anything the run
    /// did not already hold.
    CredentialRead {
        /// What it tried to read.
        what: String,
    },
    /// The payload tried to leave the surface the host declared.
    ///
    /// The sandbox-escape arm: a `path` crossing out of the tenant's declared
    /// artifact root, or a `host` field naming somewhere other than the host's
    /// own tenant. It is a typed arm rather than an `UnknownOperation` because
    /// it is observed and must stay observable — the row it serves asks that
    /// sandbox-escape refusals remain visible, and a refusal reported as
    /// "malformed" is a refusal nobody can find.
    SandboxEscape {
        /// The value that reached outside.
        field: &'static str,
        /// What it tried to reach.
        target: String,
    },
    /// A completion claim whose evidence references are not all present.
    NotEvidenced {
        /// The references the claim named.
        named: Vec<String>,
        /// How many of them the run does not hold.
        missing: usize,
    },
    /// The plan is well-formed but carries nothing to do.
    Empty,
}

impl Refusal {
    /// Whether this refusal is an attempt to widen authority rather than a
    /// malformed document.
    ///
    /// The five arms that are attempts rather than syntax: a tool install, a
    /// credential read, a capability the run does not hold, an operation
    /// nobody registered, and a sandbox escape. A caller that alerts on
    /// privilege attempts reads this rather than re-listing the arms.
    #[must_use]
    pub const fn is_privilege_attempt(&self) -> bool {
        matches!(
            self,
            Self::InstallTool { .. }
                | Self::CredentialRead { .. }
                | Self::CapabilityNotHeld { .. }
                | Self::UnknownOperation { .. }
                | Self::SandboxEscape { .. }
        )
    }

    /// The name of this arm, for a log line or a counter label.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match *self {
            Self::Oversized { .. } => "Oversized",
            Self::Malformed { .. } => "Malformed",
            Self::Limit { .. } => "Limit",
            Self::Incomplete { .. } => "Incomplete",
            Self::UnknownOperation { .. } => "UnknownOperation",
            Self::CapabilityNotHeld { .. } => "CapabilityNotHeld",
            Self::InstallTool { .. } => "InstallTool",
            Self::CredentialRead { .. } => "CredentialRead",
            Self::SandboxEscape { .. } => "SandboxEscape",
            Self::NotEvidenced { .. } => "NotEvidenced",
            Self::Empty => "Empty",
        }
    }

    /// Build the refusal for a payload that asks to install `name`.
    #[must_use]
    pub fn install_tool(name: &str) -> Self {
        Self::InstallTool {
            name: name.to_owned(),
        }
    }

    /// Build the refusal for a payload that asks to read `what`.
    #[must_use]
    pub fn credential_read(what: &str) -> Self {
        Self::CredentialRead {
            what: what.to_owned(),
        }
    }

    /// Build the refusal for a `field` value that reached outside its boundary.
    #[must_use]
    pub fn escape(field: &'static str, target: &str) -> Self {
        Self::SandboxEscape {
            field,
            target: target.to_owned(),
        }
    }
}

impl fmt::Display for Refusal {
    /// The arm and what it names. Attacker-supplied text is rendered with
    /// `{:?}`, which escapes control characters, so a refusal cannot forge a log
    /// line.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Oversized { ref got, ref limit } => {
                write!(
                    formatter,
                    "Oversized: {got} bytes exceeds the {limit}-byte ceiling"
                )
            }
            Self::Malformed { ref cause, ref at } => {
                write!(formatter, "Malformed: {cause} at byte {at}")
            }
            Self::Limit {
                ref what,
                ref got,
                ref limit,
            } => {
                write!(formatter, "Limit: {what} is {got}, outside 1..={limit}")
            }
            Self::Incomplete { ref at } => write!(
                formatter,
                "Incomplete: the payload was cut short at byte {at}, so it is not a complete document"
            ),
            Self::UnknownOperation { ref name } => {
                write!(formatter, "UnknownOperation: {name:?} is not registered")
            }
            Self::CapabilityNotHeld {
                ref operation,
                ref required,
            } => write!(
                formatter,
                "CapabilityNotHeld: {operation:?} needs {required:?}, which this run does not hold"
            ),
            Self::InstallTool { ref name } => {
                write!(
                    formatter,
                    "InstallTool: a proposal may not install {name:?}"
                )
            }
            Self::CredentialRead { ref what } => {
                write!(
                    formatter,
                    "CredentialRead: a proposal may not read {what:?}"
                )
            }
            Self::SandboxEscape {
                ref field,
                ref target,
            } => write!(
                formatter,
                "SandboxEscape: {field} = {target:?} reaches outside this run's boundary"
            ),
            Self::NotEvidenced {
                ref named,
                ref missing,
            } => write!(
                formatter,
                "NotEvidenced: {} of the {} evidence references the claim named are absent",
                missing,
                named.len()
            ),
            Self::Empty => formatter.write_str("Empty: the proposal names nothing to do"),
        }
    }
}

impl std::error::Error for Refusal {}

// ── Outcome ──────────────────────────────────────────────────────────────────

/// What admitting an untrusted payload produced.
///
/// A sum type rather than `Result<Plan, Refusal>` with a side channel, because
/// the two questions a caller asks are different: *did it become work* and *if
/// not, what did it try*. This type answers both without the caller having to
/// keep the refusal and the plan side by side.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// The payload became work, and carries both the plan and the provenance it
    /// was admitted with.
    ///
    /// The two are one variant because a caller that kept the plan and dropped
    /// the provenance would have work it cannot attribute, and one that kept the
    /// provenance and dropped the plan has nothing to run.
    Admitted {
        /// What the payload asked for, after authorization.
        plan: Plan,
        /// Where the bytes came from.
        provenance: Provenance,
    },
    /// The payload did not become work, with the provenance of what it tried.
    Refused {
        /// Why, with the payload's own values attributed.
        refusal: Box<Refusal>,
        /// Where the bytes came from.
        provenance: Provenance,
    },
    /// The run reached a finite typed intervention instead of repairing again.
    ///
    /// Carries no provenance of its own: the intervention is a property of the
    /// run's ledger, and the ledger holds the provenance of the payload that
    /// produced each recorded failure.
    Intervention(Intervention),
}

impl Outcome {
    /// Whether the payload became work.
    #[must_use]
    pub const fn is_admitted(&self) -> bool {
        matches!(self, Self::Admitted { .. })
    }

    /// The admitted plan, when this outcome is one.
    #[must_use]
    pub const fn plan(&self) -> Option<&Plan> {
        match *self {
            Self::Admitted { ref plan, .. } => Some(plan),
            Self::Refused { .. } | Self::Intervention(_) => None,
        }
    }

    /// The refusal, when this outcome is one.
    #[must_use]
    pub fn refusal(&self) -> Option<&Refusal> {
        match *self {
            Self::Refused { ref refusal, .. } => Some(refusal.as_ref()),
            Self::Admitted { .. } | Self::Intervention(_) => None,
        }
    }

    /// The provenance, when this outcome admitted or refused a payload.
    #[must_use]
    pub const fn provenance(&self) -> Option<&Provenance> {
        match *self {
            Self::Admitted { ref provenance, .. } | Self::Refused { ref provenance, .. } => {
                Some(provenance)
            }
            Self::Intervention(_) => None,
        }
    }

    /// The intervention, when this outcome is one.
    #[must_use]
    pub const fn intervention(&self) -> Option<&Intervention> {
        match *self {
            Self::Intervention(ref intervention) => Some(intervention),
            Self::Admitted { .. } | Self::Refused { .. } => None,
        }
    }

    /// Wrap a decode failure with the provenance of the bytes that caused it.
    fn refused(provenance: Provenance, refusal: Refusal) -> Self {
        Self::Refused {
            refusal: Box::new(refusal),
            provenance,
        }
    }
}

impl fmt::Display for Outcome {
    /// The arm, then the plan or the refusal, then the provenance.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Admitted {
                ref plan,
                ref provenance,
            } => write!(formatter, "admitted {plan} (from {provenance})"),
            Self::Refused {
                ref refusal,
                ref provenance,
            } => write!(formatter, "{refusal} (from {provenance})"),
            Self::Intervention(ref intervention) => write!(formatter, "{intervention}"),
        }
    }
}

// ── PlanBudget ───────────────────────────────────────────────────────────────

/// How many times one step may have a proposal admitted for it.
///
/// The bounded-repair half of the rule. A decoder that admitted a thousand
/// proposals for one step would be a repair loop with a schema on the front, so
/// the budget is a declared ceiling that a caller tops up deliberately rather
/// than a number that resets when something changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlanBudget {
    /// Admissions left.
    remaining: u32,
    /// The ceiling this budget was opened with.
    ceiling: u32,
}

impl PlanBudget {
    /// Open a budget of at most `ceiling` admissions.
    #[must_use]
    pub const fn new(ceiling: u32) -> Self {
        Self {
            remaining: ceiling,
            ceiling,
        }
    }

    /// Admissions left.
    #[must_use]
    pub const fn remaining(&self) -> u32 {
        self.remaining
    }

    /// The ceiling this budget was opened with.
    #[must_use]
    pub const fn ceiling(&self) -> u32 {
        self.ceiling
    }

    /// Whether the budget is spent.
    #[must_use]
    pub const fn is_spent(&self) -> bool {
        self.remaining == 0
    }

    /// Charge one admission, or refuse it.
    ///
    /// # Errors
    ///
    /// [`Refusal::Limit`] naming the ceiling when the budget is spent. The
    /// budget is not decremented on refusal, so a caller that reads the refusal
    /// and stops sees the same number it started with.
    pub fn charge(&mut self) -> Result<(), Refusal> {
        match self.remaining.checked_sub(1) {
            Some(left) => {
                self.remaining = left;
                Ok(())
            }
            None => Err(Refusal::Limit {
                what: "the plan budget for this step",
                got: u64::from(self.ceiling).saturating_add(1),
                limit: u64::from(self.ceiling),
            }),
        }
    }

    /// Admit at most one proposal.
    ///
    /// The convenience door: the two halves of bounded repair in one call, so a
    /// caller cannot forget the budget because the decoder already refused the
    /// payload on other grounds and the retry path is the one that loops.
    ///
    /// # Errors
    ///
    /// [`Refusal::Limit`] when the budget is spent.
    pub fn admit_once(
        &mut self,
        decoder: &Decoder,
        surface: &Surface,
        payload: &[u8],
        source: Source,
    ) -> Result<Outcome, Refusal> {
        self.charge()?;
        Ok(decoder.decode(surface, payload, source))
    }
}

// ── Module-internal helpers ──────────────────────────────────────────────────

/// The digest of `bytes`, framed under `label` so two callers hashing different
/// things never collide.
fn framed_digest(label: &str, bytes: &[u8]) -> Digest {
    let mut hasher = Hasher::new();
    hasher.write_framed(label.as_bytes()).write_framed(bytes);
    hasher.finalize()
}
