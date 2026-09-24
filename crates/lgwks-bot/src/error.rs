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

use std::fmt;

use super::broker::DispatchError;
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
    /// [`Deficit::to_grant_set`](crate::Deficit::to_grant_set) turns it back
    /// into the grant set that closes it.
    CapabilityDenied {
        /// Every requirement the check found ungranted, with the domain that
        /// declared each one where the check site knew it.
        deficit: Deficit,
    },
    /// The bot spec is incomplete: a required field is missing (currently the
    /// name; an empty chain list is allowed).
    IncompleteSpec {
        /// What is missing.
        field: &'static str,
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

/// What a failed attempt establishes about the effect it was making.
///
/// The one fact a retry decision turns on, carried *by* the failure rather than
/// inferred from it. A consumer that has to read a cause string to learn
/// whether it may retry will eventually get it wrong, and the price of getting
/// it wrong is a duplicated merge, message, or process launch.
///
/// The arms are ordered by what they permit: [`Self::Refused`] permits nothing,
/// [`Self::NotDelivered`] permits a retry as a retry, and [`Self::Unsettled`]
/// permits a retry only against the caller's evidence.
///
/// The third case is deliberately its own variant rather than a
/// [`BotError::DomainError`] carrying [`Self::Unsettled`]: an effect that may
/// or may not have happened is [`BotError::EffectIndeterminate`], which is not
/// a domain error and must not become part of one — that is the catch-all this
/// vocabulary exists to prevent. No `DomainError` this crate constructs carries
/// `Unsettled`; the arm is here because it is the third answer to the question
/// this type asks, and a consumer reading the type should see all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DispatchCertainty {
    /// Nothing was dispatched and nothing ever will be: the failure refused the
    /// input itself — a parse or schema refusal, a target that cannot be
    /// resolved, an action that will not accept the value it was handed, a
    /// schedule that will not validate.
    ///
    /// Permanent, and before dispatch. The same input produces the same
    /// refusal on every attempt, so a retry spends an attempt to learn an
    /// answer that was already known.
    Refused,
    /// The attempt was made and nothing reached the peer: no bytes could have
    /// reached a previous or live connection for this attempt, so the effect
    /// definitely did not happen.
    ///
    /// A retry is a retry, bounded by the caller's declared budget. This is the
    /// arm a connection that was never established, or a local read that failed
    /// before it produced anything, belongs to.
    NotDelivered,
    /// The attempt reached the peer, and whether it took effect is not knowable
    /// from the failure. A retry is a possible duplicate.
    Unsettled,
    /// The effect definitely happened.
    ///
    /// Produced only by a post-effect recording failure
    /// ([`BotError::EffectUnrecorded`]) whose evidence is
    /// [`Applied`](EffectEvidence::Applied): the action returned success, so the
    /// occurrence is a fact even though the journal has no record of it yet.
    /// A retry of the *effect* is forbidden — that is how a known success
    /// becomes a duplicate. Retrying the *append* is the repair, and it does not
    /// go through this type's retry map.
    Occurred,
}

impl DispatchCertainty {
    /// What this certainty permits a caller to do about the failure.
    #[must_use]
    pub const fn retry_class(self) -> RetryClass {
        match self {
            Self::Refused | Self::Occurred => RetryClass::Never,
            Self::NotDelivered => RetryClass::Safe,
            Self::Unsettled => RetryClass::RequiresEvidence,
        }
    }
}

/// What a failure permits a caller to do about it.
///
/// Read from the failure through [`BotError::retry_class`], which is total over
/// the vocabulary and never inspects a rendered cause. A caller that branches
/// on this is not choosing a retry policy by string-matching, which is the
/// thing the type exists to make unnecessary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RetryClass {
    /// No retry can change the answer: the failure is a refusal or a wiring
    /// violation, and repeating it produces the same refusal at the cost of an
    /// attempt.
    Never,
    /// The effect definitely did not happen, so a retry is a retry. Bounded by
    /// the caller's budget, which is what [`crate::spec::RetryPolicy`] declares.
    Safe,
    /// The effect may have happened, so a retry is a possible duplicate. Not
    /// attempted again without the caller reporting evidence.
    RequiresEvidence,
}

impl BotError {
    /// What this failure establishes about the effect it was making.
    ///
    /// Exhaustive over the vocabulary and computed from the value, never from
    /// the rendered cause. Every variant that is not a
    /// [`DomainError`](Self::DomainError) or an
    /// [`EffectIndeterminate`](Self::EffectIndeterminate) names a refusal that
    /// happens *before* an effect is attempted — a capability that is not
    /// granted, an erased verb handed the wrong type, a spec that does not
    /// validate, a tick that may not park the thread it was called on — so all
    /// of them answer [`DispatchCertainty::Refused`].
    #[must_use]
    pub fn dispatch_certainty(&self) -> DispatchCertainty {
        match *self {
            Self::DomainError { certainty, .. } => certainty,
            Self::EffectIndeterminate { .. } => DispatchCertainty::Unsettled,
            // The action ran and the outcome is a fact. Refused means "nothing
            // left the process", and NotDelivered means "a retry is a retry" —
            // both are lies about an effect that already returned.
            Self::EffectUnrecorded { evidence, .. } => match evidence {
                EffectEvidence::Applied => DispatchCertainty::Occurred,
                EffectEvidence::NotApplied => DispatchCertainty::NotDelivered,
            },
            _ => DispatchCertainty::Refused,
        }
    }

    /// What this failure permits a caller to do about it.
    #[must_use]
    pub fn retry_class(&self) -> RetryClass {
        // A recording failure never re-enters the action, whatever the outcome
        // was: the append is the only thing left to do, and it is not an effect
        // attempt. `Never` here means "never dispatch again for this failure",
        // not "abandon the entry" — `failure_state` maps this variant to a
        // pending-record hold rather than to terminal abandonment.
        match *self {
            Self::EffectUnrecorded { .. } => RetryClass::Never,
            _ => self.dispatch_certainty().retry_class(),
        }
    }
}

/// `Deficit`'s rendering lives here rather than in `cap.rs`, and the reason is
/// the escaping contract below rather than the module it describes: every
/// untrusted field a deficit carries is a capability name or a domain id, both
/// of which are `String`s a caller supplied, so both go through `Escaped`, the
/// private helper below. It is named here rather than linked because it is not
/// reachable from the public docs, and a link a reader cannot follow is worse
/// than the plain name.
/// Putting the arm next to `Escaped` is what keeps a later field from being
/// interpolated without it.
impl fmt::Display for Deficit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.len();
        // The count comes first because it is the number a person triaging
        // wants: "one thing is missing" and "nine things are missing" call for
        // different next actions, and the list alone does not say which.
        write!(f, "{count} ungranted")?;
        for (index, shortage) in self.shortages().enumerate() {
            f.write_str(if index == 0 { ": " } else { ", " })?;
            write!(f, "{}", Escaped(shortage.required().as_str()))?;
            if let Some(demand) = shortage.demand() {
                write!(f, " (required by {})", Escaped(demand.domain()))?;
            }
        }
        Ok(())
    }
}

impl From<Deficit> for BotError {
    fn from(deficit: Deficit) -> Self {
        Self::CapabilityDenied { deficit }
    }
}

/// Render untrusted text so it cannot forge a log record.
///
/// Control characters are the whole of the problem: a newline ends the record,
/// and an ESC sequence executes in whatever terminal later reads it. Both are
/// rendered as escapes, so the payload is still complete and still readable,
/// but it cannot add a physical line or reach the terminal.
///
/// Backslash is deliberately *not* escaped, for two reasons. It cannot forge
/// anything on its own, and leaving it alone makes this pass idempotent with
/// the constructor-level escaping that `MalformedSpec` and `MalformedFlow`
/// already perform: that escaping turns a real newline into the two characters
/// `\` and `n`, which this pass then leaves exactly as it found them. Escaping
/// backslash would render them `\\n`, and each additional pass would add
/// another. The cost is a presentational ambiguity — a payload holding a
/// literal backslash-then-n reads the same as an escaped newline — and that is
/// a triage nuisance rather than a forgery, which makes it the cheaper of the
/// two prices.
///
/// Applied at the rendering site rather than in the enum, so the structured
/// payload a consumer matches on keeps the original bytes.
///
/// `pub(crate)` because it is one implementation, not one per module: `ecs`
/// renders the cause of a held or abandoned entry through the same adapter
/// rather than with an escaping pass of its own.
pub(crate) struct Escaped<'a>(pub(crate) &'a str);

impl fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for glyph in self.0.chars() {
            match glyph {
                '\n' => f.write_str("\\n")?,
                '\r' => f.write_str("\\r")?,
                '\t' => f.write_str("\\t")?,
                control if control.is_control() => {
                    write!(f, "\\u{{{:x}}}", u32::from(control))?;
                }
                plain => {
                    let mut buffer = [0_u8; 4];
                    f.write_str(plain.encode_utf8(&mut buffer))?;
                }
            }
        }
        Ok(())
    }
}

/// Render one terminal outcome inside a diagnostic.
///
/// The two payload-bearing variants carry untrusted text — a handoff target and
/// a refusal reason can both come from the document — so both go through
/// `Debug`, which is Rust's own escaping formatter and therefore refuses to
/// emit a live control character. `Escaped` is not reusable here for the same
/// reason the `Debug` sites above do not use it: the wrapper would have to be
/// re-applied around each field separately, and one forgotten wrapper is a
/// forged log line.
fn write_terminal(formatter: &mut fmt::Formatter<'_>, terminal: &Terminal) -> fmt::Result {
    // No wildcard arm: `Terminal` is `#[non_exhaustive]` for *downstream*
    // consumers, and inside this crate the compiler knows the full set, so a
    // new variant is a compile error here — which is the prompt wanted, since
    // a diagnostic that cannot name an outcome should not be written by
    // accident.
    match *terminal {
        Terminal::Completed => formatter.write_str("completed"),
        Terminal::Referred { ref target } => write!(formatter, "referred to {target:?}"),
        Terminal::HandedOff { ref target } => write!(formatter, "handed off to {target:?}"),
        Terminal::Refused { ref reason } => write!(formatter, "refused because {reason:?}"),
    }
}

impl fmt::Display for BotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The scrutinee is `*self` so each pattern's type is the enum's own
        // type rather than a reference to it, and non-`Copy` payloads are bound
        // by `ref`. `field` is `&'static str` and the two lengths are `usize`,
        // so those bind by copy from behind the deref.
        match *self {
            // The deficit renders itself; every untrusted field it holds is
            // wrapped there, for the reason `Deficit`'s `Display` states.
            Self::CapabilityDenied { ref deficit } => {
                write!(f, "capability denied: {deficit}")
            }
            Self::IncompleteSpec { field } => {
                write!(f, "incomplete bot spec: missing {field}")
            }
            Self::UnregisteredDomain { ref domain } => {
                write!(f, "unregistered domain: {}", Escaped(domain))
            }
            Self::DuplicateDomain {
                ref domain,
                role,
                first,
                second,
            } => write!(
                f,
                "duplicate {role} domain {} declared at positions {first} and {second}",
                Escaped(domain)
            ),
            Self::SpecTooLarge { bytes, limit } => {
                write!(f, "bot spec is {bytes} bytes, over the {limit}-byte limit")
            }
            Self::MalformedSpec { ref cause } => {
                write!(f, "malformed bot spec: {}", Escaped(cause))
            }
            Self::TickInsideRuntime => f.write_str(
                "tick: the synchronous adapter cannot park a thread an async runtime is driving; \
                 await `tick_async` on that runtime, or call the tick outside it",
            ),
            Self::DomainError {
                ref domain,
                certainty: _,
                ref cause,
            } => {
                write!(f, "{}: {}", Escaped(domain), Escaped(cause))
            }
            Self::EffectIndeterminate {
                ref domain,
                ref cause,
            } => {
                // Names the indeterminacy, not just the domain: an operator
                // reading this line has to know the effect may be live, because
                // that is what makes the retry unsafe.
                write!(
                    f,
                    "{}: may have taken effect — {}",
                    Escaped(domain),
                    Escaped(cause)
                )
            }
            Self::EvaluateError { ref cause } => {
                write!(f, "evaluate: {}", Escaped(cause))
            }
            Self::TypeMismatch {
                site,
                chain,
                expected,
                observed,
            } => match chain {
                Some(index) => write!(
                    f,
                    "{site}: type mismatch on chain {index} — expected {expected}, got {observed}"
                ),
                None => write!(
                    f,
                    "{site}: type mismatch — expected {expected}, got {observed}"
                ),
            },
            Self::FlowTooLarge { bytes, limit } => {
                write!(f, "flow is {bytes} bytes, over the {limit}-byte limit")
            }
            Self::MalformedFlow { ref cause } => {
                write!(f, "malformed flow: {}", Escaped(cause))
            }
            Self::InvalidTransitionTarget {
                ref from,
                ref target,
            } => write!(
                f,
                "flow transition from {} targets missing node {}",
                Escaped(from),
                Escaped(target)
            ),
            Self::MissingAskRoute {
                ref node,
                ref option,
            } => write!(
                f,
                "ask node {} has no route for option {}",
                Escaped(node),
                Escaped(option)
            ),
            Self::VariableNeverWritten { ref name } => {
                write!(f, "declared variable {} is never written", Escaped(name))
            }
            Self::VariableReadBeforeInit { ref node, ref name } => write!(
                f,
                "flow node {} reads variable {} before any reaching assignment",
                Escaped(node),
                Escaped(name)
            ),
            Self::UndeclaredVariable { ref name } => {
                write!(f, "flow reads undeclared variable {}", Escaped(name))
            }
            Self::UnreachableNode { ref node } => {
                write!(f, "flow node {} is unreachable", Escaped(node))
            }
            Self::FlowBudgetExceeded { steps, budget } => {
                write!(
                    f,
                    "flow declares {steps} nodes over its {budget}-step budget"
                )
            }
            Self::UnknownNodeKind { ref node, ref kind } => {
                write!(
                    f,
                    "flow node {} has unknown kind {}",
                    Escaped(node),
                    Escaped(kind)
                )
            }
            Self::MissingTransition { ref node } => {
                write!(f, "flow node {} has no continuation", Escaped(node))
            }
            // `field` is `&'static str` from this crate, so it is not a payload.
            Self::MalformedTemplate { ref node, field } => {
                write!(
                    f,
                    "flow node {} has malformed {field} template",
                    Escaped(node)
                )
            }
            Self::VariableTypeMismatch { ref name } => {
                write!(f, "value for variable {} has the wrong type", Escaped(name))
            }
            // `option` renders through `Debug`, which escapes control characters
            // itself; the identifiers are wrapped, and `expected` is a
            // crate-controlled label rather than a payload.
            Self::AskOptionNotAssignable {
                ref node,
                ref variable,
                ref option,
                expected,
                ref cause,
            } => write!(
                f,
                "ask node {} offers option {option:?} for {expected} variable {}, \
                 which cannot store it: {}",
                Escaped(node),
                Escaped(variable),
                Escaped(cause)
            ),
            // Both outcomes render through `write_terminal`, which escapes the
            // two untrusted fields a `Terminal` can carry.
            Self::ConflictingTerminalDeclaration {
                ref node,
                ref intrinsic,
                ref declared,
            } => {
                write!(f, "terminal declaration on node {} ", Escaped(node))?;
                f.write_str("contradicts the outcome its kind carries: ")?;
                write_terminal(f, intrinsic)?;
                f.write_str(" declared as ")?;
                write_terminal(f, declared)
            }
            Self::UtteranceTooLarge { bytes, limit } => write!(
                f,
                "answer utterance of {bytes} bytes exceeds the {limit}-byte utterance limit"
            ),
            Self::ValueTooLarge {
                ref variable,
                bytes,
                limit,
            } => write!(
                f,
                "value for variable {} occupies {bytes} bytes, over the {limit}-byte value limit",
                Escaped(variable)
            ),
            // `option` renders through `Debug` and escapes itself; the two
            // identifiers are the payloads that do not.
            Self::AskOptionTooLarge {
                ref node,
                ref variable,
                ref option,
                bytes,
                limit,
            } => write!(
                f,
                "ask node {} offers option {option:?} storing {bytes} bytes for variable {}, \
                 over the {limit}-byte value limit",
                Escaped(node),
                Escaped(variable)
            ),
            Self::RecordTooLarge {
                ref node,
                bytes,
                limit,
            } => write!(
                f,
                "record for node {} is {bytes} bytes, over the {limit}-byte record limit",
                Escaped(node)
            ),
            Self::TemplateExpansionTooLarge {
                ref node,
                bytes,
                limit,
            } => write!(
                f,
                "template on node {} would expand to {bytes} bytes, over the {limit}-byte \
                 record limit",
                Escaped(node)
            ),
            Self::SessionRetentionExceeded { bytes, limit } => write!(
                f,
                "session would retain {bytes} bytes, over the {limit}-byte session limit"
            ),
            // `axis` is a crate-controlled label, not a payload.
            Self::ResourceLimitAboveCeiling {
                axis,
                requested,
                ceiling,
            } => write!(
                f,
                "{} limit of {requested} bytes exceeds the operator's {ceiling}-byte ceiling",
                axis.label()
            ),
            Self::PredicateTypeMismatch => f.write_str("predicate values have incompatible types"),
            Self::VariableUnset { ref name } => {
                write!(f, "variable {} has no value", Escaped(name))
            }
            // `value` renders through `Debug`, which escapes control characters
            // itself; the variable name does not, so only that one is wrapped.
            Self::InvalidVariableValue {
                ref variable,
                ref value,
            } => write!(
                f,
                "value {value:?} cannot be assigned to variable {}",
                Escaped(variable)
            ),
            Self::SessionTerminated => f.write_str("session has already terminated"),
            Self::SessionNotAwaitingAnswer => f.write_str("session is not awaiting an answer"),
            Self::ResolverReturnedInvalidOption { ref node } => {
                write!(
                    f,
                    "resolver returned an invalid option for node {}",
                    Escaped(node)
                )
            }
            Self::ReceiptNotRecorded {
                ref node,
                ref cause,
            } => {
                write!(
                    f,
                    "decision at node {} was not recorded: {}",
                    Escaped(node),
                    Escaped(&cause.to_string())
                )
            }
            Self::SessionBudgetExceeded { steps, budget } => {
                write!(
                    f,
                    "session attempted {steps} steps over its {budget}-step budget"
                )
            }
            // The payload's own `Display` renders the cause through `Escaped`,
            // at the point the untrusted bytes are reachable, so this arm wraps
            // the whole report only to keep the boundary uniform.
            Self::PendingTransition {
                ref work,
                outstanding,
            } => write!(
                f,
                "tick left work unfinished: {} ({outstanding} open)",
                Escaped(&work.to_string())
            ),
            Self::NoSuchWork { work } => write!(
                f,
                "no held effect at chain {} entry {}: only an entry whose outcome is \
                 indeterminate, or one that was given up on, can be settled with evidence",
                work.chain(),
                work.entry()
            ),
            Self::ActionNotDeclared { action } => write!(
                f,
                "action {action} is not declared by this bot: nothing was changed, and \
                 the key did not come from this bot's pending()"
            ),
            // The generation the caller named is rendered first and the one in
            // the slot second, in the order the mistake happened: a reader
            // checking this against their own ledger wants to see their number
            // where they left it, next to the one that replaced it.
            Self::EvidenceSuperseded {
                work,
                named,
                current,
            } => write!(
                f,
                "evidence names binding {named} of chain {} entry {}, which has \
                 been superseded by binding {current}: nothing was changed, re-read \
                 pending() and report the key it hands back now",
                work.chain(),
                work.entry()
            ),
            Self::EvidenceStaleAttempt {
                work,
                reported,
                outstanding,
            } => write!(
                f,
                "evidence was about attempt {} of chain {} entry {}, but that \
                 entry is on attempt {} now: nothing was changed, and the \
                 report is about an attempt that is over rather than one outstanding",
                reported.get(),
                work.chain(),
                work.entry(),
                outstanding.get()
            ),
            Self::EvidenceContradicted {
                work,
                settled,
                submitted,
            } => write!(
                f,
                "evidence {} contradicts {settled} already recorded for chain {} entry \
                 {}: nothing was changed, and repeating the same evidence would have \
                 succeeded",
                submitted,
                work.chain(),
                work.entry()
            ),
            Self::EffectUnsettled { action } => write!(
                f,
                "no attempt on action {action} was begun: the journal records an \
                 earlier attempt on it as dispatched with no outcome, so it may \
                 already be live. Settle it with the key pending() hands back \
                 before anything is sent again"
            ),
            // Matched by reference so the cause renders through its own
            // `Display`: the refusal is read once, here, and the variant keeps
            // the whole of it for a caller that wants to tell the broker's
            // refusal from the journal's.
            Self::EffectRefused { ref cause } => {
                write!(f, "nothing left the process: {cause}")
            }
            Self::EffectUnrecorded {
                key: _,
                evidence,
                ref cause,
            } => {
                write!(
                    f,
                    "the effect is {evidence} and only its record is missing: {cause}. \
                     Retry the journal append, not the action"
                )
            }
        }
    }
}

impl std::error::Error for BotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // All causes are data (`Cap`, `&'static str`, escaped `String`), never
        // a wrapped error: there is no deeper source to forward. String
        // causes are intentional here (see the module header), not a missing
        // `#[from]` impl.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{BotError, DispatchCertainty, Escaped, RetryClass, Terminal};
    use crate::cap::{Cap, Deficit, Demand, Shortage};

    /// The retry classifier a consumer writes, and the entire reason the two
    /// variants exist: it reads the *variant*, never the cause string.
    fn may_have_taken_effect(error: &BotError) -> bool {
        matches!(*error, BotError::EffectIndeterminate { .. })
    }

    #[test]
    fn an_indeterminate_effect_is_distinguishable_from_a_domain_error() {
        // Same domain and byte-identical cause text: the variant is the whole
        // difference. That is the defect this closes — before it, a consumer
        // had to infer "may have happened" from an untrusted string, so the
        // only safe reading was the pessimistic one, and the pessimistic
        // reading of "did not happen" is a retry that duplicates.
        let refused = BotError::DomainError {
            domain: String::from("gh::merge"),
            certainty: DispatchCertainty::NotDelivered,
            cause: String::from("connection closed"),
        };
        let unknown = BotError::EffectIndeterminate {
            domain: String::from("gh::merge"),
            cause: String::from("connection closed"),
        };

        assert!(
            !may_have_taken_effect(&refused),
            "a domain error is a retry, not a possible duplicate: {refused}"
        );
        assert!(
            may_have_taken_effect(&unknown),
            "an indeterminate effect must not be blind-retried: {unknown}"
        );
        assert_ne!(
            refused.to_string(),
            unknown.to_string(),
            "an operator triages from these lines, so they cannot render alike"
        );
    }

    #[test]
    fn the_retry_class_is_total_and_never_reads_a_cause() {
        // The three arms are ordered by what they permit, and every error maps
        // into exactly one of them. A wiring defect and a refused binding are
        // `Never`, a connection that never opened is `Safe`, and only an
        // unsettled effect is `RequiresEvidence` — which is the same answer the
        // variant-level classifier above gives, reached without inspecting a
        // variant.
        let cases = [
            (
                BotError::DomainError {
                    domain: String::from("test::wiring"),
                    certainty: DispatchCertainty::Refused,
                    cause: String::from("type mismatch in execute input"),
                },
                RetryClass::Never,
            ),
            (
                BotError::DomainError {
                    domain: String::from("test::read"),
                    certainty: DispatchCertainty::NotDelivered,
                    cause: String::from("connection reset before any byte was sent"),
                },
                RetryClass::Safe,
            ),
            (
                BotError::EffectIndeterminate {
                    domain: String::from("test::merge"),
                    cause: String::from("the acknowledgment never arrived"),
                },
                RetryClass::RequiresEvidence,
            ),
            (BotError::TickInsideRuntime, RetryClass::Never),
        ];

        for (error, expected) in cases {
            assert_eq!(
                error.retry_class(),
                expected,
                "the class is what the retry decision reads: {error}"
            );
            assert_eq!(
                error.dispatch_certainty().retry_class(),
                expected,
                "the two-step path must agree with the one-step path: {error}"
            );
        }
    }

    #[test]
    fn a_refusal_and_a_wiring_defect_are_both_terminal() {
        // Two producers with two different reasons to be permanent: a binding
        // that is not installed, and a chain whose stages do not line up. Both
        // must stop the entry dead rather than spend the budget, because no
        // number of attempts changes either.
        let binding = BotError::DomainError {
            domain: String::from("gh::pr_status"),
            certainty: DispatchCertainty::Refused,
            cause: String::from("binding required"),
        };
        let wiring = BotError::DomainError {
            domain: String::from("pipeline::step"),
            certainty: DispatchCertainty::Refused,
            cause: String::from("type mismatch in pipeline step input"),
        };

        for error in [&binding, &wiring] {
            assert_eq!(error.retry_class(), RetryClass::Never);
            assert_eq!(error.dispatch_certainty(), DispatchCertainty::Refused);
        }
    }

    #[test]
    fn an_indeterminate_effect_names_its_domain_and_the_indeterminacy() {
        let error = BotError::EffectIndeterminate {
            domain: String::from("notify::slack"),
            cause: String::from("request timed out"),
        };
        let rendered = error.to_string();

        assert!(
            rendered.contains("notify::slack"),
            "the line must name the domain that failed: {rendered}"
        );
        assert!(
            rendered.contains("request timed out"),
            "the line must carry the cause: {rendered}"
        );
        assert!(
            rendered.contains("may have taken effect"),
            "the failure alone is not enough — the line has to say the effect may be \
             live, because that is what makes a retry unsafe: {rendered}"
        );
    }

    /// One payload carrying every class the rendering contract has to handle:
    /// LF, CR, tab, ESC, a quote, a backslash, and ordinary Unicode.
    const HOSTILE: &str = "a\nb\rc\td\u{1b}[2J e\"f\\g ünïcode 日本";

    /// Every variant that interpolates a `String` payload.
    ///
    /// One row per arm, so an arm added later without `Escaped` fails here
    /// rather than in whatever log sink first meets a hostile domain name.
    fn hostile_arms() -> Vec<BotError> {
        let payload = String::from(HOSTILE);
        vec![
            // One row covers both payloads a deficit can carry: the capability
            // name and the domain that demanded it are both caller-supplied
            // strings, and both reach the rendering site.
            BotError::CapabilityDenied {
                deficit: Deficit::new(
                    Shortage::new(
                        Cap::new(payload.clone()),
                        Some(Demand::new(payload.clone())),
                    ),
                    Vec::new(),
                ),
            },
            BotError::MalformedSpec {
                cause: payload.clone(),
            },
            BotError::DomainError {
                domain: payload.clone(),
                certainty: DispatchCertainty::Refused,
                cause: payload.clone(),
            },
            BotError::EffectIndeterminate {
                domain: payload.clone(),
                cause: payload.clone(),
            },
            BotError::EvaluateError {
                cause: payload.clone(),
            },
            BotError::MalformedFlow {
                cause: payload.clone(),
            },
            BotError::InvalidTransitionTarget {
                from: payload.clone(),
                target: payload.clone(),
            },
            BotError::MissingAskRoute {
                node: payload.clone(),
                option: payload.clone(),
            },
            BotError::VariableNeverWritten {
                name: payload.clone(),
            },
            BotError::VariableReadBeforeInit {
                node: payload.clone(),
                name: payload.clone(),
            },
            BotError::UndeclaredVariable {
                name: payload.clone(),
            },
            BotError::UnreachableNode {
                node: payload.clone(),
            },
            BotError::UnknownNodeKind {
                node: payload.clone(),
                kind: payload.clone(),
            },
            BotError::MissingTransition {
                node: payload.clone(),
            },
            BotError::MalformedTemplate {
                node: payload.clone(),
                field: "label",
            },
            BotError::VariableTypeMismatch {
                name: payload.clone(),
            },
            // One row covers both terminal payloads: `HandedOff` carries the
            // node's target and `Refused` the declared reason, and both reach
            // the rendering site.
            BotError::ConflictingTerminalDeclaration {
                node: payload.clone(),
                intrinsic: Terminal::HandedOff {
                    target: payload.clone(),
                },
                declared: Terminal::Refused {
                    reason: payload.clone(),
                },
            },
            BotError::AskOptionNotAssignable {
                node: payload.clone(),
                variable: payload.clone(),
                option: payload.clone(),
                expected: "boolean",
                cause: payload.clone(),
            },
            BotError::VariableUnset {
                name: payload.clone(),
            },
            BotError::InvalidVariableValue {
                variable: payload.clone(),
                value: payload.clone(),
            },
            BotError::ResolverReturnedInvalidOption { node: payload },
        ]
    }

    #[test]
    fn a_rendered_payload_cannot_forge_a_log_record() {
        // The contract the module header states, and the one `Display` did not
        // implement: it interpolated the payload directly, so a domain name
        // containing a newline produced a second physical log line and an ESC
        // reached whatever terminal later read the file.
        let error = BotError::EffectIndeterminate {
            domain: String::from(HOSTILE),
            cause: String::from(HOSTILE),
        };
        let rendered = error.to_string();

        assert!(
            !rendered.contains('\n') && !rendered.contains('\r'),
            "a payload must not add a physical log record: {rendered:?}"
        );
        assert!(
            !rendered.chars().any(char::is_control),
            "a payload must not carry a live terminal control: {rendered:?}"
        );
        assert!(
            rendered.contains("\\n") && rendered.contains("\\u{1b}"),
            "the escapes must be visible rather than stripped, because the payload \
             is the diagnostic: {rendered}"
        );
    }

    #[test]
    fn every_untrusted_field_is_escaped() {
        for error in hostile_arms() {
            let rendered = error.to_string();
            assert!(
                !rendered.chars().any(char::is_control),
                "a variant rendered a live control character: {rendered:?}"
            );
            assert!(
                !rendered.contains('\n'),
                "a variant rendered a second physical line: {rendered:?}"
            );
        }
    }

    #[test]
    fn ordinary_diagnostics_keep_their_meaning() {
        // The other half of the contract, and the reason backslash is left
        // alone: escaping must not cost the diagnostic its content. Ordinary
        // Unicode and punctuation pass through byte for byte.
        let error = BotError::DomainError {
            domain: String::from("gh::merge"),
            certainty: DispatchCertainty::NotDelivered,
            cause: String::from("PR #7 — \"timeout\" after 30s, ünïcode 日本 ok"),
        };
        let rendered = error.to_string();

        assert!(
            rendered.contains("PR #7 — \"timeout\" after 30s, ünïcode 日本 ok"),
            "an ordinary message must survive unaltered: {rendered}"
        );
        assert!(
            rendered.starts_with("gh::merge: "),
            "the typed boundary stays readable: {rendered}"
        );
    }

    #[test]
    fn escaping_is_idempotent_with_constructor_level_escaping() {
        // `MalformedSpec` and `MalformedFlow` escape their payload where they
        // build it, and this pass runs again at render time over the same text.
        // If this escaped backslash the diagnostic would grow one on every hop.
        let already_escaped = Escaped("line one\\nline two").to_string();
        assert_eq!(
            already_escaped, "line one\\nline two",
            "text that is already escaped must pass through untouched"
        );

        let raw = Escaped("line one\nline two").to_string();
        assert_eq!(
            raw, "line one\\nline two",
            "and the escape it produces must be the same shape, so the two agree"
        );
    }
}
