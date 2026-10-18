//! Typed values, variable types, answer domains and the expressions over them. Split from `session.rs` (#526).

use super::{BotError, Deserialize, FlowSpec, Hasher, IntErrorKind, Serialize, VarScope, fmt};

/// Stable identifier of a flow node.
pub type NodeId = String;

/// The closed set of scalar values carried by a [`VarScope`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
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
/// Division-free and conversion-free: the digit count is the number of
/// base-ten thresholds the magnitude reaches, and the sign is one more byte when
/// the value is negative. Zero has one digit and reaches no threshold.
///
/// The thresholds are walked in `u64` and counted in `usize`, so no value is
/// ever narrowed between the magnitude and the width. A logarithm would have
/// been one instruction, but it returns a `u32` and turning that into the width
/// the caller compares needs a conversion whose failure arm would have to put a
/// number in a byte count; twenty comparisons against a multiply is not a cost
/// anyone measures next to the formatting this exists to avoid.
fn integer_bytes(value: i64) -> usize {
    // `unsigned_abs` rather than `abs`, because `i64::MIN` has no positive
    // counterpart: negating it in `i64` overflows, and the digit count does not
    // care which side of zero the magnitude came from.
    let magnitude = value.unsigned_abs();
    let mut digits = 1_usize;
    let mut threshold = 10_u64;
    // Bounded by construction: `u64::MAX` has twenty digits, so the loop runs at
    // most nineteen times and `threshold` saturates rather than wrapping.
    while magnitude >= threshold {
        digits = digits.saturating_add(1);
        threshold = threshold.saturating_mul(10);
    }
    if value < 0 {
        digits.saturating_add(1)
    } else {
        digits
    }
}

/// Declared type of one variable in a flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
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
                    let refusal = Err(AnswerRejection::NotADeclaredChoice);
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "decode_answer: returning an error to the caller");
                    return refusal;
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
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
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
#[serde(crate = "lgwks_std::json::serde", rename_all = "snake_case")]
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
            Self::Eq(ref left, ref right) => same_value(left, right, scope, true),
            Self::Ne(ref left, ref right) => same_value(left, right, scope, false),
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
            Self::And(ref items) => all_true(items, scope),
            Self::Or(ref items) => any_true(items, scope),
            Self::Not(ref inner) => Ok(!inner.evaluate(scope)?),
        }
    }
}

/// Whether the two sides resolve to the same value, or to different ones.
///
/// `equal` is `true` for `Eq` and `false` for `Ne`, so one resolution path
/// serves both: each side resolves exactly once, which matters because
/// resolution is fallible and a side that resolved twice could fail twice.
///
/// This deliberately does not go through `compare_expr`. That one refuses a
/// pair of different `Value` variants with `PredicateTypeMismatch`, where `==`
/// here answers `false`; routing `Eq` through it would turn a comparison that
/// has always answered into one that fails.
fn same_value(
    left: &ValueExpr,
    right: &ValueExpr,
    scope: &VarScope,
    equal: bool,
) -> Result<bool, BotError> {
    let left_value = resolve_expr(left, scope)?;
    let right_value = resolve_expr(right, scope)?;
    Ok((left_value == right_value) == equal)
}

/// Every item must hold; evaluation stops at the first that does not.
///
/// `Ok(true)` on an empty list, the identity for `and`. The two short-circuit
/// loops are helpers rather than arms because each was a loop with a
/// propagation operator inside it, so the `match` above held three of them
/// across two arms and a reader could not see the short-circuit rule without
/// reading both loops in full.
fn all_true(items: &[Predicate], scope: &VarScope) -> Result<bool, BotError> {
    for item in items {
        if !item.evaluate(scope)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Some item must hold; evaluation stops at the first that does.
///
/// `Ok(false)` on an empty list, the identity for `or`.
fn any_true(items: &[Predicate], scope: &VarScope) -> Result<bool, BotError> {
    for item in items {
        if item.evaluate(scope)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The byte cost of the standard ask prompt for `options`.
///
/// Computed rather than measured, so the caller can refuse an oversized prompt
/// without building it. A width that saturates is the same refusal at a larger
/// size: [`usize::MAX`] bytes exceeds every record ceiling, so the caller's
/// ceiling check answers it without being told which of the two it was.
///
/// The prefix and separator are the literals [`Session::record_prompt`]
/// formats, and they are named here rather than repeated so the estimate and
/// the string cannot disagree.
pub(super) fn prompt_bytes(options: &[String]) -> usize {
    /// Literal prefix in the recorded prompt.
    const PREFIX: &str = "Choose one: ";
    /// Literal separator between candidates.
    const SEPARATOR: &str = ", ";
    // Saturating rather than checked: the caller refuses a prompt whose width
    // exceeds the record ceiling, and a width that has saturated exceeds every
    // ceiling a `usize` can hold, so the same refusal answers both without the
    // caller having to tell which of the two it was looking at. `checked_sub(1)`
    // becomes a `saturating_sub`, which is the same value for the empty list —
    // no separator follows no candidate — and the right one past it.
    let separators = options
        .len()
        .saturating_sub(1)
        .saturating_mul(SEPARATOR.len());
    let candidates = options.iter().fold(PREFIX.len(), |total, option| {
        total.saturating_add(option.len())
    });
    candidates.saturating_add(separators)
}

/// Digest a flow document so a receipt names the revision it was decided under.
///
/// The document is digested through its JSON rendering rather than field by
/// field, so a field added to [`FlowSpec`] later is covered without anyone
/// remembering to extend a walk. `FlowSpec` holds `BTreeMap`s, so the rendering
/// is canonical: two flows that differ only in the order their maps were built
/// from digest identically, which is what makes this a *revision* rather than a
/// serialization accident.
pub(super) fn revision_of(flow: &FlowSpec) -> Result<String, BotError> {
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
pub(super) fn options_revision(options: &[String]) -> String {
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
        _ => {
            let refusal = Err(BotError::PredicateTypeMismatch);
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "compare_expr: returning an error to the caller");
            return refusal;
        }
    };
    Ok(predicate(ordering))
}
