//! How a failure renders: every untrusted field escaped, once, here.

use std::fmt;
// `write!` and `write_str` into a `String` need `fmt::Write` in scope;
// `Display`'s own `Formatter` already implements it, so this names the same
// sink trait rather than introducing another.
use std::fmt::Write as FmtWrite;

use super::BotError;
use crate::session::Terminal;

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

/// Writes one glyph's escaped form.
///
/// A helper rather than a fourth match arm inline: the loop body was four
/// fallible writes in one statement, so reading the escaping rules also meant
/// reading four propagation operators to see that each arm writes one thing.
fn write_escaped(formatter: &mut fmt::Formatter<'_>, glyph: char) -> fmt::Result {
    match glyph {
        '\n' => formatter.write_str("\\n"),
        '\r' => formatter.write_str("\\r"),
        '\t' => formatter.write_str("\\t"),
        control if control.is_control() => write!(formatter, "\\u{{{:x}}}", u32::from(control)),
        plain => {
            let mut buffer = [0_u8; 4];
            formatter.write_str(plain.encode_utf8(&mut buffer))
        }
    }
}

impl fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for glyph in self.0.chars() {
            write_escaped(f, glyph)?;
        }
        Ok(())
    }
}

/// Renders a node whose declared terminal contradicts its kind's own outcome.
///
/// Rendered into a buffer and written once: inline, the arm was five fallible
/// writes in one statement, so reading the sentence also meant reading five
/// propagation operators, and a writer that failed part-way through had already
/// emitted a truncated sentence.
fn write_conflicting_declaration(
    formatter: &mut fmt::Formatter<'_>,
    node: &str,
    intrinsic: &Terminal,
    declared: &Terminal,
) -> fmt::Result {
    let mut rendered = String::new();
    write!(rendered, "terminal declaration on node {} ", Escaped(node))?;
    rendered.write_str("contradicts the outcome its kind carries: ")?;
    write_terminal_to(&mut rendered, intrinsic)?;
    rendered.write_str(" declared as ")?;
    write_terminal_to(&mut rendered, declared)?;
    formatter.write_str(&rendered)
}

/// Renders one terminal outcome into any `fmt::Write` sink.
///
/// `write_terminal` is the `Formatter`-typed form used by the `Display` arms;
/// this is the same rendering against a buffer, so a caller composing a
/// sentence cannot reimplement the escaping and get it subtly wrong.
fn write_terminal_to<W: fmt::Write>(sink: &mut W, terminal: &Terminal) -> fmt::Result {
    match *terminal {
        Terminal::Completed => sink.write_str("completed"),
        Terminal::Referred { ref target } => write!(sink, "referred to {target:?}"),
        Terminal::HandedOff { ref target } => write!(sink, "handed off to {target:?}"),
        Terminal::Refused { ref reason } => write!(sink, "refused because {reason:?}"),
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
            // One `write!`, so there is a single fallible step and no partially
            // rendered sentence if the formatter refuses part-way. `cause` is
            // escaped for the same reason `field`'s neighbours are: it names
            // something from the spec document.
            Self::IncompleteSpec { field, ref cause } => {
                write!(
                    f,
                    "incomplete bot spec: missing {field}: {}",
                    Escaped(cause)
                )
            }
            Self::UnregisteredDomain { ref domain } => {
                write!(f, "unregistered domain: {}", Escaped(domain))
            }
            Self::UndeclaredDomains { ref needs } => {
                write!(
                    f,
                    "this flow names domains its registry does not declare: {needs}"
                )
            }
            Self::UnjournaledEffect { ref domain } => write!(
                f,
                "`act` runs only an action whose effect stays in this process, and {} \
                 reaches outside it; put it in a `BotSpec` chain, whose dispatch records \
                 intent in the effect ledger before the effect leaves",
                Escaped(domain)
            ),
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
            Self::UnsupportedSpecVersion { found, supported } => write!(
                f,
                "bot spec declares version {found}, but this build materializes version \
                 {supported} only"
            ),
            Self::UnknownCondition {
                ref condition,
                ref argument_parse,
            } => write!(
                f,
                // `argument_parse` distinguishes an identifier this build does
                // not know from one whose threshold argument does not parse --
                // two different repairs -- so it is rendered rather than dropped.
                "unknown condition {}: not in the supported wire vocabulary, or its threshold \
                 argument did not parse ({})",
                Escaped(condition),
                Escaped(argument_parse)
            ),
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
            } => write_conflicting_declaration(f, node, intrinsic, declared),
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
                ref reason,
            } => write!(
                f,
                // `reason` is untrusted like the other two: it is the decoder's
                // own rendering, and a caller may reach it through an answer that
                // reached the declaration. Escaped for the same reason `value`
                // renders through `Debug` and `variable` through `Escaped`.
                "value {value:?} cannot be assigned to variable {}: {}",
                Escaped(variable),
                Escaped(reason)
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
            Self::PollStalled { chain, deadline } => write!(
                f,
                "source poll for chain {chain} was cancelled at its declared {deadline:?} \
                 per-poll deadline: nothing was read, nothing committed, and the other chains \
                 of this tick were not stopped"
            ),
            Self::CredentialExpired {
                ref capabilities,
                expired_at,
                now,
            } => {
                let names: Vec<&str> = capabilities.iter().map(|cap| cap.as_str()).collect();
                write!(
                    f,
                    "the credential expired at {expired_at:?} and this call is at {now:?}; it \
                     covered [{}], and re-granting is the repair",
                    names.join(", ")
                )
            }
            Self::CredentialRejected {
                ref domain,
                status,
                ref needs,
            } => write!(
                f,
                "{} was refused by its upstream with HTTP {status}: {needs}",
                Escaped(domain)
            ),
            Self::Conflict {
                ref domain,
                ref record,
                expected,
                found,
            } => write!(
                f,
                "{} refused a write to record {}: expected version {expected}, found \
                 version {found}; re-read the record and name the version found",
                Escaped(domain),
                Escaped(record)
            ),
            Self::UnstableObservation {
                ref domain,
                unstable,
            } => write!(
                f,
                "source {} read a subject that did not settle ({}); no reading was committed and \
                 the observation is pending the next tick",
                Escaped(domain),
                unstable
            ),
            Self::PollDeadlineUnbounded { deadline } => write!(
                f,
                "per-poll deadline of {deadline:?} bounds no wait: every poll would be \
                 cancelled before it was first polled"
            ),
            Self::PollDeadlineExceeded { deadline, ceiling } => write!(
                f,
                "per-poll deadline of {deadline:?} is above the {ceiling:?} ceiling: a source \
                 that stops answering would hold the tick for longer than any bound this bot \
                 grants"
            ),
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
