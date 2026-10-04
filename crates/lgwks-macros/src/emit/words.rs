//! Inline words: `run`, bounds, durations, and the map entries they leave.

use lgwks_deps::proc_macro2::{Group, Ident, Literal, Span, TokenStream, TokenTree};
use lgwks_deps::quote::{ToTokens, quote};
use lgwks_deps::syn::{Error, Result};

use crate::lines::{Line, group, ident, is_ident, is_parens, is_punct, text};

use super::{Labels, Shapes, runtime};

/// Pass Rust through, turning `run name(args)` into a call in this scope.
pub(super) fn rewrite(
    tokens: &[TokenTree],
    labels: &mut Labels,
    shapes: &mut Shapes,
    line: &Line,
) -> Result<TokenStream> {
    let mut output = TokenStream::new();
    let mut index: usize = 0;
    while let Some(token) = tokens.get(index) {
        if is_ident(token, "run")
            && let Some((consumed, call)) = run_call(
                tokens.get(index.saturating_add(1)..).unwrap_or_default(),
                labels,
                shapes,
                line,
            )?
        {
            output.extend(quote!((#call?)));
            index = index.saturating_add(consumed).saturating_add(1);
            continue;
        }
        if let TokenTree::Group(ref group) = *token {
            let inner: Vec<TokenTree> = group.stream().into_iter().collect();
            let mut rebuilt = Group::new(group.delimiter(), rewrite(&inner, labels, shapes, line)?);
            rebuilt.set_span(group.span());
            output.extend([TokenTree::Group(rebuilt)]);
        } else {
            output.extend([token.clone()]);
        }
        index = index.saturating_add(1);
    }
    Ok(output)
}

/// `path(args)` after a `run`: the call, awaited and propagated, and how many
/// tokens it used. `None` when what follows `run` is not a call.
pub(super) fn run_call(
    tokens: &[TokenTree],
    labels: &mut Labels,
    shapes: &mut Shapes,
    line: &Line,
) -> Result<Option<(usize, TokenStream)>> {
    let mut path_end: usize = 0;
    let mut callee: Option<&Ident> = None;
    while let Some(segment) = tokens.get(path_end).and_then(ident) {
        callee = Some(segment);
        path_end = path_end.saturating_add(1);
        let colons = (tokens.get(path_end), tokens.get(path_end.saturating_add(1)));
        if let (Some(first), Some(second)) = colons
            && is_punct(first, ':')
            && is_punct(second, ':')
        {
            path_end = path_end.saturating_add(2);
            continue;
        }
        break;
    }
    let (Some(callee), Some(args)) = (callee, tokens.get(path_end)) else {
        return Ok(None);
    };
    let Some(args_group) = group(args).filter(|_| is_parens(args)) else {
        return Ok(None);
    };
    let path: TokenStream = tokens.iter().take(path_end).cloned().collect();
    let arguments: Vec<TokenTree> = args_group.stream().into_iter().collect();
    let arguments = rewrite(&arguments, labels, shapes, line)?;
    let callee_name = callee.to_string();
    let label = labels.next(&callee_name);
    let scope_argument = if label == callee_name {
        quote!(scope)
    } else {
        quote!(&scope.enter(#label)?)
    };
    let call = if arguments.is_empty() {
        quote!(#path(#scope_argument).await)
    } else {
        quote!(#path(#scope_argument, #arguments).await)
    };
    shapes.push(shape_tokens(
        "Run",
        &callee_name,
        &format!("run {}", text(tokens.get(..path_end).unwrap_or_default())),
        line,
        &[],
    ));
    Ok(Some((path_end.saturating_add(1), call)))
}

/// Words a line must start with, and the tokens after them.
pub(super) fn expect_words<'line>(
    line: &'line Line,
    words: &[&str],
    form: &str,
) -> Result<&'line [TokenTree]> {
    let matches = words.iter().enumerate().all(|(index, word)| {
        line.tokens
            .get(index)
            .is_some_and(|token| is_ident(token, word))
    });
    let rest = line.tokens.get(words.len()..).unwrap_or_default();
    if !matches || rest.is_empty() {
        let refusal = Err(Error::new(line.span, format!("this line reads {form}")));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "expect_words: returning an error to the caller");
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
        let refusal = Err(Error::new(
            literal.span(),
            "a typed concurrency number is right on one machine and wrong on the next: drop \
         `at most` and the runtime sizes the fan-out to the host, or name where the limit \
         comes from, `at most (upstream.limit) at once`",
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "refuse_typed_concurrency: returning an error to the caller");
        return refusal;
    }
    Ok(())
}

/// A bound: a literal checked here against `1..=max`, or an expression the
/// runtime checks.
pub(super) fn bound(
    tokens: &[TokenTree],
    line: &Line,
    clause: &str,
    max: u64,
) -> Result<TokenStream> {
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
                let refusal = Err(Error::new(
                    literal.span(),
                    format!("`{clause}` takes a whole number"),
                ));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "bound: returning an error to the caller");
                return refusal;
            }
        };
        if value == 0 || value > max {
            let refusal = Err(Error::new(
                literal.span(),
                format!("`{clause} {value}` is outside 1..={max}"),
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "bound: returning an error to the caller");
            return refusal;
        }
        let literal = Literal::u64_unsuffixed(value);
        return Ok(literal.into_token_stream());
    }
    if tokens.is_empty() {
        let refusal = Err(Error::new(line.span, format!("`{clause}` needs a number")));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "bound: returning an error to the caller");
        return refusal;
    }
    if let [ref only] = *tokens
        && let Some(inner) = group(only).filter(|_| is_parens(only))
    {
        return Ok(inner.stream());
    }
    let expr: TokenStream = tokens.iter().cloned().collect();
    Ok(quote!((#expr)))
}

/// Nanoseconds per unit a duration literal may use.
pub(super) const UNITS: [(&str, u128); 4] = [
    ("ms", 1_000_000),
    ("s", 1_000_000_000),
    ("m", 60_000_000_000),
    ("h", 3_600_000_000_000),
];

/// A duration: `250ms`, `2s`, `1.5s`, `5m`, `1h`, or `(expr)` of type `Duration`.
pub(super) fn duration(tokens: &[TokenTree], line: &Line) -> Result<TokenStream> {
    match *tokens {
        [TokenTree::Literal(ref literal)] => {
            let written = literal.to_string().replace('_', "");
            let split =
                written.find(|character: char| !(character.is_ascii_digit() || character == '.'));
            let (number, unit) = written.split_at(split.unwrap_or(written.len()));
            let Some(&(_, scale)) = UNITS.iter().find(|&&(name, _)| name == unit) else {
                let refusal = Err(Error::new(
                    literal.span(),
                    "a duration is a number with a unit: `250ms`, `2s`, `5m`, `1h`",
                ));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "duration: returning an error to the caller");
                return refusal;
            };
            let nanos = scaled(number, scale).ok_or_else(|| {
                Error::new(literal.span(), "this duration is too large or not a number")
            })?;
            if nanos == 0 {
                let refusal = Err(Error::new(
                    literal.span(),
                    "a zero duration is no deadline; give a positive one",
                ));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "duration: returning an error to the caller");
                return refusal;
            }
            // As above: the conversion error is "out of range integral type
            // conversion attempted", and the span plus the size is what the
            // author needs.
            let nanos = match u64::try_from(nanos) {
                Ok(nanos) => nanos,
                Err(_too_large) => {
                    let refusal = Err(Error::new(literal.span(), "this duration is too large"));
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "duration: returning an error to the caller");
                    return refusal;
                }
            };
            let literal = Literal::u64_unsuffixed(nanos);
            Ok(quote!(::core::time::Duration::from_nanos(#literal)))
        }
        [ref only] if is_parens(only) => Ok(group(only).map(Group::stream).unwrap_or_default()),
        _ => Err(Error::new(
            line.span,
            "a duration is `250ms`, `2s`, `1.5s`, `5m`, `1h`, or `(expr)`",
        )),
    }
}

/// `number` (digits with at most one `.`) times `scale`, exactly.
pub(super) fn scaled(number: &str, scale: u128) -> Option<u128> {
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
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

/// A `StepShape::new(..)` constant for the architecture map.
pub(super) fn shape_tokens(
    kind: &str,
    subject: &str,
    detail: &str,
    line: &Line,
    children: &[TokenStream],
) -> TokenStream {
    let script = runtime();
    let kind = Ident::new(kind, Span::call_site());
    let number = line_literal(line);
    quote! {
        #script::StepShape::new(#script::StepKind::#kind, #subject, #detail, #number, &[#(#children),*])
    }
}

/// The line number as a `u32` literal.
pub(super) fn line_literal(line: &Line) -> Literal {
    Literal::u32_unsuffixed(u32::try_from(line.number).unwrap_or(u32::MAX))
}

/// Tokens as a person would write them.
pub(super) fn text_of(stream: &TokenStream) -> String {
    let tokens: Vec<TokenTree> = stream.clone().into_iter().collect();
    text(&tokens)
}

/// Whether any identifier in `stream`, at any depth, is one of `words`.
pub(super) fn mentions(stream: &TokenStream, words: &[&str]) -> bool {
    stream.clone().into_iter().any(|token| match token {
        TokenTree::Ident(ref word) => words.iter().any(|wanted| word == wanted),
        TokenTree::Group(ref inner) => mentions(&inner.stream(), words),
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

/// A line that is exactly `run name(args)`: the call as a flow `Result`,
/// without the `?` a call inside a larger expression needs.
pub(super) fn run_only(
    tokens: &[TokenTree],
    labels: &mut Labels,
    shapes: &mut Shapes,
    line: &Line,
) -> Result<Option<TokenStream>> {
    let Some((first, rest)) = tokens.split_first() else {
        return Ok(None);
    };
    if !is_ident(first, "run") {
        return Ok(None);
    }
    let lookahead_labels = labels.seen.clone();
    let lookahead_shapes = shapes.len();
    match run_call(rest, labels, shapes, line)? {
        Some((consumed, call)) if consumed == rest.len() => Ok(Some(call)),
        _ => {
            labels.seen = lookahead_labels;
            shapes.truncate(lookahead_shapes);
            Ok(None)
        }
    }
}
