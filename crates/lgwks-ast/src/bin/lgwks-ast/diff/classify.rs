//! Which direction one architecture change moves (#655).
//!
//! A change is widening when it admits more work, a wider domain, a looser
//! bound or a removed deadline; narrowing when it is the reverse; neutral when
//! the source does not fix a direction. The rules are stated per word, so a new
//! word must decide its own direction rather than inherit one from the shape of
//! its line.

use lgwks_ast::script::Kind;

use super::Class;

/// The subject `each` carries when the runtime sizes the fan-out, as
/// `blocks::each` writes it.
const SIZED_TO_THE_MACHINE: &str = "sized to the machine";

/// The clause every bound comparison adds when a side is an expression whose
/// value the source does not fix.
const UNREAD_BOUND: &str = "and one side is an expression the source does not fix";

/// Nanoseconds per duration unit the language accepts, as `words::duration`
/// accepts them.
const UNITS: [(&str, u128); 4] = [
    ("ms", 1_000_000),
    ("s", 1_000_000_000),
    ("m", 60_000_000_000),
    ("h", 3_600_000_000_000),
];

/// Classify a subject change on a matched step.
pub(super) fn subject(kind: Kind, from: &str, to: &str) -> (Class, String) {
    match kind {
        Kind::Retry => attempts(from, to),
        Kind::Each => fan_out(from, to),
        Kind::Within => deadline(from, to),
        _ => (
            Class::Neutral,
            "the subject changed, and this word's subject has no direction".to_owned(),
        ),
    }
}

/// Classify a change to the written line of a matched step whose subject did
/// not move: the only direction left in a `retry` line is its `waiting`
/// duration, because a wait added or lengthened lets the run take longer.
pub(super) fn detail(kind: Kind, from: &str, to: &str) -> (Class, String) {
    if kind != Kind::Retry {
        return (
            Class::Neutral,
            "the line changed; the word, subject and nesting did not".to_owned(),
        );
    }
    waiting(from, to)
}

/// The direction a `waiting` clause moved.
fn waiting(from: &str, to: &str) -> (Class, String) {
    match (waiting_of(from), waiting_of(to)) {
        (None, Some(_)) => (
            Class::Widening,
            "a wait was added between attempts".to_owned(),
        ),
        (Some(_), None) => (
            Class::Narrowing,
            "the wait between attempts was removed".to_owned(),
        ),
        (Some(before), Some(after)) => match (nanos(before), nanos(after)) {
            (Some(before_nanos), Some(after_nanos)) if after_nanos > before_nanos => (
                Class::Widening,
                format!("the wait between attempts was raised from `{before}` to `{after}`"),
            ),
            (Some(before_nanos), Some(after_nanos)) if after_nanos < before_nanos => (
                Class::Narrowing,
                format!("the wait between attempts was lowered from `{before}` to `{after}`"),
            ),
            _ => (
                Class::Neutral,
                format!("the wait between attempts changed, {UNREAD_BOUND}"),
            ),
        },
        (None, None) => (
            Class::Neutral,
            "the line changed; the word, subject and nesting did not".to_owned(),
        ),
    }
}

/// The duration written after `waiting` in `detail`, or `None` when the line
/// has no such clause.
fn waiting_of(detail: &str) -> Option<&str> {
    let mut words = detail.split_whitespace();
    while let Some(word) = words.next() {
        if word == "waiting" {
            return words.next().map(|duration| duration.trim_end_matches(','));
        }
    }
    None
}

/// Classify a step the head revision adds.
pub(super) fn added(kind: Kind) -> (Class, String) {
    match kind {
        Kind::Observe => (
            Class::Widening,
            "a new `observe` domain is read where nothing was read before".to_owned(),
        ),
        Kind::Act => (
            Class::Widening,
            "a new `act` domain changes the world where nothing did before".to_owned(),
        ),
        Kind::Retry => (
            Class::Widening,
            "a retry admits further attempts where one failure used to end the step".to_owned(),
        ),
        Kind::Within => (
            Class::Narrowing,
            "a `within` adds a deadline where the step ran without one".to_owned(),
        ),
        _ => (
            Class::Neutral,
            "the step is new, and no bound it carries moved in a widening direction".to_owned(),
        ),
    }
}

/// Classify a step the head revision removes.
pub(super) fn removed(kind: Kind) -> (Class, String) {
    match kind {
        Kind::Within => (
            Class::Widening,
            "a `within` was removed: the work now runs without a deadline".to_owned(),
        ),
        Kind::Observe => (
            Class::Narrowing,
            "the domain is no longer observed".to_owned(),
        ),
        Kind::Act => (
            Class::Narrowing,
            "the domain is no longer acted on".to_owned(),
        ),
        Kind::Retry => (
            Class::Narrowing,
            "the retry bound is gone: one attempt where there were several".to_owned(),
        ),
        _ => (
            Class::Neutral,
            "the step was removed, and removing it widens no bound".to_owned(),
        ),
    }
}

/// A flow the head revision adds.
pub(super) fn flow_added() -> (Class, String) {
    (
        Class::Widening,
        "a new flow adds work the base revision did not run".to_owned(),
    )
}

/// A flow the head revision removes.
pub(super) fn flow_removed() -> (Class, String) {
    (
        Class::Narrowing,
        "the flow is gone, and its work no longer runs".to_owned(),
    )
}

/// The direction an attempt bound moved.
fn attempts(from: &str, to: &str) -> (Class, String) {
    match (count(from), count(to)) {
        (Some(before), Some(after)) if after > before => (
            Class::Widening,
            format!("the attempt bound was raised from {before} to {after}"),
        ),
        (Some(before), Some(after)) if after < before => (
            Class::Narrowing,
            format!("the attempt bound was lowered from {before} to {after}"),
        ),
        _ => (
            Class::Neutral,
            format!("the attempt bound changed, {UNREAD_BOUND}"),
        ),
    }
}

/// The direction a fan-out bound moved. A bound the runtime sizes is the
/// widest reading, because nothing in the source limits it.
fn fan_out(from: &str, to: &str) -> (Class, String) {
    match (from == SIZED_TO_THE_MACHINE, to == SIZED_TO_THE_MACHINE) {
        (false, true) => (
            Class::Widening,
            "the fan-out bound was removed: the runtime now sizes it to the machine".to_owned(),
        ),
        (true, false) => (
            Class::Narrowing,
            "the fan-out gained an explicit bound where the runtime sized it".to_owned(),
        ),
        _ => match (count(from), count(to)) {
            (Some(before), Some(after)) if after > before => (
                Class::Widening,
                format!("the fan-out bound was raised from {before} to {after}"),
            ),
            (Some(before), Some(after)) if after < before => (
                Class::Narrowing,
                format!("the fan-out bound was lowered from {before} to {after}"),
            ),
            _ => (
                Class::Neutral,
                format!("the fan-out bound changed, {UNREAD_BOUND}"),
            ),
        },
    }
}

/// The direction a deadline moved: a longer one lets the work run longer.
fn deadline(from: &str, to: &str) -> (Class, String) {
    match (nanos(from), nanos(to)) {
        (Some(before), Some(after)) if after > before => (
            Class::Widening,
            format!("the deadline was raised from `{from}` to `{to}`"),
        ),
        (Some(before), Some(after)) if after < before => (
            Class::Narrowing,
            format!("the deadline was lowered from `{from}` to `{to}`"),
        ),
        _ => (
            Class::Neutral,
            format!("the deadline changed, {UNREAD_BOUND}"),
        ),
    }
}

/// A whole-number bound. The language admits `1_000`, and an underscore is
/// spelling, not a digit.
fn count(text: &str) -> Option<u64> {
    let digits: String = text.chars().filter(|character| *character != '_').collect();
    digits.trim().parse().ok()
}

/// A literal duration's length in nanoseconds, or `None` for an expression.
fn nanos(text: &str) -> Option<u128> {
    let trimmed = text.trim();
    let at = trimmed.find(|character: char| !(character.is_ascii_digit() || character == '.'))?;
    let (number, unit) = trimmed.split_at(at);
    let scale = scale_of(unit)?;
    scaled(&number.replace('_', ""), scale)
}

/// Nanoseconds per unit named by `unit`.
fn scale_of(unit: &str) -> Option<u128> {
    UNITS
        .iter()
        .find(|&&(name, _scale)| name == unit)
        .map(|&(_name, scale)| scale)
}

/// `number` (digits with at most one `.`) times `scale`, exactly.
fn scaled(number: &str, scale: u128) -> Option<u128> {
    let (whole, fraction) = match number.split_once('.') {
        Some(split) => split,
        None => (number, ""),
    };
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    let whole: u128 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let mut total = whole.checked_mul(scale)?;
    let mut place = scale;
    for digit in fraction.chars() {
        place = place.checked_div(10)?;
        let value = u128::from(digit.to_digit(10)?);
        total = total.checked_add(value.checked_mul(place)?)?;
    }
    Some(total)
}
