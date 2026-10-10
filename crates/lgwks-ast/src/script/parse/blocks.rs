//! The blocks: `if`, `each`, `within`, `retry`, `step`, `together`, `for`.

use lgwks_deps::proc_macro2::{Spacing, TokenStream, TokenTree};

use super::super::lexicon::{self, Kind};
use super::super::lines::{Line, Node, after, ident, is_ident, is_punct, text};
use super::super::refuse;
use super::super::tree::{
    Block, Branch, BranchValue, Construct, Each, For, IfBranch, IfChain, Retry, Site, Statement,
    Step, StepShape, Together, Within,
};
use super::super::{Refusal, Result};
use super::words::{bound, duration, refuse_typed_concurrency, rewrite, run_only, shape};
use super::{Labels, Place, block, construct};

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
    let refusal = Err(Refusal::new(
        line.span,
        format!("`{form}` reads `{form} <name> in <items>:`"),
    ));
    tracing::debug!(error = ?refusal.as_ref().err(), "pattern_and_items: returning an error to the caller");
    refusal
}

/// `if cond:` with its `else if cond:` and `else:` siblings.
pub(super) fn if_chain(
    head: &Node,
    chain: &[&Node],
    labels: &mut Labels,
    place: Place,
    wants_value: bool,
) -> Result<(Statement, Vec<StepShape>)> {
    let has_else = chain.last().is_some_and(|last| last.line.tokens.len() == 1);
    let branch_place = Place {
        nested: place.nested,
        wants_value: wants_value && has_else,
    };
    let mut shapes = Vec::new();
    let mut branches = Vec::new();
    let mut every_branch_leaves = has_else;
    for (position, node) in std::iter::once(head)
        .chain(chain.iter().copied())
        .enumerate()
    {
        let line = &node.line;
        refuse::check(&line.tokens)?;
        if !line.opens_block {
            let refusal = Err(Refusal::new(
                line.span,
                "`else` opens a block: end it with `:`",
            ));
            tracing::debug!(error = ?refusal.as_ref().err(), "if_chain: returning an error to the caller");
            return refusal;
        }
        let is_final_else = position > 0 && line.tokens.len() == 1;
        let condition_tokens: &[TokenTree] = if is_final_else {
            &[]
        } else if position == 0 {
            after(&line.tokens, 1)
        } else {
            match *after(&line.tokens, 1) {
                [ref word, ref rest @ ..] if lexicon::is(word, Kind::If) && !rest.is_empty() => {
                    rest
                }
                _ => {
                    let refusal = Err(Refusal::new(
                        line.span,
                        "an `else` line is `else:` or `else if <condition>:`",
                    ));
                    tracing::debug!(error = ?refusal.as_ref().err(), "if_chain: returning an error to the caller");
                    return refusal;
                }
            }
        };
        if !is_final_else && condition_tokens.is_empty() {
            let refusal = Err(Refusal::new(
                line.span,
                "`if` needs a condition before its `:`",
            ));
            tracing::debug!(error = ?refusal.as_ref().err(), "if_chain: returning an error to the caller");
            return refusal;
        }
        if is_final_else && position.saturating_add(1) != chain.len().saturating_add(1) {
            let refusal = Err(Refusal::new(line.span, "`else:` must be the last branch"));
            tracing::debug!(error = ?refusal.as_ref().err(), "if_chain: returning an error to the caller");
            return refusal;
        }
        let mut run_shapes = Vec::new();
        let condition =
            (!is_final_else).then(|| rewrite(condition_tokens, labels, &mut run_shapes, line));
        let (body, inner) = block(&node.children, labels, branch_place)?;
        every_branch_leaves = every_branch_leaves && body.diverges();
        run_shapes.extend(inner);
        let kind = if position == 0 { Kind::If } else { Kind::Else };
        shapes.push(shape(
            kind,
            String::new(),
            text(&line.tokens),
            line,
            run_shapes,
        ));
        branches.push(IfBranch { condition, body });
    }
    // A chain whose every branch leaves the block is itself a way out, not a
    // value: wrapping it as one would be an unreachable `Ok(..)`.
    let chain = IfChain {
        branches,
        wants_value: branch_place.wants_value,
        leaves: every_branch_leaves,
    };
    Ok((Statement::If(chain), shapes))
}

/// `each <pattern> in <items>:`, optionally `, at most (<limit>) at once`.
pub(super) fn each(
    line: &Line,
    children: &[Node],
    labels: &mut Labels,
) -> Result<(Construct, StepShape)> {
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
                    let refusal = Err(Refusal::new(
                        line.span,
                        "the bound reads `, at most (limit) at once`",
                    ));
                    tracing::debug!(error = ?refusal.as_ref().err(), "each: returning an error to the caller");
                    return refusal;
                }
            }
        }
    };
    if pattern.is_empty() || items.is_empty() {
        let refusal = Err(Refusal::new(
            line.span,
            "`each` reads `each <name> in <items>:`",
        ));
        tracing::debug!(error = ?refusal.as_ref().err(), "each: returning an error to the caller");
        return refusal;
    }
    let (written_bound, subject) = match limit_tokens {
        None => (None, String::from("sized to the machine")),
        Some(limit_tokens) => {
            refuse_typed_concurrency(limit_tokens)?;
            let limit = bound(limit_tokens, line, "at most", 65_536)?;
            (Some(limit), text(limit_tokens))
        }
    };
    let label = labels.next(&format!("each:{}", text(pattern)));
    let mut run_shapes = Vec::new();
    let items = rewrite(items, labels, &mut run_shapes, line);
    let pattern: TokenStream = pattern.iter().cloned().collect();
    let (body, inner) = nested_body(children)?;
    run_shapes.extend(inner);
    let map = shape(Kind::Each, subject, text(&line.tokens), line, run_shapes);
    let fan = Each {
        site: Site::of(&map),
        label,
        bound: written_bound,
        pattern,
        items,
        body,
    };
    Ok((Construct::Each(fan), map))
}

/// `within <duration>:`
pub(super) fn within(
    line: &Line,
    children: &[Node],
    labels: &mut Labels,
) -> Result<(Construct, StepShape)> {
    let spec = after(&line.tokens, 1);
    let limit = duration(spec, line)?;
    let label = labels.next("within");
    let (body, inner) = nested_body(children)?;
    let map = shape(Kind::Within, text(spec), text(&line.tokens), line, inner);
    let deadline = Within {
        site: Site::of(&map),
        label,
        limit,
        body,
    };
    Ok((Construct::Within(deadline), map))
}

/// `retry up to <N> times[, waiting <duration>]:`
pub(super) fn retry(
    line: &Line,
    children: &[Node],
    labels: &mut Labels,
) -> Result<(Construct, StepShape)> {
    let mut rest = after(&line.tokens, 1);
    if let [ref up, ref to, ref tail @ ..] = *rest
        && is_ident(up, "up")
        && is_ident(to, "to")
    {
        rest = tail;
    }
    let Some((count, after_times)) = split_at_keyword(rest, "times") else {
        let refusal = Err(Refusal::new(
            line.span,
            "`retry` reads `retry up to N times:` or `retry up to N times, waiting 100ms:`",
        ));
        tracing::debug!(error = ?refusal.as_ref().err(), "retry: returning an error to the caller");
        return refusal;
    };
    let subject = text(count);
    let waiting = match *after_times {
        [] => None,
        [ref comma, ref word, ref spec @ ..]
            if is_punct(comma, ',') && is_ident(word, "waiting") =>
        {
            Some(duration(spec, line)?)
        }
        _ => {
            let refusal = Err(Refusal::new(
                line.span,
                "after `times` comes `, waiting <duration>` or `:`",
            ));
            tracing::debug!(error = ?refusal.as_ref().err(), "retry: returning an error to the caller");
            return refusal;
        }
    };
    let attempts = bound(count, line, "up to", 1_000)?;
    let label = labels.next("retry");
    let (body, inner) = nested_body(children)?;
    let map = shape(Kind::Retry, subject, text(&line.tokens), line, inner);
    let again = Retry {
        site: Site::of(&map),
        label,
        attempts,
        waiting,
        body,
    };
    Ok((Construct::Retry(again), map))
}

/// `step <name>:`
pub(super) fn step(
    line: &Line,
    children: &[Node],
    labels: &mut Labels,
) -> Result<(Construct, StepShape)> {
    let name = match *after(&line.tokens, 1) {
        [ref only] => ident(only),
        _ => None,
    };
    let Some(name) = name else {
        let refusal = Err(Refusal::new(
            line.span,
            "`step` reads `step <name>:`, where the name is one word",
        ));
        tracing::debug!(error = ?refusal.as_ref().err(), "step: returning an error to the caller");
        return refusal;
    };
    let name = name.to_string();
    let label = labels.next(&name);
    let (body, inner) = nested_body(children)?;
    let map = shape(Kind::Step, name, text(&line.tokens), line, inner);
    let named = Step {
        site: Site::of(&map),
        label,
        body,
    };
    Ok((Construct::Step(named), map))
}

/// `together:` and its branches, run concurrently on this task.
pub(super) fn together(
    line: &Line,
    children: &[Node],
    labels: &mut Labels,
) -> Result<(Statement, StepShape)> {
    if line.tokens.len() != 1 {
        let refusal = Err(Refusal::new(
            line.span,
            "`together:` takes nothing before its `:`",
        ));
        tracing::debug!(error = ?refusal.as_ref().err(), "together: returning an error to the caller");
        return refusal;
    }
    let mut branches = Vec::new();
    let mut shapes = Vec::new();
    for child in children {
        branches.push(together_child(child, labels, &mut shapes)?);
    }
    let count = branches.len().to_string();
    let map = shape(
        Kind::Together,
        count,
        String::from("together"),
        line,
        shapes,
    );
    Ok((Statement::Together(Together { branches }), map))
}

/// Reads one `together:` child as a branch.
///
/// A helper rather than the loop body inline: as one block the loop carried
/// four propagation operators across the refusal check, the `let` split, the
/// block construct and the inline rewrite, so a caller reading the `together:`
/// arm had to hold four distinct refusal paths to see what one child can refuse
/// on.
fn together_child(
    child: &Node,
    labels: &mut Labels,
    shapes: &mut Vec<StepShape>,
) -> Result<Branch> {
    let child_line = &child.line;
    refuse::check(&child_line.tokens)?;
    let (pattern, value) = if lexicon::kind_of(child_line) == Some(Kind::Let) {
        let (pattern, rest) = split_let(child_line)?;
        (Some(pattern), rest)
    } else {
        (None, child_line.tokens.clone())
    };
    let value_line = Line {
        tokens: value,
        column: child_line.column,
        number: child_line.number,
        opens_block: child_line.opens_block,
        span: child_line.span,
    };
    let value = if value_line.opens_block {
        let (block, map) = construct(&value_line, &child.children, labels)?;
        shapes.push(map);
        BranchValue::Construct(block)
    } else {
        if !child.children.is_empty() {
            let refusal = Err(Refusal::new(
                child_line.span,
                "unexpected indent inside `together:`",
            ));
            tracing::debug!(error = ?refusal.as_ref().err(), "together_child: returning an error to the caller");
            return refusal;
        }
        match run_only(&value_line.tokens, labels, shapes, &value_line) {
            Some(call) => BranchValue::Run(call),
            None => BranchValue::Rust(rewrite(&value_line.tokens, labels, shapes, &value_line)),
        }
    };
    Ok(Branch { pattern, value })
}

/// `for <pattern> in <items>:`, sequential, one scope per iteration.
pub(super) fn for_loop(
    line: &Line,
    children: &[Node],
    labels: &mut Labels,
    place: Place,
) -> Result<(Statement, StepShape)> {
    let tokens = after(&line.tokens, 1);
    let (pattern, items) = pattern_and_items(line, tokens, "for")?;
    let label = labels.next(&format!("for:{}", text(pattern)));
    let mut run_shapes = Vec::new();
    let items = rewrite(items, labels, &mut run_shapes, line);
    let pattern: TokenStream = pattern.iter().cloned().collect();
    let mut inner_labels = Labels::default();
    let (body, inner) = block(
        children,
        &mut inner_labels,
        Place {
            nested: place.nested,
            wants_value: false,
        },
    )?;
    run_shapes.extend(inner);
    let map = shape(
        Kind::For,
        label.clone(),
        text(&line.tokens),
        line,
        run_shapes,
    );
    let each = For {
        site: Site::of(&map),
        label,
        pattern,
        items,
        body,
    };
    Ok((Statement::For(each), map))
}

/// The body of a construct with its own async block: a fresh scope for labels,
/// `give back` refused, and the last line as the value.
fn nested_body(children: &[Node]) -> Result<(Block, Vec<StepShape>)> {
    let mut labels = Labels::default();
    block(
        children,
        &mut labels,
        Place {
            nested: true,
            wants_value: true,
        },
    )
}

/// Split `let <pattern> = <rest>` at its assignment `=`.
pub(super) fn split_let(line: &Line) -> Result<(TokenStream, Vec<TokenTree>)> {
    let tokens = after(&line.tokens, 1);
    let mut previous_joint = false;
    let mut at = None;
    for (index, token) in tokens.iter().enumerate() {
        if let TokenTree::Punct(ref punct) = *token {
            if punct.as_char() == '=' && !previous_joint && punct.spacing() == Spacing::Alone {
                at = Some(index);
                break;
            }
            previous_joint = punct.spacing() == Spacing::Joint;
        } else {
            previous_joint = false;
        }
    }
    let Some(at) = at else {
        let refusal = Err(Refusal::new(
            line.span,
            "`let` reads `let <name> = <value>`",
        ));
        tracing::debug!(error = ?refusal.as_ref().err(), "split_let: returning an error to the caller");
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
        let refusal = Err(Refusal::new(
            line.span,
            "`let` reads `let <name> = <value>`",
        ));
        tracing::debug!(error = ?refusal.as_ref().err(), "split_let: returning an error to the caller");
        return refusal;
    }
    Ok((pattern.iter().cloned().collect(), rest.to_vec()))
}
