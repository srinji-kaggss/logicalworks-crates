//! Inline words: `run`, `observe`, `act`, bounds, durations, and the map
//! entries they leave.

use lgwks_deps::proc_macro2::{Delimiter, Group, Ident, TokenStream, TokenTree};

use super::super::lexicon::{self, Kind};
use super::super::lines::{Line, after, group, ident, is_ident, is_parens, is_punct, text};
use super::super::tree::{Bound, Call, Code, Domain, Duration, Fragment, Site, StepShape};
use super::super::{Refusal, Result};
use super::Labels;

/// Read a line of Rust, turning each `run name(args)` into a [`Call`] and each
/// `observe` or `act` of a registry identifier into a [`Domain`], in this
/// scope.
///
/// # Errors
///
/// An `observe` or `act` followed by an identifier but not by the rest of its
/// form: that is never Rust (two names cannot stand side by side), so it is
/// the word written wrong, and the refusal names the form.
pub(super) fn rewrite(
    tokens: &[TokenTree],
    labels: &mut Labels,
    shapes: &mut Vec<StepShape>,
    line: &Line,
) -> Result<Code> {
    let mut fragments = Vec::new();
    // The walk carries the tail rather than an index into it, so "the tokens
    // after `run`" is the slice itself and a call that consumed several tokens
    // hands back where it stopped, instead of the caller re-deriving both.
    let mut rest = tokens;
    while let Some((token, tail)) = rest.split_first() {
        if lexicon::is(token, Kind::Run)
            && let Some((remaining, call)) = run_call(tail, labels, shapes, line)?
        {
            fragments.push(Fragment::Run(call));
            rest = remaining;
            continue;
        }
        // A domain word takes the rest of the line, or of the bracket it stands
        // in, as its target and value, so nothing is left after it to walk.
        if let Some(kind) = domain_word(token)
            && let Some(domain) = domain_call(kind, rest, labels, shapes, line)?
        {
            fragments.push(Fragment::Domain(domain));
            break;
        }
        if let TokenTree::Group(ref group) = *token {
            let inner: Vec<TokenTree> = group.stream().into_iter().collect();
            fragments.push(Fragment::Group {
                delimiter: group.delimiter(),
                span: group.span(),
                inner: rewrite(&inner, labels, shapes, line)?,
            });
        } else {
            fragments.push(Fragment::Token(token.clone()));
        }
        rest = tail;
    }
    Ok(Code { fragments })
}

/// The domain word `token` is, when it is `observe` or `act`.
fn domain_word(token: &TokenTree) -> Option<Kind> {
    [Kind::Observe, Kind::Act]
        .into_iter()
        .find(|&kind| lexicon::is(token, kind))
}

/// `observe id of target` or `act id on target with value`, starting at the
/// word: the domain step, or `None` when what follows the word is not an
/// identifier, in which case the word is a name of plain Rust (`observe(x)`,
/// `let act = ..`) and nothing was consumed.
///
/// # Errors
///
/// An identifier with no particle after it, a particle with nothing after it,
/// or an `act` with no `with`.
fn domain_call(
    kind: Kind,
    tokens: &[TokenTree],
    labels: &mut Labels,
    shapes: &mut Vec<StepShape>,
    line: &Line,
) -> Result<Option<Domain>> {
    let Some((word, after_word)) = tokens.split_first() else {
        return Ok(None);
    };
    let Some((id, base, after_id)) = identifier(after_word) else {
        return Ok(None);
    };
    let (particle, form) = if kind == Kind::Observe {
        ("of", "`observe <domain::id> of <target>`")
    } else {
        ("on", "`act <domain::id> on <target> with <value>`")
    };
    let after_particle = match after_id.split_first() {
        Some((next, rest)) if is_ident(next, particle) && !rest.is_empty() => rest,
        _ => {
            let refusal = Err(Refusal::new(
                word.span(),
                format!(
                    "this line reads {form}: the identifier is a key of the host's `DomainRegistry`"
                ),
            ));
            tracing::debug!(error = ?refusal.as_ref().err(), "domain_call: returning an error to the caller");
            return refusal;
        }
    };
    let (target, value) = if kind == Kind::Observe {
        (after_particle, None)
    } else {
        let split = after_particle
            .iter()
            .position(|token| is_ident(token, "with"))
            .and_then(|at| after_particle.split_at_checked(at));
        match split {
            Some((target, &[_, ref value @ ..])) if !target.is_empty() && !value.is_empty() => {
                (target, Some(value))
            }
            _ => {
                let refusal = Err(Refusal::new(
                    word.span(),
                    format!(
                        "this line reads {form}: an action is built from its target and handed its value"
                    ),
                ));
                tracing::debug!(error = ?refusal.as_ref().err(), "domain_call: returning an error to the caller");
                return refusal;
            }
        }
    };
    let target = rewrite(target, labels, shapes, line)?;
    let value = match value {
        Some(tokens) => Some(rewrite(tokens, labels, shapes, line)?),
        None => None,
    };
    let label = labels.next(&base);
    let map = shape(kind, id.clone(), text(tokens), line, Vec::new());
    let site = Site::of(&map);
    shapes.push(map);
    Ok(Some(Domain {
        site,
        kind,
        id,
        label,
        target,
        value,
    }))
}

/// A path of names joined by `::` at the front of `tokens`: the path as one
/// string, its last name, and the tokens after it; `None` when `tokens` does
/// not start with a name.
fn identifier(tokens: &[TokenTree]) -> Option<(String, String, &[TokenTree])> {
    let mut id = String::new();
    let mut last: Option<String> = None;
    let mut rest = tokens;
    while let Some((segment, tail)) = rest.split_first() {
        let Some(name) = ident(segment) else {
            break;
        };
        let name = name.to_string();
        if last.is_some() {
            id.push_str("::");
        }
        id.push_str(&name);
        last = Some(name);
        rest = tail;
        match *rest {
            [ref first, ref second, ref after @ ..]
                if is_punct(first, ':') && is_punct(second, ':') =>
            {
                rest = after;
            }
            _ => break,
        }
    }
    last.map(|last| (id, last, rest))
}

/// `path(args)` after a `run`: the call and the tokens left after it. `None`
/// when what follows `run` is not a call, in which case nothing was consumed
/// and the caller continues from the first token.
///
/// # Errors
///
/// A domain word in the arguments written wrong (see [`rewrite`]).
fn run_call<'line>(
    tokens: &'line [TokenTree],
    labels: &mut Labels,
    shapes: &mut Vec<StepShape>,
    line: &Line,
) -> Result<Option<(&'line [TokenTree], Call)>> {
    let mut path: Vec<TokenTree> = Vec::new();
    let mut rest = tokens;
    let mut callee: Option<&Ident> = None;
    while let Some((segment, tail)) = rest.split_first() {
        let Some(name) = ident(segment) else {
            break;
        };
        callee = Some(name);
        path.push(segment.clone());
        // A `::` continues the path, so the segment after it is the callee; a
        // parenthesis ends it, and that group is the call's arguments.
        if let Some((first, after_first)) = tail.split_first()
            && let Some((second, after_second)) = after_first.split_first()
            && is_punct(first, ':')
            && is_punct(second, ':')
        {
            path.push(first.clone());
            path.push(second.clone());
            rest = after_second;
        } else {
            rest = tail;
            break;
        }
    }
    let (Some(callee), Some(arguments)) = (callee, rest.first()) else {
        return Ok(None);
    };
    let Some(arguments) = group(arguments).filter(|_| is_parens(arguments)) else {
        return Ok(None);
    };
    let callee = callee.to_string();
    // The walk above left `rest` on the argument group, so the call consumed that
    // group too and what follows the call is what is past it.
    let after_arguments = after(rest, 1);
    let inner: Vec<TokenTree> = arguments.stream().into_iter().collect();
    let arguments = rewrite(&inner, labels, shapes, line)?;
    let label = labels.next(&callee);
    let map = shape(
        Kind::Run,
        callee.clone(),
        format!("run {}", text(&path)),
        line,
        Vec::new(),
    );
    let site = Site::of(&map);
    shapes.push(map);
    Ok(Some((
        after_arguments,
        Call {
            site,
            path: path.into_iter().collect(),
            callee,
            label,
            arguments,
        },
    )))
}

/// Words a line must start with, and the tokens after them.
///
/// Each word is read off the front of the remaining tokens rather than compared
/// by index, so a line shorter than the form is refused by the same refusal a
/// line that starts with something else is, rather than answering with an empty
/// remainder that reads as "there is nothing after `give back`".
pub(super) fn expect_words<'line>(
    line: &'line Line,
    words: &[&str],
    form: &str,
) -> Result<&'line [TokenTree]> {
    let mut rest: &[TokenTree] = &line.tokens;
    for word in words {
        match rest.split_first() {
            Some((token, tail)) if is_ident(token, word) => rest = tail,
            _ => {
                let refusal = Err(Refusal::new(line.span, format!("this line reads {form}")));
                tracing::debug!(error = ?refusal.as_ref().err(), "expect_words: returning an error to the caller");
                return refusal;
            }
        }
    }
    if rest.is_empty() {
        let refusal = Err(Refusal::new(line.span, format!("this line reads {form}")));
        tracing::debug!(error = ?refusal.as_ref().err(), "expect_words: returning an error to the caller");
        return refusal;
    }
    Ok(rest)
}

/// Refuse a concurrency bound typed as a number above one.
///
/// How many things can wait at once depends on the machine and on everything
/// else running, which the author cannot see; a typed `16` is right on one
/// host and wrong on the next. `1` (one at a time) is a statement about the
/// work, not the machine, so it stands.
pub(super) fn refuse_typed_concurrency(tokens: &[TokenTree]) -> Result<()> {
    if let [TokenTree::Literal(ref literal)] = *tokens
        && literal
            .to_string()
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            != "1"
    {
        let refusal = Err(Refusal::new(
            literal.span(),
            "a typed concurrency number is right on one machine and wrong on the next: drop \
         `at most` and the runtime sizes the fan-out to the host, or name where the limit \
         comes from, `at most (upstream.limit) at once`",
        ));
        tracing::debug!(error = ?refusal.as_ref().err(), "refuse_typed_concurrency: returning an error to the caller");
        return refusal;
    }
    Ok(())
}

/// A bound: a literal checked here against `1..=max`, or an expression the
/// runtime checks.
pub(super) fn bound(tokens: &[TokenTree], line: &Line, clause: &str, max: u64) -> Result<Bound> {
    if let [TokenTree::Literal(ref literal)] = *tokens {
        let digits: String = literal
            .to_string()
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        // An explicit match rather than `map_err(|_| ..)`: the parse error is
        // `ParseIntError`, whose text is "invalid digit found in string", and the
        // message the author needs names the clause instead. Stating the refusal
        // in the arm keeps that decision on the line that makes it.
        let value: u64 = match digits.parse() {
            Ok(value) => value,
            Err(_not_a_whole_number) => {
                let refusal = Err(Refusal::new(
                    literal.span(),
                    format!("`{clause}` takes a whole number"),
                ));
                tracing::debug!(error = ?refusal.as_ref().err(), "bound: returning an error to the caller");
                return refusal;
            }
        };
        if value == 0 || value > max {
            let refusal = Err(Refusal::new(
                literal.span(),
                format!("`{clause} {value}` is outside 1..={max}"),
            ));
            tracing::debug!(error = ?refusal.as_ref().err(), "bound: returning an error to the caller");
            return refusal;
        }
        return Ok(Bound::Count(value));
    }
    if tokens.is_empty() {
        let refusal = Err(Refusal::new(
            line.span,
            format!("`{clause}` needs a number"),
        ));
        tracing::debug!(error = ?refusal.as_ref().err(), "bound: returning an error to the caller");
        return refusal;
    }
    if let [ref only] = *tokens
        && let Some(inner) = group(only).filter(|_| is_parens(only))
    {
        return Ok(Bound::Expr(inner.stream()));
    }
    let expr: TokenStream = tokens.iter().cloned().collect();
    let parenthesised = Group::new(Delimiter::Parenthesis, expr);
    Ok(Bound::Expr(TokenStream::from(TokenTree::Group(
        parenthesised,
    ))))
}

/// Nanoseconds per unit a duration literal may use.
const UNITS: [(&str, u128); 4] = [
    ("ms", 1_000_000),
    ("s", 1_000_000_000),
    ("m", 60_000_000_000),
    ("h", 3_600_000_000_000),
];

/// A duration: `250ms`, `2s`, `1.5s`, `5m`, `1h`, or `(expr)` of type `Duration`.
pub(super) fn duration(tokens: &[TokenTree], line: &Line) -> Result<Duration> {
    match *tokens {
        [TokenTree::Literal(ref literal)] => {
            let written = literal.to_string().replace('_', "");
            // Two readings, and the second is not a fallback: a literal whose
            // every character is numeric or a point names no unit at all, which
            // is the refusal below rather than a unit of nothing.
            let (number, unit) = match written
                .find(|character: char| !(character.is_ascii_digit() || character == '.'))
            {
                Some(at) => written.split_at(at),
                None => (written.as_str(), ""),
            };
            let Some(&(_, scale)) = UNITS.iter().find(|&&(name, _)| name == unit) else {
                let refusal = Err(Refusal::new(
                    literal.span(),
                    "a duration is a number with a unit: `250ms`, `2s`, `5m`, `1h`",
                ));
                tracing::debug!(error = ?refusal.as_ref().err(), "duration: returning an error to the caller");
                return refusal;
            };
            let Some(nanos) = scaled(number, scale) else {
                let refusal = Err(Refusal::new(
                    literal.span(),
                    "this duration is too large or not a number",
                ));
                tracing::debug!(error = ?refusal.as_ref().err(), "duration: returning an error to the caller");
                return refusal;
            };
            if nanos == 0 {
                let refusal = Err(Refusal::new(
                    literal.span(),
                    "a zero duration is no deadline; give a positive one",
                ));
                tracing::debug!(error = ?refusal.as_ref().err(), "duration: returning an error to the caller");
                return refusal;
            }
            // As above: the conversion error is "out of range integral type
            // conversion attempted", and the span plus the size is what the
            // author needs.
            match u64::try_from(nanos) {
                Ok(nanos) => Ok(Duration::Nanos(nanos)),
                Err(_too_large) => {
                    let refusal = Err(Refusal::new(literal.span(), "this duration is too large"));
                    tracing::debug!(error = ?refusal.as_ref().err(), "duration: returning an error to the caller");
                    refusal
                }
            }
        }
        [ref only] if is_parens(only) => {
            // The guard above already decided this token is a parenthesised
            // group, so the group is there to read; the arm states that rather
            // than answering with an empty stream for a group it just matched.
            match group(only) {
                Some(inner) => Ok(Duration::Expr(inner.stream())),
                None => {
                    let refusal = Err(Refusal::new(
                        line.span,
                        "a duration is `250ms`, `2s`, `1.5s`, `5m`, `1h`, or `(expr)`",
                    ));
                    tracing::debug!(error = ?refusal.as_ref().err(), "duration: returning an error to the caller");
                    refusal
                }
            }
        }
        _ => {
            let refusal = Err(Refusal::new(
                line.span,
                "a duration is `250ms`, `2s`, `1.5s`, `5m`, `1h`, or `(expr)`",
            ));
            tracing::debug!(error = ?refusal.as_ref().err(), "duration: returning an error to the caller");
            refusal
        }
    }
}

/// `number` (digits with at most one `.`) times `scale`, exactly.
fn scaled(number: &str, scale: u128) -> Option<u128> {
    // A number with no point is a whole number: its fraction is empty, and an
    // empty fraction is what a whole number multiplies by, not a default for a
    // missing one.
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
        let step = fraction_step(total, place, digit)?;
        total = step.total;
        place = step.place;
    }
    Some(total)
}

/// One decimal digit's contribution to a scaled number.
struct FractionStep {
    /// The running total with this digit folded in.
    total: u128,
    /// The place value the next digit would carry.
    place: u128,
}

/// Folds one fraction digit into `total` at `place`.
///
/// A helper rather than the loop body inline: as one block the loop carried
/// three propagation operators across the place division, the digit decode and
/// the product, so "why does an over-long fraction refuse" meant reading all
/// three to find the overflow check.
fn fraction_step(total: u128, place: u128, digit: char) -> Option<FractionStep> {
    let place = place.checked_div(10)?;
    let value = u128::from(digit.to_digit(10)?);
    let total = total.checked_add(value.checked_mul(place)?)?;
    Some(FractionStep { total, place })
}

/// A step's entry in the architecture map.
pub(super) fn shape(
    kind: Kind,
    subject: String,
    detail: String,
    line: &Line,
    children: Vec<StepShape>,
) -> StepShape {
    StepShape {
        kind,
        subject,
        detail,
        line: line.number,
        children,
    }
}

/// Tokens as a person would write them.
pub(super) fn text_of(stream: &TokenStream) -> String {
    let tokens: Vec<TokenTree> = stream.clone().into_iter().collect();
    text(&tokens)
}

/// A line that is exactly `run name(args)`: the call, whose result is the
/// line's.
///
/// # Errors
///
/// A domain word in the arguments written wrong (see [`rewrite`]).
pub(super) fn run_only(
    tokens: &[TokenTree],
    labels: &mut Labels,
    shapes: &mut Vec<StepShape>,
    line: &Line,
) -> Result<Option<Call>> {
    let Some((first, rest)) = tokens.split_first() else {
        return Ok(None);
    };
    if !lexicon::is(first, Kind::Run) {
        return Ok(None);
    }
    let lookahead_labels = labels.clone();
    let lookahead_shapes = shapes.len();
    // The call must consume the whole line to be the line's value; anything left
    // after it is another statement, so the lookahead is rolled back.
    match run_call(rest, labels, shapes, line)? {
        Some((&[], call)) => Ok(Some(call)),
        _ => {
            *labels = lookahead_labels;
            shapes.truncate(lookahead_shapes);
            Ok(None)
        }
    }
}
