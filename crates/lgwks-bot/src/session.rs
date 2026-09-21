//! Validated declarative guidance flows and a synchronous session runner.
//!
//! The module deliberately has no relation to [`crate::domain::flow::Pipeline`].
//! A pipeline composes capability-gated effects; this module interprets a
//! human-facing, validated graph and records the cursor's conversation.
//!
//! # Resource bounds
//!
//! Validation bounds the *shape* of a document and [`FlowBounds`] bounds the
//! number of *steps* a session takes. Neither bounds bytes, and the two are
//! independent: `${value}` written a hundred thousand times is a
//! two-hundred-kilobyte document that interpolates to ten gigabytes, and a
//! step budget of two hundred and fifty-six does not notice, because a single
//! `say` node performs one step however much text it renders.
//!
//! [`ResourceLimits`] is the byte bound, and it is enforced in four places:
//!
//! - [`MAX_UTTERANCE_BYTES`] — one answer, checked at the ingress of
//!   [`Session::answer`] before the utterance is resolved, stored, or recorded;
//! - [`MAX_VALUE_BYTES`] — one stored value, checked when an answer is assigned
//!   and again when the document names a candidate no session could store;
//! - [`MAX_RECORD_BYTES`] — one rendered record payload, checked against the
//!   compiled template's *computed* expansion before that expansion is
//!   allocated ([`CompiledTemplate::expanded_bytes`]) and against the built
//!   prompt's computed size before the prompt is built;
//! - [`MAX_SESSION_BYTES`] — every record and every visited node id a session
//!   retains, checked before either sink is written.
//!
//! Every one of them refuses rather than truncates: a record cut in half is a
//! record the journal claims happened and nobody can read. The refusal is
//! always before the write, so a rejected expansion leaves no partial journal
//! entry behind.
//!
//! These are the *operator's* ceilings. A document may ask for tighter ones in
//! its [`FlowBounds`], and an embedding application may ask for tighter ones on
//! a session ([`Session::with_limits`]); neither may ask for looser, and a
//! request above the ceiling is [`BotError::ResourceLimitAboveCeiling`] rather
//! than a silently applied bound. The two requests combine by taking the
//! smaller value on each axis.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::num::IntErrorKind;

use lgwks_std::hash::Hasher;
use lgwks_std::json::Value as JsonValue;
use lgwks_std::json::{Deserialize, Serialize};

use crate::error::BotError;
use crate::semantic::EmbedderIdentity;

/// Maximum JSON flow size accepted before parsing begins.
pub const MAX_FLOW_BYTES: usize = 2_097_152;

/// Alias for [`MAX_FLOW_BYTES`] with an explicit schema-oriented name.
pub const MAX_FLOW_SPEC_BYTES: usize = MAX_FLOW_BYTES;

/// Maximum bytes accepted in one answer utterance.
///
/// An ingress ceiling, checked before the utterance is resolved or recorded.
/// A person typing an answer does not need more than this, and a caller
/// feeding a session from a file or a socket is the case where an unbounded
/// one costs memory for nothing.
pub const MAX_UTTERANCE_BYTES: usize = 8_192;

/// Maximum bytes one stored variable value may occupy when rendered.
///
/// Applies to the *value*, not to the option string that produced it: an
/// integer stores its decimal digits whatever the candidate said, and a choice
/// candidate stores the declared spelling. It is therefore a bound on what
/// interpolation can later paste into a record, which is the size that matters.
pub const MAX_VALUE_BYTES: usize = 65_536;

/// Maximum bytes of one rendered record payload.
///
/// The ceiling that makes expansion a linear operation. Compiling and sizing a
/// template under this bound costs no more than the bound itself, so a
/// document of a few hundred kilobytes cannot become a record of a few
/// gigabytes: the expansion is refused before it is allocated.
pub const MAX_RECORD_BYTES: usize = 1_048_576;

/// Maximum bytes one session retains across all of its records and visited
/// node ids.
///
/// The aggregate ceiling. Per-record and per-value bounds compose into an
/// unbounded total only if the graph can be walked again, and a budget bounds
/// steps rather than bytes, so the sum needs its own bound.
pub const MAX_SESSION_BYTES: usize = 8_388_608;

/// The byte axis a [`ResourceLimits`] ceiling applies to.
///
/// Reported by [`BotError::ResourceLimitAboveCeiling`] so a refusal names which
/// limit was raised, and used to tighten one axis without spelling the other
/// three. A typed axis rather than a flag per axis, because a caller that wants
/// to raise every ceiling should have to write every axis down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResourceAxis {
    /// Bytes accepted in one answer utterance ([`MAX_UTTERANCE_BYTES`]).
    Utterance,
    /// Bytes one stored variable value may occupy ([`MAX_VALUE_BYTES`]).
    Value,
    /// Bytes of one rendered record payload ([`MAX_RECORD_BYTES`]).
    Record,
    /// Bytes one session retains in total ([`MAX_SESSION_BYTES`]).
    Session,
}

impl ResourceAxis {
    /// The stable label naming this axis.
    ///
    /// A label for diagnostics and serialization prose, not a serde
    /// discriminator: a `ResourceLimits` document spells the axes as its field
    /// names.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match *self {
            Self::Utterance => "utterance",
            Self::Value => "value",
            Self::Record => "record",
            Self::Session => "session",
        }
    }
}

/// Byte ceilings applied to every session, and the one place they are resolved.
///
/// A validated flow bounds *shape*: steps, nodes, reachable transitions, and
/// templates whose markers name declared variables. It does not bound *bytes*,
/// and the two are independent — `${value}` repeated a hundred thousand times
/// in a two-hundred-kilobyte document is a valid flow that interpolates to ten
/// gigabytes. These four ceilings are the byte bound:
///
/// - [`ResourceAxis::Utterance`] — one answer;
/// - [`ResourceAxis::Value`] — one stored value;
/// - [`ResourceAxis::Record`] — one rendered record payload, checked against
///   the compiled template's computed expansion *before* it is allocated;
/// - [`ResourceAxis::Session`] — everything one session retains.
///
/// [`ResourceLimits::shipped`] is the operator's hard ceiling, and it is a
/// ceiling rather than a default: a shorthand constructor may ask for less on
/// any axis and never for more. A request above the ceiling is refused with
/// [`BotError::ResourceLimitAboveCeiling`] where it is made rather than clamped
/// quietly, because a bound that is silently ignored is a bound nobody can
/// reason about.
///
/// # Examples
///
/// ```
/// use lgwks_bot::{BotError, ResourceAxis, ResourceLimits};
///
/// let tight = ResourceLimits::shipped().tighten(ResourceAxis::Record, 4_096);
/// assert_eq!(tight.get(ResourceAxis::Record), 4_096);
/// assert_eq!(tight.get(ResourceAxis::Session), ResourceLimits::shipped().get(ResourceAxis::Session));
/// assert!(tight.within_ceiling().is_ok());
///
/// let greedy = ResourceLimits::shipped().tighten(ResourceAxis::Session, usize::MAX);
/// assert!(matches!(
///     greedy.within_ceiling(),
///     Err(BotError::ResourceLimitAboveCeiling {
///         axis: ResourceAxis::Session,
///         ..
///     })
/// ));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields, default)]
#[non_exhaustive]
pub struct ResourceLimits {
    /// Bytes accepted in one answer utterance.
    utterance_bytes: usize,
    /// Bytes one stored variable value may occupy when rendered.
    value_bytes: usize,
    /// Bytes of one rendered record payload.
    record_bytes: usize,
    /// Bytes one session retains in total.
    session_bytes: usize,
}

impl ResourceLimits {
    /// The operator's shipped ceilings, applied when nothing tighter is asked
    /// for.
    #[must_use]
    pub const fn shipped() -> Self {
        Self {
            utterance_bytes: MAX_UTTERANCE_BYTES,
            value_bytes: MAX_VALUE_BYTES,
            record_bytes: MAX_RECORD_BYTES,
            session_bytes: MAX_SESSION_BYTES,
        }
    }

    /// Return the ceiling applied to one axis.
    #[must_use]
    pub const fn get(self, axis: ResourceAxis) -> usize {
        match axis {
            ResourceAxis::Utterance => self.utterance_bytes,
            ResourceAxis::Value => self.value_bytes,
            ResourceAxis::Record => self.record_bytes,
            ResourceAxis::Session => self.session_bytes,
        }
    }

    /// Return these ceilings with one axis set to `bytes`.
    ///
    /// Tightening only in intent: a value above the operator's ceiling is
    /// accepted by this constructor and refused by [`Self::within_ceiling`],
    /// so the caller learns that the bound it asked for is not the bound in
    /// force. `const` so a caller can build a ceiling at compile time.
    #[must_use]
    pub const fn tighten(mut self, axis: ResourceAxis, bytes: usize) -> Self {
        match axis {
            ResourceAxis::Utterance => self.utterance_bytes = bytes,
            ResourceAxis::Value => self.value_bytes = bytes,
            ResourceAxis::Record => self.record_bytes = bytes,
            ResourceAxis::Session => self.session_bytes = bytes,
        }
        self
    }

    /// The stricter of two ceiling sets, axis by axis.
    ///
    /// How an author's tightening and an operator's tightening combine: the
    /// smaller bound wins, so neither can widen what the other narrowed.
    #[must_use]
    pub const fn narrowed(self, other: Self) -> Self {
        Self {
            utterance_bytes: smaller(self.utterance_bytes, other.utterance_bytes),
            value_bytes: smaller(self.value_bytes, other.value_bytes),
            record_bytes: smaller(self.record_bytes, other.record_bytes),
            session_bytes: smaller(self.session_bytes, other.session_bytes),
        }
    }

    /// Refuse any axis set above the operator's ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::ResourceLimitAboveCeiling`] naming the first axis
    /// that exceeds its ceiling.
    pub fn within_ceiling(self) -> Result<Self, BotError> {
        let ceiling = Self::shipped();
        for axis in [
            ResourceAxis::Utterance,
            ResourceAxis::Value,
            ResourceAxis::Record,
            ResourceAxis::Session,
        ] {
            let requested = self.get(axis);
            let allowed = ceiling.get(axis);
            if requested > allowed {
                return Err(BotError::ResourceLimitAboveCeiling {
                    axis,
                    requested,
                    ceiling: allowed,
                });
            }
        }
        Ok(self)
    }
}

impl Default for ResourceLimits {
    /// The operator's shipped ceilings — not "no limits".
    fn default() -> Self {
        Self::shipped()
    }
}

/// The smaller of two byte counts, named because `Ord::min` is not `const`.
const fn smaller(left: usize, right: usize) -> usize {
    if left < right { left } else { right }
}

/// Stable identifier of a flow node.
pub type NodeId = String;

/// The closed set of scalar values carried by a [`VarScope`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    crate = "lgwks_std::json::serde",
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
#[non_exhaustive]
pub enum Value {
    /// A free-form string.
    String(String),
    /// A signed integer.
    Integer(i64),
    /// A boolean.
    Boolean(bool),
    /// A selected member of a declared choice variable.
    Choice(String),
}

impl fmt::Display for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::String(ref value) | Self::Choice(ref value) => formatter.write_str(value),
            Self::Integer(value) => write!(formatter, "{value}"),
            Self::Boolean(value) => write!(formatter, "{value}"),
        }
    }
}

impl Value {
    /// The number of bytes this value occupies when a template interpolates
    /// it.
    ///
    /// The exact cost, computed without rendering the value: for the two string
    /// variants it is the string's length, and for the two scalar variants it is
    /// the width of the decimal or boolean spelling that [`fmt::Display`]
    /// produces. Sizing an expansion needs this before it has the text, and
    /// measuring by formatting would allocate the buffer the check exists to
    /// avoid allocating.
    #[must_use]
    pub fn rendered_bytes(&self) -> usize {
        match *self {
            Self::String(ref value) | Self::Choice(ref value) => value.len(),
            // "true" is four bytes and "false" is five, and both are constants
            // rather than a call into the boolean formatter.
            Self::Boolean(true) => 4,
            Self::Boolean(false) => 5,
            Self::Integer(value) => integer_bytes(value),
        }
    }
}

/// The number of bytes [`fmt::Display`] writes for one integer.
///
/// Division-free: the digit count is the base-ten logarithm rounded down plus
/// one, and the sign is one more byte when the value is negative. Zero has one
/// digit and no logarithm, which is the `None` arm.
fn integer_bytes(value: i64) -> usize {
    // `unsigned_abs` rather than `abs`, because `i64::MIN` has no positive
    // counterpart: negating it in `i64` overflows, and the digit count does not
    // care which side of zero the magnitude came from.
    let digits = value
        .unsigned_abs()
        .checked_ilog10()
        .map_or(1, |exponent| exponent.saturating_add(1));
    let total = if value < 0 {
        digits.saturating_add(1)
    } else {
        digits
    };
    // At most twenty digits, so this cannot fail on any target this crate
    // builds for. The fallback is `usize::MAX` rather than a panic because the
    // only caller compares the result against a ceiling: an impossible width
    // reported as enormous is refused, which is the failing-closed direction.
    usize::try_from(total).unwrap_or(usize::MAX)
}

/// Declared type of one variable in a flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    crate = "lgwks_std::json::serde",
    tag = "kind",
    content = "options",
    rename_all = "snake_case"
)]
#[non_exhaustive]
pub enum VarType {
    /// A free-form string assigned by an ask.
    String,
    /// A signed integer parsed from an answer.
    Integer,
    /// A boolean parsed from `true`/`false` or `yes`/`no`.
    Boolean,
    /// A finite set of allowed answer values.
    Choice(Vec<String>),
}

impl VarType {
    /// Returns how an answer to a question writing this variable must be read.
    ///
    /// `Integer` is the only declaration whose answers are not text. The other
    /// three are labels: a `Choice` answer is matched against its own option
    /// strings, and a `Boolean` answer is a word (`yes`/`no`) that the same
    /// punctuation folding should tolerate.
    #[must_use]
    pub const fn answer_domain(&self) -> AnswerDomain {
        match *self {
            Self::Integer => AnswerDomain::Integer,
            Self::String | Self::Boolean | Self::Choice(_) => AnswerDomain::Label,
        }
    }

    /// The stable label naming this declared type.
    ///
    /// Stable prose for diagnostics and nothing else: it is not the serde
    /// discriminator, which is `rename_all = "snake_case"` on the variants and
    /// should be read from the document instead of from here.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match *self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Boolean => "boolean",
            Self::Choice(_) => "choice",
        }
    }

    /// Decode one answer string into a value of this declared type.
    ///
    /// The single decoder. Flow validation runs it over every `ask` candidate
    /// when the document is loaded, and runtime assignment runs it over the
    /// option the resolver selected; one implementation means the two cannot
    /// disagree about which candidates are storable, and a flow that loads is
    /// a flow every one of whose options can be stored.
    ///
    /// A display label is therefore a *value* of the declared type rather than
    /// a free string: `"yes"` offered for a boolean variable stores
    /// [`Value::Boolean`]`(true)`, and a choice candidate that differs from the
    /// declared spelling only in case stores the declared spelling. The route
    /// is keyed by the label the resolver matched; the variable receives what
    /// this returns.
    ///
    /// # Errors
    ///
    /// Returns the [`AnswerRejection`] naming why the answer cannot be stored.
    pub fn decode_answer(&self, answer: &str) -> Result<Value, AnswerRejection> {
        let trimmed = answer.trim();
        match *self {
            // A free-form string accepts anything, including the untrimmed
            // bytes: trimming a person's answer for them is a decision the
            // flow author makes downstream, not one this decoder makes.
            Self::String => Ok(Value::String(answer.to_owned())),
            Self::Integer => match trimmed.parse::<i64>() {
                Ok(value) => Ok(Value::Integer(value)),
                Err(error) => Err(match *error.kind() {
                    IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => {
                        AnswerRejection::IntegerOutOfRange
                    }
                    _ => AnswerRejection::NotAnInteger,
                }),
            },
            Self::Boolean => {
                if trimmed.eq_ignore_ascii_case("true") || trimmed.eq_ignore_ascii_case("yes") {
                    Ok(Value::Boolean(true))
                } else if trimmed.eq_ignore_ascii_case("false")
                    || trimmed.eq_ignore_ascii_case("no")
                {
                    Ok(Value::Boolean(false))
                } else {
                    Err(AnswerRejection::NotABoolean)
                }
            }
            Self::Choice(ref options) => {
                let Some(option) = options
                    .iter()
                    .find(|option| option.eq_ignore_ascii_case(trimmed))
                else {
                    return Err(AnswerRejection::NotADeclaredChoice);
                };
                Ok(Value::Choice(option.clone()))
            }
        }
    }
}

/// Decodes one whole-number answer, preserving its sign.
///
/// The **resolver's** reading of a whole number: presence-only, because the
/// question the tiered search asks is *does this decode to the value the person
/// named*, and the answer to that is yes or no. The scope's reading is
/// [`VarType::decode_answer`], which has to say *why* an answer cannot be
/// stored and therefore distinguishes an overflow from prose; both run the same
/// `parse::<i64>` over the same trimmed bytes, so they cannot disagree about
/// which answers are whole numbers.
///
/// `None` means *not a whole number in range* — an empty answer, prose, a
/// decimal, or a magnitude beyond [`i64`]. It is not an error: an unrecognized
/// answer is re-asked, and the caller decides whether that or a refusal is the
/// right report.
pub(crate) fn decode_integer(raw: &str) -> Option<i64> {
    raw.trim().parse::<i64>().ok()
}

/// How a question's answers are meant to be read.
///
/// A property of the *answer*, not of the option text. Two questions can offer
/// option lists that look identical and mean different things by them, and
/// nothing in the option strings says which: `["-5", "5"]` at an integer
/// question is two distinct answers, while the same pair at a label question is
/// two spellings the resolver cannot tell apart, because the label policy folds
/// punctuation and the sign is punctuation.
///
/// That is the whole reason this exists as an explicit product of states rather
/// than a flag on one code path. The folding is correct for labels — `"yes!"`
/// and `"yes"` are one answer, and a person should not be re-asked for typing an
/// exclamation mark — and it is destructive for numbers, where it silently turns
/// `-5` into `5` and reports it at maximum confidence. One policy cannot be
/// both, so the answer says which one applies and the resolver reads the
/// question accordingly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AnswerDomain {
    /// Natural-language option labels: compared after [`crate::language::normalize`].
    Label,
    /// Whole numbers: compared as decoded values, sign preserved.
    Integer,
}

impl fmt::Display for AnswerDomain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Label => formatter.write_str("label"),
            Self::Integer => formatter.write_str("integer"),
        }
    }
}

/// Why one answer string cannot be stored in a variable.
///
/// A closed verdict rather than a message, so a caller branches on the cause
/// instead of parsing prose, and so the set of ways an answer can be unusable
/// is stated where a reviewer reads it. It is what
/// [`VarType::decode_answer`] returns, and therefore what flow validation
/// reports through [`BotError::AskOptionNotAssignable`] when a candidate is
/// unusable at load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AnswerRejection {
    /// The answer is not a decimal integer.
    NotAnInteger,
    /// The answer is a decimal integer outside the signed 64-bit range.
    IntegerOutOfRange,
    /// The answer is not `true`/`false` or `yes`/`no`.
    NotABoolean,
    /// The answer is not one of the variable's declared choice values.
    NotADeclaredChoice,
}

impl fmt::Display for AnswerRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::NotAnInteger => formatter.write_str("it is not a decimal integer"),
            Self::IntegerOutOfRange => {
                formatter.write_str("it is outside the signed 64-bit integer range")
            }
            Self::NotABoolean => formatter.write_str("it is not true/false or yes/no"),
            Self::NotADeclaredChoice => {
                formatter.write_str("it is not one of the declared choice values")
            }
        }
    }
}

/// A variable reference or literal used by a branch predicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    crate = "lgwks_std::json::serde",
    tag = "kind",
    content = "value",
    rename_all = "snake_case"
)]
#[non_exhaustive]
pub enum ValueExpr {
    /// A literal scalar.
    Literal(Value),
    /// The current value of a named variable.
    Var(String),
}

impl ValueExpr {
    /// Construct a variable reference.
    #[must_use]
    pub fn variable(name: impl Into<String>) -> Self {
        Self::Var(name.into())
    }
}

/// Closed, side-effect-free branch predicate language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    crate = "lgwks_std::json::serde",
    tag = "op",
    content = "args",
    rename_all = "snake_case"
)]
#[non_exhaustive]
pub enum Predicate {
    /// A constant predicate.
    Const(bool),
    /// Equality comparison.
    Eq(ValueExpr, ValueExpr),
    /// Inequality comparison.
    Ne(ValueExpr, ValueExpr),
    /// Less-than comparison.
    Lt(ValueExpr, ValueExpr),
    /// Less-than-or-equal comparison.
    Le(ValueExpr, ValueExpr),
    /// Greater-than comparison.
    Gt(ValueExpr, ValueExpr),
    /// Greater-than-or-equal comparison.
    Ge(ValueExpr, ValueExpr),
    /// Conjunction of predicates.
    And(Vec<Predicate>),
    /// Disjunction of predicates.
    Or(Vec<Predicate>),
    /// Negation of one predicate.
    Not(Box<Predicate>),
}

impl Predicate {
    /// Evaluate this predicate against a variable scope.
    pub fn evaluate(&self, scope: &VarScope) -> Result<bool, BotError> {
        match *self {
            Self::Const(value) => Ok(value),
            Self::Eq(ref left, ref right) => {
                Ok(resolve_expr(left, scope)? == resolve_expr(right, scope)?)
            }
            Self::Ne(ref left, ref right) => {
                Ok(resolve_expr(left, scope)? != resolve_expr(right, scope)?)
            }
            Self::Lt(ref left, ref right) => {
                compare_expr(left, right, scope, |ordering| ordering.is_lt())
            }
            Self::Le(ref left, ref right) => {
                compare_expr(left, right, scope, |ordering| ordering.is_le())
            }
            Self::Gt(ref left, ref right) => {
                compare_expr(left, right, scope, |ordering| ordering.is_gt())
            }
            Self::Ge(ref left, ref right) => {
                compare_expr(left, right, scope, |ordering| ordering.is_ge())
            }
            Self::And(ref items) => {
                for item in items {
                    if !item.evaluate(scope)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Self::Or(ref items) => {
                for item in items {
                    if item.evaluate(scope)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Self::Not(ref inner) => Ok(!inner.evaluate(scope)?),
        }
    }
}

/// A declared flow node and its closed operation kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    crate = "lgwks_std::json::serde",
    tag = "kind",
    rename_all = "snake_case"
)]
#[non_exhaustive]
pub enum NodeKind {
    /// Emit interpolated text and follow its [`FlowEdge`] continuation.
    Say {
        /// Text shown to the user.
        text: String,
    },
    /// Ask for a value and route the recognized option.
    Ask {
        /// Variable receiving the selected option.
        var: String,
        /// Candidate answer strings passed to the resolver.
        options: Vec<String>,
        /// Route keyed by the selected option string.
        routes: BTreeMap<String, NodeId>,
    },
    /// Evaluate a predicate and choose one of two destinations.
    Branch {
        /// Variable read by the branch.
        var: String,
        /// Predicate deciding the `then` destination.
        when: Predicate,
        /// Destination when `when` is true.
        then: NodeId,
        /// Destination when `when` is false.
        otherwise: NodeId,
    },
    /// End the session with a handoff outcome.
    Handoff {
        /// Human or system target receiving the handoff.
        target: String,
    },
    /// End the session with a referral outcome after emitting text.
    Refer {
        /// Human or system target receiving the referral.
        target: String,
        /// Text shown before the referral is recorded.
        text: String,
    },
    /// Choose `dispatch` after an accepted answer, or `fallback` at a cold
    /// route with no accepted answer.
    Route {
        /// Destination used after a recognized answer.
        dispatch: NodeId,
        /// Destination used when no answer has been accepted.
        fallback: NodeId,
    },
    /// End the session. The flow terminal map can override the default
    /// [`Terminal::Completed`] outcome with a refusal.
    End,
}

/// Compatibility name for the reference flow representation.
pub type FlowNodeKind = NodeKind;

/// One explicit continuation in a flow graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    crate = "lgwks_std::json::serde",
    tag = "kind",
    rename_all = "snake_case"
)]
#[non_exhaustive]
pub enum FlowEdge {
    /// Continue from one node to another.
    Next {
        /// Source node.
        from: NodeId,
        /// Destination node.
        to: NodeId,
    },
    /// Reference-compatible spelling for an unconditional continuation.
    After {
        /// Source node.
        from: NodeId,
        /// Destination node.
        to: NodeId,
    },
}

impl FlowEdge {
    /// Construct an unconditional continuation.
    #[must_use]
    pub fn next(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self::Next {
            from: from.into(),
            to: to.into(),
        }
    }

    /// Return the source and destination node ids.
    #[must_use]
    pub fn endpoints(&self) -> (&str, &str) {
        match *self {
            Self::Next { ref from, ref to } | Self::After { ref from, ref to } => (from, to),
        }
    }
}

/// How a run's workflow ended, without its payload.
///
/// The routing class: a consumer acting on a refusal, a referral or a handoff
/// matches here, and reads the detail from [`Terminal`]. Payload-free so it is
/// `Copy` and can be matched in a guard.
///
/// This says nothing about effects. It is deliberately a separate fact from
/// [`EffectLedger`] rather than an arm of one enum, because "the flow was
/// refused" and "an earlier effect may be live" are independent, and a single
/// classifier that prioritises either one destroys the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Disposition {
    /// The flow ran to its own end.
    Completed,
    /// The flow referred the caller to another tier.
    Referred,
    /// The flow handed ownership to another actor.
    HandedOff,
    /// The flow refused to continue. A *decided* outcome, which settles nothing
    /// about an effect an earlier step already attempted.
    Refused,
}

/// What a run knows about the effects it attempted.
///
/// Two counts, because they answer different questions and neither can be
/// inferred from the other. `confirmed` is what the run watched take effect.
/// `unsettled` is what it attempted and could not settle — the effects a domain
/// reported as [`BotError::EffectIndeterminate`].
///
/// A non-zero `unsettled` does **not** imply that anything succeeded. One
/// attempted effect whose response was lost may have produced zero effects or
/// one, and that ambiguity is what this type keeps rather than resolves: the
/// honest report is "nothing is confirmed, and one is in doubt", not "part of
/// the output exists".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct EffectLedger {
    /// Effects the run watched take effect.
    confirmed: usize,
    /// Effects whose occurrence could not be determined.
    unsettled: usize,
}

impl EffectLedger {
    /// Record a run's effect history.
    #[must_use]
    pub const fn new(confirmed: usize, unsettled: usize) -> Self {
        Self {
            confirmed,
            unsettled,
        }
    }

    /// Effects the run watched take effect.
    #[must_use]
    pub const fn confirmed(&self) -> usize {
        self.confirmed
    }

    /// Effects whose occurrence could not be determined.
    #[must_use]
    pub const fn unsettled(&self) -> usize {
        self.unsettled
    }

    /// Whether any attempted effect's occurrence is unknown, and therefore
    /// whether a retry is a possible duplicate rather than a repair.
    ///
    /// Reads the count and never the disposition: a refusal does not settle an
    /// earlier network outcome, so this stays `true` for a refused run that
    /// left an effect in doubt.
    #[must_use]
    pub const fn needs_reconciliation(&self) -> bool {
        self.unsettled > 0
    }
}

/// A run's report: what the workflow decided, and what is known about the
/// effects it attempted.
///
/// Two independent fields rather than arms of one enum. A refused run that also
/// leaves an effect in doubt is both *refused* and *in need of reconciliation*,
/// and no single arm can say that: an earlier version of this type returned
/// `Failure` for every refusal and erased the uncertainty, which is the exact
/// information [`BotError::EffectIndeterminate`] was introduced to preserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct Outcome {
    /// What the workflow decided.
    disposition: Disposition,
    /// What is known about the effects it attempted.
    effects: EffectLedger,
}

impl Outcome {
    /// What the workflow decided.
    #[must_use]
    pub const fn disposition(&self) -> Disposition {
        self.disposition
    }

    /// What is known about the effects the run attempted.
    #[must_use]
    pub const fn effects(&self) -> EffectLedger {
        self.effects
    }

    /// Whether any attempted effect's occurrence is unknown.
    #[must_use]
    pub const fn needs_reconciliation(&self) -> bool {
        self.effects.needs_reconciliation()
    }
}

/// One predicate arm used by callers building a higher-level choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct ChoiceArm {
    /// Predicate guarding the arm.
    when: Predicate,
    /// Destination selected by the arm.
    then: NodeId,
}

impl ChoiceArm {
    /// Construct one guarded destination.
    #[must_use]
    pub fn new(when: Predicate, then: impl Into<String>) -> Self {
        Self {
            when,
            then: then.into(),
        }
    }

    /// Return the guard predicate.
    #[must_use]
    pub fn when(&self) -> &Predicate {
        &self.when
    }

    /// Return the destination node id.
    #[must_use]
    pub fn then(&self) -> &str {
        &self.then
    }
}

/// A terminal outcome returned by a completed session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    crate = "lgwks_std::json::serde",
    tag = "kind",
    rename_all = "snake_case"
)]
#[non_exhaustive]
pub enum Terminal {
    /// The flow completed normally.
    Completed,
    /// The flow referred the user to another target.
    Referred {
        /// Referral target.
        target: String,
    },
    /// The flow handed the user to another target.
    HandedOff {
        /// Handoff target.
        target: String,
    },
    /// The flow refused to continue.
    Refused {
        /// Human-readable refusal reason.
        reason: String,
    },
}

impl Terminal {
    /// Report this terminal together with the run's effect history.
    ///
    /// Both facts are carried through unchanged, and that is the whole of the
    /// method. It does not classify, because classifying is where the
    /// information was being lost: an earlier version returned a single enum
    /// and had to choose, for a refused run that also left an effect unsettled,
    /// whether to report the refusal or the uncertainty. Both are true, both
    /// matter to different consumers, and a report that drops either one is
    /// wrong for whoever needed it.
    #[must_use]
    pub fn outcome(&self, effects: EffectLedger) -> Outcome {
        let disposition = match *self {
            Self::Completed => Disposition::Completed,
            Self::Referred { .. } => Disposition::Referred,
            Self::HandedOff { .. } => Disposition::HandedOff,
            Self::Refused { .. } => Disposition::Refused,
        };
        Outcome {
            disposition,
            effects,
        }
    }
}

/// Bounds applied to a flow and every session executing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct FlowBounds {
    /// Maximum number of runtime steps, including answer attempts.
    budget: usize,
    /// Byte ceilings the document asks to be held to, if any.
    ///
    /// Absent means "whatever the operator enforces". Present means "no more
    /// than this", and the requested ceilings are checked against the
    /// operator's hard ceiling when the document loads: an author may declare
    /// that this flow's text is small and may not declare that it is allowed to
    /// be large.
    #[serde(default)]
    resources: Option<ResourceLimits>,
}

impl FlowBounds {
    /// Construct bounds with a declared step budget and the operator's shipped
    /// byte ceilings.
    #[must_use]
    pub const fn new(budget: usize) -> Self {
        Self {
            budget,
            resources: None,
        }
    }

    /// Return the declared step budget.
    #[must_use]
    pub const fn budget(self) -> usize {
        self.budget
    }

    /// Return the byte ceilings the document asks for, if it asks for any.
    #[must_use]
    pub const fn resources(self) -> Option<ResourceLimits> {
        self.resources
    }

    /// Return these bounds with a declared byte-ceiling request attached.
    #[must_use]
    pub const fn with_resources(mut self, resources: ResourceLimits) -> Self {
        self.resources = Some(resources);
        self
    }
}

impl Default for FlowBounds {
    fn default() -> Self {
        Self::new(256)
    }
}

/// A validated, declarative flow document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct FlowSpec {
    /// Variable declarations.
    vars: BTreeMap<String, VarType>,
    /// Entry node id.
    entry: NodeId,
    /// Node map keyed by stable node id.
    nodes: BTreeMap<NodeId, NodeKind>,
    /// Explicit continuations, primarily used by [`NodeKind::Say`].
    #[serde(default)]
    edges: Vec<FlowEdge>,
    /// Explicit terminal outcomes keyed by node id.
    #[serde(default)]
    terminals: BTreeMap<NodeId, Terminal>,
    /// Static and runtime bounds.
    #[serde(default)]
    bounds: FlowBounds,
}

impl FlowSpec {
    /// Construct and validate a flow document.
    pub fn new(
        vars: BTreeMap<String, VarType>,
        entry: impl Into<String>,
        nodes: BTreeMap<NodeId, NodeKind>,
        edges: Vec<FlowEdge>,
        terminals: BTreeMap<NodeId, Terminal>,
        bounds: FlowBounds,
    ) -> Result<Self, BotError> {
        let spec = Self {
            vars,
            entry: entry.into(),
            nodes,
            edges,
            terminals,
            bounds,
        };
        spec.validate()?;
        Ok(spec)
    }

    /// Construct a flow without explicit edges or terminal declarations.
    /// `Say` nodes still require a continuation edge, so this helper is useful
    /// only for flows made entirely from direct-transition node kinds.
    pub fn from_nodes(
        vars: BTreeMap<String, VarType>,
        entry: impl Into<String>,
        nodes: BTreeMap<NodeId, NodeKind>,
        bounds: FlowBounds,
    ) -> Result<Self, BotError> {
        Self::new(vars, entry, nodes, Vec::new(), BTreeMap::new(), bounds)
    }

    /// Serialize the validated flow using the shared JSON facade.
    pub fn to_json(&self) -> Result<String, lgwks_std::json::Error> {
        crate::json::to_string_pretty(self)
    }

    /// Parse and validate a flow using the shared JSON facade.
    pub fn from_json(source: &str) -> Result<Self, BotError> {
        if source.len() > MAX_FLOW_BYTES {
            return Err(BotError::FlowTooLarge {
                bytes: source.len(),
                limit: MAX_FLOW_BYTES,
            });
        }
        let value: JsonValue =
            crate::json::from_str(source).map_err(|error| BotError::MalformedFlow {
                cause: error.to_string().escape_debug().to_string(),
            })?;
        reject_unknown_node_kinds(&value)?;
        let spec: Self =
            crate::json::from_value(value).map_err(|error| BotError::MalformedFlow {
                cause: error.to_string().escape_debug().to_string(),
            })?;
        spec.validate()?;
        Ok(spec)
    }

    /// Re-run the structural validation for a flow already held in memory.
    pub fn validate(&self) -> Result<(), BotError> {
        flow::validate(self)
    }

    /// Return the declared variable map.
    #[must_use]
    pub fn vars(&self) -> &BTreeMap<String, VarType> {
        &self.vars
    }

    /// Return the entry node id.
    #[must_use]
    pub fn entry(&self) -> &str {
        &self.entry
    }

    /// Return the node map.
    #[must_use]
    pub fn nodes(&self) -> &BTreeMap<NodeId, NodeKind> {
        &self.nodes
    }

    /// Return the explicit continuation list.
    #[must_use]
    pub fn edges(&self) -> &[FlowEdge] {
        &self.edges
    }

    /// Return the explicit terminal map.
    #[must_use]
    pub fn terminals(&self) -> &BTreeMap<NodeId, Terminal> {
        &self.terminals
    }

    /// Return the applied bounds.
    #[must_use]
    pub const fn bounds(&self) -> FlowBounds {
        self.bounds
    }

    /// Look up one node by id.
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&NodeKind> {
        self.nodes.get(id)
    }

    /// The outcome one node ends a session with, after the terminal map is
    /// consulted.
    ///
    /// The one calculation of "what does this document say happens here",
    /// shared by validation and execution. `None` means the node is not a
    /// terminal node — an `End`, `Handoff`, or `Refer` — and no outcome is
    /// defined for it.
    ///
    /// A declared terminal is authoritative for every terminal node, which is
    /// what makes an `End` node's refusal override work. On a `Handoff` or
    /// `Refer` node that same authority would silently discard the target the
    /// node was written to hand to, so a declaration that *contradicts* the
    /// node's own outcome is refused at load
    /// ([`BotError::ConflictingTerminalDeclaration`]) rather than accepted and
    /// ignored: a document that says a referral is refused is either corrected
    /// or rejected, never run as a permissive referral. A declaration that
    /// repeats the node's own outcome is accepted and is what this method
    /// returns, so the declaration is read rather than merely tolerated.
    #[must_use]
    pub fn effective_terminal(&self, node_id: &str) -> Option<Terminal> {
        // A `?` here is the non-terminal case, and it is why the declared
        // lookup cannot be written first: an `End`-only declaration on a `Say`
        // node is refused by validation and has no effective outcome.
        let intrinsic = self.intrinsic_terminal(node_id)?;
        match self.terminals.get(node_id) {
            Some(declared) => Some(declared.clone()),
            None => Some(intrinsic),
        }
    }

    /// The outcome a terminal node carries before the terminal map is
    /// consulted: `End` completes, `Handoff` names its target, and `Refer`
    /// names its target.
    ///
    /// Private because it is the half of [`Self::effective_terminal`] that is
    /// only meaningful inside the crate: a caller that wants the document's
    /// outcome wants the declared one, and getting the intrinsic half would be
    /// getting the answer this method exists to be corrected from.
    fn intrinsic_terminal(&self, node_id: &str) -> Option<Terminal> {
        match *self.nodes.get(node_id)? {
            NodeKind::End => Some(Terminal::Completed),
            NodeKind::Handoff { ref target } => Some(Terminal::HandedOff {
                target: target.clone(),
            }),
            NodeKind::Refer { ref target, .. } => Some(Terminal::Referred {
                target: target.clone(),
            }),
            _ => None,
        }
    }

    /// Return all explicit edge destinations from one node.
    fn edge_targets(&self, from: &str) -> Vec<NodeId> {
        self.edges
            .iter()
            .filter_map(|edge| {
                let (source, target) = edge.endpoints();
                (source == from).then(|| target.to_owned())
            })
            .collect()
    }
}

/// Flow validation entry point.
pub mod flow {
    use super::{BotError, FlowSpec, validate_flow};

    /// Validate every structural invariant required before a session can run.
    pub fn validate(spec: &FlowSpec) -> Result<(), BotError> {
        validate_flow(spec)
    }
}

/// Validate a flow's graph, variables, templates, and budget against the
/// operator's shipped byte ceilings.
fn validate_flow(spec: &FlowSpec) -> Result<(), BotError> {
    validate_flow_within(spec, ResourceLimits::shipped())
}

/// Validate a flow's graph, variables, templates, budget, and byte ceilings.
///
/// The ceilings are a parameter because two callers enforce different ones:
/// [`FlowSpec::validate`] applies the shipped ceilings a document must satisfy
/// on its own, and a session applies the effective ceilings it will run under —
/// its own, narrowed by whatever the document asked for. Both runs are the same
/// validation, so "the document is valid" and "the document can be run here"
/// cannot disagree about a candidate list or a template.
fn validate_flow_within(spec: &FlowSpec, limits: ResourceLimits) -> Result<(), BotError> {
    // First, and before any structural work: a document that asks for a
    // ceiling above the operator's gets a diagnostic naming the axis it asked
    // about, not a run under a bound it did not choose.
    let requested = spec.bounds.resources.unwrap_or_default().within_ceiling()?;
    let effective = requested.narrowed(limits);
    if spec.nodes.is_empty() {
        return Err(BotError::MalformedFlow {
            cause: "flow declares no nodes".into(),
        });
    }
    if !spec.nodes.contains_key(&spec.entry) {
        return Err(BotError::InvalidTransitionTarget {
            from: "<entry>".into(),
            target: spec.entry.clone(),
        });
    }
    if spec.bounds.budget == 0 || spec.nodes.len() > spec.bounds.budget {
        return Err(BotError::FlowBudgetExceeded {
            steps: spec.nodes.len(),
            budget: spec.bounds.budget,
        });
    }

    validate_declarations(&spec.vars)?;
    validate_terminals(spec)?;

    let mut writers = BTreeSet::new();
    for (node_id, kind) in &spec.nodes {
        validate_node(spec, node_id, kind, effective, &mut writers)?;
    }
    for name in spec.vars.keys() {
        if !writers.contains(name) {
            return Err(BotError::VariableNeverWritten { name: name.clone() });
        }
    }
    for edge in &spec.edges {
        let (from, to) = edge.endpoints();
        if !spec.nodes.contains_key(from) {
            return Err(BotError::InvalidTransitionTarget {
                from: from.to_owned(),
                target: from.to_owned(),
            });
        }
        check_target(&spec.nodes, from, to)?;
    }
    reject_uninitialized_reads(spec)?;
    reject_unreachable(spec)
}

/// Reject every read of a variable that is not definitely assigned on all
/// paths reaching the reading node.
///
/// A global writer set answers "does some node write this variable", which is
/// not the question a read has to pass: a session starts with an empty
/// [`VarScope`], so a writer that executes only after the read, or only on a
/// branch the read is not reached through, leaves the read with no value and
/// [`TemplateInterpolator`] returning [`BotError::VariableUnset`] on the first
/// step. This is a forward *must* (definite-assignment) analysis over the
/// execution graph instead of a membership test over the document.
///
/// The state of a node is the set of variables assigned on **every** path from
/// the entry to that node, which is a greatest fixed point: every node is
/// seeded with the full declaration set and each step intersects an outgoing
/// state into its successors, so states only shrink and a cycle converges. A
/// node no entry path reaches keeps that top element and is never accused
/// here — reachability is [`reject_unreachable`]'s single verdict, and one
/// defect must not surface as two.
///
/// Supported assignments and guards, stated so the boundary is not guessed at:
/// an [`NodeKind::Ask`] assigns its variable on each outgoing route and
/// nowhere earlier; every other node kind passes its incoming state through
/// unchanged. No symbolic reasoning about [`Predicate`] values is attempted,
/// so a branch is treated as able to take both successors, and a variable
/// assigned on one predecessor of a join but not another is *not* available at
/// the join. Both choices fail toward silence rather than rejecting a flow the
/// runner would have executed.
fn reject_uninitialized_reads(spec: &FlowSpec) -> Result<(), BotError> {
    let universe: BTreeSet<String> = spec.vars.keys().cloned().collect();
    let mut available: BTreeMap<NodeId, BTreeSet<String>> = spec
        .nodes
        .keys()
        .map(|node_id| (node_id.clone(), universe.clone()))
        .collect();
    available.insert(spec.entry.clone(), BTreeSet::new());

    let mut pending: VecDeque<NodeId> = spec.nodes.keys().cloned().collect();
    while let Some(node_id) = pending.pop_front() {
        let Some(kind) = spec.nodes.get(&node_id) else {
            continue;
        };
        let Some(incoming) = available.get(&node_id).cloned() else {
            continue;
        };
        let mut outgoing = incoming;
        if let NodeKind::Ask { ref var, .. } = *kind {
            outgoing.insert(var.clone());
        }
        for target in successor_targets(spec, &node_id, kind) {
            let Some(state) = available.get_mut(&target) else {
                continue;
            };
            let before = state.len();
            state.retain(|name| outgoing.contains(name));
            if state.len() != before {
                pending.push_back(target);
            }
        }
    }

    for (node_id, kind) in &spec.nodes {
        let Some(state) = available.get(node_id) else {
            continue;
        };
        for name in node_reads(spec, node_id, kind)? {
            if !state.contains(&name) {
                return Err(BotError::VariableReadBeforeInit {
                    node: node_id.clone(),
                    name,
                });
            }
        }
    }
    Ok(())
}

/// Collect every variable a node reads when it executes.
///
/// "Reads" means the references the executor actually resolves, not every name
/// a node mentions. [`NodeKind::Branch`] is the case that separates the two:
/// its `var` field is documented as the branch subject, but the executor
/// destructures `when` alone and never resolves `var` against the scope, so a
/// branch subject that some path leaves unassigned cannot fail a run and is
/// not counted here. The predicate's own [`ValueExpr::Var`] references are
/// where a branch reads, and they are counted.
///
/// Only declared names are returned. An undeclared one is already
/// [`BotError::UndeclaredVariable`] from `validate_node`, which runs first, so
/// the definite-assignment pass cannot preempt it with a second verdict on the
/// same defect.
fn node_reads(
    spec: &FlowSpec,
    node_id: &str,
    kind: &NodeKind,
) -> Result<BTreeSet<String>, BotError> {
    match *kind {
        NodeKind::Say { ref text } => declared_template_reads(spec, node_id, text, "say.text"),
        NodeKind::Refer { ref text, .. } => {
            declared_template_reads(spec, node_id, text, "refer.text")
        }
        NodeKind::Branch { ref when, .. } => {
            let mut names = BTreeSet::new();
            collect_predicate_variables(when, &mut names);
            names.retain(|name| spec.vars.contains_key(name));
            Ok(names)
        }
        NodeKind::Ask { .. }
        | NodeKind::Handoff { .. }
        | NodeKind::Route { .. }
        | NodeKind::End => Ok(BTreeSet::new()),
    }
}

/// Return the declared variables a template interpolates.
fn declared_template_reads(
    spec: &FlowSpec,
    node_id: &str,
    text: &str,
    field: &'static str,
) -> Result<BTreeSet<String>, BotError> {
    let compiled =
        CompiledTemplate::compile(text).map_err(|_error| BotError::MalformedTemplate {
            node: node_id.to_owned(),
            field,
        })?;
    let mut names = BTreeSet::new();
    for part in compiled.parts() {
        if let TemplatePart::Variable(name) = *part
            && spec.vars.contains_key(name)
        {
            names.insert(name.to_owned());
        }
    }
    Ok(names)
}

/// Collect the variable references of a predicate expression.
fn collect_predicate_variables(predicate: &Predicate, into: &mut BTreeSet<String>) {
    match *predicate {
        Predicate::Const(_) => {}
        Predicate::Eq(ref left, ref right)
        | Predicate::Ne(ref left, ref right)
        | Predicate::Lt(ref left, ref right)
        | Predicate::Le(ref left, ref right)
        | Predicate::Gt(ref left, ref right)
        | Predicate::Ge(ref left, ref right) => {
            collect_expr_variables(left, into);
            collect_expr_variables(right, into);
        }
        Predicate::And(ref items) | Predicate::Or(ref items) => {
            for item in items {
                collect_predicate_variables(item, into);
            }
        }
        Predicate::Not(ref inner) => collect_predicate_variables(inner, into),
    }
}

/// Collect the variable references of one predicate operand.
fn collect_expr_variables(expression: &ValueExpr, into: &mut BTreeSet<String>) {
    if let ValueExpr::Var(ref name) = *expression {
        into.insert(name.clone());
    }
}

/// Validate variable declaration names and choice contents.
fn validate_declarations(vars: &BTreeMap<String, VarType>) -> Result<(), BotError> {
    for (name, kind) in vars {
        if !valid_identifier(name) {
            return Err(BotError::MalformedFlow {
                cause: format!("invalid variable name {name:?}"),
            });
        }
        match *kind {
            VarType::Choice(ref options)
                if options.is_empty() || has_duplicate_strings(options) =>
            {
                return Err(BotError::MalformedFlow {
                    cause: format!("choice variable {name:?} has invalid options"),
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// Validate one terminal declaration and its node kind.
///
/// Two invariants, and the second is the one that was missing: a declaration
/// must name a terminal node, and on a node whose kind already fixes the
/// outcome it must not contradict it. `End` completes with nothing said about
/// where the person goes, so every declared outcome is a genuine override and
/// is accepted. A `Handoff` or `Refer` node already names its target, so a
/// declaration of a different outcome — or of the same kind aimed somewhere
/// else — is an authoring contradiction between two statements in one
/// document, and refusing it is the only reading that cannot be wrong. A
/// declaration that repeats the node's own outcome is accepted; it is then
/// read by [`FlowSpec::effective_terminal`] rather than ignored.
fn validate_terminals(spec: &FlowSpec) -> Result<(), BotError> {
    for (node_id, declared) in &spec.terminals {
        let Some(kind) = spec.nodes.get(node_id) else {
            return Err(BotError::InvalidTransitionTarget {
                from: "<terminal>".into(),
                target: node_id.clone(),
            });
        };
        if !matches!(
            *kind,
            NodeKind::End | NodeKind::Handoff { .. } | NodeKind::Refer { .. }
        ) {
            return Err(BotError::MalformedFlow {
                cause: format!("terminal declaration {node_id:?} names a non-terminal node"),
            });
        }
        // `End` has an intrinsic outcome too, but it is `Completed` — the
        // absence of a destination rather than a claim about one — so a
        // declaration replaces it instead of conflicting with it. Only the two
        // node kinds that name a target can be contradicted.
        if matches!(*kind, NodeKind::End) {
            continue;
        }
        let Some(intrinsic) = spec.intrinsic_terminal(node_id) else {
            // Unreachable on a validated document: the kind was just checked to
            // be one of the three terminal kinds, and every one of them has an
            // intrinsic outcome. Reported rather than skipped so a future node
            // kind added to the check above cannot silently lose the rule.
            return Err(BotError::MalformedFlow {
                cause: format!("terminal declaration {node_id:?} has no intrinsic outcome"),
            });
        };
        if intrinsic != *declared {
            return Err(BotError::ConflictingTerminalDeclaration {
                node: node_id.clone(),
                intrinsic,
                declared: declared.clone(),
            });
        }
    }
    Ok(())
}

/// Validate one node's references, variables, and text.
fn validate_node(
    spec: &FlowSpec,
    node_id: &str,
    kind: &NodeKind,
    limits: ResourceLimits,
    writers: &mut BTreeSet<String>,
) -> Result<(), BotError> {
    match *kind {
        NodeKind::Say { ref text } => {
            validate_template(spec, node_id, text, "say.text", limits)?;
            if spec.edge_targets(node_id).len() > 1 {
                return Err(BotError::MalformedFlow {
                    cause: format!("say node {node_id:?} has multiple continuations"),
                });
            }
            if spec.edge_targets(node_id).is_empty() && !spec.terminals.contains_key(node_id) {
                return Err(BotError::MissingTransition {
                    node: node_id.to_owned(),
                });
            }
        }
        NodeKind::Ask {
            ref var,
            ref options,
            ref routes,
        } => {
            let declared = declared_variable(spec, var)?;
            writers.insert(var.clone());
            if options.is_empty() || has_duplicate_strings(options) {
                return Err(BotError::MalformedFlow {
                    cause: format!("ask node {node_id:?} has invalid options"),
                });
            }
            // Distinct strings are not distinct answers. Two options the answer
            // policy cannot tell apart are one option the person cannot choose
            // between, and the resolver can only ever report the tie — so the
            // authoring mistake is refused here, where it names an option,
            // rather than at run time, where it names none.
            if let Some((first, second)) = colliding_options(options, answer_domain_of(spec, var)) {
                return Err(BotError::MalformedFlow {
                    cause: format!(
                        "ask node {node_id:?} offers {first:?} and {second:?} as the same \
                         {} answer",
                        answer_domain_of(spec, var)
                    ),
                });
            }
            for option in options {
                let Some(target) = routes.get(option) else {
                    return Err(BotError::MissingAskRoute {
                        node: node_id.to_owned(),
                        option: option.clone(),
                    });
                };
                check_target(&spec.nodes, node_id, target)?;
                // Every candidate is decoded with the same decoder the runtime
                // assigns with, so "the flow asks a question the variable
                // cannot hold an answer to" is a load-time refusal rather than
                // a conversation the person cannot complete.
                let value = match declared.decode_answer(option) {
                    Ok(value) => value,
                    Err(rejection) => {
                        return Err(BotError::AskOptionNotAssignable {
                            node: node_id.to_owned(),
                            variable: var.clone(),
                            option: option.clone(),
                            expected: declared.label(),
                            cause: rejection.to_string(),
                        });
                    }
                };
                // Then that the value fits the session that will store it.
                // "The candidate cannot be held" and "the candidate cannot be
                // held *here*" are different statements, and both are knowable
                // while the document is loading: the second one is a session
                // whose value ceiling is below the option it asked for, and
                // deferring it to the person's answer would charge them a step
                // for an operator's configuration.
                let value_bytes = limits.get(ResourceAxis::Value);
                if value.rendered_bytes() > value_bytes {
                    return Err(BotError::AskOptionTooLarge {
                        node: node_id.to_owned(),
                        variable: var.clone(),
                        option: option.clone(),
                        bytes: value.rendered_bytes(),
                        limit: value_bytes,
                    });
                }
            }
            for (option, target) in routes {
                if !options.iter().any(|candidate| candidate == option) {
                    return Err(BotError::MalformedFlow {
                        cause: format!("ask node {node_id:?} routes undeclared option {option:?}"),
                    });
                }
                check_target(&spec.nodes, node_id, target)?;
            }
        }
        NodeKind::Branch {
            ref var,
            ref when,
            ref then,
            ref otherwise,
        } => {
            validate_variable_reference(spec, node_id, var)?;
            validate_predicate(spec, node_id, when)?;
            check_target(&spec.nodes, node_id, then)?;
            check_target(&spec.nodes, node_id, otherwise)?;
        }
        NodeKind::Handoff { .. } => {}
        NodeKind::Refer { ref text, .. } => {
            validate_template(spec, node_id, text, "refer.text", limits)?;
        }
        NodeKind::Route {
            ref dispatch,
            ref fallback,
        } => {
            check_target(&spec.nodes, node_id, dispatch)?;
            check_target(&spec.nodes, node_id, fallback)?;
        }
        NodeKind::End => {}
    }
    Ok(())
}

/// Returns how answers writing `name` are read in `spec`.
///
/// The flow's own reading of a node's answers, so validation and resolution
/// cannot disagree about which policy a question is under — the same reason
/// there is one decoder rather than two.
fn answer_domain_of(spec: &FlowSpec, name: &str) -> AnswerDomain {
    spec.vars
        .get(name)
        .map_or(AnswerDomain::Label, VarType::answer_domain)
}

/// Returns the first pair of options the answer policy cannot tell apart.
///
/// `None` under [`AnswerDomain::Integer`] for an option that is not a whole
/// number: an option that cannot produce the declared value is a different
/// defect, owned by the check that reports it, and folding it into a collision
/// report would name the wrong cause.
fn colliding_options(options: &[String], domain: AnswerDomain) -> Option<(String, String)> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for option in options {
        let identity = match domain {
            AnswerDomain::Label => crate::language::normalize(option),
            AnswerDomain::Integer => match decode_integer(option) {
                Some(value) => value.to_string(),
                None => continue,
            },
        };
        if let Some(first) = seen.get(&identity) {
            return Some((first.clone(), option.clone()));
        }
        seen.insert(identity, option.clone());
    }
    None
}

/// Validate a variable reference in a node.
fn validate_variable_reference(spec: &FlowSpec, node_id: &str, name: &str) -> Result<(), BotError> {
    if spec.vars.contains_key(name) {
        Ok(())
    } else {
        let _ = node_id;
        Err(BotError::UndeclaredVariable {
            name: name.to_owned(),
        })
    }
}

/// Resolve a variable's declared type, refusing an undeclared name.
fn declared_variable<'a>(spec: &'a FlowSpec, name: &str) -> Result<&'a VarType, BotError> {
    spec.vars
        .get(name)
        .ok_or_else(|| BotError::UndeclaredVariable {
            name: name.to_owned(),
        })
}

/// Validate a predicate's variable references and non-empty boolean lists.
fn validate_predicate(
    spec: &FlowSpec,
    node_id: &str,
    predicate: &Predicate,
) -> Result<(), BotError> {
    match *predicate {
        Predicate::Const(_) => Ok(()),
        Predicate::Eq(ref left, ref right)
        | Predicate::Ne(ref left, ref right)
        | Predicate::Lt(ref left, ref right)
        | Predicate::Le(ref left, ref right)
        | Predicate::Gt(ref left, ref right)
        | Predicate::Ge(ref left, ref right) => {
            validate_expr(spec, node_id, left)?;
            validate_expr(spec, node_id, right)
        }
        Predicate::And(ref items) | Predicate::Or(ref items) => {
            if items.is_empty() {
                return Err(BotError::MalformedFlow {
                    cause: format!("node {node_id:?} has an empty predicate"),
                });
            }
            for item in items {
                validate_predicate(spec, node_id, item)?;
            }
            Ok(())
        }
        Predicate::Not(ref inner) => validate_predicate(spec, node_id, inner),
    }
}

/// Validate one predicate expression.
fn validate_expr(spec: &FlowSpec, _node_id: &str, expression: &ValueExpr) -> Result<(), BotError> {
    if let ValueExpr::Var(ref name) = *expression
        && !spec.vars.contains_key(name)
    {
        return Err(BotError::UndeclaredVariable { name: name.clone() });
    }
    Ok(())
}

/// Validate one template's syntax, its declared names, and its static floor.
///
/// The floor is the part of the expansion that is known without running the
/// flow: the literal bytes, which every render must contain whatever the
/// variables turn out to hold. A template whose literals alone exceed the
/// record ceiling can never render, so it is refused here rather than at the
/// node that would speak it. That is the only static size claim available —
/// the rest of the expansion depends on answers this cannot see — and it is
/// exactly the claim that is true unconditionally.
fn validate_template(
    spec: &FlowSpec,
    node_id: &str,
    template: &str,
    field: &'static str,
    limits: ResourceLimits,
) -> Result<(), BotError> {
    let compiled =
        CompiledTemplate::compile(template).map_err(|_error| BotError::MalformedTemplate {
            node: node_id.to_owned(),
            field,
        })?;
    let mut literals = 0usize;
    for part in compiled.parts() {
        match *part {
            TemplatePart::Literal(text) => {
                literals = literals.checked_add(text.len()).ok_or(
                    BotError::TemplateExpansionTooLarge {
                        node: node_id.to_owned(),
                        bytes: usize::MAX,
                        limit: limits.get(ResourceAxis::Record),
                    },
                )?;
            }
            TemplatePart::Variable(name) => {
                if !spec.vars.contains_key(name) {
                    return Err(BotError::UndeclaredVariable {
                        name: name.to_owned(),
                    });
                }
            }
        }
    }
    let record_bytes = limits.get(ResourceAxis::Record);
    if literals > record_bytes {
        return Err(BotError::TemplateExpansionTooLarge {
            node: node_id.to_owned(),
            bytes: literals,
            limit: record_bytes,
        });
    }
    Ok(())
}

/// Validate that one target node exists.
fn check_target(
    nodes: &BTreeMap<NodeId, NodeKind>,
    from: &str,
    target: &str,
) -> Result<(), BotError> {
    if nodes.contains_key(target) {
        Ok(())
    } else {
        Err(BotError::InvalidTransitionTarget {
            from: from.to_owned(),
            target: target.to_owned(),
        })
    }
}

/// Reject a graph node that cannot be reached from the entry.
fn reject_unreachable(spec: &FlowSpec) -> Result<(), BotError> {
    let mut seen = BTreeSet::new();
    let mut pending = VecDeque::from([spec.entry.clone()]);
    while let Some(node_id) = pending.pop_front() {
        if !seen.insert(node_id.clone()) {
            continue;
        }
        let Some(kind) = spec.nodes.get(&node_id) else {
            continue;
        };
        for target in successor_targets(spec, &node_id, kind) {
            if !seen.contains(&target) {
                pending.push_back(target);
            }
        }
    }
    for node_id in spec.nodes.keys() {
        if !seen.contains(node_id) {
            return Err(BotError::UnreachableNode {
                node: node_id.clone(),
            });
        }
    }
    Ok(())
}

/// Collect every statically possible destination from one node.
fn successor_targets(spec: &FlowSpec, node_id: &str, kind: &NodeKind) -> Vec<NodeId> {
    let mut targets = spec.edge_targets(node_id);
    match *kind {
        NodeKind::Ask { ref routes, .. } => targets.extend(routes.values().cloned()),
        NodeKind::Branch {
            ref then,
            ref otherwise,
            ..
        } => {
            targets.push(then.clone());
            targets.push(otherwise.clone());
        }
        NodeKind::Route {
            ref dispatch,
            ref fallback,
        } => {
            targets.push(dispatch.clone());
            targets.push(fallback.clone());
        }
        NodeKind::Say { .. }
        | NodeKind::Handoff { .. }
        | NodeKind::Refer { .. }
        | NodeKind::End => {}
    }
    targets
}

/// Reject repeated strings in a finite declaration.
fn has_duplicate_strings(values: &[String]) -> bool {
    let mut seen = BTreeSet::new();
    values.iter().any(|value| !seen.insert(value))
}

/// Return whether a variable or node id uses the supported identifier shape.
fn valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Reject unknown tagged node kinds before serde turns them into a generic
/// malformed-document diagnostic.
fn reject_unknown_node_kinds(value: &JsonValue) -> Result<(), BotError> {
    let Some(nodes) = value.get("nodes").and_then(JsonValue::as_object) else {
        return Ok(());
    };
    for (node_id, node) in nodes {
        let Some(kind) = node.get("kind").and_then(JsonValue::as_str) else {
            continue;
        };
        if !matches!(
            kind,
            "say" | "ask" | "branch" | "handoff" | "refer" | "route" | "end"
        ) {
            return Err(BotError::UnknownNodeKind {
                node: node_id.clone(),
                kind: kind.to_owned(),
            });
        }
    }
    Ok(())
}

/// Named, typed variable declarations and their assigned values.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VarScope {
    /// Declared variable types.
    declarations: BTreeMap<String, VarType>,
    /// Values assigned by accepted answers.
    values: BTreeMap<String, Value>,
}

impl VarScope {
    /// Construct an empty-value scope from declarations.
    pub fn new(declarations: BTreeMap<String, VarType>) -> Result<Self, BotError> {
        validate_declarations(&declarations)?;
        Ok(Self {
            declarations,
            values: BTreeMap::new(),
        })
    }

    /// Return the declared variable types.
    #[must_use]
    pub fn declarations(&self) -> &BTreeMap<String, VarType> {
        &self.declarations
    }

    /// Returns how an answer writing `name` must be read.
    ///
    /// Defaults to [`AnswerDomain::Label`] for a name that is not declared,
    /// which is the permissive reading rather than a silent claim: flow
    /// validation rejects an undeclared writer, so reaching this with an unknown
    /// name is a caller's mistake and not a question about the answer.
    #[must_use]
    pub fn answer_domain(&self, name: &str) -> AnswerDomain {
        self.declarations
            .get(name)
            .map_or(AnswerDomain::Label, VarType::answer_domain)
    }

    /// Return all currently assigned values.
    #[must_use]
    pub fn values(&self) -> &BTreeMap<String, Value> {
        &self.values
    }

    /// Return one assigned value, if the variable has been written.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.values.get(name)
    }

    /// Assign a typed value after checking its declaration.
    pub fn set(&mut self, name: &str, value: Value) -> Result<(), BotError> {
        let Some(declared) = self.declarations.get(name) else {
            return Err(BotError::UndeclaredVariable {
                name: name.to_owned(),
            });
        };
        if value_matches_type(declared, &value) {
            self.values.insert(name.to_owned(), value);
            Ok(())
        } else {
            Err(BotError::VariableTypeMismatch {
                name: name.to_owned(),
            })
        }
    }

    /// Parse and assign an answer according to the variable declaration.
    ///
    /// Runs [`VarType::decode_answer`], the same decoder flow validation runs
    /// over every ask candidate. For a validated flow the decode cannot fail:
    /// the candidate that reaches here is one validation already accepted. It
    /// stays a `Result` because this method is public and a caller may hold a
    /// scope that no `FlowSpec` validated.
    pub fn set_from_answer(&mut self, name: &str, answer: &str) -> Result<(), BotError> {
        self.set_from_answer_within(name, answer, MAX_VALUE_BYTES)
    }

    /// Parse and assign an answer under an explicit value ceiling.
    ///
    /// The decoder and the store, plus the third step a session needs: the
    /// value has to fit. Sizing happens between decoding and storing, so a
    /// candidate that decodes is refused before the scope holds it — which is
    /// what keeps "accepted" and "retained" the same word.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::InvalidVariableValue`] for an answer the declared
    /// type cannot hold, and [`BotError::ValueTooLarge`] for one that decodes
    /// to a value wider than `value_bytes`.
    pub fn set_from_answer_within(
        &mut self,
        name: &str,
        answer: &str,
        value_bytes: usize,
    ) -> Result<(), BotError> {
        let Some(declared) = self.declarations.get(name) else {
            return Err(BotError::UndeclaredVariable {
                name: name.to_owned(),
            });
        };
        // One decoder, not two: `VarType::decode_answer` is the same reading of
        // an answer that flow validation runs, so a value this store accepts is
        // a value the declaration would have accepted. The variant is dropped
        // here because the scope's contract is a single "cannot hold this"
        // error; the distinction between a non-integer and an out-of-range one
        // is a resolver's business and is kept there.
        let value = declared.decode_answer(answer).map_err(|_rejection| {
            BotError::InvalidVariableValue {
                variable: name.to_owned(),
                value: answer.to_owned(),
            }
        })?;
        let bytes = value.rendered_bytes();
        if bytes > value_bytes {
            return Err(BotError::ValueTooLarge {
                variable: name.to_owned(),
                bytes,
                limit: value_bytes,
            });
        }
        self.set(name, value)
    }

    /// Interpolate `${name}` markers using the assigned values, under the
    /// operator's shipped record ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::MalformedTemplate`] for invalid syntax,
    /// [`BotError::VariableUnset`] for an unresolved marker, and
    /// [`BotError::TemplateExpansionTooLarge`] past the ceiling.
    pub fn interpolate(&self, template: &str) -> Result<String, BotError> {
        self.interpolate_within(template, MAX_RECORD_BYTES)
    }

    /// Interpolate `${name}` markers under an explicit byte ceiling.
    ///
    /// A scope on its own has no session behind it, so the ceiling is an
    /// argument. The expansion is sized before it is allocated and refused
    /// rather than truncated: a truncated record would be a record the journal
    /// claims happened and that nobody can read.
    ///
    /// # Errors
    ///
    /// As [`Self::interpolate`], with `record_bytes` in place of the shipped
    /// ceiling.
    pub fn interpolate_within(
        &self,
        template: &str,
        record_bytes: usize,
    ) -> Result<String, BotError> {
        TemplateInterpolator::new().interpolate(template, self, "<scope>", record_bytes)
    }
}

/// Check one scalar against its declared type.
fn value_matches_type(declared: &VarType, value: &Value) -> bool {
    match *declared {
        VarType::String => matches!(value, &Value::String(_)),
        VarType::Integer => matches!(value, &Value::Integer(_)),
        VarType::Boolean => matches!(value, &Value::Boolean(_)),
        VarType::Choice(ref options) => match *value {
            Value::Choice(ref value) => options.contains(value),
            _ => false,
        },
    }
}

/// One compiled piece of a template.
///
/// Borrowed from the document rather than owned: a compiled template is a
/// second view of bytes that are already resident, and copying them would make
/// compiling cost what the expansion costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TemplatePart<'a> {
    /// Text copied to the output unchanged.
    Literal(&'a str),
    /// A `${name}` marker replaced by the scope's value for `name`.
    Variable(&'a str),
}

/// A template compiled once into literal and variable parts.
///
/// Compilation is the step that makes a byte ceiling enforceable before
/// allocation. The parts are the template's placeholders resolved to nothing
/// but the names they mention, so the expanded size of the whole is one
/// addition per part, computed with checked arithmetic
/// ([`Self::expanded_bytes`]) and compared with the caller's ceiling *before*
/// [`Self::render`] allocates the output. Rendering an unbounded expansion to
/// find out that it is unbounded is exactly the failure this ordering removes.
///
/// # Examples
///
/// ```
/// use lgwks_bot::{CompiledTemplate, VarScope};
/// use std::collections::BTreeMap;
///
/// let scope = VarScope::new(BTreeMap::new())?;
/// let compiled = CompiledTemplate::compile("hello ${name}")?;
/// assert_eq!(compiled.parts().len(), 2);
/// // The name has no value, so the size cannot be known yet.
/// assert!(compiled.expanded_bytes(&scope).is_err());
/// # Ok::<(), lgwks_bot::BotError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CompiledTemplate<'a> {
    /// Literal and variable parts in source order.
    parts: Vec<TemplatePart<'a>>,
}

impl<'a> CompiledTemplate<'a> {
    /// Compile `${name}` markers into parts.
    ///
    /// The one template parser: the loader's template check uses it to confirm
    /// every marker names a declared variable, and the interpolator uses it to
    /// size and render. A second scanner would be a second grammar.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::MalformedTemplate`] for an unterminated marker or a
    /// name that is not an identifier.
    pub fn compile(template: &'a str) -> Result<Self, BotError> {
        let mut parts = Vec::new();
        let mut cursor = 0;
        while let Some(relative_start) = template[cursor..].find("${") {
            let start = cursor.saturating_add(relative_start);
            if start > cursor {
                parts.push(TemplatePart::Literal(&template[cursor..start]));
            }
            let name_start = start.saturating_add(2);
            let Some(relative_end) = template[name_start..].find('}') else {
                return Err(BotError::MalformedTemplate {
                    node: "<runtime>".into(),
                    field: "template",
                });
            };
            let end = name_start.saturating_add(relative_end);
            let name = &template[name_start..end];
            if !valid_identifier(name) {
                return Err(BotError::MalformedTemplate {
                    node: "<runtime>".into(),
                    field: "template",
                });
            }
            parts.push(TemplatePart::Variable(name));
            cursor = end.saturating_add(1);
        }
        if cursor < template.len() {
            parts.push(TemplatePart::Literal(&template[cursor..]));
        }
        Ok(Self { parts })
    }

    /// Return the compiled parts in source order.
    #[must_use]
    pub fn parts(&self) -> &[TemplatePart<'a>] {
        &self.parts
    }

    /// The exact number of bytes this template expands to under `scope`.
    ///
    /// Exact, and computed before any output is allocated. Every part
    /// contributes a known count, so the total is a checked sum and cannot
    /// overflow into a small number that would pass a ceiling it should fail.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::VariableUnset`] for a marker with no value, or
    /// [`BotError::TemplateExpansionTooLarge`] with `bytes` of [`usize::MAX`]
    /// if the sum overflows, which no real expansion can and which is reported
    /// rather than wrapped.
    pub fn expanded_bytes(&self, scope: &VarScope) -> Result<usize, BotError> {
        let mut total = 0usize;
        for part in &self.parts {
            let bytes = match *part {
                TemplatePart::Literal(text) => text.len(),
                TemplatePart::Variable(name) => {
                    let Some(value) = scope.get(name) else {
                        return Err(BotError::VariableUnset {
                            name: name.to_owned(),
                        });
                    };
                    value.rendered_bytes()
                }
            };
            total = total
                .checked_add(bytes)
                .ok_or(BotError::TemplateExpansionTooLarge {
                    node: "<runtime>".into(),
                    bytes: usize::MAX,
                    limit: usize::MAX,
                })?;
        }
        Ok(total)
    }

    /// Render the template into a new string.
    ///
    /// Allocates exactly [`Self::expanded_bytes`] and cannot fail where that
    /// did not: every marker was resolved during sizing, and the values are
    /// re-read from the same scope. Callers with a ceiling call
    /// [`Self::expanded_bytes`] first — [`Interpolate::interpolate`] is that
    /// caller — so the refusal happens before this runs rather than after it
    /// has produced the text it was supposed to avoid producing.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::VariableUnset`] if a value disappeared between
    /// sizing and rendering. A scope is not mutated during a render, so this is
    /// reachable only through a caller that interleaves the two.
    pub fn render(&self, scope: &VarScope) -> Result<String, BotError> {
        let mut output = String::new();
        for part in &self.parts {
            match *part {
                TemplatePart::Literal(text) => output.push_str(text),
                TemplatePart::Variable(name) => {
                    let Some(value) = scope.get(name) else {
                        return Err(BotError::VariableUnset {
                            name: name.to_owned(),
                        });
                    };
                    output.push_str(&value.to_string());
                }
            }
        }
        Ok(output)
    }
}

/// One question's candidate vocabulary, as the resolver seam sees it.
///
/// A resolver is handed three things and a slice of options is only two of
/// them: what the person typed, the options in front of them, and the *identity
/// of the question* those options belong to. The identity is not decoration. A
/// learned alias is a confirmed fact about a question — *in this question, "the
/// usual" means "Repeat last order"* — and a slice of strings is not a question:
/// the same slice can appear in two different questions, and the same question
/// offers a *different* slice after an edit or a reorder. Carrying the identity
/// alongside the options is what lets a confirmation follow its option to a new
/// position, and refuse to follow whatever occupies its old one.
///
/// Borrowed rather than owned, because the caller already holds both: an owned
/// copy would be a second copy of the same fact, and two copies of a fact drift.
///
/// The third fact is how the answers are read. [`AnswerDomain`] travels with the
/// options because it is a property of *this* question, and a resolver that had
/// to infer it from the option strings would be guessing: the same list is two
/// distinct answers at an integer question and one folded answer at a label
/// question.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Question<'a> {
    /// Stable identity of the question. In a flow this is the ask node id.
    id: &'a str,
    /// The options offered right now.
    options: &'a [String],
    /// How this question's answers are meant to be read.
    domain: AnswerDomain,
}

impl<'a> Question<'a> {
    /// Names one question's vocabulary, read as natural-language labels.
    #[must_use]
    pub const fn new(id: &'a str, options: &'a [String]) -> Self {
        Self {
            id,
            options,
            domain: AnswerDomain::Label,
        }
    }

    /// Reads this question's answers under `domain`.
    ///
    /// A builder rather than a fourth argument on [`Self::new`], because the
    /// overwhelming majority of questions are label questions and a resolver
    /// call site should not have to spell that out. The default is the one the
    /// option strings are written in; a caller that has a typed variable in hand
    /// has the one fact that changes it.
    #[must_use]
    pub const fn with_domain(self, domain: AnswerDomain) -> Self {
        Self { domain, ..self }
    }

    /// Returns the question's stable identity.
    #[must_use]
    pub const fn id(&self) -> &'a str {
        self.id
    }

    /// Returns the options offered right now.
    #[must_use]
    pub const fn options(&self) -> &'a [String] {
        self.options
    }

    /// Returns how this question's answers are meant to be read.
    #[must_use]
    pub const fn domain(&self) -> AnswerDomain {
        self.domain
    }
}

/// Interpolation seam for replacing the template engine later.
pub trait Interpolate {
    /// Expand `${name}` markers using the supplied scope, refusing an
    /// expansion larger than `limit` bytes.
    ///
    /// `limit` is a parameter rather than a property of the interpolator
    /// because the ceiling is the session's, not the engine's: the same
    /// interpolator serves a scope with the shipped ceiling and a session with
    /// a tighter one. `node` names the flow node the template came from and is
    /// carried into the refusal, which is what makes an oversized expansion
    /// attributable to the line that caused it.
    ///
    /// Implementations must check the size *before* allocating the output.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::MalformedTemplate`] for invalid syntax,
    /// [`BotError::VariableUnset`] for an unresolved marker, and
    /// [`BotError::TemplateExpansionTooLarge`] for an expansion over `limit`.
    fn interpolate(
        &self,
        template: &str,
        scope: &VarScope,
        node: &str,
        limit: usize,
    ) -> Result<String, BotError>;
}

/// The shipped bounded interpolation implementation.
///
/// Allocation-bounded in the strict sense: the expanded byte count is computed
/// from the compiled template and refused when it exceeds the caller's ceiling,
/// so the output buffer this creates is never larger than that ceiling no
/// matter how many times a placeholder repeats.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct TemplateInterpolator;

impl TemplateInterpolator {
    /// Construct the default interpolator.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Interpolate for TemplateInterpolator {
    fn interpolate(
        &self,
        template: &str,
        scope: &VarScope,
        node: &str,
        limit: usize,
    ) -> Result<String, BotError> {
        let compiled = CompiledTemplate::compile(template).map_err(|error| match error {
            // The compile-time diagnostics name the runtime as their node,
            // because the compiler is not handed the node it is compiling for.
            // Here it is known, so it replaces the placeholder.
            BotError::MalformedTemplate { field, .. } => BotError::MalformedTemplate {
                node: node.to_owned(),
                field,
            },
            other => other,
        })?;
        let bytes = compiled.expanded_bytes(scope)?;
        if bytes > limit {
            return Err(BotError::TemplateExpansionTooLarge {
                node: node.to_owned(),
                bytes,
                limit,
            });
        }
        compiled.render(scope)
    }
}

/// Free-text option resolver seam.
pub trait Resolver {
    /// Resolves an utterance against the question in front of the person.
    ///
    /// Returns a [`Verdict`] — the [`Resolution`] together with the policy and
    /// model that produced it — and never an `Option<usize>`. The two-way form
    /// collapses *nothing matched* and *several matched equally well* into one
    /// `None`, and the second is the dangerous one: an answer that fits two
    /// options as well as each other is reported as unrecognized, so the
    /// session re-asks the full list and the identical answer resolves the
    /// identical way forever. A caller that cannot see the tie cannot narrow.
    ///
    /// The provenance is part of the return value rather than a second call,
    /// because *this score came from that model under that rule* is one fact.
    /// A caller that has to ask a resolver afterwards what it just did is
    /// re-deriving a fact it was already handed, and the derivation is only as
    /// good as the resolver's willingness to keep answering the same way.
    /// A question is more than its option list — see [`Question`] for why the
    /// identity travels with the options.
    fn resolve(&self, utterance: &str, question: &Question<'_>) -> Verdict;
}

/// The identity of the decision policy that produced a verdict.
///
/// Two fields rather than one string, because they answer different questions.
/// `label` names the tier whose parameters were in force, so a reader knows
/// what kind of decision this was before reading any score. `revision` is a
/// digest over the parameters themselves, so a threshold that moved is a
/// different version **without anyone remembering to bump a number by hand**:
/// a hand-bumped version is a promise, and a receipt cannot rest on a promise.
///
/// The parameters are hashed by their bit patterns rather than by their decimal
/// renderings, so two thresholds differing in the last bit are two versions and
/// no rounding stands between the value used and the value recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct PolicyVersion {
    /// The tier whose policy was in force, such as `lexicon` or `semantic`.
    label: String,
    /// Content digest over the parameters in force, as lowercase hex.
    revision: String,
}

impl PolicyVersion {
    /// Versions a policy from its label and the parameters in force.
    ///
    /// The label is hashed in as well, so two tiers that happen to declare the
    /// same parameters still have different revisions: a revision alone
    /// identifies one policy, which is what lets it be compared without reading
    /// the label beside it.
    #[must_use]
    pub fn new(label: impl Into<String>, parameters: &[f64]) -> Self {
        let label = label.into();
        let mut hasher = Hasher::new();
        hasher.update(label.as_bytes());
        for parameter in parameters {
            hasher.update(&parameter.to_bits().to_le_bytes());
        }
        Self {
            revision: hasher.finalize().to_hex(),
            label,
        }
    }

    /// Returns the tier label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the content digest over the policy's parameters.
    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }
}

impl fmt::Display for PolicyVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.label, self.revision)
    }
}

/// What produced a verdict: the policy in force, and the model if there was one.
///
/// The two travel together because they are one fact. A receipt that named a
/// score and a tier but not the rule that scored it cannot answer the question
/// a score actually raises, which is *would this have decided the same way
/// yesterday*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct Provenance {
    /// The decision policy in force when the verdict was reached.
    policy: PolicyVersion,
    /// The model consulted, when the verdict came from one.
    model: Option<EmbedderIdentity>,
}

impl Provenance {
    /// The deterministic tiers decided; no model was consulted.
    #[must_use]
    pub fn without_model(policy: PolicyVersion) -> Self {
        Self {
            policy,
            model: None,
        }
    }

    /// A model was consulted under a policy, whether or not it answered.
    ///
    /// A degraded verdict names its model deliberately. [`DegradedReason`] says
    /// the resolver never reached a verdict, and *which* model was unavailable
    /// is exactly what the repair needs; a record that dropped the identity on
    /// the failure path would describe the one case an operator has to act on
    /// as though no model had been involved.
    #[must_use]
    pub fn with_model(policy: PolicyVersion, model: EmbedderIdentity) -> Self {
        Self {
            policy,
            model: Some(model),
        }
    }

    /// Returns the policy in force.
    #[must_use]
    pub fn policy(&self) -> &PolicyVersion {
        &self.policy
    }

    /// Returns the model consulted, or `None` when none was.
    ///
    /// `None` is a finding rather than a missing value: the deterministic tiers
    /// answered, and naming a model here would claim a comparison that never
    /// happened.
    #[must_use]
    pub fn model(&self) -> Option<&EmbedderIdentity> {
        self.model.as_ref()
    }
}

/// A resolver's answer: the verdict, and what produced it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Verdict {
    /// The three-way verdict.
    resolution: Resolution,
    /// The policy and model behind it.
    provenance: Provenance,
}

impl Verdict {
    /// Pairs a resolution with what produced it.
    #[must_use]
    pub const fn new(resolution: Resolution, provenance: Provenance) -> Self {
        Self {
            resolution,
            provenance,
        }
    }

    /// Returns the verdict.
    #[must_use]
    pub const fn resolution(&self) -> &Resolution {
        &self.resolution
    }

    /// Returns what produced the verdict.
    #[must_use]
    pub const fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// Consumes the verdict and returns the resolution alone.
    #[must_use]
    pub fn into_resolution(self) -> Resolution {
        self.resolution
    }
}

/// Which match tier produced a resolution.
///
/// Reported rather than inferred, because it is what makes a wrong resolution
/// explicable and therefore repairable. An `Exact` miss is a learned alias
/// pointing at the wrong option; a `Phonetic` miss is the English bias of
/// [`crate::language::phonetic_key`]; a `Fuzzy` miss is a threshold. Three
/// different repairs, and a bare `usize` names none of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MatchTier {
    /// The utterance matched exactly, directly or through a learned alias.
    Exact,
    /// The utterance and the option reduce to the same sound-alike key.
    Phonetic,
    /// The utterance and the option are lexically close.
    Fuzzy,
    /// The utterance and the option are close in a model's embedding space.
    Semantic,
}

/// Why a resolver could not reach a verdict.
///
/// A closed set rather than a string, so a caller can branch on the cause
/// without parsing prose, and so the set of ways a resolver can fail is stated
/// where a reviewer reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DegradedReason {
    /// The semantic tier's embedder returned an error, or a vector of the
    /// wrong length.
    EmbedderUnavailable,
    /// An embedding came back as a value no angle can be computed from, so a
    /// comparison the decision needs was never made.
    ///
    /// The embedder answered, and its answer is not a direction: an all-zero
    /// vector, or one carrying `NaN` or an infinity. That is a different
    /// failure from [`Self::EmbedderUnavailable`] and has a different repair —
    /// nothing is down, one of the vectors is degenerate — which is why it is
    /// not folded into it.
    ///
    /// The finer cause is carried by [`lgwks_std::similarity::CosineError`],
    /// which separates a zero magnitude from a non-finite one. It is not
    /// repeated here because the routing is identical: a comparison set missing
    /// even one member cannot produce the `Resolved` or `Absent` verdict a
    /// complete set produces, so the session re-asks and records the cause
    /// whichever of the two it was.
    UnmeasurableEmbedding,
}

impl fmt::Display for DegradedReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::EmbedderUnavailable => formatter.write_str("the embedder is unavailable"),
            Self::UnmeasurableEmbedding => {
                formatter.write_str("an embedding could not be measured")
            }
        }
    }
}

/// The outcome of resolving an utterance against the candidate options.
///
/// Serializable, and not only for convenience: this is the shape a
/// [`DecisionReceipt`] carries verbatim, so a receipt cannot hold a verdict the
/// journal could not write down.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Resolution {
    /// One option matched and led the runner-up by the required margin.
    Resolved {
        /// Index of the selected option.
        index: usize,
        /// The tier that produced the match.
        tier: MatchTier,
        /// The match score.
        score: f64,
        /// The lead held over the runner-up *within the winning tier*.
        ///
        /// Within, not across: tier scores are not comparable, and a resolver
        /// that applied precedence has already excluded every lower tier from
        /// the comparison. A `lead` of `1.0` on a lone exact candidate therefore
        /// says "nothing in the exact tier contested this", not "nothing at all
        /// came close".
        lead: f64,
    },
    /// Options matched but none led by the required margin.
    ///
    /// The caller narrows its re-ask to [`Self::Ambiguous::tied`] rather than
    /// repeating the full list. This is the state `Option<usize>` cannot
    /// express, and the one that made a repeated ambiguous answer a loop.
    Ambiguous {
        /// Every option index still in play, lowest first.
        tied: Vec<usize>,
        /// The tier whose candidates tied.
        ///
        /// Reported for the reason [`Self::Resolved::tier`] is: two exact
        /// candidates that fold to one normalized form, two phonetic candidates
        /// that share a sound-alike key, and two fuzzy candidates inside a
        /// margin are three different ties with three different repairs, and a
        /// bare list of indices names none of them.
        tier: MatchTier,
        /// The highest score observed.
        score: f64,
    },
    /// No option matched.
    Absent {
        /// The highest score observed.
        best_score: f64,
    },
    /// The person used a phrase whose confirmed meaning has been withdrawn.
    ///
    /// Distinct from [`Self::Absent`] in the same way [`Self::Degraded`] is, and
    /// for the same reason: the two render identically in a transcript and need
    /// different repairs. `Absent` says the person's words did not fit the
    /// options and a rephrase may help. `StaleAlias` says the words *did* have a
    /// confirmed meaning, at this question, and the option it was bound to is no
    /// longer offered — so the answer is not a rephrase but a conversation, and
    /// the operator reading the record is the one who can have it.
    ///
    /// Returning this rather than a fuzzy reading is the whole point: the
    /// binding is authority a person granted, and re-spending it on whatever now
    /// occupies the old option's position would be using a confirmation the
    /// person never gave. The persona's own reading of the phrase still stands
    /// when the current list produces one; this verdict appears where the
    /// superseded binding is the only thing that would have matched.
    StaleAlias {
        /// The question whose vocabulary the confirmation was made in.
        question: String,
        /// The option text the confirmation bound, which is no longer offered.
        option: String,
    },
    /// The resolver could not reach a verdict at all.
    ///
    /// This is [`Self::Absent`]'s neighbour and not a spelling of it. `Absent`
    /// means the options were all considered and none fit: the person was not
    /// understood, and asking again in different words can help. `Degraded`
    /// means the resolver never got to consider them: a dependency it needs is
    /// unavailable, or answered with a value no comparison can be drawn from,
    /// and asking the person again cannot help — the same utterance will
    /// degrade identically.
    ///
    /// Carries no `best_score`, deliberately. There is no score; a failed
    /// resolver observed no similarity, and reporting `0.0` would be a
    /// measurement that was never taken. It is the same two-valued record
    /// [`Self::Ambiguous`] exists to remove — *nothing matched* versus *may have
    /// matched, and we could not look* — arriving a fourth time.
    ///
    /// That is also why an incomplete comparison set degrades instead of
    /// deciding: a winner measured against fewer competitors than the list
    /// holds is a winner over a field that was never fully looked at, and the
    /// margin it reports would be the whole of its own score.
    Degraded {
        /// The cause, for the caller to record and route on.
        reason: DegradedReason,
    },
}

/// Schema version of the [`DecisionReceipt`] this crate writes.
///
/// Bumped when a field's meaning changes or a field is removed. A reader that
/// finds a version it does not know can refuse the record rather than
/// misinterpret it, which is the only reason to version it at all: a receipt
/// outlives the code that wrote it, and the code that reads it is not always
/// the same revision.
pub const RECEIPT_VERSION: u32 = 1;

/// One structured record of a decision, written as part of the transition it
/// describes.
///
/// The receipt exists because a transcript cannot be the audit record. A
/// transcript is text: it says a person said something and the session moved,
/// and it cannot say which option was considered, on whose authority the
/// comparison was made, or whether a score of nothing meant *no similarity* or
/// *no measurement*. Those are the questions a run has to be able to answer
/// afterwards, and each field here is one of them:
///
/// - `session` and `flow` — which session, running which revision of the flow.
///   A receipt that names a node but not the document it came from cannot be
///   read against the flow a reader has on disk.
/// - `node` and `options` — where the decision was taken, and the *ordered*
///   candidate set it was taken against. The option list is digested rather
///   than counted, because a reordering is a different question with a
///   different right answer.
/// - `resolution` — the verdict verbatim, including the tier. A score is not
///   comparable across tiers, so a receipt holding a score without its tier
///   holds an unreadable number.
/// - `provenance` — the policy in force, and the model when one was consulted.
/// - `selected` and `route` — what was chosen, by the option's own text, and
///   where the session went. The text is what survives a reordering; the route
///   is what the session actually did.
///
/// Fields that were not measured are absent rather than zero. A [`Resolution`]
/// carries a score where a score was observed, and a [`Resolution::Degraded`]
/// carries none at all, which is the difference between *the options were
/// compared and nothing was close* and *nothing was compared*.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct DecisionReceipt {
    /// Schema version; see [`RECEIPT_VERSION`].
    version: u32,
    /// The session the decision was taken in.
    session: SessionId,
    /// Digest of the flow document the session is running.
    flow: String,
    /// The ask node the decision was taken at.
    node: NodeId,
    /// Digest of the ordered candidate options the verdict was reached against.
    options: String,
    /// The verdict, verbatim.
    resolution: Resolution,
    /// The policy in force, and the model when one was consulted.
    provenance: Provenance,
    /// The selected option's own text, when the verdict selected one.
    ///
    /// The text rather than the index: an index is a position in a list that
    /// can be reordered, and the text is the only thing that still names the
    /// choice once it is.
    selected: Option<String>,
    /// The node the accepted transition moved to, when it moved.
    route: Option<NodeId>,
}

impl DecisionReceipt {
    /// Returns the schema version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// Returns the session identity.
    #[must_use]
    pub fn session(&self) -> &SessionId {
        &self.session
    }

    /// Returns the flow revision digest.
    #[must_use]
    pub fn flow(&self) -> &str {
        &self.flow
    }

    /// Returns the ask node the decision was taken at.
    #[must_use]
    pub fn node(&self) -> &str {
        &self.node
    }

    /// Returns the digest of the ordered candidate options.
    #[must_use]
    pub fn options(&self) -> &str {
        &self.options
    }

    /// Returns the verdict.
    #[must_use]
    pub fn resolution(&self) -> &Resolution {
        &self.resolution
    }

    /// Returns the policy and model behind the verdict.
    #[must_use]
    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// Returns the selected option's text, when the verdict selected one.
    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Returns the node the accepted transition moved to, when it moved.
    #[must_use]
    pub fn route(&self) -> Option<&str> {
        self.route.as_deref()
    }
}

/// Where a journal accepted a decision receipt.
///
/// Reported by the sink rather than assumed by the session, because the two are
/// different facts and only the sink knows which one is true. A receipt held in
/// memory is evidence for the lifetime of the process; a receipt held durably
/// is evidence after it. A run that could not tell them apart would put the
/// second claim on the first record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReceiptAcceptance {
    /// Held only in this process, and gone when it ends.
    InMemory,
    /// Written to a sink that outlives the process.
    Durable,
}

/// The failure a journal reports when it cannot accept a receipt.
///
/// A named sink and its own description rather than a closed enum of causes:
/// the failure belongs to the sink, which owns its vocabulary — a full disk, a
/// refused connection, a closed file — and this crate cannot enumerate another
/// component's failures without inventing a taxonomy the sink would then have
/// to translate into. What is fixed here is that the failure crosses the seam
/// as a value the session must handle, rather than a diagnostic it may print
/// and continue past.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct JournalError {
    /// The sink that refused the receipt.
    sink: String,
    /// The sink's own description of the failure.
    cause: String,
}

impl JournalError {
    /// Records a sink's failure to accept a receipt.
    #[must_use]
    pub fn new(sink: impl Into<String>, cause: impl Into<String>) -> Self {
        Self {
            sink: sink.into(),
            cause: cause.into(),
        }
    }

    /// Returns the sink that refused.
    #[must_use]
    pub fn sink(&self) -> &str {
        &self.sink
    }

    /// Returns the sink's description of the failure.
    #[must_use]
    pub fn cause(&self) -> &str {
        &self.cause
    }
}

impl fmt::Display for JournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "journal {} refused a decision receipt: {}",
            self.sink, self.cause
        )
    }
}

impl std::error::Error for JournalError {}

/// One receipt, and the way the journal accepted it.
///
/// The pair rather than a receipt carrying its own acceptance: the receipt is
/// what both sides hold, and the acceptance is the journal's answer about it.
/// Folding the answer into the record the journal was already given would mean
/// the two copies differ on the field that matters most.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RecordedDecision {
    /// The receipt the journal accepted.
    receipt: DecisionReceipt,
    /// Where the journal said it accepted it.
    acceptance: ReceiptAcceptance,
}

impl RecordedDecision {
    /// Returns the receipt.
    #[must_use]
    pub fn receipt(&self) -> &DecisionReceipt {
        &self.receipt
    }

    /// Returns where the journal accepted it.
    #[must_use]
    pub const fn acceptance(&self) -> ReceiptAcceptance {
        self.acceptance
    }
}

/// Journal seam for session path records.
pub trait Journal {
    /// Receive a path node, role label, and rendered text record.
    fn record(&mut self, path_node: &str, role: &str, text: &str);

    /// Accept one decision receipt, reporting where it was accepted.
    ///
    /// Fallible, and required rather than defaulted. [`Self::record`] cannot
    /// report persistence failure, and neither could a defaulted version of
    /// this method: a sink that has to do nothing to look successful is a sink
    /// that silently drops every receipt, and the session would report an
    /// accepted transition whose audit record does not exist. Requiring it
    /// makes every implementation state what it does with a receipt — including
    /// [`MemoryJournal`], which says `InMemory` in as many words.
    ///
    /// `Err` aborts the transition. A receipt is not a log line written beside
    /// the work; it is part of accepting the answer, so a session that cannot
    /// record the decision has not taken it.
    fn record_decision(
        &mut self,
        receipt: &DecisionReceipt,
    ) -> Result<ReceiptAcceptance, JournalError>;
}

/// One in-memory transcript record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", deny_unknown_fields)]
#[non_exhaustive]
pub struct TranscriptEntry {
    /// Node id at which the record was produced.
    path_node: String,
    /// Role label, such as `assistant` or `user`.
    role: String,
    /// Rendered record text.
    text: String,
}

impl TranscriptEntry {
    /// Return the path node id.
    #[must_use]
    pub fn path_node(&self) -> &str {
        &self.path_node
    }

    /// Return the role label.
    #[must_use]
    pub fn role(&self) -> &str {
        &self.role
    }

    /// Return the rendered record text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Bounded-by-session in-memory journal implementation.
///
/// `PartialEq` but not `Eq`: the receipts it holds carry the scores a verdict
/// was reached on, and `f64` is not `Eq`. That is a property of the record
/// rather than a gap — `NaN` is not a score any tier produces, and `DecisionReceipt`
/// has no `Eq` for the same reason.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct MemoryJournal {
    /// Records in insertion order.
    records: Vec<TranscriptEntry>,
    /// Decision receipts in insertion order.
    receipts: Vec<DecisionReceipt>,
}

impl MemoryJournal {
    /// Construct an empty journal.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            records: Vec::new(),
            receipts: Vec::new(),
        }
    }

    /// Return records in insertion order.
    #[must_use]
    pub fn records(&self) -> &[TranscriptEntry] {
        &self.records
    }

    /// Return decision receipts in insertion order.
    #[must_use]
    pub fn receipts(&self) -> &[DecisionReceipt] {
        &self.receipts
    }

    /// Consume the journal and return its records.
    #[must_use]
    pub fn into_records(self) -> Vec<TranscriptEntry> {
        self.records
    }

    /// Consume the journal and return its decision receipts.
    #[must_use]
    pub fn into_receipts(self) -> Vec<DecisionReceipt> {
        self.receipts
    }
}

impl Journal for MemoryJournal {
    fn record(&mut self, path_node: &str, role: &str, text: &str) {
        self.records.push(TranscriptEntry {
            path_node: path_node.to_owned(),
            role: role.to_owned(),
            text: text.to_owned(),
        });
    }

    /// Accepts a receipt, and says so: `InMemory` is the honest answer for a
    /// sink whose whole storage is a `Vec` in this process. A caller that needs
    /// a durable record has to install a journal that says `Durable`, and can
    /// see from the receipt that this one did not.
    fn record_decision(
        &mut self,
        receipt: &DecisionReceipt,
    ) -> Result<ReceiptAcceptance, JournalError> {
        self.receipts.push(receipt.clone());
        Ok(ReceiptAcceptance::InMemory)
    }
}

/// Public identity of one in-memory session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
#[non_exhaustive]
pub struct SessionId(String);

impl SessionId {
    /// Construct an id from its stable string representation.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Return the stable string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SessionId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for SessionId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Synchronous cursor over one validated flow and its conversation state.
#[non_exhaustive]
pub struct Session {
    /// Stable session identity.
    id: SessionId,
    /// Validated flow document owned by this session.
    flow: FlowSpec,
    /// Digest of the flow document, computed once at construction.
    flow_revision: String,
    /// Current node, or `None` after terminal completion.
    current: Option<NodeId>,
    /// Typed variable scope.
    scope: VarScope,
    /// Nodes executed in order, including repeated cycle visits.
    visited: Vec<NodeId>,
    /// Transcript mirrored to the journal seam.
    transcript: Vec<TranscriptEntry>,
    /// Pluggable free-text resolver.
    resolver: Box<dyn Resolver>,
    /// Pluggable record sink.
    journal: Box<dyn Journal>,
    /// Decision receipts in insertion order, with how each was accepted.
    decisions: Vec<RecordedDecision>,
    /// Terminal outcome, if the cursor has stopped.
    terminal: Option<Terminal>,
    /// Number of charged execution and answer steps.
    steps: usize,
    /// Last accepted answer, used by [`NodeKind::Route`].
    last_utterance: Option<String>,
    /// Effective byte ceilings for this session.
    limits: ResourceLimits,
    /// Bytes retained so far: every transcript record and visited node id.
    retained: usize,
}

impl Session {
    /// Start a session with the default language resolver and memory journal.
    pub fn new(id: impl Into<SessionId>, flow: FlowSpec) -> Result<Self, BotError> {
        Self::with_components(
            id,
            flow,
            crate::language::LanguageResolver::new(),
            MemoryJournal::new(),
        )
    }

    /// Start a session with a custom resolver and the default memory journal.
    pub fn with_resolver<R>(
        id: impl Into<SessionId>,
        flow: FlowSpec,
        resolver: R,
    ) -> Result<Self, BotError>
    where
        R: Resolver + 'static,
    {
        Self::with_components(id, flow, resolver, MemoryJournal::new())
    }

    /// Start a session with a custom resolver, a custom journal, and explicit
    /// byte ceilings.
    ///
    /// The operator's knob. `limits` may be tighter than
    /// [`ResourceLimits::shipped`] and may not be looser; a value above the
    /// shipped ceiling is refused with [`BotError::ResourceLimitAboveCeiling`]
    /// before the flow is even examined. What the session ends up enforcing is
    /// the narrower of `limits` and whatever the document asks for in its
    /// [`FlowBounds`], per axis, so neither side can widen what the other
    /// narrowed.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::ResourceLimitAboveCeiling`] for a ceiling above the
    /// operator's, and every error [`Self::with_components`] returns.
    pub fn with_components_and_limits<R, J>(
        id: impl Into<SessionId>,
        flow: FlowSpec,
        resolver: R,
        journal: J,
        limits: ResourceLimits,
    ) -> Result<Self, BotError>
    where
        R: Resolver + 'static,
        J: Journal + 'static,
    {
        // Both sets are checked against the ceiling and then narrowed against
        // each other, in that order: a request above the ceiling is a refusal
        // wherever it comes from, and two in-range requests combine by taking
        // the smaller. Doing it the other way round would let a document's
        // in-range tightening hide an operator's out-of-range one.
        let operator = limits.within_ceiling()?;
        let requested = flow.bounds.resources.unwrap_or_default().within_ceiling()?;
        let effective = operator.narrowed(requested);
        // Re-validated here rather than trusted from construction, because the
        // public `json` façade parses a `FlowSpec` without calling
        // `FlowSpec::from_json`, and because this is the point at which the
        // effective ceilings — the ones this session will actually enforce —
        // become known to validation.
        validate_flow_within(&flow, effective)?;
        let scope = VarScope::new(flow.vars.clone())?;
        let flow_revision = revision_of(&flow)?;
        let mut session = Self {
            id: id.into(),
            current: Some(flow.entry.clone()),
            flow,
            flow_revision,
            scope,
            visited: Vec::new(),
            transcript: Vec::new(),
            resolver: Box::new(resolver),
            journal: Box::new(journal),
            decisions: Vec::new(),
            terminal: None,
            steps: 0,
            last_utterance: None,
            limits: effective,
            retained: 0,
        };
        session.drive()?;
        Ok(session)
    }

    /// Start a session with explicit byte ceilings and the default seams.
    ///
    /// # Errors
    ///
    /// As [`Self::with_components_and_limits`].
    pub fn with_limits(
        id: impl Into<SessionId>,
        flow: FlowSpec,
        limits: ResourceLimits,
    ) -> Result<Self, BotError> {
        Self::with_components_and_limits(
            id,
            flow,
            crate::language::LanguageResolver::new(),
            MemoryJournal::new(),
            limits,
        )
    }

    /// Start a session with both replacement seams installed.
    pub fn with_components<R, J>(
        id: impl Into<SessionId>,
        flow: FlowSpec,
        resolver: R,
        journal: J,
    ) -> Result<Self, BotError>
    where
        R: Resolver + 'static,
        J: Journal + 'static,
    {
        Self::with_components_and_limits(id, flow, resolver, journal, ResourceLimits::shipped())
    }

    /// Return the session identity.
    #[must_use]
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// Return the current node id, or `None` once terminal.
    #[must_use]
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// Return the current node kind, or `None` once terminal.
    #[must_use]
    pub fn current_node(&self) -> Option<&NodeKind> {
        self.current.as_deref().and_then(|id| self.flow.node(id))
    }

    /// Return the typed variable scope.
    #[must_use]
    pub fn scope(&self) -> &VarScope {
        &self.scope
    }

    /// Return the visited node path.
    #[must_use]
    pub fn visited(&self) -> &[NodeId] {
        &self.visited
    }

    /// Return transcript records in insertion order.
    #[must_use]
    pub fn transcript(&self) -> &[TranscriptEntry] {
        &self.transcript
    }

    /// Return the decision receipts written so far, in insertion order.
    ///
    /// This is the audit record. [`Self::transcript`] is a rendering of the
    /// same events for a person to read; these are what a consumer reconciles
    /// against, and they carry what a rendering cannot: the policy in force,
    /// the model if one was consulted, the candidate set, and where each
    /// receipt was accepted.
    #[must_use]
    pub fn decisions(&self) -> &[RecordedDecision] {
        &self.decisions
    }

    /// Return the digest of the flow document this session is running.
    #[must_use]
    pub fn flow_revision(&self) -> &str {
        &self.flow_revision
    }

    /// Return the terminal outcome, if reached.
    #[must_use]
    pub fn terminal(&self) -> Option<&Terminal> {
        self.terminal.as_ref()
    }

    /// Return the number of charged steps.
    #[must_use]
    pub fn steps(&self) -> usize {
        self.steps
    }

    /// Return the byte ceilings this session enforces.
    ///
    /// The effective set: the operator's shipped ceiling, narrowed by whatever
    /// this session's caller and this flow's [`FlowBounds`] asked for. Reported
    /// because a bound a caller cannot read back is a bound they have to
    /// discover by running into it.
    #[must_use]
    pub const fn limits(&self) -> ResourceLimits {
        self.limits
    }

    /// Return the bytes this session currently retains.
    ///
    /// The sum of every transcript record's payload, path node, and role, plus
    /// every visited node id. Each is an owned allocation the session holds, so
    /// the total is what a caller needs to bound their own memory rather than
    /// an estimate of it.
    #[must_use]
    pub const fn retained_bytes(&self) -> usize {
        self.retained
    }

    /// Submit one free-text answer. Unrecognized input is recorded, the same
    /// ask remains current, and the prompt is recorded again.
    ///
    /// Every verdict — resolved, ambiguous, absent, or degraded — writes a
    /// [`DecisionReceipt`], because a re-ask is a decision too and the one an
    /// operator most needs to see afterwards.
    ///
    /// The receipt is written through the journal before the transition it
    /// describes is applied, and a journal that refuses it aborts the answer
    /// with [`BotError::ReceiptNotRecorded`] leaving the session exactly as it
    /// was: not advanced, not terminated, the variable unwritten, the
    /// transcript untouched, and no receipt held. Recording is part of
    /// accepting an answer, not a report written beside it, so the one failure
    /// this ordering leaves behind is a decision that was reached and recorded
    /// whose answer could not then be stored — which is the direction an audit
    /// record must err in, and the opposite of an accepted answer with no
    /// record of why.
    pub fn answer(&mut self, utterance: &str) -> Result<(), BotError> {
        if self.terminal.is_some() {
            return Err(BotError::SessionTerminated);
        }
        let Some(node_id) = self.current.clone() else {
            return Err(BotError::SessionNotAwaitingAnswer);
        };
        let Some(NodeKind::Ask {
            var,
            options,
            routes,
        }) = self.flow.node(&node_id).cloned()
        else {
            return Err(BotError::SessionNotAwaitingAnswer);
        };
        // The ingress ceiling, and it comes before the step is charged and
        // before the utterance is copied anywhere: a refused line must neither
        // consume budget nor leave a record that reads as part of the
        // conversation. The check is one comparison against a bounded length,
        // so refusing costs nothing that accepting would not have cost more of.
        let utterance_limit = self.limits.get(ResourceAxis::Utterance);
        if utterance.len() > utterance_limit {
            return Err(BotError::UtteranceTooLarge {
                bytes: utterance.len(),
                limit: utterance_limit,
            });
        }
        self.charge_step()?;
        let question =
            Question::new(&node_id, &options).with_domain(self.scope.answer_domain(&var));
        let verdict = self.resolver.resolve(utterance, &question);
        match verdict.resolution().clone() {
            Resolution::Resolved { index, .. } => {
                let Some(option) = options.get(index) else {
                    return Err(BotError::ResolverReturnedInvalidOption { node: node_id });
                };
                let Some(target) = routes.get(option).cloned() else {
                    return Err(BotError::MissingAskRoute {
                        node: node_id,
                        option: option.clone(),
                    });
                };
                // Written before the transition it describes, which is the
                // ordering `answer`'s contract above requires: a journal that
                // refuses the receipt aborts the answer with the session
                // untouched — not advanced, the variable unwritten, the
                // transcript bare.
                self.write_receipt(
                    &node_id,
                    &options,
                    &verdict,
                    Some(option.clone()),
                    Some(&target),
                )?;
                self.scope.set_from_answer_within(
                    &var,
                    option,
                    self.limits.get(ResourceAxis::Value),
                )?;
                self.last_utterance = Some(utterance.to_owned());
                self.record(&node_id, "user", utterance)?;
                self.current = Some(target);
                self.drive()
            }
            Resolution::Ambiguous { tied, .. } => {
                // Narrow the re-ask to the options still in play. Repeating the
                // full list is what a two-way verdict forced, and it is why an
                // ambiguous answer could loop: the same utterance resolves the
                // same way every time it is asked, so the session never leaves
                // the node. A re-ask that is strictly smaller terminates.
                let narrowed: Vec<String> = tied
                    .iter()
                    .filter_map(|candidate| options.get(*candidate).cloned())
                    .collect();
                self.write_receipt(&node_id, &options, &verdict, None, None)?;
                self.record(&node_id, "user", utterance)?;
                if narrowed.len() >= 2 {
                    self.record_prompt(&node_id, &narrowed)?;
                } else {
                    self.record_prompt(&node_id, &options)?;
                }
                Ok(())
            }
            Resolution::Absent { .. } => {
                self.write_receipt(&node_id, &options, &verdict, None, None)?;
                self.record(&node_id, "user", utterance)?;
                self.record_prompt(&node_id, &options)?;
                Ok(())
            }
            Resolution::StaleAlias {
                question: bound_question,
                option,
            } => {
                // Re-ask, as for `Absent`, but record what was withdrawn under
                // its own role. The resolver is not asked to forget the binding:
                // the seam is read-only by design, and a session that silently
                // rewrote its resolver's learned vocabulary would be editing the
                // audit record it is supposed to be producing. The record names
                // the binding and the question; retiring it is the owner's call.
                // A re-ask is a decision, so it is receipted like every other
                // verdict; the receipt names the withdrawal, and the roles
                // below name it again for whoever reads the transcript.
                self.write_receipt(&node_id, &options, &verdict, None, None)?;
                self.record(&node_id, "user", utterance)?;
                self.record_stale_alias(&node_id, &bound_question, &option)?;
                self.record_prompt(&node_id, &options)?;
                Ok(())
            }
            Resolution::Degraded { reason } => {
                // Re-ask, as for `Absent`, but record the cause under its own
                // role: a transcript that renders a degraded re-ask exactly as
                // an unclear one is how an operator concludes the person was
                // being difficult while the embedder was down.
                self.write_receipt(&node_id, &options, &verdict, None, None)?;
                self.record(&node_id, "user", utterance)?;
                self.record_degraded(&node_id, reason)?;
                self.record_prompt(&node_id, &options)?;
                Ok(())
            }
        }
    }

    /// Build one receipt and write it through the journal, or refuse the
    /// answer.
    ///
    /// The session's own copy is stored only after the sink has taken it, and
    /// the acceptance that comes back is stored beside it: what the journal
    /// said about the receipt is its own fact, and a receipt that carried its
    /// own acceptance would be the session answering a question only the sink
    /// can answer.
    fn write_receipt(
        &mut self,
        node_id: &str,
        options: &[String],
        verdict: &Verdict,
        selected: Option<String>,
        route: Option<&str>,
    ) -> Result<(), BotError> {
        let receipt = DecisionReceipt {
            version: RECEIPT_VERSION,
            session: self.id.clone(),
            flow: self.flow_revision.clone(),
            node: node_id.to_owned(),
            options: options_revision(options),
            resolution: verdict.resolution().clone(),
            provenance: verdict.provenance().clone(),
            selected,
            route: route.map(str::to_owned),
        };
        // The sink takes it first. The session's own copy is stored only once
        // the journal has accepted it, so a refusal leaves no receipt held.
        let acceptance = self.journal.record_decision(&receipt).map_err(|cause| {
            BotError::ReceiptNotRecorded {
                node: node_id.to_owned(),
                cause,
            }
        })?;
        self.decisions.push(RecordedDecision {
            receipt,
            acceptance,
        });
        Ok(())
    }

    /// Charge one bounded step or return [`BotError::SessionBudgetExceeded`].
    fn charge_step(&mut self) -> Result<(), BotError> {
        let attempted = self
            .steps
            .checked_add(1)
            .ok_or(BotError::SessionBudgetExceeded {
                steps: usize::MAX,
                budget: self.flow.bounds.budget,
            })?;
        if attempted > self.flow.bounds.budget {
            return Err(BotError::SessionBudgetExceeded {
                steps: attempted,
                budget: self.flow.bounds.budget,
            });
        }
        self.steps = attempted;
        Ok(())
    }

    /// Execute deterministic nodes until an ask or terminal is reached.
    fn drive(&mut self) -> Result<(), BotError> {
        loop {
            let Some(node_id) = self.current.clone() else {
                return Ok(());
            };
            self.charge_step()?;
            // The path is retained like a record is, and charged like one: a
            // visited id is an owned copy of the node id the document carries,
            // so the same loop that produces unbounded records would otherwise
            // produce an unbounded path out of the same repetition.
            self.charge_retention(node_id.len())?;
            self.visited.push(node_id.clone());
            let Some(kind) = self.flow.node(&node_id).cloned() else {
                return Err(BotError::InvalidTransitionTarget {
                    from: "<session>".into(),
                    target: node_id,
                });
            };
            match kind {
                NodeKind::Say { text } => {
                    let rendered = self.render(&node_id, &text)?;
                    self.record(&node_id, "assistant", &rendered)?;
                    let Some(target) = self.flow.edge_targets(&node_id).into_iter().next() else {
                        return Err(BotError::MissingTransition { node: node_id });
                    };
                    self.current = Some(target);
                }
                NodeKind::Ask { options, .. } => {
                    self.record_prompt(&node_id, &options)?;
                    return Ok(());
                }
                NodeKind::Branch {
                    when,
                    then,
                    otherwise,
                    ..
                } => {
                    self.current = Some(if when.evaluate(&self.scope)? {
                        then
                    } else {
                        otherwise
                    });
                }
                // The two terminal kinds that emit nothing before they end.
                // Both read the outcome from the document rather than
                // constructing it from the node, so a declared outcome is
                // executed here and not only checked at load.
                NodeKind::Handoff { .. } | NodeKind::End => self.finish(&node_id)?,
                NodeKind::Refer { text, .. } => {
                    // The text is emitted before the outcome is set, and the
                    // only declared outcome a `refer` node can carry is the
                    // referral itself — validation refuses anything else — so
                    // this text is never spoken for an outcome that
                    // contradicts it.
                    let rendered = self.render(&node_id, &text)?;
                    self.record(&node_id, "assistant", &rendered)?;
                    self.finish(&node_id)?;
                }
                NodeKind::Route { dispatch, fallback } => {
                    self.current = Some(if self.last_utterance.is_some() {
                        dispatch
                    } else {
                        fallback
                    });
                }
            }
        }
    }

    /// End the session with the outcome the document gives one terminal node.
    ///
    /// The one place a session reads a terminal outcome. `drive` reaches this
    /// for every terminal node kind, so the outcome validation checked and the
    /// outcome execution returns are the same value from the same calculation
    /// ([`FlowSpec::effective_terminal`]) and cannot drift: an earlier runner
    /// consulted the terminal map for `End` alone, and a refusal declared on a
    /// `handoff` node was accepted by validation and ignored here.
    fn finish(&mut self, node_id: &str) -> Result<(), BotError> {
        let Some(outcome) = self.flow.effective_terminal(node_id) else {
            return Err(BotError::MalformedFlow {
                cause: format!("node {node_id:?} reached with no terminal outcome"),
            });
        };
        self.terminal = Some(outcome);
        self.current = None;
        Ok(())
    }

    /// Render one node's template under this session's record ceiling.
    ///
    /// The ceiling is applied to the *computed* expansion, inside the
    /// interpolator, before the output buffer exists. What that buys is the
    /// whole of the amplification fix: a valid document can name a placeholder
    /// a million times, and the cost of discovering that is a million additions
    /// rather than a multi-gigabyte allocation.
    fn render(&self, node_id: &str, template: &str) -> Result<String, BotError> {
        TemplateInterpolator::new().interpolate(
            template,
            &self.scope,
            node_id,
            self.limits.get(ResourceAxis::Record),
        )
    }

    /// Charge `bytes` against the session's retention ceiling.
    ///
    /// Charged before the allocation, so a refused write leaves nothing behind
    /// to clean up: the journal has not been told, the transcript has not grown,
    /// and the counter has not moved.
    fn charge_retention(&mut self, bytes: usize) -> Result<(), BotError> {
        let limit = self.limits.get(ResourceAxis::Session);
        let total = self
            .retained
            .checked_add(bytes)
            .ok_or(BotError::SessionRetentionExceeded {
                bytes: usize::MAX,
                limit,
            })?;
        if total > limit {
            return Err(BotError::SessionRetentionExceeded {
                bytes: total,
                limit,
            });
        }
        self.retained = total;
        Ok(())
    }

    /// Append one transcript record and forward it to the journal seam.
    ///
    /// Two ceilings, in this order, and both before either sink is touched: the
    /// payload against the per-record ceiling, which is what makes the refusal
    /// name the node whose text is too long, and the whole record against the
    /// aggregate, which is what bounds a conversation the graph lets repeat.
    /// The record's cost counts the path node and the role as well as the text,
    /// because all three are owned copies this session retains.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::RecordTooLarge`] or
    /// [`BotError::SessionRetentionExceeded`]. Neither has written anything.
    fn record(&mut self, node_id: &str, role: &str, text: &str) -> Result<(), BotError> {
        let record_bytes = self.limits.get(ResourceAxis::Record);
        if text.len() > record_bytes {
            return Err(BotError::RecordTooLarge {
                node: node_id.to_owned(),
                bytes: text.len(),
                limit: record_bytes,
            });
        }
        let cost = text
            .len()
            .saturating_add(node_id.len())
            .saturating_add(role.len());
        self.charge_retention(cost)?;
        self.journal.record(node_id, role, text);
        self.transcript.push(TranscriptEntry {
            path_node: node_id.to_owned(),
            role: role.to_owned(),
            text: text.to_owned(),
        });
        Ok(())
    }

    /// Record the standard prompt for an ask node.
    ///
    /// The prompt is built from the node's candidates, so its size is checked
    /// before it is built rather than after: a node with many long options
    /// would otherwise allocate the joined string and *then* discover that the
    /// record ceiling refuses it. `checked_mul` for the separators and
    /// `checked_add` for each option, with the overflow case reported as the
    /// same refusal rather than wrapped.
    ///
    /// # Errors
    ///
    /// Returns [`BotError::RecordTooLarge`] for a prompt over the ceiling.
    fn record_prompt(&mut self, node_id: &str, options: &[String]) -> Result<(), BotError> {
        let bytes = prompt_bytes(options).unwrap_or(usize::MAX);
        let record_bytes = self.limits.get(ResourceAxis::Record);
        if bytes > record_bytes {
            return Err(BotError::RecordTooLarge {
                node: node_id.to_owned(),
                bytes,
                limit: record_bytes,
            });
        }
        let prompt = format!("Choose one: {}", options.join(", "));
        self.record(node_id, "assistant", &prompt)
    }

    /// Records that the resolver could not reach a verdict.
    ///
    /// A distinct role rather than a second assistant prompt, because the two
    /// failures need different repairs and a transcript is where that is
    /// decided: an absent resolution says the person's words did not fit the
    /// options, and a degraded one says the resolver never got to compare them.
    fn record_degraded(&mut self, node_id: &str, reason: DegradedReason) -> Result<(), BotError> {
        let text = format!("Resolver unavailable: {reason}");
        self.record(node_id, "resolver-degraded", &text)
    }

    /// Records that a confirmed phrase's option is no longer offered.
    ///
    /// A distinct role for the reason the degraded case has one: this record is
    /// read by whoever maintains the vocabulary, and it has to name both halves
    /// of the broken binding — the question it was confirmed in and the option
    /// that went away — because either one alone is unactionable. "A stale alias
    /// was ignored" tells that reader nothing they can repair.
    fn record_stale_alias(
        &mut self,
        node_id: &str,
        question: &str,
        option: &str,
    ) -> Result<(), BotError> {
        let text = format!("Superseded alias: question {question} no longer offers \"{option}\"");
        self.record(node_id, "resolver-stale-alias", &text)
    }
}

/// The byte cost of the standard ask prompt for `options`.
///
/// Computed rather than measured, so the caller can refuse an oversized prompt
/// without building it. `None` means the arithmetic overflowed, which is the
/// same refusal at a larger size: the caller maps it to the ceiling with
/// [`usize::MAX`] bytes, and `usize::MAX` exceeds every ceiling.
///
/// The prefix and separator are the literals [`Session::record_prompt`]
/// formats, and they are named here rather than repeated so the estimate and
/// the string cannot disagree.
fn prompt_bytes(options: &[String]) -> Option<usize> {
    /// Literal prefix in the recorded prompt.
    const PREFIX: &str = "Choose one: ";
    /// Literal separator between candidates.
    const SEPARATOR: &str = ", ";
    let separators = options
        .len()
        .checked_sub(1)
        .and_then(|count| count.checked_mul(SEPARATOR.len()))?;
    options
        .iter()
        .try_fold(PREFIX.len(), |total, option| {
            total.checked_add(option.len())
        })?
        .checked_add(separators)
}

/// Digest a flow document so a receipt names the revision it was decided under.
///
/// The document is digested through its JSON rendering rather than field by
/// field, so a field added to [`FlowSpec`] later is covered without anyone
/// remembering to extend a walk. `FlowSpec` holds `BTreeMap`s, so the rendering
/// is canonical: two flows that differ only in the order their maps were built
/// from digest identically, which is what makes this a *revision* rather than a
/// serialization accident.
fn revision_of(flow: &FlowSpec) -> Result<String, BotError> {
    crate::json::to_string(flow)
        .map(|rendered| lgwks_std::hash::blake3(rendered.as_bytes()).to_hex())
        .map_err(|error| BotError::MalformedFlow {
            cause: error.to_string().escape_debug().to_string(),
        })
}

/// Digest an ordered candidate list, so a reordering is a different identity.
///
/// Each option is length-prefixed rather than separated, because an option is
/// free text and could contain any separator: a digest that concatenated
/// `["a", "b"]` and `["ab"]` to the same bytes would report two different
/// questions as one identity, and the whole point of the field is to tell
/// whether the question changed.
fn options_revision(options: &[String]) -> String {
    let mut hasher = Hasher::new();
    for option in options {
        hasher.update(&option.len().to_le_bytes());
        hasher.update(option.as_bytes());
    }
    hasher.finalize().to_hex()
}

/// Resolve one scalar expression from a variable scope.
fn resolve_expr(expression: &ValueExpr, scope: &VarScope) -> Result<Value, BotError> {
    match *expression {
        ValueExpr::Literal(ref value) => Ok(value.clone()),
        ValueExpr::Var(ref name) => scope
            .get(name)
            .cloned()
            .ok_or_else(|| BotError::VariableUnset { name: name.clone() }),
    }
}

/// Compare two scalar expressions with a typed ordering operation.
fn compare_expr<F>(
    left: &ValueExpr,
    right: &ValueExpr,
    scope: &VarScope,
    predicate: F,
) -> Result<bool, BotError>
where
    F: FnOnce(std::cmp::Ordering) -> bool,
{
    let left = resolve_expr(left, scope)?;
    let right = resolve_expr(right, scope)?;
    let ordering = match (left, right) {
        (Value::String(left), Value::String(right))
        | (Value::Choice(left), Value::Choice(right)) => left.cmp(&right),
        (Value::Integer(left), Value::Integer(right)) => left.cmp(&right),
        (Value::Boolean(left), Value::Boolean(right)) => left.cmp(&right),
        _ => return Err(BotError::PredicateTypeMismatch),
    };
    Ok(predicate(ordering))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Return a small validated flow used by focused unit tests.
    fn end_flow() -> Result<FlowSpec, BotError> {
        FlowSpec::new(
            BTreeMap::new(),
            "end",
            BTreeMap::from([(String::from("end"), NodeKind::End)]),
            Vec::new(),
            BTreeMap::new(),
            FlowBounds::new(4),
        )
    }

    #[test]
    fn predicates_compare_typed_values() -> Result<(), BotError> {
        let mut declarations = BTreeMap::new();
        declarations.insert(String::from("answer"), VarType::Integer);
        let mut scope = VarScope::new(declarations)?;
        scope.set_from_answer("answer", "7")?;
        let predicate = Predicate::Gt(
            ValueExpr::Var(String::from("answer")),
            ValueExpr::Literal(Value::Integer(3)),
        );
        assert!(predicate.evaluate(&scope)?);
        Ok(())
    }

    #[test]
    fn templates_use_scope_values() -> Result<(), BotError> {
        let mut declarations = BTreeMap::new();
        declarations.insert(String::from("name"), VarType::String);
        let mut scope = VarScope::new(declarations)?;
        scope.set_from_answer("name", "Ada")?;
        let rendered = scope.interpolate("Hello ${name}")?;
        assert_eq!(rendered, "Hello Ada");
        Ok(())
    }

    /// One declared type paired with candidate answer strings and the value
    /// each must decode to.
    fn decode_cases() -> Vec<(VarType, &'static str, Result<Value, AnswerRejection>)> {
        let choice = || VarType::Choice(vec![String::from("yes"), String::from("no")]);
        vec![
            // A string variable accepts anything, untrimmed: the declared type
            // is what decides, not a literal spelling.
            (
                VarType::String,
                "Continue",
                Ok(Value::String(String::from("Continue"))),
            ),
            (
                VarType::String,
                "  padded  ",
                Ok(Value::String(String::from("  padded  "))),
            ),
            (VarType::Integer, "7", Ok(Value::Integer(7))),
            (VarType::Integer, " -12 ", Ok(Value::Integer(-12))),
            (VarType::Integer, "007", Ok(Value::Integer(7))),
            (VarType::Integer, "7.5", Err(AnswerRejection::NotAnInteger)),
            (VarType::Integer, "", Err(AnswerRejection::NotAnInteger)),
            (
                VarType::Integer,
                "99999999999999999999",
                Err(AnswerRejection::IntegerOutOfRange),
            ),
            (VarType::Boolean, "yes", Ok(Value::Boolean(true))),
            (VarType::Boolean, " TRUE ", Ok(Value::Boolean(true))),
            (VarType::Boolean, "No", Ok(Value::Boolean(false))),
            (
                VarType::Boolean,
                "Continue",
                Err(AnswerRejection::NotABoolean),
            ),
            (choice(), "yes", Ok(Value::Choice(String::from("yes")))),
            (choice(), " No ", Ok(Value::Choice(String::from("no")))),
            (
                choice(),
                "approve",
                Err(AnswerRejection::NotADeclaredChoice),
            ),
        ]
    }

    #[test]
    fn the_answer_decoder_is_one_function_for_validation_and_assignment() {
        // The decode table is the whole statement of what "this candidate can
        // be stored" means, and both the load-time check and the runtime
        // assignment read it. Every branch of every declared type is here,
        // including the two integer failures, which are distinct verdicts: a
        // typo and a value too large to store need different repairs.
        for (declared, answer, expected) in decode_cases() {
            assert_eq!(
                declared.decode_answer(answer),
                expected,
                "decoding {answer:?} as {} disagreed with the table",
                declared.label()
            );
        }
    }

    #[test]
    fn assignment_uses_the_same_decoder_validation_does() -> Result<(), BotError> {
        // The property that makes the load-time check worth anything: for every
        // case the decoder accepts, assigning supplies the answer to a scope
        // and stores exactly the value the decoder named. A second, separately
        // written assignment path is what let a flow load and then be
        // unanswerable.
        for (declared, answer, expected) in decode_cases() {
            let mut declarations = BTreeMap::new();
            declarations.insert(String::from("slot"), declared.clone());
            let mut scope = VarScope::new(declarations)?;

            let assigned = scope.set_from_answer("slot", answer);
            match expected {
                Ok(value) => {
                    assigned?;
                    assert_eq!(
                        scope.get("slot"),
                        Some(&value),
                        "assigning {answer:?} as {} stored the wrong value",
                        declared.label()
                    );
                }
                Err(rejection) => {
                    assert!(
                        matches!(assigned, Err(BotError::InvalidVariableValue { .. })),
                        "assigning {answer:?} as {} must refuse with the typed value error, got \
                         {assigned:?} ({rejection})",
                        declared.label()
                    );
                    assert!(
                        scope.get("slot").is_none(),
                        "a refused assignment must not write the variable"
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn every_rejection_renders_a_cause_a_reader_can_act_on() {
        let rejections = [
            AnswerRejection::NotAnInteger,
            AnswerRejection::IntegerOutOfRange,
            AnswerRejection::NotABoolean,
            AnswerRejection::NotADeclaredChoice,
        ];
        for rejection in rejections {
            let rendered = rejection.to_string();
            assert!(
                !rendered.is_empty() && rendered.chars().any(char::is_alphabetic),
                "a rejection is rendered into a diagnostic, so it cannot be empty: {rendered:?}"
            );
        }
    }

    #[test]
    fn an_ask_whose_variable_cannot_hold_its_options_is_refused_at_load() -> Result<(), BotError> {
        // The counterexample the review filed: a boolean variable and a
        // question offering "Continue"/"Cancel". Every candidate is decodable
        // at load time, and neither one is storable, so the document is refused
        // rather than accepted and then unanswerable.
        let outcome = FlowSpec::new(
            BTreeMap::from([(String::from("decision"), VarType::Boolean)]),
            "ask",
            BTreeMap::from([
                (
                    String::from("ask"),
                    NodeKind::Ask {
                        var: String::from("decision"),
                        options: vec![String::from("Continue"), String::from("Cancel")],
                        routes: BTreeMap::from([
                            (String::from("Continue"), String::from("done")),
                            (String::from("Cancel"), String::from("done")),
                        ]),
                    },
                ),
                (String::from("done"), NodeKind::End),
            ]),
            Vec::new(),
            BTreeMap::new(),
            FlowBounds::new(8),
        );
        let BotError::AskOptionNotAssignable {
            node,
            variable,
            option,
            expected,
            cause,
        } = (match outcome {
            Err(error) => error,
            Ok(spec) => {
                return Err(BotError::MalformedFlow {
                    cause: format!("accepted a flow with {} nodes", spec.nodes().len()),
                });
            }
        })
        else {
            return Err(BotError::MalformedFlow {
                cause: String::from("refused for the wrong reason"),
            });
        };

        // The five facts the diagnostic has to carry: where, which variable,
        // which candidate, what type was expected, and why.
        assert_eq!(node, "ask", "the refusal names the ask node");
        assert_eq!(variable, "decision", "the refusal names the variable");
        assert_eq!(
            option, "Continue",
            "the refusal names the candidate, not just the node"
        );
        assert_eq!(expected, "boolean", "the refusal names the expected type");
        assert_eq!(
            cause,
            AnswerRejection::NotABoolean.to_string(),
            "the refusal carries the decoder's own reason"
        );
        Ok(())
    }

    #[test]
    fn an_ask_the_variable_can_hold_is_still_accepted() -> Result<(), BotError> {
        // The other half of the rule, so the check above cannot be satisfied by
        // refusing everything: a boolean variable offered boolean literals, a
        // choice variable offered its own declared values, and an integer
        // variable offered integers all load.
        let accepted = [
            (
                VarType::Boolean,
                vec![String::from("yes"), String::from("no")],
            ),
            (
                VarType::Integer,
                vec![String::from("0"), String::from("-1")],
            ),
            (
                VarType::Choice(vec![String::from("yes"), String::from("no")]),
                vec![String::from("Yes"), String::from("NO")],
            ),
            (
                VarType::String,
                vec![String::from("anything"), String::from("")],
            ),
        ];
        for (declared, options) in accepted {
            let routes = options
                .iter()
                .map(|option| (option.clone(), String::from("done")))
                .collect();
            let spec = FlowSpec::new(
                BTreeMap::from([(String::from("slot"), declared.clone())]),
                "ask",
                BTreeMap::from([
                    (
                        String::from("ask"),
                        NodeKind::Ask {
                            var: String::from("slot"),
                            options,
                            routes,
                        },
                    ),
                    (String::from("done"), NodeKind::End),
                ]),
                Vec::new(),
                BTreeMap::new(),
                FlowBounds::new(8),
            )?;
            assert_eq!(
                spec.nodes().len(),
                2,
                "a {} ask over storable candidates must load",
                declared.label()
            );
        }
        Ok(())
    }

    #[test]
    fn the_default_resolver_has_an_explicit_unrecognized_case() {
        let options = vec![String::from("yes"), String::from("no")];
        let question = Question::new("ask", &options);
        let resolver = crate::language::LanguageResolver::new();
        assert!(
            matches!(
                resolver.resolve("maybe", &question).into_resolution(),
                Resolution::Absent { .. }
            ),
            "an unrecognized answer is Absent, which is not the same as Ambiguous"
        );
        assert_eq!(
            resolver.resolve("yes", &question).into_resolution(),
            Resolution::Resolved {
                index: 0,
                tier: MatchTier::Exact,
                score: 1.0,
                lead: 1.0,
            }
        );
    }

    #[test]
    fn end_session_stops_at_terminal() -> Result<(), BotError> {
        let session = Session::new("test", end_flow()?)?;
        assert_eq!(session.current(), None);
        assert_eq!(session.terminal(), Some(&Terminal::Completed));
        Ok(())
    }

    /// The four terminals, each paired with the disposition it must report.
    fn terminals() -> [(Terminal, Disposition); 4] {
        [
            (Terminal::Completed, Disposition::Completed),
            (
                Terminal::Referred {
                    target: String::from("tier-2"),
                },
                Disposition::Referred,
            ),
            (
                Terminal::HandedOff {
                    target: String::from("agent"),
                },
                Disposition::HandedOff,
            ),
            (
                Terminal::Refused {
                    reason: String::from("out of scope"),
                },
                Disposition::Refused,
            ),
        ]
    }

    #[test]
    fn disposition_and_effect_knowledge_are_reported_independently() {
        // The cross-product: every terminal against every effect history. Each
        // axis comes back unchanged whatever the other says, because they are
        // two facts rather than two arms of one choice.
        let histories = [
            EffectLedger::new(0, 0), // nothing attempted
            EffectLedger::new(1, 0), // one effect, confirmed
            EffectLedger::new(0, 1), // one attempted, occurrence unknown
            EffectLedger::new(2, 1), // confirmed and unknown at once
            EffectLedger::new(3, 2),
        ];

        for (terminal, expected) in terminals() {
            for ledger in histories {
                let report = terminal.outcome(ledger);
                assert_eq!(
                    report.disposition(),
                    expected,
                    "no effect history may change what the workflow decided"
                );
                assert_eq!(
                    report.effects(),
                    ledger,
                    "no disposition may change what is known about the effects"
                );
                assert_eq!(
                    report.needs_reconciliation(),
                    ledger.unsettled() > 0,
                    "reconciliation follows the unsettled count and nothing else"
                );
            }
        }
    }

    #[test]
    fn a_refusal_no_longer_erases_an_unsettled_effect() {
        // The counterexample from the review, kept as a regression. Two runs
        // refused for the same reason, differing only in whether an earlier
        // effect was left in doubt. The old classifier returned `Failure` for
        // both, which is the one answer that cannot be acted on: only the
        // second one needs an effect reconciled before anyone retries.
        let refused = Terminal::Refused {
            reason: String::from("later step denied"),
        };

        let settled = refused.outcome(EffectLedger::new(0, 0));
        let unknown = refused.outcome(EffectLedger::new(0, 1));

        assert_eq!(
            settled.disposition(),
            unknown.disposition(),
            "both runs were refused, and the report says so in both cases"
        );
        assert!(
            !settled.needs_reconciliation(),
            "nothing was attempted, so there is nothing to reconcile"
        );
        assert!(
            unknown.needs_reconciliation(),
            "an earlier effect may be live; a refusal does not settle it"
        );
        assert_ne!(
            settled, unknown,
            "histories differing on a possibly-live effect must stay distinguishable"
        );
    }

    #[test]
    fn an_unsettled_effect_does_not_imply_a_confirmed_one() {
        // The second inference error the review named. `unsettled > 0` was
        // documented as "part of the output exists", which does not follow: one
        // attempted effect whose response was lost may have produced none, and
        // the report has to say what is known rather than the flattering half.
        let report = Terminal::Completed.outcome(EffectLedger::new(0, 1));

        assert_eq!(
            report.effects().confirmed(),
            0,
            "nothing was confirmed, so nothing may be claimed to exist"
        );
        assert_eq!(
            report.effects().unsettled(),
            1,
            "the unknown effect is still counted"
        );
        assert!(
            report.needs_reconciliation(),
            "the run needs reconciling, which is not the same statement as having \
             partly succeeded"
        );
    }

    #[test]
    fn an_indeterminate_effect_is_what_puts_a_run_in_doubt() {
        // The law tying the two halves of this change together: the error says
        // an effect may have happened, and that is exactly what puts the run in
        // doubt. Neither half is reachable in practice without the other.
        let errors = [BotError::EffectIndeterminate {
            domain: String::from("gh::merge"),
            cause: String::from("request timed out"),
        }];
        let unsettled = errors
            .iter()
            .filter(|error| matches!(**error, BotError::EffectIndeterminate { .. }))
            .count();

        assert_eq!(unsettled, 1, "the run left one effect unsettled");
        let report = Terminal::Completed.outcome(EffectLedger::new(0, unsettled));
        assert_eq!(
            report.disposition(),
            Disposition::Completed,
            "an unsettled effect does not change what the workflow decided"
        );
        assert!(
            report.needs_reconciliation(),
            "an indeterminate effect is what puts a completed run in doubt"
        );
    }

    #[test]
    fn a_report_round_trips_through_json() -> Result<(), Box<dyn std::error::Error>> {
        // A report is written to a journal and read back by whoever reconciles
        // it, so it has to survive the trip with both facts intact.
        let report = Terminal::Refused {
            reason: String::from("denied"),
        }
        .outcome(EffectLedger::new(2, 1));

        let json = crate::json::to_string(&report)?;
        let back: Outcome = crate::json::from_str(&json)?;

        assert_eq!(
            back, report,
            "a report has to survive the journal it is written to"
        );
        assert_eq!(
            back.disposition(),
            Disposition::Refused,
            "the refusal survives the round trip"
        );
        assert!(back.needs_reconciliation(), "so does the unresolved effect");
        Ok(())
    }
}
