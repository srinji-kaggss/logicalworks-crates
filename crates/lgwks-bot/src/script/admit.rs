//! The one step a task body uses to admit model or tool output.
//!
//! # Why this is a `script` block
//!
//! Every other untrusted-input path in this crate ends at a typed value the caller
//! has to interpret. This one ends at a [`FlowError`] located at a step path,
//! like every other failure a flow can have — because that is what a task body can
//! actually act on. A run that admitted a proposal and got a refusal must be able
//! to hand that refusal to `retry`, to [`Report`] and to a
//! log without translating it first, and a translation is where provenance
//! quietly gets dropped.
//!
//! So [`admit`] is a block, not a helper:
//!
//! - it **enters the step**, so a refusal reads `run/plan: UnknownOperation: ...`
//!   exactly like a timeout reads `run/plan: timed out after 30s`;
//! - it charges one **run-scoped** [`PlanBudget`] and records every refusal in one
//!   run-scoped [`RepairLedger`], so the fifth identical refusal across five calls
//!   in one run is a finite [`Intervention`] rather than five refusals a caller
//!   has to correlate;
//! - it returns the admitted [`Plan`], and turns a [`Refusal`] or an
//!   [`Intervention`] into its own typed [`FlowError`] arm — both keeping the
//!   [`Provenance`] — rather than into a string;
//! - and when the run is inside a [`Host::run`] with a store installed, it
//!   **records the refusal**, so a run resumed on a fresh host reads back what this
//!   run refused rather than re-deriving that nothing was ever refused.
//!
//! # What it is not
//!
//! It is not a fifth verb and not a plan interpreter. An admitted [`Plan`] is a
//! list of operation *names* the host registered; performing them is the caller's
//! job through the existing verbs, and nothing here performs anything.
//!
//! [`Host::run`]: crate::task::Host::run

use std::sync::{Arc, Mutex, MutexGuard};

use lgwks_std::wire::{Archive, Deserialize, Serialize};

use crate::proposal::{
    Decoder, Intervention, LedgerLimits, Outcome, Plan, PlanBudget, PlanLimits, Provenance,
    Refusal, RepairLedger, Source, Surface, payload_digest,
};

use super::run_store::{remember_at, step_definition};
use super::{FlowError, Scope};

/// The run-scoped state one run's admissions share.
///
/// A [`Gate`] is the bundle a task body needs and nothing else: what the decoder
/// charges against, what operations the host registered, how many admissions the
/// run has left, and what it has already failed at. The last two have to be
/// *shared*, which is why this is one struct behind a [`Mutex`] rather than four
/// arguments a caller has to keep in step.
///
/// Cloning shares, like [`Scope`]: a fan-out body that clones the gate charges the
/// same run, so a task body can hand it to [`each`](super::each) without the
/// budget becoming per-body. The lock is held across the synchronous charge,
/// decode and ledger update and **never across an `.await`**, so a fan-out of a
/// thousand bodies contending for one admission ceiling never parks on it.
#[derive(Clone)]
pub struct Gate {
    /// Everything one admission needs, behind one lock.
    inner: Arc<Mutex<Admission>>,
}

/// The state behind a [`Gate`].
struct Admission {
    /// The ceilings every decode is measured against.
    decoder: Decoder,
    /// What the host registered, and what the run holds.
    surface: Surface,
    /// Admissions left for the whole run.
    budget: PlanBudget,
    /// What this run has already failed at.
    ledger: RepairLedger,
}

impl Gate {
    /// Open a gate for a run on `tenant` whose surface is `surface`.
    ///
    /// `admissions` is the [`PlanBudget`] ceiling — how many proposals this run may
    /// have admitted for one step across *all* its calls — and `ledger` the
    /// [`RepairLedger`] ceilings. Both are named here rather than defaulted inside
    /// the step, because a repair ceiling a caller cannot read is a repair loop
    /// they cannot see coming.
    ///
    /// The ledger's provenance is this run's tenant, so every refusal it records is
    /// attributable to the tenant whose run recorded it.
    #[must_use]
    pub fn new(
        tenant: &str,
        surface: Surface,
        decoder: Decoder,
        admissions: PlanBudget,
        ledger: LedgerLimits,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Admission {
                decoder,
                surface,
                budget: admissions,
                ledger: RepairLedger::new(ledger, Provenance::of(Source::Model, tenant, b"")),
            })),
        }
    }

    /// The decoder's ceilings, so a reader can ask what its refusals are measured
    /// against without reaching past the lock.
    #[must_use]
    pub fn limits(&self) -> PlanLimits {
        self.lock().decoder.limits()
    }

    /// The surface's tenant, which is the tenant every admission is measured
    /// against.
    ///
    /// Returned by copy through the lock rather than borrowed: the surface lives
    /// behind the same mutex as the budget, so a reference would hold the lock
    /// across an `.await`.
    #[must_use]
    pub fn tenant(&self) -> String {
        self.lock().surface.tenant().to_owned()
    }

    /// How many admissions this run has left.
    #[must_use]
    pub fn remaining(&self) -> u32 {
        self.lock().budget.remaining()
    }

    /// How many failures this run has recorded, across every fingerprint.
    ///
    /// Monotone for the life of the gate, which is what makes "root spend" one
    /// number a caller can read at the end of a run.
    #[must_use]
    pub fn spent(&self) -> u64 {
        self.lock().ledger.spent()
    }

    /// How many times `fingerprint` has failed in this run.
    #[must_use]
    pub fn repetitions(&self, fingerprint: &str) -> u32 {
        self.lock().ledger.repetitions(fingerprint)
    }

    /// Whether this run has gathered something new under `fingerprint`.
    ///
    /// Moves no repetition count and does not clear what the fingerprint has
    /// already cost, so a run that gathers evidence cannot buy itself another pass
    /// at the failure it has already paid for.
    #[must_use]
    pub fn has_progress(&self, fingerprint: &str) -> bool {
        self.lock().ledger.has_progress(fingerprint)
    }

    /// Record that this run gathered something new under `fingerprint`.
    ///
    /// Moves no repetition count and does not clear what the fingerprint has
    /// already cost, so a run that gathers evidence cannot buy itself another pass
    /// at the failure it has already paid for.
    #[must_use]
    pub fn record_evidence(&self, fingerprint: &str) -> bool {
        self.lock().ledger.record_evidence(fingerprint)
    }

    /// The gate's lock, recovering a poison.
    ///
    /// Recovering is right for the reason it is right in `journal::owner`: every arm
    /// of the critical section returns its result rather than panicking, so a poison
    /// is a bug in an unrelated task, and propagating it would let one panicking
    /// body refuse every later admission in the run.
    fn lock(&self) -> MutexGuard<'_, Admission> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One admission's decision, before it becomes a [`FlowError`].
///
/// A three-armed type rather than `Result<Plan, Refusal>` because the third arm is
/// a different *fact* — the run reached a finite intervention — and flattening it
/// into a refusal would lose exactly what T29 asks to be observable.
enum Decision {
    /// The payload became work.
    Admitted(Plan),
    /// The payload did not, with what it tried and where it came from.
    Refused {
        /// Why it was refused.
        refusal: Refusal,
        /// Where the bytes came from.
        provenance: Provenance,
    },
    /// The run reached a finite typed intervention instead of repairing again.
    Intervention(Intervention),
}

/// Admit `payload` as a proposal for the `step` under `scope`, through `gate`.
///
/// The one way a task body crosses untrusted bytes into a run. On success the
/// [`Plan`] is the payload's own, with its [`Provenance`] beside it; on refusal the
/// [`FlowError`] is [`FlowError::Refused`] carrying the same [`Provenance`]; and
/// when this run has already failed the same way past its declared ceiling the
/// error is [`FlowError::Intervention`] instead.
///
/// The budget is charged once per call *before* the decode, so a payload refused
/// for its content still costs an admission: a run that retries by feeding a new
/// payload in is spending its repair ceiling either way, and a budget that only
/// charged successes would be one a refusal loop never reaches.
///
/// A refusal is recorded through the run store **before** it is returned, so a run
/// resumed on a fresh host reads back what this run refused. Without a store
/// nothing is recorded and nothing is claimed — the same honesty every other
/// durable step keeps.
///
/// # Errors
///
/// [`FlowError::Cancelled`] or [`FlowError::TooDeep`] when the step cannot be
/// entered; [`FlowError::Refused`] with the payload's [`Refusal`] and
/// [`Provenance`]; [`FlowError::Intervention`] when the run reached a finite typed
/// intervention; and [`FlowError`] when the refusal could not be recorded through
/// the run store.
pub async fn admit(
    scope: &Scope,
    step: &str,
    gate: &Gate,
    payload: &[u8],
    source: Source,
) -> Result<Plan, FlowError> {
    let here = scope.enter(step)?;
    match decide(gate, payload, source) {
        Decision::Admitted(plan) => Ok(plan),
        Decision::Refused {
            refusal,
            provenance,
        } => {
            // Recorded before answered: a refused admission is exactly the fact a
            // resumed run most needs and cannot re-derive, since the new instance
            // never saw the bytes. Recording first means a store refusal is what the
            // caller learns, rather than a silent success that left nothing behind.
            record_refusal(&here, payload, source, &refusal).await?;
            Err(FlowError::Refused {
                at: Arc::clone(here.shared_path()),
                refusal: Box::new(refusal),
                provenance,
            })
        }
        Decision::Intervention(intervention) => Err(FlowError::Intervention {
            at: Arc::clone(here.shared_path()),
            intervention: Box::new(intervention),
        }),
    }
}

/// Charge one admission and decode, or say why not.
///
/// The whole of the admission decision in one synchronous block, so the lock is
/// never held across an `.await` and the budget charge, the decode and the ledger
/// record cannot interleave with another body's.
fn decide(gate: &Gate, payload: &[u8], source: Source) -> Decision {
    let mut admission = gate.lock();
    if let Err(refusal) = admission.budget.charge() {
        // A spent budget is not a payload refusal and is not recorded against the
        // ledger: no payload was tried, so there is no unchanged failure to count
        // and counting one would let a caller that ignored the budget reach the
        // ledger ceiling instead of the plan ceiling.
        return Decision::Refused {
            refusal,
            provenance: Provenance::of(source, admission.surface.tenant(), payload),
        };
    }
    match admission
        .decoder
        .decode(&admission.surface, payload, source)
    {
        Outcome::Admitted { plan, .. } => Decision::Admitted(plan),
        Outcome::Refused {
            refusal,
            provenance,
        } => {
            // The ledger is charged with the *refusal arm*, not the payload's
            // digest: two different payloads refused for the same reason are one
            // unchanged failure, and the run that loops is looping on the reason,
            // not on the bytes. `record_failure` may itself return an
            // intervention — that is the moment this unchanged failure has now been
            // seen past the ceiling, and the intervention is the better fact to
            // report than this one refusal, because it ends the repair loop rather
            // than counting it.
            match admission.ledger.record_failure(refusal.label()) {
                Outcome::Intervention(intervention) => Decision::Intervention(intervention),
                _ => Decision::Refused {
                    refusal: *refusal,
                    provenance,
                },
            }
        }
        // A decode never produces an intervention — only the ledger does, and this
        // call has just recorded into it rather than read from it. Passed through
        // rather than fabricated, so the arm stays a fact even if a future decoder
        // learns to emit one.
        Outcome::Intervention(intervention) => Decision::Intervention(intervention),
    }
}

/// The sub-step a refusal is recorded under, relative to the step that refused.
///
/// A child key rather than the step's own, so a run that refuses twice and then
/// admits once keeps both facts: the value an admitted plan records and the
/// refusals that preceded it must not overwrite one another.
const REFUSAL_SUB_STEP: &str = "refusal";

/// Record a refusal through the run store, when this run has one.
///
/// A hand-built [`Scope`] and a [`Host`] in local mode both
/// record nothing and claim nothing, which is the same honesty every other durable
/// step keeps: no store means no run id, and no run id means nothing could be read
/// back.
async fn record_refusal(
    scope: &Scope,
    payload: &[u8],
    source: Source,
    refusal: &Refusal,
) -> Result<(), FlowError> {
    if super::run_store::installed().is_none() || scope.run().is_none() {
        return Ok(());
    }
    let record = refusal_record(scope.path(), refusal, payload, source);
    let run = scope.run();
    let child = scope.enter(REFUSAL_SUB_STEP)?;
    let definition = step_definition(scope, REFUSAL_SUB_STEP);
    // `remember_at` archives the record under the child's step key, durably, and
    // locates any store refusal at the child — which is the step the write belongs
    // to, so a ceiling the store refused names where it was refused.
    remember_at(&child, run, &definition, move || async move { Ok(record) })
        .await
        .map(|_| ())
}

/// A refusal recorded through the run store, in the archivable form a resumed run
/// reads back.
///
/// The [`Refusal`] itself is not archived: its arms carry `&'static str` and a
/// per-arm shape that would make the archive a second, drifting description of the
/// decoder's vocabulary. What survives a resume is the *fact* — which arm fired, at
/// which step, for which bytes, from which untrusted producer — and
/// [`Refusal::label`] names the arm exactly as the live value does, so a resumed run
/// reports the label the original did.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(crate = lgwks_std::wire::rkyv)]
pub struct RefusalRecord {
    /// The step path the refusal was located at.
    at: String,
    /// The refusal's arm label, as [`Refusal::label`] spells it.
    arm: String,
    /// The provenance digest of the exact bytes refused, in hex.
    digest: String,
    /// Which untrusted producer the bytes came from, as [`Source::label`] spells it.
    source: String,
}

impl RefusalRecord {
    /// The step path the refusal was located at.
    #[must_use]
    pub fn at(&self) -> &str {
        &self.at
    }

    /// The refusal's arm label, exactly as [`Refusal::label`] spells it.
    #[must_use]
    pub fn arm(&self) -> &str {
        &self.arm
    }

    /// The digest of the exact bytes refused, in hex.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Which untrusted producer the bytes came from, as [`Source::label`] spells it.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// The archivable record of one refusal.
///
/// Built once here so the shape is defined beside the step that writes it rather
/// than in a second place that could drift from it. `at` is the step path the
/// refusal was located at, so a resumed run can name the step a reader has to look
/// at even if it did not itself raise the refusal.
#[must_use]
pub fn refusal_record(
    at: &str,
    refusal: &Refusal,
    payload: &[u8],
    source: Source,
) -> RefusalRecord {
    RefusalRecord {
        at: at.to_owned(),
        arm: refusal.label().to_owned(),
        digest: payload_digest(payload).to_hex(),
        source: source.label().to_owned(),
    }
}

/// Read a recorded refusal back, for a run resuming on a fresh host.
///
/// The read side of [`refusal_record`]: the archived bytes decode into the same
/// four facts, so a run whose process refused and is gone can still say which arm
/// fired and for which bytes.
///
/// # Errors
///
/// [`FlowError::Failed`] when the bytes are not a complete refusal record — which
/// is what a truncated archive is, and what a decoder must never report as a
/// refusal that did not happen.
pub fn read_refusal(bytes: &[u8]) -> Result<RefusalRecord, FlowError> {
    <RefusalRecord as super::run_store::Durable>::from_record(bytes)
        .map_err(|cause| FlowError::failed(format!("the recorded value is not a refusal: {cause}")))
}

/// The refusal a [`FlowError::Refused`] carries, when it is one.
///
/// The read side of the arm [`admit`] writes, so a caller inspecting a
/// [`Report`](crate::task::Report) reads the same typed refusal the step produced
/// rather than parsing a message.
#[must_use]
pub fn refusal_of(error: &FlowError) -> Option<&Refusal> {
    match *error {
        FlowError::Refused { ref refusal, .. } => Some(refusal.as_ref()),
        _ => None,
    }
}

/// The provenance a [`FlowError::Refused`] carries, when it is one.
#[must_use]
pub fn provenance_of(error: &FlowError) -> Option<&Provenance> {
    match *error {
        FlowError::Refused { ref provenance, .. } => Some(provenance),
        _ => None,
    }
}

/// The intervention a [`FlowError::Intervention`] carries, when it is one.
#[must_use]
pub fn intervention_of(error: &FlowError) -> Option<&Intervention> {
    match *error {
        FlowError::Intervention {
            ref intervention, ..
        } => Some(intervention.as_ref()),
        _ => None,
    }
}
