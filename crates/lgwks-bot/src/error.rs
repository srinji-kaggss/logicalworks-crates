//! `error` owns the bot error vocabulary and enforces INV-BOT-ERROR-TYPED:
//! every failure is a distinct typed variant carrying the capability, field,
//! domain, or condition that caused it. Variants that surface untrusted
//! runtime text carry it as a `String` by design: the boundary is typed (which
//! domain failed is always known), while the foreign payload stays a `String`
//! so a structured consumer still has the original bytes. It is escaped where
//! it is *rendered*, by the `Escaped` adapter below, so a payload cannot forge
//! a log line or carry a live terminal control into whatever reads the file.
//! There is no bare-string failure with no typed envelope, and no arm that
//! interpolates a payload without that adapter.
//!
//! The vocabulary draws one distinction that a single failure variant cannot:
//! **whether the effect happened**. [`BotError::DomainError`] means the action
//! did not take effect, so a retry is a retry. [`BotError::EffectIndeterminate`]
//! means it may have, so a retry is a possible duplicate. They are separate
//! variants rather than one variant carrying a flag because a consumer's retry
//! decision has to be readable from the type: a caller that must parse a cause
//! string to learn whether it may retry will eventually get it wrong, and the
//! price of getting it wrong is a duplicated merge, message, or process launch.
//!
//! The links above resolve at the crate root, where `BotError` is re-exported,
//! rather than in this module — see `frontier.rs` for the module whose items
//! are *not* all re-exported and which therefore needs reference definitions.
//!
//! "Did not take effect" is necessary but not sufficient for a retry decision,
//! and the second half is what [`DispatchCertainty`] carries. A refusal that
//! happened before dispatch and a connection that never opened are both "the
//! effect did not happen", and they do not want the same answer: the first is
//! permanent, the second is a plain retry. Every `DomainError` producer states
//! which of the two it is, and [`BotError::retry_class`] is the single total map
//! from the vocabulary to [`RetryClass`] that a retry policy reads. No retry
//! decision in this crate inspects a rendered cause: a policy that turns on the
//! wording of a diagnostic message is a policy that changes when someone edits
//! the message.

// The rendering of every variant lives in `error/render.rs`, beside the
// `Escaped` adapter each untrusted field goes through, and the certainty map a
// retry policy reads lives in `error/certainty.rs`. This file is the
// vocabulary alone, so a new variant is reviewed here and its rendering and its
// retry answer are each one compile error away.
use std::time::Duration;

use super::broker::DispatchError;
use super::cap::Cap;
use super::cap::Deficit;
use super::ecs::{EffectEvidence, PendingWork, WorkId};
use super::effect::{ActionDigest, ActionId, AttemptId, EffectKey};
use super::session::{ResourceAxis, Terminal};

/// Error from bot construction, admission, or execution.
///
/// `#[non_exhaustive]`: the vocabulary is expected to grow as domains gain
/// failure modes, and a consumer matching on it must keep a wildcard arm so a
/// new variant is a compile-time prompt there rather than a silent fallthrough.
#[derive(Debug)]
#[non_exhaustive]
pub enum BotError {
    /// One or more required capabilities were not granted.
    ///
    /// Carries the **whole** shortfall rather than its first element. A check
    /// that reported one missing capability at a time made admission a loop in
    /// which each pass revealed one more word, so a requirement list of length
    /// `n` cost `n` round trips to discover and the repair could not be written
    /// until the last of them. [`Deficit`] is what the check already computed,
    /// reported without discarding the rest of it, and
    /// [`Deficit::to_grant_set`] turns it back
    /// into the grant set that closes it.
    CapabilityDenied {
        /// Every requirement the check found ungranted, with the domain that
        /// declared each one where the check site knew it.
        deficit: Deficit,
    },
    /// The bot spec is incomplete: a required field is missing (currently the
    /// name; an empty chain list is allowed).
    ///
    /// `cause` is the detail the producing site refused with. It is required
    /// rather than optional because the field name alone does not say *why* the
    /// value was missing: a threshold that does not parse and a chain list that
    /// is absent are both "missing target", and only the cause tells a caller
    /// which one it is looking at. Rendered through `Escaped` because it
    /// originates in the spec document.
    IncompleteSpec {
        /// What is missing.
        field: &'static str,
        /// Why the field is missing, in the producing site's own words.
        ///
        /// Required rather than optional because the two ways this is reached
        /// are different defects with different repairs: a field that was absent
        /// from a document, and a field whose translation failed. It is untrusted
        /// -- it names something from the spec document -- so it renders through
        /// the escaping wrapper.
        ///
        /// A site with no underlying error states its own reason rather than
        /// leaving this empty; there is no "unknown" here, because a caller
        /// holding this is being told what to repair.
        cause: String,
    },
    /// The spec names a domain this binary does not run.
    ///
    /// The registry is the list of domains a binary declares at compile time —
    /// see [`crate::DomainRegistry`] — so this is a mismatch between the
    /// document and the program it was handed to, not a runtime failure. The
    /// domain may exist and be registered elsewhere; it is simply not linked
    /// here, and no amount of retrying adds it. The identifier is carried as a
    /// `String` because it came from the document, and is escaped where it is
    /// rendered so a hostile spec cannot forge a log line with it.
    UnregisteredDomain {
        /// The identifier the spec named, spelled as the spec spelled it.
        domain: String,
    },
    /// A flow names domains its run's registry does not declare (#388).
    ///
    /// Every unknown identifier the flow writes, found in one pass at the
    /// flow's entry before any of its steps runs, so a flow short of three
    /// domains learns all three at once — the [`NeedSet`](crate::spec::NeedSet)
    /// shape every admission shortfall arrives in. Each need names the
    /// registry and the closest identifier it declares.
    UndeclaredDomains {
        /// One [`Need::UnknownDomain`](crate::spec::Need::UnknownDomain) per
        /// identifier, in source order.
        needs: crate::spec::NeedSet,
    },
    /// A flow's `act` names an action whose effect leaves the process (#388).
    ///
    /// `act` runs an action as a step and nothing more; it writes no effect
    /// journal, so an action that reaches a file, a socket or a queue would
    /// leave with no intent recorded before it and no way to tell after a
    /// crash whether it happened. Such an action belongs in a
    /// [`BotSpec`](crate::spec::BotSpec) chain, whose dispatch records intent
    /// before the effect leaves (the effect ledger). Refused before the
    /// action runs, so nothing left the process.
    UnjournaledEffect {
        /// The action identifier the flow named.
        domain: String,
    },
    /// The registry declares one identifier twice within a single role.
    ///
    /// A duplicate would make dispatch depend on declaration order: the first
    /// constructor would silently win and a spec would have no way to name the
    /// ambiguity. The registry therefore refuses to build anything until the
    /// lists are distinct — see [`crate::DomainRegistry::validate`]. An
    /// identifier may still appear once as a source and once as an action: a
    /// domain that observes a repository and also acts on one is one domain
    /// with two roles, not a collision.
    DuplicateDomain {
        /// The identifier declared twice, spelled as the declaration spelled it.
        domain: String,
        /// Which declaration list holds the pair: `"source"` or `"action"`.
        role: &'static str,
        /// The first declaration position, zero-based.
        first: usize,
        /// The second declaration position, zero-based.
        second: usize,
    },
    /// The serialized spec exceeds [`crate::spec::MAX_SPEC_BYTES`]. The bound is
    /// defensive: it stops a hostile or runaway manifest from allocating
    /// without limit before any validation runs.
    SpecTooLarge {
        /// Observed length in bytes.
        bytes: usize,
        /// The applied bound.
        limit: usize,
    },
    /// The serialized spec is not valid JSON for the schema.
    MalformedSpec {
        /// The parser's positional diagnostic, with control characters escaped
        /// so an untrusted field name cannot forge a log line.
        cause: String,
    },
    /// The serialized spec declares a version this binary does not implement.
    ///
    /// A document is a versioned contract, not a schema to guess at: a spec
    /// written for a later materializer may name fields with meanings this one
    /// does not have, so it is refused before any of it is interpreted rather
    /// than partially understood. The found and supported versions are carried
    /// as numbers so a caller can decide the repair without parsing the
    /// rendering.
    UnsupportedSpecVersion {
        /// The version the document declared.
        found: u32,
        /// The version this build implements.
        supported: u32,
    },
    /// A spec named a condition identifier outside the supported vocabulary.
    ///
    /// Conditions are the one part of a chain that carries parameters (a
    /// threshold), and the vocabulary is therefore closed and versioned with the
    /// type it evaluates rather than open to arbitrary wire text. An identifier
    /// this build does not know is refused before the chain is built, never
    /// treated as an always-true condition — a silently inert gate is how an
    /// effect a person expected to be guarded fires anyway.
    UnknownCondition {
        /// The condition identifier the spec spelled.
        condition: String,
        /// The parse failure the registry recorded for the identifier's
        /// argument, rendered as the `FromStr` error's own text.
        ///
        /// It is required because the identifier alone cannot distinguish the
        /// two ways this is reached: an identifier outside the vocabulary, and
        /// an identifier in it whose threshold argument does not parse. A
        /// caller repairing a spec needs to know which. Held as a `String` and
        /// rendered through `Escaped`, exactly as [`BotError::MalformedSpec`]
        /// holds its cause, so a payload that reaches it cannot forge a log
        /// line.
        argument_parse: String,
    },
    /// The synchronous adapter, [`Bot::tick`](crate::Bot::tick), was called on a
    /// thread an async runtime is already driving.
    ///
    /// This is a refusal rather than a failure of the work: the adapter parks
    /// the calling thread until its verb futures resolve, and the thread it was
    /// handed may be the one that owns the runtime's driver. Parked, that
    /// driver cannot advance a timer, report a socket ready, or run the sibling
    /// task a verb is waiting on, so the tick would never return. The repair is
    /// at the call site and there are two of them: await the tick from inside
    /// the runtime ([`Bot::tick_async`](crate::Bot::tick_async)), or move the
    /// call out of the runtime.
    ///
    /// Reported even when the runtime has several worker threads, where parking
    /// one of them may happen to work: which thread the caller was handed is
    /// not knowable from the adapter, and a deadlock that only appears on a
    /// current-thread runtime is exactly what this refuses to have.
    ///
    /// Reachable with the default `rt` feature. Without it there is no engine
    /// runtime in the build (`--no-default-features` withdraws tokio entirely)
    /// and nothing for the adapter to detect, so this variant is documented
    /// rather than constructed.
    TickInsideRuntime,
    /// A source poll was cancelled by the bot's declared per-poll deadline
    /// before it resolved.
    ///
    /// Not a [`Self::DomainError`] because the domain reported nothing: the
    /// substrate stopped waiting, so it knows the poll did not finish and
    /// nothing at all about why it was slow. Carries the budget the poll was
    /// actually given, so a caller triaging the stall is reading the number the
    /// bot declared rather than one the report had to invent.
    ///
    /// `NotDelivered`, so a consumer's retry classifier reads it as safe: a
    /// cancelled observation commits nothing and no effect was attempted on its
    /// account.
    PollStalled {
        /// The chain whose source stopped answering, in declaration order.
        chain: usize,
        /// The budget the poll was given, as the bot declared it.
        deadline: Duration,
    },
    /// A source's subject was still moving when it was read, so no reading of
    /// it could be confirmed.
    ///
    /// The torn read, as a typed outcome rather than as a value. A source that
    /// read a document while another writer was half-way through it has seen
    /// bytes no one ever held as a whole; reporting that as the current state
    /// makes the substrate commit a value and fire a chain against a document
    /// that does not exist. So the reading is refused and the observation is
    /// **pending**: the chain keeps the value it already holds and reads again
    /// on the next tick, which is the same discipline a failed poll follows and
    /// for the same reason — the value that was to be replaced is still there.
    ///
    /// Its own variant rather than a [`DomainError`](Self::DomainError) because
    /// the repair differs: nothing is wrong with the domain and no amount of
    /// retrying *this* call helps beyond the bound the read protocol already
    /// applied. A caller triaging a source that keeps reporting this is looking
    /// at a writer that never settles, which is a fact about the other side.
    ///
    /// `NotDelivered`, so a consumer's retry classifier reads it as safe: no
    /// effect was attempted and nothing was committed.
    UnstableObservation {
        /// The source that could not confirm a reading.
        domain: String,
        /// How many reads were taken, and which axis kept moving.
        unstable: crate::stability::Unstable,
    },
    /// The declared per-poll deadline was refused, because it would bound
    /// nothing.
    ///
    /// A zero budget cancels every poll before it is first polled, so a bot
    /// built with one observes nothing forever and reports every chain as
    /// stalled. That is a wiring mistake with a distinct repair, not a tuning
    /// choice, and it is refused at build rather than discovered on the first
    /// tick.
    PollDeadlineUnbounded {
        /// The zero that was asked for.
        deadline: Duration,
    },
    /// The declared per-poll deadline was refused, because it is above the
    /// ceiling this crate will grant.
    ///
    /// The ceiling exists because the deadline is the one bound standing between
    /// a source that stops answering and a bot that stops ticking. A budget past
    /// it converts a recoverable stall into a hang, and the caller who wants a
    /// longer one has to say why rather than discovering it at run time.
    PollDeadlineExceeded {
        /// The budget that was asked for.
        deadline: Duration,
        /// The largest budget that would have been accepted.
        ceiling: Duration,
    },
    /// The credential behind a proof expired, so the proof is worthless however
    /// much authority it still carries.
    ///
    /// The opposite of [`Self::CapabilityDenied`] rather than a variant of it: a
    /// deficit is closed by *granting*, and an expired credential is closed by
    /// **re-granting** — the capabilities are still held and now name nothing.
    /// A caller handed one arm for both would grant what it already has and be
    /// refused again, which is the admission loop `Deficit` exists to end, one
    /// layer out.
    ///
    /// Carries the capabilities and the two readings, so a caller can report how
    /// much life the credential had rather than only that it had none. `Refused`
    /// and so `RetryClass::Never`: presenting the same credential again produces
    /// the same refusal, and the repair is a new grant.
    CredentialExpired {
        /// Every capability the lapsed proof covered, in name order.
        capabilities: Vec<Cap>,
        /// The reading on the granting clock at which it expired.
        expired_at: Duration,
        /// The reading the check was made at.
        now: Duration,
    },
    /// A receiver refused the credential an adapter presented.
    ///
    /// Distinct from [`Self::CapabilityDenied`] because the authority *was*
    /// granted and the receiver rejected it anyway: its issuer cancelled,
    /// rotated or expired it upstream. The repair is carried in the error as a
    /// [`NeedSet`](crate::spec::NeedSet), so a caller reads what to re-grant
    /// rather than reconstructing it from a status code — and it is a `NeedSet`,
    /// not a string, so the whole shortfall arrives at once the way every other
    /// admission shortfall does.
    ///
    /// Never a retry. The same credential against the same receiver gives the
    /// same status, so this is the typed outcome that ends a refresh-then-retry
    /// loop rather than feeding one.
    CredentialRejected {
        /// The domain whose upstream refused the credential.
        domain: String,
        /// The status the receiver reported.
        status: u16,
        /// What a caller must re-grant for the call to be worth making again.
        needs: crate::spec::NeedSet,
    },
    /// Two editors wrote one record and this write lost (issue #278, row 5).
    ///
    /// The expected-version precondition an [`Execute`](crate::verb::Execute)
    /// writer of shared records carries: `expected` is the version the write
    /// saw, `found` is the version the store holds, and the two together are
    /// the whole fact a loser needs to re-read and try again. Recorded where
    /// the write was refused (the store's conflict log), never retried
    /// blindly: the same expectation against the same record gives the same
    /// refusal, so a retry without a re-read spends an attempt to learn
    /// nothing. `Refused` and so [`RetryClass::Never`].
    Conflict {
        /// The domain that refused the write (e.g. `"cas::store"`).
        domain: String,
        /// The record the write named.
        record: String,
        /// The version the write expected.
        expected: u64,
        /// The version the store holds.
        found: u64,
    },
    /// A domain action failed at runtime.
    ///
    /// Carries [`DispatchCertainty`] rather than leaving a consumer to infer
    /// the answer from the variant or from the cause. "It failed" is not the
    /// fact a retry decision turns on; "nothing reached the peer" and "the
    /// input was refused outright" are, and they are the same variant today
    /// without this field.
    DomainError {
        /// The domain that failed (e.g. `"gh::pr_status"`).
        domain: String,
        /// What the failure establishes about the effect. Required, so every
        /// producer states what it knows instead of defaulting to the
        /// retriable answer.
        certainty: DispatchCertainty,
        /// The underlying cause.
        cause: String,
    },
    /// A domain action failed *after* it may already have taken effect, so
    /// whether the effect occurred cannot be determined from the failure.
    ///
    /// The sibling of [`BotError::DomainError`], and the difference is the
    /// whole point: `DomainError` means the effect did not happen, so a retry
    /// is a retry; this means it may have happened, so a retry is a possible
    /// duplicate. A consumer that retries every error it is handed will
    /// duplicate a merge, a message, or a process launch here, and it can only
    /// avoid that if the two are distinguishable without reading the cause.
    ///
    /// Reached when a request timed out, or a connection dropped, *after* the
    /// request was sent. [`crate::verb::Execute`] is the only verb with an
    /// effect, so the variant names no verb: it is an effect by construction.
    EffectIndeterminate {
        /// The domain whose action may or may not have taken effect.
        domain: String,
        /// Why the outcome could not be settled.
        cause: String,
    },
    /// An evaluate condition failed structurally: not a false result, but an
    /// error in the condition itself.
    EvaluateError {
        /// What went wrong.
        cause: String,
    },
    /// A value at the erasure boundary was handed to a stage that expects a
    /// different type.
    ///
    /// A wiring defect in the chain, not a domain failure, and this variant
    /// exists so the two cannot be confused for one another. The action never
    /// ran; no domain was reached; there is nothing to retry. It used to be
    /// reported as a [`DomainError`](Self::DomainError), which read as "the
    /// domain failed" and — before certainty was carried — spent a whole retry
    /// budget on a defect no attempt could repair.
    ///
    /// The fields are all `&'static str` because this is a *constructive* error:
    /// it is caught where the concrete types are still in hand, so it can name
    /// them, and naming them is the whole diagnostic. It is never built from
    /// runtime text.
    TypeMismatch {
        /// Where it was caught, as a stable site name (`"spec::typed_entry"`,
        /// `"observe_fold rendezvous"`), so the loud report is also greppable.
        site: &'static str,
        /// The chain index, when the site knows which chain it was walking.
        chain: Option<usize>,
        /// The type the stage was built for.
        expected: &'static str,
        /// The type it was handed.
        observed: &'static str,
    },
    /// The serialized flow exceeds [`crate::session::MAX_FLOW_BYTES`].
    FlowTooLarge {
        /// Observed length in bytes.
        bytes: usize,
        /// The applied bound.
        limit: usize,
    },
    /// The serialized flow is not valid JSON for the flow schema.
    MalformedFlow {
        /// The parser's escaped diagnostic.
        cause: String,
    },
    /// A flow transition points at a node that is not in the node map.
    InvalidTransitionTarget {
        /// The node or field that owns the transition.
        from: String,
        /// The unresolved target node id.
        target: String,
    },
    /// An ask option is missing its route.
    MissingAskRoute {
        /// The ask node id.
        node: String,
        /// The option value without a route.
        option: String,
    },
    /// A declared variable has no ask node that writes it.
    VariableNeverWritten {
        /// The declared variable name.
        name: String,
    },
    /// A node reads a variable that no path reaching it has assigned yet.
    ///
    /// Distinct from [`BotError::VariableNeverWritten`], which is a writer
    /// that does not exist anywhere, and from [`BotError::VariableUnset`],
    /// which is the same condition found at run time. This one is the
    /// load-time defect where a writer exists but executes only *after* the
    /// read, or only on another branch: the document is accepted, and the
    /// session then fails on its first step with no provider involved.
    VariableReadBeforeInit {
        /// The node performing the read.
        node: String,
        /// The variable read before any reaching assignment.
        name: String,
    },
    /// A flow reads a variable that was not declared.
    UndeclaredVariable {
        /// The variable name that was read.
        name: String,
    },
    /// A node in the flow document cannot be reached from the entry node.
    UnreachableNode {
        /// The unreachable node id.
        node: String,
    },
    /// The number of flow nodes exceeds the declared execution budget.
    FlowBudgetExceeded {
        /// The number of declared nodes.
        steps: usize,
        /// The declared maximum.
        budget: usize,
    },
    /// A node kind in a JSON flow is outside the closed node language.
    UnknownNodeKind {
        /// The node carrying the unknown kind.
        node: String,
        /// The unknown kind label.
        kind: String,
    },
    /// A non-terminal node has no continuation.
    MissingTransition {
        /// The node without a continuation.
        node: String,
    },
    /// A flow template is syntactically invalid.
    MalformedTemplate {
        /// The node containing the template.
        node: String,
        /// The template field being checked.
        field: &'static str,
    },
    /// A declared variable is used with a value of the wrong type.
    VariableTypeMismatch {
        /// The variable name.
        name: String,
    },
    /// An ask node offers a candidate that the variable it writes cannot hold.
    ///
    /// A load-time verdict, not a runtime one. Every candidate and the
    /// variable's declared type are known once the document is parsed, so a
    /// flow whose only answers the variable cannot store is refused when the
    /// document is loaded: there is no answer the person could give that would
    /// repair it, and asking them is charging a conversation step for an
    /// authoring error.
    AskOptionNotAssignable {
        /// The ask node carrying the candidate.
        node: String,
        /// The variable the ask writes.
        variable: String,
        /// The candidate option string.
        option: String,
        /// Stable label of the declared type the variable holds.
        expected: &'static str,
        /// Why the candidate cannot be stored.
        cause: String,
    },
    /// A terminal declaration on a `handoff` or `refer` node contradicts the
    /// outcome that node's kind already carries.
    ///
    /// A load-time verdict, and a refusal rather than an override. The node
    /// names the target it hands to or refers to; a declared outcome naming a
    /// different target, or a refusal, is a second statement in the same
    /// document that cannot both be true. Accepting it and executing the node
    /// anyway is how a document that says "this handoff is not authorized"
    /// produces a permissive handoff classification, so the conflict is
    /// refused where it was authored. A declaration that *repeats* the node's
    /// outcome is accepted and is what the session executes.
    ConflictingTerminalDeclaration {
        /// The node carrying both outcomes.
        node: String,
        /// The outcome the node's kind implies.
        intrinsic: Terminal,
        /// The outcome the document declared for it.
        declared: Terminal,
    },
    /// One answer utterance is larger than the session may accept.
    ///
    /// Checked at the ingress, before the utterance reaches the resolver, the
    /// journal, or the retained transcript, so an oversized line costs one
    /// comparison rather than a copy of itself.
    UtteranceTooLarge {
        /// Bytes offered.
        bytes: usize,
        /// The applied utterance ceiling.
        limit: usize,
    },
    /// A value decoded from an accepted candidate is larger than the scope may
    /// retain.
    ValueTooLarge {
        /// The variable being written.
        variable: String,
        /// Bytes the stored value occupies when rendered.
        bytes: usize,
        /// The applied value ceiling.
        limit: usize,
    },
    /// An ask node offers a candidate whose stored value is larger than the
    /// session may retain.
    ///
    /// The load-time half of [`BotError::ValueTooLarge`], on the same argument
    /// as [`BotError::AskOptionNotAssignable`]: the candidate list and the
    /// applied ceiling are both known when the document loads, so an ask whose
    /// answer could never be stored is refused there rather than after the
    /// person has answered it.
    AskOptionTooLarge {
        /// The ask node carrying the candidate.
        node: String,
        /// The variable the ask writes.
        variable: String,
        /// The candidate option string.
        option: String,
        /// Bytes the stored value would occupy when rendered.
        bytes: usize,
        /// The applied value ceiling.
        limit: usize,
    },
    /// One rendered record payload is larger than the session may retain.
    RecordTooLarge {
        /// The node whose text this is.
        node: String,
        /// Bytes of the rendered payload.
        bytes: usize,
        /// The applied per-record ceiling.
        limit: usize,
    },
    /// A template's expansion would exceed the per-record ceiling.
    ///
    /// Refused before the expansion is allocated. A validated document is
    /// small; what it can *expand to* is not, because a placeholder repeats.
    /// The compiled template's expanded size is therefore computed with
    /// checked arithmetic and compared with the ceiling first, so an
    /// amplification that would materialise a multi-gigabyte string from a
    /// sub-megabyte document costs one checked addition per template part and
    /// allocates nothing.
    TemplateExpansionTooLarge {
        /// The node whose template this is.
        node: String,
        /// Bytes the expansion would occupy.
        bytes: usize,
        /// The applied per-record ceiling.
        limit: usize,
    },
    /// A session's retained records have reached its byte ceiling.
    ///
    /// The aggregate bound. The per-record ceiling bounds one write; this one
    /// bounds the sum, so a flow whose graph lets the cursor loop cannot retain
    /// an unbounded conversation out of individually acceptable records.
    SessionRetentionExceeded {
        /// Bytes that would be retained by the refused write.
        bytes: usize,
        /// The applied session ceiling.
        limit: usize,
    },
    /// A configured byte ceiling is larger than the operator's hard ceiling.
    ///
    /// Author- and operator-supplied bounds may tighten a ceiling and may not
    /// raise it: a document cannot grant itself a larger budget than the
    /// embedding application enforces. Refused rather than clamped, because a
    /// flow that asks for ten gigabytes and silently receives the shipped eight
    /// mebibytes has been neither obeyed nor corrected, and the next reader of
    /// that document would have to run it to find out which happened.
    ResourceLimitAboveCeiling {
        /// The byte axis the limit applies to.
        axis: ResourceAxis,
        /// The value the document or caller asked for.
        requested: usize,
        /// The operator's hard ceiling for that axis.
        ceiling: usize,
    },
    /// A predicate could not compare values of different types.
    PredicateTypeMismatch,
    /// A predicate or interpolation needs a variable that has no value yet.
    VariableUnset {
        /// The variable name.
        name: String,
    },
    /// An answer could not be converted to the declared variable type.
    InvalidVariableValue {
        /// The variable receiving the answer.
        variable: String,
        /// The answer that failed conversion.
        value: String,
        /// Why the conversion refused.
        ///
        /// Carried rather than dropped: "cannot be assigned to variable x" and
        /// "it is not a decimal integer" are different facts, and a caller that
        /// only sees the first has to guess which of the four reasons it was.
        reason: String,
    },
    /// A session has already reached a terminal outcome.
    SessionTerminated,
    /// The current session node is not an ask node.
    SessionNotAwaitingAnswer,
    /// A resolver returned an option index outside the candidate list.
    ResolverReturnedInvalidOption {
        /// The current ask node id.
        node: String,
    },
    /// The journal refused to record a decision receipt.
    ///
    /// The decision itself was reachable — the resolver produced a verdict and
    /// the flow had a transition for it — but the audit record for that decision
    /// was not accepted, so the answer was **not** applied. This variant exists
    /// because a receipt and a transition are one event: a session that moved
    /// while nothing recorded why would be a run whose transcript describes
    /// something the run never decided.
    ReceiptNotRecorded {
        /// The current ask node id.
        node: String,
        /// The journal's own refusal.
        cause: crate::session::JournalError,
    },
    /// A session attempted to execute more nodes than its flow budget allows.
    SessionBudgetExceeded {
        /// The number of attempted steps.
        steps: usize,
        /// The flow's declared maximum.
        budget: usize,
    },
    /// A tick finished with a transition still holding work.
    ///
    /// This is the outcome the ECS substrate used to express by *silence*: a
    /// tick that failed partway committed its revision, so an unchanged source
    /// never reached the entries the failure skipped, and a later clean tick was
    /// indistinguishable from a handled transition. The work is now retained
    /// and this variant is how its retention is reported — held for an attempt
    /// or for evidence, or given up on.
    PendingTransition {
        /// The entry that needs the caller: the first one still open, in
        /// `(chain, entry)` order.
        ///
        /// A tick that gave up on an entry reports the error that caused the
        /// abandonment instead, since that keeps the typed variant intact;
        /// [`crate::spec::Bot::pending`] is where abandoned entries are named.
        work: PendingWork,
        /// Entries across every chain that are still unresolved: open, or
        /// abandoned and still blocking the entries behind them.
        ///
        /// Abandoned entries are counted because they are counted by the scan
        /// that picked `work`: a count that omitted them could pair a named
        /// abandonment with `outstanding: 0`, which is the one reading this
        /// error exists to prevent — a chain that is somehow both stuck and
        /// finished.
        outstanding: usize,
    },
    /// `resolve_effect` was given an entry whose outcome is already decided.
    ///
    /// Not a no-op: a caller that believes it acknowledged an effect that
    /// nothing was holding has a wrong model of the world, and the error is
    /// where they find that out.
    ///
    /// Deliberately narrower than "your evidence was refused". A settlement can
    /// be refused because it names a generation that is no longer there, or
    /// because it contradicts one that is; those are
    /// [`EvidenceSuperseded`](Self::EvidenceSuperseded) and
    /// [`EvidenceContradicted`](Self::EvidenceContradicted). Keeping this
    /// variant meaning only "there is no held effect at this address" is what
    /// lets a caller tell a wrong address from stale or contradictory evidence,
    /// which are three different repairs.
    NoSuchWork {
        /// The address that matched no held entry.
        work: WorkId,
    },
    /// `resolve_effect` was given a key for an action this bot does not declare.
    ///
    /// Distinct from [`NoSuchWork`](Self::NoSuchWork), which is an action the
    /// bot *does* declare at an entry with nothing to settle. This one has no
    /// address to report at all, because the key's action does not appear in the
    /// declaration: the key came from somewhere other than
    /// [`Bot::pending`](crate::spec::Bot::pending) — another bot's journal, or a
    /// journal this run does not own — and no evidence about it can be applied
    /// here.
    ActionNotDeclared {
        /// The action the key named.
        action: ActionId,
    },
    /// `resolve_effect` named a generation that is not the one in the slot.
    ///
    /// The chain has been re-opened since the caller read
    /// [`PendingWork`], so the attempt the evidence is about is gone and a
    /// different one stands in its place. Accepting the report would apply it
    /// to work the caller never observed: `NotApplied` in particular would make
    /// an attempt nobody can account for eligible to run again. Nothing was
    /// changed, and re-reading the pending work gives the generation to report
    /// against.
    ///
    /// The two digests are the two bindings, not two counters: an
    /// [`ActionDigest`] is derived from the generation's revision together with
    /// the entry it belongs to, so comparing them is comparing which observed
    /// input the attempt was made against. That is the fact that decides
    /// whether evidence is about the same work, and it is why settlement does
    /// not have to be told the revision separately.
    EvidenceSuperseded {
        /// The address the evidence was submitted for.
        work: WorkId,
        /// The binding the caller's key names.
        named: ActionDigest,
        /// The binding the chain holds now.
        current: ActionDigest,
    },
    /// `resolve_effect` was given evidence about an attempt that is no longer
    /// the outstanding one on that entry.
    ///
    /// The generation the work was read from is still the live one — otherwise
    /// this would be [`EvidenceSuperseded`](Self::EvidenceSuperseded) — but the
    /// entry has been attempted again since, so the report is about work that
    /// is over. Nothing was changed.
    ///
    /// This is the one settlement refusal that is not a caller error at all. A
    /// caller that repeats a delivery it could not confirm is behaving exactly
    /// as it should, and the repeat is normally answered idempotently; it lands
    /// here only when the first delivery *did* take effect, the entry was
    /// attempted again, and the repeat then arrives after the new attempt
    /// began. Reporting success would be a lie about which attempt the evidence
    /// moved, and applying it would make an attempt whose effect may be live
    /// eligible to run a third time.
    EvidenceStaleAttempt {
        /// The address the evidence was submitted for.
        work: WorkId,
        /// The attempt the evidence was about.
        reported: AttemptId,
        /// The attempt the entry is on now.
        outstanding: AttemptId,
    },
    /// `resolve_effect` was given evidence opposite to what already settled the
    /// entry for this generation.
    ///
    /// Repeating the *same* evidence is not this error: it succeeds
    /// idempotently, so a caller whose first delivery was ambiguous can send it
    /// again without having to know whether it landed. This is the case where
    /// the entry's outcome is recorded and the new report says the other thing.
    EvidenceContradicted {
        /// The identity the evidence was submitted for.
        work: WorkId,
        /// The evidence already recorded for this generation.
        settled: EffectEvidence,
        /// The evidence that was refused.
        submitted: EffectEvidence,
    },
    /// A fresh attempt on an action was refused because the journal records an
    /// earlier attempt on it as dispatched with no outcome.
    ///
    /// This is the one refusal in this enum that is a *recovery* answer rather
    /// than a caller mistake: the bot was reconstructed against a journal, the
    /// journal says an attempt on this action was prepared and nothing recorded
    /// what became of it, and the substrate will not begin another one. Nothing
    /// established that the bytes did not arrive, and a blind resend of a
    /// non-idempotent effect is exactly the duplicate the durable path exists
    /// to prevent.
    ///
    /// The repair is [`Bot::resolve_effect`](crate::spec::Bot::resolve_effect)
    /// with the key from [`Bot::pending`](crate::spec::Bot::pending):
    /// [`NotApplied`](EffectEvidence::NotApplied) makes the entry eligible
    /// again, [`Applied`](EffectEvidence::Applied) records that the effect
    /// landed and the entry the key names is done for the generation the
    /// acknowledgement covers.
    EffectUnsettled {
        /// The action whose earlier attempt never settled.
        action: ActionId,
    },
    /// The broker or the journal refused a dispatch on its way out.
    ///
    /// Both halves arrive as one variant because they are refused at one
    /// instant — the handoff — and a caller's next move is the same for either:
    /// nothing left the process, and the tick reports it rather than retrying
    /// blind. [`DispatchError`] is where the two causes are told apart.
    EffectRefused {
        /// Which half refused, and why.
        cause: DispatchError,
    },
    /// The action ran and produced a known outcome; the journal refused to
    /// record it.
    ///
    /// Distinct from [`EffectRefused`](Self::EffectRefused), which is a
    /// *pre-dispatch* refusal: nothing left the process. This one is a
    /// *post-effect* recording failure. The occurrence is already established
    /// and travels on the error, so a controller never has to infer it. Only
    /// the journal append is retryable; the action is never re-entered for a
    /// recording failure, because re-entering it is how a known outcome becomes
    /// a duplicate.
    EffectUnrecorded {
        /// The exact attempt identity the append was for.
        ///
        /// Boxed because `EffectKey` is 128 bytes and an error carrying one by
        /// value makes every `Result` in the workspace pay for a case that is
        /// refused before it changes anything — the same reason
        /// `JournalError::OutOfOrder` boxes it.
        key: Box<EffectKey>,
        /// What the action did. `Applied` means the effect is live;
        /// `NotApplied` means it definitely is not.
        evidence: EffectEvidence,
        /// Why the append was refused.
        ///
        /// Boxed with the key so this variant does not make `BotError` itself
        /// large enough that every `Result<_, BotError>` in the workspace pays
        /// `result_large_err`.
        cause: Box<DispatchError>,
    },
}

// The certainty map and the rendering live in their own files; their items are
// re-exported so every path stays `crate::error::<Name>`.
mod certainty;
mod render;

pub use certainty::{DispatchCertainty, RetryClass};
pub(crate) use render::{Escaped, unbound_domain};

#[cfg(test)]
mod tests;
