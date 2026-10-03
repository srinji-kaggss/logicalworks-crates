//! Request-keyed durable submission.
//!
//! [`Host::submit`](crate::task::Host::submit) is the explicit durable door a
//! client uses when it wants an *idempotent request* rather than one run: the
//! caller supplies a [`RequestKey`], the host derives the run identity from the
//! tenant and the key, and the input's canonical [`InputDigest`] is recorded as
//! a durable receipt before any step runs. The three outcomes a caller can then
//! observe are distinct facts, not one success:
//!
//! - a first submission **executes** and records a terminal outcome;
//! - a repeat with the *same* key and the *same* input **reattaches** to the
//!   recorded outcome without re-running the body;
//! - a repeat with the same key and a *different* input is a typed
//!   [`RequestError::Conflict`] naming both digests.
//!
//! A key is an identity, not a lock: it names one logical request, and the
//! digest decides whether a later submission is the same request or a
//! contradiction of it. Distinct keys derive distinct run identities and are
//! never collapsed into one.
//!
//! # What outlives the client
//!
//! The durable *record* outlives the caller's waiter. If the client that
//! submitted a request drops its future while the body is in flight, the
//! receipt is already on the disk and no terminal outcome is; a later client
//! that submits the same key reattaches and receives [`Submission::InFlight`],
//! which reports the uncertainty (the effect of the in-flight step may be live)
//! separately from the cleanup handle (the run to settle by resuming it). The
//! host is the cleanup owner because the run store is its: a durable step's
//! record — and therefore what it takes to resume without repeating an effect —
//! is never the client's to hold.
//!
//! # Which outcomes are the request's own
//!
//! A request ends when *the request* ends: its body returned, its body refused,
//! or its declared deadline ran out. Those three are recorded under `@terminal`
//! and a later submission of the same key and input reattaches to them. The
//! host's own stops — [`Disposition::Cancelled`] after admission and
//! [`Disposition::Refused`] before it — are recorded under **no** terminal path,
//! because a key whose terminal record names a host stop is a key nothing can
//! ever finish: the key *is* the request's identity, its body runs at most once,
//! and the one fact that made the request resumable (that no terminal record
//! exists) is the very fact a stop recorded as the verdict destroys. So a stop
//! returns [`Submission::Executed`] for the call that saw it, leaves the
//! receipt, and leaves the request completable by the next submission or by a
//! [`Host::resume`](crate::task::Host::resume) under the run it named.
//!
//! Enforced by `tests/request_key.rs`
//! (`a_host_stop_mid_run_leaves_the_request_resumable`,
//! `a_refusal_before_admission_is_not_recorded_as_the_outcome`,
//! `a_failed_run_is_the_requests_recorded_outcome`) and
//! `tests/sim_request_key.rs` (`host_stops_never_poison_a_key_band_00..03`,
//! `same_seed_replays_host_stops_band_00..03`).
//!
//! # Boundary
//!
//! A dropped waiter is not drained in the background. The crate deliberately
//! provides no way to spawn a non-`Send` body onto a thread that outlives the
//! call, and the four verbs are non-`Send` by design; so a body whose caller
//! went away stops where it was. What survives is exactly the durable record —
//! which is the part that matters, because resuming the run is how the
//! un-recorded step is settled (INV-BOT-100, INV-BOT-101).

use std::fmt;
use std::num::NonZeroU128;
use std::sync::Arc;

use lgwks_std::hash::Hasher;
use lgwks_std::wire::{Archive, Deserialize, Serialize, WireError, from_bytes, to_bytes};

use crate::effect::{Id128, InputIdentity, RunId};
use crate::script::{Durable, FlowError};

use super::{Disposition, Report, StoreError};

/// The longest request key, in bytes.
pub const MAX_REQUEST_KEY_BYTES: usize = 128;

/// The reserved step path a request's input digest is recorded under.
///
/// The leading `@` is outside the identifier set a step name accepts, so a body
/// can never enter this path and collide with the receipt.
pub(crate) const BINDING_STEP: &str = "@request";

/// The reserved step path a request's terminal outcome is recorded under.
pub(crate) const TERMINAL_STEP: &str = "@terminal";

/// The domain-separation label mixed into a request-keyed run identity.
const RUN_DOMAIN: &[u8] = b"lgwks.bot.request-run.v1";

/// The caller-supplied identity of one logical request.
///
/// An idempotency key: the same key submitted twice names the same request, and
/// the recorded input digest decides whether the second submission is a repeat
/// or a contradiction. It is 1 to [`MAX_REQUEST_KEY_BYTES`] bytes of ASCII
/// letters, digits and `-`, `_`, `.`, `:`, so it is safe to log and to hash
/// without escaping.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RequestKey(Arc<str>);

impl RequestKey {
    /// Validate `key` and hold it.
    ///
    /// # Errors
    ///
    /// [`FlowError::InvalidRequestKey`] naming what is wrong with it.
    pub fn new(key: &str) -> Result<Self, FlowError> {
        use crate::script::scope::{NameFault, name_fault};
        let reason = match name_fault(key, MAX_REQUEST_KEY_BYTES) {
            None => return Ok(Self(Arc::from(key))),
            Some(NameFault::Empty) => "the request key is empty",
            Some(NameFault::TooLong) => "the request key is longer than MAX_REQUEST_KEY_BYTES",
            Some(NameFault::BadChar) => {
                "the request key may hold only ASCII letters, digits, '-', '_', '.' and ':'"
            }
        };
        Err(FlowError::InvalidRequestKey { reason })
    }

    /// The validated key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestKey {
    /// The key itself.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The canonical content identity of one submission's input.
///
/// Derived from the input through [`InputIdentity`], so the same value of the
/// same type hashes the same on every host and a changed value hashes
/// differently. It is the fact that decides whether a second submission under a
/// key is the same request or a [`RequestError::Conflict`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InputDigest([u8; 32]);

impl InputDigest {
    /// Wrap 32 digest bytes already computed.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The 32 digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex, 64 characters.
    #[must_use]
    pub fn to_hex(&self) -> String {
        lgwks_std::hex::encode(self.0)
    }

    /// The digest of `input`'s canonical identity.
    ///
    /// The input type's [`SCHEMA_ID`](InputIdentity::SCHEMA_ID) is framed in
    /// first, so two different wire schemas that happen to write the same bytes
    /// still hash differently — the binding is to the value *and* its declared
    /// schema, not to the bytes alone.
    #[must_use]
    pub fn of<I: InputIdentity>(input: &I) -> Self {
        let mut hasher = Hasher::new();
        hasher.write_framed(I::SCHEMA_ID);
        input.write_identity(&mut hasher);
        Self(*hasher.finalize().as_bytes())
    }
}

impl fmt::Display for InputDigest {
    /// The digest's hex spelling.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

/// What one [`Host::submit`](crate::task::Host::submit) call observed.
///
/// `#[non_exhaustive]`: the vocabulary grows as submission gains outcomes, and a
/// consumer matching on it keeps a wildcard arm so a new arm is a compile-time
/// prompt there rather than a silent fallthrough.
#[derive(Debug)]
#[non_exhaustive]
pub enum Submission<O> {
    /// The body ran now (or resumed under the derived run) and this is its
    /// report.
    Executed(Report<O>),
    /// The request had already reached a recorded terminal outcome under this
    /// key and the same input; this is the recorded report, returned without
    /// re-running the body.
    Reattached(Report<O>),
    /// The request has not reached a recorded terminal outcome under this key,
    /// so the recorded evidence is incomplete and an effect may be live: either a
    /// previous client submitted the key and went away before the run finished,
    /// or a host stopped the run and recorded no verdict of its own. This is the
    /// uncertainty and the handle to settle it, kept apart; see [`InFlight`].
    InFlight(InFlight),
}

impl<O> Submission<O> {
    /// The report, for the two arms that carry one.
    ///
    /// `None` for [`Submission::InFlight`], which has no report: it is a
    /// statement that there is not one yet.
    #[must_use]
    pub fn report(&self) -> Option<&Report<O>> {
        match *self {
            Self::Executed(ref report) | Self::Reattached(ref report) => Some(report),
            Self::InFlight(_) => None,
        }
    }

    /// The run identity this submission is about, for every arm.
    #[must_use]
    pub fn run_id(&self) -> Option<crate::effect::RunId> {
        match *self {
            Self::Executed(ref report) | Self::Reattached(ref report) => report.run_id(),
            Self::InFlight(ref in_flight) => Some(in_flight.run()),
        }
    }
}

/// A request whose client left before its run reached a recorded outcome.
///
/// The two facts a caller needs are kept apart rather than merged into one
/// status: the **uncertainty** is that an effect may be live and no terminal
/// record says otherwise, and the **cleanup handle** is [`InFlight::run`], the
/// run to settle by resuming it. The host owns the terminal record — it is the
/// store's — so the cleanup is not the client's to perform, only to trigger.
///
/// A host that *stopped* the run leaves exactly this state, because a stop is
/// not the request's verdict and is never recorded as one; so this arm reports
/// an interrupted run and an abandoned client identically, which is the
/// truth about the durable record both leave behind.
#[derive(Debug, Clone)]
pub struct InFlight {
    /// The run the request derived, and what a resume settles.
    run: crate::effect::RunId,
    /// The task the first client submitted, as a label.
    task: String,
    /// How many durable records the store holds for the run so far, receipt
    /// included. Below two means no terminal record exists.
    records: usize,
}

impl InFlight {
    /// Build the in-flight report.
    pub(crate) fn new(run: crate::effect::RunId, task: String, records: usize) -> Self {
        Self { run, task, records }
    }

    /// The run to resume to settle the in-flight step.
    #[must_use]
    pub const fn run(&self) -> crate::effect::RunId {
        self.run
    }

    /// The task the first client submitted.
    #[must_use]
    pub fn task(&self) -> &str {
        &self.task
    }

    /// Durable records the store holds for the run, receipt included.
    #[must_use]
    pub const fn records(&self) -> usize {
        self.records
    }
}

/// Two submissions under one key disagreed about their input.
///
/// Both digests are carried so a caller can tell a genuine contradiction (a
/// different payload) from a re-hash of the same one, without parsing a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestConflict {
    /// The key both submissions used.
    key: RequestKey,
    /// The digest the key was already bound to.
    existing: InputDigest,
    /// The digest the refused submission offered.
    requested: InputDigest,
}

impl RequestConflict {
    /// Build a conflict from the two digests.
    pub(crate) fn new(key: RequestKey, existing: InputDigest, requested: InputDigest) -> Self {
        Self {
            key,
            existing,
            requested,
        }
    }

    /// The request key both submissions used.
    #[must_use]
    pub fn key(&self) -> &RequestKey {
        &self.key
    }

    /// The digest already bound to the key.
    #[must_use]
    pub const fn existing(&self) -> InputDigest {
        self.existing
    }

    /// The digest the refused submission offered.
    #[must_use]
    pub const fn requested(&self) -> InputDigest {
        self.requested
    }
}

impl fmt::Display for RequestConflict {
    /// Names the key and both digests, so the refusal is actionable from the
    /// text alone.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "request key {:?} is already bound to input digest {} and cannot be rebound to {}",
            self.key.as_str(),
            self.existing.to_hex(),
            self.requested.to_hex()
        )
    }
}

impl std::error::Error for RequestConflict {}

/// Why a durable submission was refused.
#[derive(Debug)]
#[non_exhaustive]
pub enum RequestError {
    /// The host has no run store, so there is nowhere to record the receipt a
    /// durable submission is defined by. A host without a store cannot promise
    /// the semantics, so it refuses rather than downgrading to a plain run.
    NoStore,
    /// The request key was already submitted with a different input.
    Conflict(RequestConflict),
    /// The run store refused a read or a write.
    Store(StoreError),
    /// A reserved request record could not be read, written or decoded.
    Record(FlowError),
    /// The tenant and key hashed to the all-zero run identity, which is not a
    /// valid id. Vanishingly unlikely and typed rather than panicked, for the
    /// same reason every other refusal here is.
    IdCollision,
}

impl fmt::Display for RequestError {
    /// The refusal, naming the conflict's key and digests where there is one.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::NoStore => formatter.write_str(
                "this host has no run store, so a durable request has nowhere to record its \
                 receipt; install one with HostBuilder::run_store",
            ),
            Self::Conflict(ref conflict) => conflict.fmt(formatter),
            Self::Store(ref cause) => write!(formatter, "the run store refused: {cause}"),
            Self::Record(ref cause) => write!(formatter, "a request record was refused: {cause}"),
            Self::IdCollision => formatter.write_str(
                "the request key hashed to the all-zero run identity; use a different key",
            ),
        }
    }
}

impl std::error::Error for RequestError {
    /// The store's or the conflict's own error, when there is one.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Conflict(ref conflict) => Some(conflict),
            Self::Store(ref cause) => Some(cause),
            Self::Record(ref cause) => Some(cause),
            Self::NoStore | Self::IdCollision => None,
        }
    }
}

impl From<StoreError> for RequestError {
    /// A store refusal is a submission refusal.
    fn from(cause: StoreError) -> Self {
        Self::Store(cause)
    }
}

impl From<FlowError> for RequestError {
    /// A reserved record's own failure is a submission refusal.
    fn from(cause: FlowError) -> Self {
        Self::Record(cause)
    }
}

/// The terminal outcome of a request, as the store holds it.
///
/// One record so a reattach can reproduce the report without re-running the
/// body: a success carries the archived output, and a stop carries the
/// disposition and a located reason. It is the second reserved record a request
/// writes (after [`BINDING_STEP`]), and its presence is exactly the difference
/// between [`Submission::Reattached`] and [`Submission::InFlight`].
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(crate = lgwks_std::wire::rkyv, compare(PartialEq), derive(Debug))]
pub(crate) enum TerminalRecord {
    /// The body returned a value, archived as its own type's record bytes.
    Succeeded {
        /// The archived output.
        value: Vec<u8>,
    },
    /// The run stopped: the disposition code plus the location and reason.
    Stopped {
        /// The disposition, as a compact code (see `disposition_code`).
        disposition: u8,
        /// The step path the run stopped in, when the failure had one.
        at: String,
        /// The rendering of the failure.
        reason: String,
    },
}

/// The compact code for a [`Disposition`], and back.
///
/// A code rather than the enum value so the stored record carries no dependency
/// on the enum's wire form; the two functions are the one place the mapping
/// lives, and an unknown code is refused rather than defaulted.
const DISPOSITION_SUCCEEDED: u8 = 0;
/// See [`DISPOSITION_SUCCEEDED`].
const DISPOSITION_FAILED: u8 = 1;
/// See [`DISPOSITION_SUCCEEDED`].
const DISPOSITION_CANCELLED: u8 = 2;
/// See [`DISPOSITION_SUCCEEDED`].
const DISPOSITION_DEADLINE: u8 = 3;
/// See [`DISPOSITION_SUCCEEDED`].
const DISPOSITION_REFUSED: u8 = 4;

/// The code stored for `disposition`.
fn disposition_code(disposition: Disposition) -> u8 {
    match disposition {
        Disposition::Succeeded => DISPOSITION_SUCCEEDED,
        Disposition::Failed => DISPOSITION_FAILED,
        Disposition::Cancelled => DISPOSITION_CANCELLED,
        Disposition::DeadlineExceeded => DISPOSITION_DEADLINE,
        Disposition::Refused => DISPOSITION_REFUSED,
    }
}

/// The disposition a stored code names, or `None` for an unknown code.
fn disposition_from_code(code: u8) -> Option<Disposition> {
    match code {
        DISPOSITION_SUCCEEDED => Some(Disposition::Succeeded),
        DISPOSITION_FAILED => Some(Disposition::Failed),
        DISPOSITION_CANCELLED => Some(Disposition::Cancelled),
        DISPOSITION_DEADLINE => Some(Disposition::DeadlineExceeded),
        DISPOSITION_REFUSED => Some(Disposition::Refused),
        _ => None,
    }
}

impl TerminalRecord {
    /// The record for a run that succeeded with `output`.
    ///
    /// # Errors
    ///
    /// [`FlowError`] when the output cannot be archived.
    pub(crate) fn succeeded<O: Durable>(output: &O) -> Result<Self, FlowError> {
        let value = output.to_record().map_err(|cause| {
            FlowError::failed(format!(
                "the request's output could not be archived: {cause}"
            ))
        })?;
        Ok(Self::Succeeded { value })
    }

    /// The record for a run that stopped with a disposition, a location and a
    /// reason.
    pub(crate) fn stopped(disposition: Disposition, at: &str, reason: String) -> Self {
        Self::Stopped {
            disposition: disposition_code(disposition),
            at: at.to_owned(),
            reason,
        }
    }

    /// Encode for the store's reserved record.
    ///
    /// # Errors
    ///
    /// [`FlowError`] when the record cannot be archived.
    pub(crate) fn encode(&self) -> Result<Vec<u8>, FlowError> {
        to_bytes::<WireError>(self)
            .map(|bytes| bytes.as_ref().to_vec())
            .map_err(|cause| {
                FlowError::failed(format!(
                    "the request terminal record could not be archived: {cause}"
                ))
            })
    }

    /// Decode a stored reserved record.
    ///
    /// # Errors
    ///
    /// [`FlowError`] when the bytes are not a terminal record.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, FlowError> {
        from_bytes::<Self, WireError>(bytes).map_err(|cause| {
            FlowError::failed(format!(
                "the request terminal record could not be decoded: {cause}"
            ))
        })
    }

    /// The report parts this record reproduces: disposition, output and located
    /// error, each consistent with the other two.
    ///
    /// # Errors
    ///
    /// [`FlowError`] when a success's archived output is not the caller's type,
    /// or when a stop carries an unknown disposition code.
    pub(crate) fn parts<O: Durable>(
        self,
    ) -> Result<(Disposition, Option<O>, Option<FlowError>), FlowError> {
        match self {
            Self::Succeeded { value } => {
                let output = O::from_record(&value).map_err(|cause| {
                    FlowError::failed(format!(
                        "the value recorded for this request is not the type it returns: {cause}"
                    ))
                })?;
                Ok((Disposition::Succeeded, Some(output), None))
            }
            Self::Stopped {
                disposition,
                at,
                reason,
            } => {
                let disposition = disposition_from_code(disposition).ok_or_else(|| {
                    FlowError::failed(format!("unknown disposition code {disposition}"))
                })?;
                let error = FlowError::Failed {
                    at: Arc::from(at.as_str()),
                    reason,
                };
                Ok((disposition, None, Some(error)))
            }
        }
    }
}

/// The run identity a request key derives, for `tenant`.
///
/// Deterministic and content-addressed on the tenant and the key: the same
/// request on another host, another process or another day derives the same
/// run, which is what makes a durable receipt findable after a restart without a
/// second lookup table. The domain label is framed first so a request run cannot
/// collide with any other hash this crate takes over the same fields.
///
/// # Errors
///
/// [`RequestError::IdCollision`] when the 128-bit head is the all-zero identity.
pub(crate) fn derive_run(tenant: &str, key: &RequestKey) -> Result<RunId, RequestError> {
    let mut hasher = Hasher::new();
    hasher
        .write_framed(RUN_DOMAIN)
        .write_framed(tenant.as_bytes())
        .write_framed(key.as_str().as_bytes());
    let digest = hasher.finalize();
    let mut raw = [0u8; 16];
    match digest.as_bytes().get(..raw.len()) {
        Some(head) => raw.copy_from_slice(head),
        None => return Err(RequestError::IdCollision),
    }
    match NonZeroU128::new(u128::from_be_bytes(raw)) {
        Some(value) => Ok(RunId::new(Id128::from_nonzero(value))),
        None => Err(RequestError::IdCollision),
    }
}

/// The [`InputDigest`] a stored receipt holds.
///
/// # Errors
///
/// [`RequestError::Record`] when the reserved record is not the fixed 32 bytes a
/// digest is, which cannot happen for a record this crate wrote and is refused
/// rather than truncated.
pub(crate) fn digest_of_record(bytes: &[u8]) -> Result<InputDigest, RequestError> {
    let raw: [u8; 32] = bytes.try_into().map_err(|_| {
        RequestError::Record(FlowError::failed(
            "the request receipt is not a 32-byte input digest",
        ))
    })?;
    Ok(InputDigest::from_bytes(raw))
}
