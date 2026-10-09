//! Inline words: `run`, bounds, durations, and the map entries they leave.

use lgwks_deps::proc_macro2::{Group, Ident, Literal, Span, TokenStream, TokenTree};
use lgwks_deps::quote::{ToTokens, quote};
use lgwks_deps::syn::{Error, Result};

use crate::lexicon::{self, Kind};
use crate::lines::{Line, after, group, ident, is_ident, is_parens, is_punct, text};

use super::{Labels, Shapes, runtime};

/// Pass Rust through, turning `run name(args)` into a call in this scope.
pub(super) fn rewrite(
    tokens: &[TokenTree],
    labels: &mut Labels,
    shapes: &mut Shapes,
    line: &Line,
) -> Result<TokenStream> {
    let mut output = TokenStream::new();
    // The walk carries the tail rather than an index into it, so "the tokens
    // after `run`" is the slice itself and a call that consumed several tokens
    // hands back where it stopped, instead of the caller re-deriving both.
    let mut rest = tokens;
    while let Some((token, tail)) = rest.split_first() {
        if lexicon::is(token, Kind::Run)
            && let Some((remaining, call)) = run_call(tail, labels, shapes, line)?
        {
            output.extend(quote!((#call?)));
            rest = remaining;
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
        rest = tail;
    }
    Ok(output)
}

/// `path(args)` after a `run`: the call, awaited and propagated, and the tokens
/// left after it. `None` when what follows `run` is not a call, in which case
/// nothing was consumed and the caller continues from the first token.
pub(super) fn run_call<'line>(
    tokens: &'line [TokenTree],
    labels: &mut Labels,
    shapes: &mut Shapes,
    line: &Line,
) -> Result<Option<(&'line [TokenTree], TokenStream)>> {
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
    let (Some(callee), Some(args)) = (callee, rest.first()) else {
        return Ok(None);
    };
    let Some(args_group) = group(args).filter(|_| is_parens(args)) else {
        return Ok(None);
    };
    // The walk above left `rest` on the argument group, so the call consumed that
    // group too and what follows the call is what is past it.
    let after_args = after(rest, 1);
    let path_tokens: TokenStream = path.iter().cloned().collect();
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
        quote!(#path_tokens(#scope_argument).await)
    } else {
        quote!(#path_tokens(#scope_argument, #arguments).await)
    };
    shapes.push(shape_tokens(
        "Run",
        &callee_name,
        &format!("run {}", text(&path)),
        line,
        &[],
    ));
    Ok(Some((after_args, call)))
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
                let refusal = Err(Error::new(line.span, format!("this line reads {form}")));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "expect_words: returning an error to the caller");
                return refusal;
            }
        }
    }
    if rest.is_empty() {
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
        [ref only] if is_parens(only) => {
            // The guard above already decided this token is a parenthesised
            // group, so the group is there to read; the arm states that rather
            // than answering with an empty stream for a group it just matched.
            match group(only) {
                Some(inner) => Ok(inner.stream()),
                None => {
                    let refusal = Err(Error::new(
                        line.span,
                        "a duration is `250ms`, `2s`, `1.5s`, `5m`, `1h`, or `(expr)`",
                    ));
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "duration: returning an error to the caller");
                    refusal
                }
            }
        }
        _ => Err(Error::new(
            line.span,
            "a duration is `250ms`, `2s`, `1.5s`, `5m`, `1h`, or `(expr)`",
        )),
    }
}

/// `number` (digits with at most one `.`) times `scale`, exactly.
pub(super) fn scaled(number: &str, scale: u128) -> Option<u128> {
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

/// The line number as a `u32` literal, saturating at `u32::MAX`.
///
/// The arithmetic saturates rather than substituting a value: a source with more
/// lines than `u32::MAX` cannot be read by any host, and a saturated line number
/// keeps the emitted literal the last line there is rather than wrapping onto
/// some other line of the file.
pub(super) fn line_literal(line: &Line) -> Literal {
    match u32::try_from(line.number) {
        Ok(number) => Literal::u32_unsuffixed(number),
        Err(_more_lines_than_a_u32_holds) => Literal::u32_unsuffixed(u32::MAX),
    }
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
    if !lexicon::is(first, Kind::Run) {
        return Ok(None);
    }
    let lookahead_labels = labels.seen.clone();
    let lookahead_shapes = shapes.len();
    // The call must consume the whole line to be the line's value; anything left
    // after it is another statement, so the lookahead is rolled back.
    match run_call(rest, labels, shapes, line)? {
        Some((&[], call)) => Ok(Some(call)),
        _ => {
            labels.seen = lookahead_labels;
            shapes.truncate(lookahead_shapes);
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use lgwks_deps::proc_macro2::TokenStream;

    use crate::tests::expand;

    /// An expansion carrying a `run` call is still Rust.
    ///
    /// The re-parse is the whole assertion: `script!` emits an `async fn` and an
    /// `ARCHITECTURE` const, so text that parses back is an expansion a compiler
    /// can read. Splicing a call into the middle of a line is the one thing that
    /// can break that, and it is invisible to an assertion that only looks for a
    /// callee's name — a call whose tail arithmetic was off by one still
    /// contained the callee.
    #[test]
    fn an_expansion_with_a_run_call_is_still_rust() -> Result<(), String> {
        // Every shape a `run` call reaches: the whole line, a `let` binding, a
        // binding inside a `for` body, and a `::` callee with tokens after the
        // call. Each is a different route through `rewrite`, so a tail that is
        // off by one in one of them shows here.
        let source = "flow f(log: &RefCell<Vec<u8>>, rows: Vec<u8>) -> u8:\n\
                      \x20   together:\n\
                      \x20       let joined = run branch(log, \"a\")\n\
                      \x20   let first = run branch(log, \"a\")\n\
                      \x20   for value in [1, 2]:\n\
                      \x20       let seen = run outer::inner(value)\n\
                      \x20   let scaled = run outer::fetch(rows).pow(2)\n\
                      \x20   give back first + seen + scaled\n";
        let expanded = expand(source)?;
        assert!(
            expanded.contains("branch"),
            "the callee is in the expansion: {expanded}"
        );
        assert!(
            expanded.contains("outer :: inner"),
            "a `::` path is in the expansion: {expanded}"
        );
        // The tokens after a call on the same line belong to the line, not to
        // the call: a callee that consumed one token too many takes the rest of
        // the expression with it, and the expansion is still valid Rust
        // afterwards — which is why the re-parse alone does not catch it.
        assert!(
            expanded.contains("pow"),
            "the tokens after a `run` call survive: {expanded}"
        );
        // A `let` inside `together:` binds the joined result, so the pattern the
        // split produced is the name alone and never the name with its `=`.
        assert!(
            expanded.contains("let (joined ,) ="),
            "a `let` binding binds its name: {expanded}"
        );
        TokenStream::from_str(&expanded)
            .map(|_| ())
            .map_err(|error| format!("the expansion is not Rust ({error}): {expanded}"))
    }
}
