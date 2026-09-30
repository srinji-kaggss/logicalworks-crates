//! The blocks: `if`, `each`, `within`, `retry`, `step`, `together`, `for`.

use lgwks_deps::proc_macro2::{TokenStream, TokenTree};
use lgwks_deps::quote::quote;
use lgwks_deps::syn::{Error, Result};

use crate::lines::{Line, Node, ident, is_ident, is_punct, text};
use crate::refuse;

use super::words::{bound, duration, refuse_typed_concurrency, rewrite, run_only, shape_tokens};
use super::{Labels, Piece, Place, Shapes, block, construct_expr, ok, runtime};

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
            return Err(Error::new(
                line.span,
                "`else` opens a block: end it with `:`",
            ));
        }
        let is_final_else = position > 0 && line.tokens.len() == 1;
        let condition_tokens: &[TokenTree] = if is_final_else {
            &[]
        } else if position == 0 {
            line.tokens.get(1..).unwrap_or_default()
        } else {
            match *line.tokens.get(1..).unwrap_or_default() {
                [ref word, ref rest @ ..] if is_ident(word, "if") && !rest.is_empty() => rest,
                _ => {
                    return Err(Error::new(
                        line.span,
                        "an `else` line is `else:` or `else if <condition>:`",
                    ));
                }
            }
        };
        if !is_final_else && condition_tokens.is_empty() {
            return Err(Error::new(
                line.span,
                "`if` needs a condition before its `:`",
            ));
        }
        if is_final_else && position.saturating_add(1) != chain.len().saturating_add(1) {
            return Err(Error::new(line.span, "`else:` must be the last branch"));
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
    let tokens = line.tokens.get(1..).unwrap_or_default();
    let Some(in_at) = tokens.iter().position(|token| is_ident(token, "in")) else {
        return Err(Error::new(
            line.span,
            "`each` reads `each <name> in <items>:`",
        ));
    };
    let (pattern, after_in) = tokens.split_at(in_at);
    let after_in = after_in.get(1..).unwrap_or_default();
    let bound_at = after_in.windows(3).rposition(|window| {
        matches!(*window, [ref comma, ref at, ref most] if is_punct(comma, ',') && is_ident(at, "at") && is_ident(most, "most"))
    });
    let (items, limit_tokens) = match bound_at {
        None => (after_in, None),
        Some(bound_at) => {
            let (items, clause) = after_in.split_at(bound_at);
            match *clause.get(3..).unwrap_or_default() {
                [ref limit @ .., ref at, ref once]
                    if is_ident(at, "at") && is_ident(once, "once") && !limit.is_empty() =>
                {
                    (items, Some(limit))
                }
                _ => {
                    return Err(Error::new(
                        line.span,
                        "the bound reads `, at most (limit) at once`",
                    ));
                }
            }
        }
    };
    if pattern.is_empty() || items.is_empty() {
        return Err(Error::new(
            line.span,
            "`each` reads `each <name> in <items>:`",
        ));
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
    let spec = line.tokens.get(1..).unwrap_or_default();
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
    let mut rest = line.tokens.get(1..).unwrap_or_default();
    if let [ref up, ref to, ref tail @ ..] = *rest
        && is_ident(up, "up")
        && is_ident(to, "to")
    {
        rest = tail;
    }
    let Some(times_at) = rest.iter().position(|token| is_ident(token, "times")) else {
        return Err(Error::new(
            line.span,
            "`retry` reads `retry up to N times:` or `retry up to N times, waiting 100ms:`",
        ));
    };
    let (count, after) = rest.split_at(times_at);
    let subject = text(count);
    let after = after.get(1..).unwrap_or_default();
    let waiting = match *after {
        [] => quote!(::core::time::Duration::from_millis(100)),
        [ref comma, ref word, ref spec @ ..]
            if is_punct(comma, ',') && is_ident(word, "waiting") =>
        {
            duration(spec, line)?
        }
        _ => {
            return Err(Error::new(
                line.span,
                "after `times` comes `, waiting <duration>` or `:`",
            ));
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
    let name = match *line.tokens.get(1..).unwrap_or_default() {
        [ref only] => ident(only),
        _ => None,
    };
    let Some(name) = name else {
        return Err(Error::new(
            line.span,
            "`step` reads `step <name>:`, where the name is one word",
        ));
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
        return Err(Error::new(
            line.span,
            "`together:` takes nothing before its `:`",
        ));
    }
    let mut patterns = Vec::new();
    let mut branches = Vec::new();
    let mut shapes = Shapes::default();
    for child in children {
        let child_line = &child.line;
        refuse::check(&child_line.tokens)?;
        let (pattern, value) = if child_line.keyword().as_deref() == Some("let") {
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
            let keyword = value_line.keyword().unwrap_or_default();
            let (expr, shape) = construct_expr(&value_line, &keyword, &child.children, labels)?;
            shapes.push(shape);
            expr
        } else {
            if !child.children.is_empty() {
                return Err(Error::new(
                    child_line.span,
                    "unexpected indent inside `together:`",
                ));
            }
            match run_only(&value_line.tokens, labels, &mut shapes, &value_line)? {
                Some(call) => call,
                None => ok(&rewrite(
                    &value_line.tokens,
                    labels,
                    &mut shapes,
                    &value_line,
                )?),
            }
        };
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

/// `for <pattern> in <items>:`, sequential, one scope per iteration.
pub(super) fn for_loop(
    line: &Line,
    children: &[Node],
    labels: &mut Labels,
    place: Place,
) -> Result<(Piece, TokenStream)> {
    let tokens = line.tokens.get(1..).unwrap_or_default();
    let Some(in_at) = tokens.iter().position(|token| is_ident(token, "in")) else {
        return Err(Error::new(
            line.span,
            "`for` reads `for <name> in <items>:`",
        ));
    };
    let (pattern, items) = tokens.split_at(in_at);
    let items = items.get(1..).unwrap_or_default();
    if pattern.is_empty() || items.is_empty() {
        return Err(Error::new(
            line.span,
            "`for` reads `for <name> in <items>:`",
        ));
    }
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
    let tokens = line.tokens.get(1..).unwrap_or_default();
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
        return Err(Error::new(line.span, "`let` reads `let <name> = <value>`"));
    };
    let (pattern, rest) = tokens.split_at(at);
    let rest = rest.get(1..).unwrap_or_default();
    if pattern.is_empty() || rest.is_empty() {
        return Err(Error::new(line.span, "`let` reads `let <name> = <value>`"));
    }
    Ok((pattern.iter().cloned().collect(), rest.to_vec()))
}
