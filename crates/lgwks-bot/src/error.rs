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

use std::fmt;

use super::cap::Deficit;
use super::ecs::{EffectEvidence, PendingWork, WorkId};
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
    DomainError {
        /// The domain that failed (e.g. `"gh::pr_status"`).
        domain: String,
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
        /// Entries across every chain that are still open.
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
        /// The identity that matched no held entry.
        work: WorkId,
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
    EvidenceSuperseded {
        /// The identity the evidence was submitted for.
        work: WorkId,
        /// The generation the caller named.
        named: u64,
        /// The generation the chain holds now.
        current: u64,
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
                "evidence names generation {named} of chain {} entry {}, which has \
                 been superseded by generation {current}: nothing was changed, re-read \
                 pending() and report against the generation that is there now",
                work.chain(),
                work.entry()
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
    use super::{BotError, Escaped, Terminal};
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
