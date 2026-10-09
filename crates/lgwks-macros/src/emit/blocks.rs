//! The blocks: `if`, `each`, `within`, `retry`, `step`, `together`, `for`.

use lgwks_deps::proc_macro2::{TokenStream, TokenTree};
use lgwks_deps::quote::quote;
use lgwks_deps::syn::{Error, Result};

use crate::lexicon::{self, Kind};
use crate::lines::{Line, Node, after, ident, is_ident, is_punct, text};
use crate::refuse;

use super::words::{bound, duration, refuse_typed_concurrency, rewrite, run_only, shape_tokens};
use super::{Labels, Piece, Place, Shapes, block, construct_expr, ok, runtime};

/// The pattern before a line's `in`, and the items after it.
///
/// `None` when the line names no such keyword, which is the refusal each form
/// reports in its own words. The search is what proves the keyword is on the
/// line, so neither side is a stand-in for a tail that is not there.
fn split_at_keyword<'a>(
    tokens: &'a [TokenTree],
    keyword: &str,
) -> Option<(&'a [TokenTree], &'a [TokenTree])> {
    let at = tokens.iter().position(|token| is_ident(token, keyword))?;
    let (before, at_keyword) = tokens.split_at(at);
    Some((before, after(at_keyword, 1)))
}

/// The pattern and the items `each x in xs:` and `for x in xs:` both read.
///
/// Both forms refuse the same two ways — no pattern before the `in`, and no
/// items after it — so the search and the two refusals are stated once here and
/// each form passes the keyword its author wrote.
fn pattern_and_items<'a>(
    line: &Line,
    tokens: &'a [TokenTree],
    form: &str,
) -> Result<(&'a [TokenTree], &'a [TokenTree])> {
    if let Some((pattern, items)) = split_at_keyword(tokens, "in")
        && !pattern.is_empty()
        && !items.is_empty()
    {
        return Ok((pattern, items));
    }
    let refusal = Err(Error::new(
        line.span,
        format!("`{form}` reads `{form} <name> in <items>:`"),
    ));
    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "pattern_and_items: returning an error to the caller");
    refusal
}

/// `if cond:` with its `else if cond:` and `else:` siblings.
pub(super) fn if_chain(
    head: &Node,
    chain: &[&Node],
    labels: &mut Labels,
    place: Place,
    wants_value: bool,
) -> Result<(Piece, Shapes)> {
    let has_else = chain.last().is_some_and(|last| last.line.tokens.len() == 1);
    let branch_place = Place {
        nested: place.nested,
        wants_value: wants_value && has_else,
    };
    let mut shapes = Shapes::default();
    let mut code = TokenStream::new();
    let mut every_branch_leaves = has_else;
    for (position, node) in std::iter::once(head)
        .chain(chain.iter().copied())
        .enumerate()
    {
        let line = &node.line;
        refuse::check(&line.tokens)?;
        if !line.opens_block {
            let refusal = Err(Error::new(
                line.span,
                "`else` opens a block: end it with `:`",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "if_chain: returning an error to the caller");
            return refusal;
        }
        let is_final_else = position > 0 && line.tokens.len() == 1;
        let condition_tokens: &[TokenTree] = if is_final_else {
            &[]
        } else if position == 0 {
            after(&line.tokens, 1)
        } else {
            match *after(&line.tokens, 1) {
                [ref word, ref rest @ ..] if lexicon::is(word, Kind::If) && !rest.is_empty() => rest,
                _ => {
                    let refusal = Err(Error::new(
                        line.span,
                        "an `else` line is `else:` or `else if <condition>:`",
                    ));
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "if_chain: returning an error to the caller");
                    return refusal;
                }
            }
        };
        if !is_final_else && condition_tokens.is_empty() {
            let refusal = Err(Error::new(
                line.span,
                "`if` needs a condition before its `:`",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "if_chain: returning an error to the caller");
            return refusal;
        }
        if is_final_else && position.saturating_add(1) != chain.len().saturating_add(1) {
            let refusal = Err(Error::new(line.span, "`else:` must be the last branch"));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "if_chain: returning an error to the caller");
            return refusal;
        }
        let mut run_shapes = Shapes::default();
        let condition = rewrite(condition_tokens, labels, &mut run_shapes, line)?;
        let body = block(&node.children, labels, branch_place)?;
        every_branch_leaves = every_branch_leaves && body.diverges;
        let (branch, inner) = body.into_block();
        run_shapes.extend(inner);
        let kind = if position == 0 { "If" } else { "Else" };
        shapes.push(shape_tokens(
            kind,
            "",
            &text(&line.tokens),
            line,
            run_shapes.as_slice(),
        ));
        let keyword = match (position, is_final_else) {
            (0, _) => quote!(if #condition),
            (_, true) => quote!(else),
            (_, false) => quote!(else if #condition),
        };
        code.extend(quote!(#keyword #branch));
    }
    // A chain whose every branch leaves the block is itself a way out, not a
    // value: wrapping it as one would be an unreachable `Ok(..)`.
    if every_branch_leaves {
        return Ok((Piece::leaving(code), shapes));
    }
    Ok((Piece::new(code, branch_place.wants_value), shapes))
}

/// `each <pattern> in <items>:`, optionally `, at most (<limit>) at once`.
pub(super) fn each(
    line: &Line,
    children: &[Node],
    labels: &mut Labels,
) -> Result<(TokenStream, TokenStream)> {
    let tokens = after(&line.tokens, 1);
    let (pattern, after_in) = pattern_and_items(line, tokens, "each")?;
    let bound_at = after_in.windows(3).rposition(|window| {
        matches!(*window, [ref comma, ref at, ref most] if is_punct(comma, ',') && is_ident(at, "at") && is_ident(most, "most"))
    });
    let (items, limit_tokens) = match bound_at {
        None => (after_in, None),
        Some(bound_at) => {
            let (items, clause) = after_in.split_at(bound_at);
            // The window that found the bound matched three tokens, so the
            // clause is at least `, at most` long and dropping them cannot run
            // off its end.
            let (_, after_bound) = clause.split_at(3);
            match *after_bound {
                [ref limit @ .., ref at, ref once]
                    if is_ident(at, "at") && is_ident(once, "once") && !limit.is_empty() =>
                {
                    (items, Some(limit))
                }
                _ => {
                    let refusal = Err(Error::new(
                        line.span,
                        "the bound reads `, at most (limit) at once`",
                    ));
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "each: returning an error to the caller");
                    return refusal;
                }
            }
        }
    };
    if pattern.is_empty() || items.is_empty() {
        let refusal = Err(Error::new(
            line.span,
            "`each` reads `each <name> in <items>:`",
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "each: returning an error to the caller");
        return refusal;
    }
    let script = runtime();
    let (limit, detail) = match limit_tokens {
        None => (
            quote!(::core::option::Option::None),
            String::from("sized to the machine"),
        ),
        Some(limit_tokens) => {
            refuse_typed_concurrency(limit_tokens)?;
            let limit = bound(limit_tokens, line, "at most", 65_536)?;
            (
                quote!(::core::option::Option::Some(#script::at_most(#limit)?)),
                text(limit_tokens),
            )
        }
    };
    let pattern_text = text(pattern);
    let label = labels.next(&format!("each:{pattern_text}"));
    let mut run_shapes = Shapes::default();
    let items = rewrite(items, labels, &mut run_shapes, line)?;
    let pattern: TokenStream = pattern.iter().cloned().collect();
    let (body, inner) = nested_body(children)?;
    run_shapes.extend(inner);
    let expr = quote! {
        #script::each(scope, #label, #limit, #items, async |scope: #script::Scope, #pattern| {
            let scope = &scope;
            #body
        }).await
    };
    Ok((
        expr,
        shape_tokens(
            "Each",
            &detail,
            &text(&line.tokens),
            line,
            run_shapes.as_slice(),
        ),
    ))
}

/// `within <duration>:`
pub(super) fn within(
    line: &Line,
    children: &[Node],
    labels: &mut Labels,
) -> Result<(TokenStream, TokenStream)> {
    let spec = after(&line.tokens, 1);
    let limit = duration(spec, line)?;
    let label = labels.next("within");
    let (body, inner) = nested_body(children)?;
    let script = runtime();
    let expr = quote! {
        #script::within(scope, #label, #limit, async #body).await
    };
    Ok((
        expr,
        shape_tokens(
            "Within",
            &text(spec),
            &text(&line.tokens),
            line,
            inner.as_slice(),
        ),
    ))
}

/// `retry up to <N> times[, waiting <duration>]:`
pub(super) fn retry(
    line: &Line,
    children: &[Node],
    labels: &mut Labels,
) -> Result<(TokenStream, TokenStream)> {
    let mut rest = after(&line.tokens, 1);
    if let [ref up, ref to, ref tail @ ..] = *rest
        && is_ident(up, "up")
        && is_ident(to, "to")
    {
        rest = tail;
    }
    let Some((count, after)) = split_at_keyword(rest, "times") else {
        let refusal = Err(Error::new(
            line.span,
            "`retry` reads `retry up to N times:` or `retry up to N times, waiting 100ms:`",
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "retry: returning an error to the caller");
        return refusal;
    };
    let subject = text(count);
    let waiting = match *after {
        [] => quote!(::core::time::Duration::from_millis(100)),
        [ref comma, ref word, ref spec @ ..]
            if is_punct(comma, ',') && is_ident(word, "waiting") =>
        {
            duration(spec, line)?
        }
        _ => {
            let refusal = Err(Error::new(
                line.span,
                "after `times` comes `, waiting <duration>` or `:`",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "retry: returning an error to the caller");
            return refusal;
        }
    };
    let count = bound(count, line, "up to", 1_000)?;
    let label = labels.next("retry");
    let (body, inner) = nested_body(children)?;
    let script = runtime();
    let expr = quote! {
        #script::retry(scope, #label, #script::attempts(#count)?, #waiting, async |scope: #script::Scope, attempt: u32| {
            let scope = &scope;
            let _ = attempt;
            #body
        }).await
    };
    Ok((
        expr,
        shape_tokens(
            "Retry",
            &subject,
            &text(&line.tokens),
            line,
            inner.as_slice(),
        ),
    ))
}

/// `step <name>:`
pub(super) fn step(
    line: &Line,
    children: &[Node],
    labels: &mut Labels,
) -> Result<(TokenStream, TokenStream)> {
    let name = match *after(&line.tokens, 1) {
        [ref only] => ident(only),
        _ => None,
    };
    let Some(name) = name else {
        let refusal = Err(Error::new(
            line.span,
            "`step` reads `step <name>:`, where the name is one word",
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "step: returning an error to the caller");
        return refusal;
    };
    let label = labels.next(&name.to_string());
    let (body, inner) = nested_body(children)?;
    let script = runtime();
    let expr = quote! {
        {
            let scope = &scope.enter(#label)?;
            let outcome: ::core::result::Result<_, #script::FlowError> = async #body.await;
            outcome.map_err(|error| error.located(scope))
        }
    };
    Ok((
        expr,
        shape_tokens(
            "Step",
            &name.to_string(),
            &text(&line.tokens),
            line,
            inner.as_slice(),
        ),
    ))
}

/// `together:` and its branches, run concurrently on this task.
pub(super) fn together(
    line: &Line,
    children: &[Node],
    labels: &mut Labels,
) -> Result<(Piece, TokenStream)> {
    if line.tokens.len() != 1 {
        let refusal = Err(Error::new(
            line.span,
            "`together:` takes nothing before its `:`",
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "together: returning an error to the caller");
        return refusal;
    }
    let mut patterns = Vec::new();
    let mut branches = Vec::new();
    let mut shapes = Shapes::default();
    for child in children {
        let (pattern, branch) = together_child(child, labels, &mut shapes)?;
        patterns.push(pattern);
        branches.push(quote!(async { #branch }));
    }
    let count = branches.len().to_string();
    let piece = Piece::new(
        quote!(let (#(#patterns,)*) = ::lgwks_bot::try_join!(#(#branches),*)?;),
        false,
    );
    Ok((
        piece,
        shape_tokens("Together", &count, "together", line, shapes.as_slice()),
    ))
}

/// Emits one `together:` child as a `(pattern, async body)` pair.
///
/// A helper rather than the loop body inline: as one block the loop carried
/// four propagation operators across the refusal check, the `let` split, the
/// block construct and the inline rewrite, so a caller reading the `together:`
/// arm had to hold four distinct refusal paths to see what one child can refuse
/// on.
fn together_child(
    child: &Node,
    labels: &mut Labels,
    shapes: &mut Shapes,
) -> Result<(TokenStream, TokenStream)> {
    let child_line = &child.line;
    refuse::check(&child_line.tokens)?;
    let (pattern, value) = if lexicon::kind_of(child_line) == Some(Kind::Let) {
        let (pattern, rest) = split_let(child_line)?;
        (pattern, rest)
    } else {
        (quote!(_), child_line.tokens.clone())
    };
    let value_line = Line {
        tokens: value,
        column: child_line.column,
        number: child_line.number,
        opens_block: child_line.opens_block,
        span: child_line.span,
    };
    let branch = if value_line.opens_block {
        let (expr, shape) = construct_expr(&value_line, &child.children, labels)?;
        shapes.push(shape);
        expr
    } else {
        if !child.children.is_empty() {
            let refusal = Err(Error::new(
                child_line.span,
                "unexpected indent inside `together:`",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "together_child: returning an error to the caller");
            return refusal;
        }
        match run_only(&value_line.tokens, labels, shapes, &value_line)? {
            Some(call) => call,
            None => ok(&rewrite(&value_line.tokens, labels, shapes, &value_line)?),
        }
    };
    Ok((pattern, branch))
}

/// `for <pattern> in <items>:`, sequential, one scope per iteration.
pub(super) fn for_loop(
    line: &Line,
    children: &[Node],
    labels: &mut Labels,
    place: Place,
) -> Result<(Piece, TokenStream)> {
    let tokens = after(&line.tokens, 1);
    let (pattern, items) = pattern_and_items(line, tokens, "for")?;
    let label = labels.next(&format!("for:{}", text(pattern)));
    let mut run_shapes = Shapes::default();
    let items = rewrite(items, labels, &mut run_shapes, line)?;
    let pattern: TokenStream = pattern.iter().cloned().collect();
    let mut inner_labels = Labels::default();
    let body = block(
        children,
        &mut inner_labels,
        Place {
            nested: place.nested,
            wants_value: false,
        },
    )?;
    let (body, inner) = body.into_block();
    run_shapes.extend(inner);
    let tokens = quote! {
        for (lgwks_script_index, #pattern) in ::core::iter::IntoIterator::into_iter(#items).enumerate() {
            let scope = &scope.item(#label, lgwks_script_index)?;
            #body
        }
    };
    Ok((
        Piece::new(tokens, false),
        shape_tokens(
            "For",
            &label,
            &text(&line.tokens),
            line,
            run_shapes.as_slice(),
        ),
    ))
}

/// The body of a construct with its own async block: a fresh scope for labels,
/// `give back` refused, and the last line as the value.
pub(super) fn nested_body(children: &[Node]) -> Result<(TokenStream, Shapes)> {
    let mut labels = Labels::default();
    let body = block(
        children,
        &mut labels,
        Place {
            nested: true,
            wants_value: true,
        },
    )?;
    Ok(body.into_ok_block())
}

/// Split `let <pattern> = <rest>` at its assignment `=`.
pub(super) fn split_let(line: &Line) -> Result<(TokenStream, Vec<TokenTree>)> {
    let tokens = after(&line.tokens, 1);
    let mut previous_joint = false;
    let mut at = None;
    for (index, token) in tokens.iter().enumerate() {
        if let TokenTree::Punct(ref punct) = *token {
            if punct.as_char() == '='
                && !previous_joint
                && punct.spacing() == lgwks_deps::proc_macro2::Spacing::Alone
            {
                at = Some(index);
                break;
            }
            previous_joint = punct.spacing() == lgwks_deps::proc_macro2::Spacing::Joint;
        } else {
            previous_joint = false;
        }
    }
    let Some(at) = at else {
        let refusal = Err(Error::new(line.span, "`let` reads `let <name> = <value>`"));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "split_let: returning an error to the caller");
        return refusal;
    };
    // The `=` this search found splits the line: the pattern is every token
    // before it, and `after` reads the value off the tokens past it, which is
    // one reading of "after the word the caller matched" shared with every other
    // block form. Splitting past the `=` instead would bind `let x =` as the
    // pattern, which is the one thing a pattern cannot be.
    let (pattern, at_equals) = tokens.split_at(at);
    let rest = after(at_equals, 1);
    if pattern.is_empty() || rest.is_empty() {
        let refusal = Err(Error::new(line.span, "`let` reads `let <name> = <value>`"));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "split_let: returning an error to the caller");
        return refusal;
    }
    Ok((pattern.iter().cloned().collect(), rest.to_vec()))
}
