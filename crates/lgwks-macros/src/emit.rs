//! From blocks to Rust: every block becomes one call into `lgwks_bot::script`.
//!
//! The emitted code is meant to be read. A flow expands to an `async fn` whose
//! body is a sequence of `each(..)`, `within(..)`, `retry(..)` and
//! `try_join!(..)` calls with ordinary Rust between them, so `cargo expand`
//! shows the orchestration the script declared and nothing it did not.
//!
//! # Step labels are structural
//!
//! A step's [`StepKey`] is the hash of its tenant and its path, and the path is
//! built from labels this module chooses. Labels come from the structure of the
//! script (the flow name, `each:<pattern>`, `retry`, a `step` name, a `run`
//! callee), numbered `~2`, `~3` among siblings of one scope. They never contain
//! a line number: an unrelated edit above a step must not change its key, or a
//! restart after a deploy would repeat every effect the previous process had
//! already made.
//!
//! [`StepKey`]: https://docs.rs/lgwks_bot/latest/lgwks_bot/script/struct.StepKey.html

use std::collections::HashMap;

use lgwks_deps::proc_macro2::{Delimiter, Group, Ident, Literal, Span, TokenStream, TokenTree};
use lgwks_deps::quote::{ToTokens, quote};
use lgwks_deps::syn::{Error, Result};

use crate::lines::{Line, Node, group, ident, is_ident, is_parens, is_punct, text};
use crate::refuse;

/// The runtime every expansion calls into.
fn runtime() -> TokenStream {
    quote!(::lgwks_bot::script)
}

/// `Result<T, FlowError>` spelled out, so the expansion names nothing the
/// caller must import.
///
/// A value that ends in `?` is bound first: `Ok(x?)` is what a reader (and
/// `clippy::needless_question_mark`) flags, yet the `?` must stay, because it
/// is what converts a verb's `BotError` or an `io::Error` into a flow failure.
fn ok(value: &TokenStream) -> TokenStream {
    let script = runtime();
    let ends_in_question = value
        .clone()
        .into_iter()
        .last()
        .is_some_and(|token| is_punct(&token, '?'));
    if ends_in_question {
        quote!({
            let flow_value = #value;
            ::core::result::Result::Ok::<_, #script::FlowError>(flow_value)
        })
    } else {
        quote!(::core::result::Result::Ok::<_, #script::FlowError>(#value))
    }
}

/// Ordinals for sibling labels within one scope.
#[derive(Default)]
struct Labels {
    /// How many times each base label has been handed out.
    seen: HashMap<String, u32>,
}

impl Labels {
    /// `base` the first time, then `base~2`, `base~3`, ...
    fn next(&mut self, base: &str) -> String {
        let count = self.seen.entry(base.to_owned()).or_insert(0);
        *count = count.saturating_add(1);
        if *count == 1 {
            base.to_owned()
        } else {
            format!("{base}~{count}")
        }
    }
}

/// One emitted statement or expression.
struct Piece {
    /// The Rust tokens.
    tokens: TokenStream,
    /// Whether the tokens are an expression that could be a block's value.
    is_expr: bool,
    /// Whether the piece always leaves the block (`give back`, `fail`).
    diverges: bool,
    /// Whether the expression is a flow `Result` that needs `?` to become the
    /// value. Kept apart so a block whose last line is a step can hand the
    /// `Result` straight back instead of writing `Ok(step?)`.
    fallible: bool,
}

impl Piece {
    /// A statement or expression that falls through to the next line.
    fn new(tokens: TokenStream, is_expr: bool) -> Self {
        Self {
            tokens,
            is_expr,
            diverges: false,
            fallible: false,
        }
    }

    /// An expression of type `Result<_, FlowError>`: a block or a `run`.
    fn fallible(tokens: TokenStream) -> Self {
        Self {
            tokens,
            is_expr: true,
            diverges: false,
            fallible: true,
        }
    }

    /// A statement that leaves the block.
    fn leaving(tokens: TokenStream) -> Self {
        Self {
            tokens,
            is_expr: false,
            diverges: true,
            fallible: false,
        }
    }
}

/// A block's value.
struct Tail {
    /// The expression.
    tokens: TokenStream,
    /// Whether it is a flow `Result` rather than the value itself.
    fallible: bool,
}

/// A block's emitted body.
struct Body {
    /// Every statement, `;`-terminated.
    statements: TokenStream,
    /// The block's value, when it has one.
    tail: Option<Tail>,
    /// The architecture entries for what the block contains.
    shapes: Vec<TokenStream>,
    /// Whether the last line leaves the block, so no value follows it.
    diverges: bool,
}

impl Body {
    /// `{ statements; Ok(tail) }` for an async body that returns a flow result.
    fn into_ok_block(self) -> (TokenStream, Vec<TokenStream>) {
        let Self {
            statements,
            tail,
            shapes,
            diverges,
        } = self;
        let value = match tail {
            _ if diverges => TokenStream::new(),
            Some(Tail {
                tokens,
                fallible: true,
            }) => tokens,
            Some(Tail {
                tokens,
                fallible: false,
            }) => ok(&tokens),
            None => ok(&quote!(())),
        };
        (quote!({ #statements #value }), shapes)
    }

    /// `{ statements; tail }` for a plain Rust block.
    fn into_block(self) -> (TokenStream, Vec<TokenStream>) {
        let Self {
            statements,
            tail,
            shapes,
            ..
        } = self;
        let value = tail.map(
            |Tail { tokens, fallible }| {
                if fallible { quote!(#tokens?) } else { tokens }
            },
        );
        (quote!({ #statements #value }), shapes)
    }
}

/// Where a block sits, which decides what may appear in it.
#[derive(Clone, Copy)]
struct Place {
    /// Inside the body of `each`, `within`, `retry`, `together` or `step`,
    /// where `return` would leave that block rather than the flow.
    nested: bool,
    /// Whether the block's last expression is its value.
    wants_value: bool,
}

/// Expand a whole script: every flow, then the architecture map.
///
/// # Errors
///
/// The first line that is not valid script.
pub(crate) fn script(nodes: Vec<Node>) -> Result<TokenStream> {
    let mut output = TokenStream::new();
    let mut flows = Vec::new();
    let mut attributes: Vec<TokenTree> = Vec::new();
    for node in nodes {
        if node
            .line
            .tokens
            .first()
            .is_some_and(|token| is_punct(token, '#'))
        {
            attributes.extend(node.line.tokens);
            continue;
        }
        let (code, shape) = flow(node, std::mem::take(&mut attributes))?;
        output.extend(code);
        flows.push(shape);
    }
    if let Some(stray) = attributes.first() {
        return Err(Error::new(
            stray.span(),
            "this attribute is not followed by a flow",
        ));
    }
    let script = runtime();
    output.extend(quote! {
        /// The orchestration this `script!` block declares, compiled from the
        /// same tokens as its flows. See `lgwks_bot::script::Architecture`.
        pub const ARCHITECTURE: #script::Architecture = #script::Architecture::new(&[#(#flows),*]);
    });
    Ok(output)
}

/// Expand one `flow name(params) -> Output:` and its body.
fn flow(node: Node, attributes: Vec<TokenTree>) -> Result<(TokenStream, TokenStream)> {
    let Node { line, children } = node;
    let tokens = &line.tokens;
    let at_flow = tokens.iter().position(|token| is_ident(token, "flow"));
    let (Some(at_flow), true) = (at_flow, line.opens_block) else {
        return Err(Error::new(
            line.span,
            "a script is a list of flows: `flow name(param: Type) -> Output:` followed by \
             its indented body",
        ));
    };
    let visibility: TokenStream = tokens.iter().take(at_flow).cloned().collect();
    let mut rest = tokens.iter().skip(at_flow.saturating_add(1));
    let (Some(name), Some(params)) = (rest.next().and_then(ident), rest.next().and_then(group))
    else {
        return Err(Error::new(
            line.span,
            "expected `flow name(params)`; a flow with no inputs is `flow name():`",
        ));
    };
    if params.delimiter() != Delimiter::Parenthesis {
        return Err(Error::new(params.span(), "a flow's inputs are in `( )`"));
    }
    let after: Vec<TokenTree> = rest.cloned().collect();
    let output_type = match *after.as_slice() {
        [] => None,
        [ref dash, ref arrow, ref ty @ ..]
            if is_punct(dash, '-') && is_punct(arrow, '>') && !ty.is_empty() =>
        {
            Some(ty.iter().cloned().collect::<TokenStream>())
        }
        _ => {
            return Err(Error::new(
                line.span,
                "after a flow's inputs comes `-> Output:` or just `:`",
            ));
        }
    };
    refuse::check_stream(&params.stream())?;
    if let Some(ref ty) = output_type {
        refuse::check_stream(ty)?;
    }

    let mut labels = Labels::default();
    let place = Place {
        nested: false,
        wants_value: output_type.is_some(),
    };
    let body = block(&children, &mut labels, place)?;
    if output_type.is_some() && body.tail.is_none() && !body.diverges {
        let span = children.last().map_or(line.span, |last| last.line.span);
        return Err(Error::new(
            span,
            "this flow promises an output (`-> ..`) but its last line produces no value; \
             end it with `give back <value>` or with the value itself",
        ));
    }
    let (body_block, steps) = body.into_ok_block();

    let script = runtime();
    let name_text = name.to_string();
    let inputs = params.stream();
    let inputs = if inputs.is_empty() {
        quote!(scope: &#script::Scope)
    } else {
        quote!(scope: &#script::Scope, #inputs)
    };
    let output = output_type.clone().unwrap_or_else(|| quote!(()));
    let has_doc = attributes.iter().any(|token| {
        matches!(*token, TokenTree::Group(ref group) if group.stream().into_iter().next().is_some_and(|first| is_ident(&first, "doc")))
    });
    let doc = if has_doc {
        TokenStream::new()
    } else {
        let text = format!(" Flow `{name_text}`, written with `script!`.");
        quote!(#[doc = #text])
    };
    let attributes: TokenStream = attributes.into_iter().collect();
    // The helper traits are imported only where a flow calls them, so an
    // expansion never carries an unused import into a crate that denies one.
    let helpers = if mentions(&body_block, &["or_fail", "or_retry"]) {
        quote!(use #script::{OptionExt as _, ResultExt as _};)
    } else {
        TokenStream::new()
    };
    let code = quote! {
        #attributes
        #doc
        #visibility async fn #name(#inputs) -> ::core::result::Result<#output, #script::FlowError> {
            #helpers
            let scope = &scope.enter(#name_text)?;
            let outcome: ::core::result::Result<#output, #script::FlowError> = async #body_block.await;
            outcome.map_err(|error| error.located(scope))
        }
    };

    let signature = match output_type {
        Some(ref ty) => format!(
            "{name_text}({}) -> {}",
            text_of(&params.stream()),
            text_of(ty)
        ),
        None => format!("{name_text}({})", text_of(&params.stream())),
    };
    let line_number = line_literal(&line);
    let shape = quote! {
        #script::FlowShape::new(#name_text, #signature, #line_number, &[#(#steps),*])
    };
    Ok((code, shape))
}

/// Expand a block of sibling nodes.
fn block(nodes: &[Node], labels: &mut Labels, place: Place) -> Result<Body> {
    let mut pieces: Vec<Piece> = Vec::new();
    let mut shapes = Vec::new();
    let mut index: usize = 0;
    while let Some(node) = nodes.get(index) {
        let keyword = node.line.keyword();
        if keyword.as_deref() == Some("else") {
            return Err(Error::new(
                node.line.span,
                "`else:` must follow an `if ..:` block at the same indentation",
            ));
        }
        let consumed = if keyword.as_deref() == Some("if") && node.line.opens_block {
            let chain: Vec<&Node> = nodes
                .iter()
                .skip(index.saturating_add(1))
                .take_while(|sibling| sibling.line.keyword().as_deref() == Some("else"))
                .collect();
            let last_in_block = index.saturating_add(chain.len()).saturating_add(1) == nodes.len();
            let wants_value = place.wants_value && last_in_block;
            let (piece, chain_shapes) = if_chain(node, &chain, labels, place, wants_value)?;
            pieces.push(piece);
            shapes.extend(chain_shapes);
            chain.len().saturating_add(1)
        } else {
            let (piece, shape) = statement(node, labels, place, &mut shapes)?;
            pieces.push(piece);
            shapes.extend(shape);
            1
        };
        index = index.saturating_add(consumed);
    }

    let diverges = pieces.last().is_some_and(|last| last.diverges);
    let tail = match pieces.last() {
        Some(last) if place.wants_value && last.is_expr => pieces.pop().map(|piece| Tail {
            tokens: piece.tokens,
            fallible: piece.fallible,
        }),
        _ => None,
    };
    let mut statements = TokenStream::new();
    for piece in pieces {
        let tokens = piece.tokens;
        match (piece.is_expr, piece.fallible) {
            (_, true) => statements.extend(quote!(#tokens?;)),
            (true, false) => statements.extend(quote!(#tokens;)),
            (false, false) => statements.extend(tokens),
        }
    }
    Ok(Body {
        statements,
        tail,
        shapes,
        diverges,
    })
}

/// Expand one line and the block it opens.
fn statement(
    node: &Node,
    labels: &mut Labels,
    place: Place,
    run_shapes: &mut Vec<TokenStream>,
) -> Result<(Piece, Option<TokenStream>)> {
    let line = &node.line;
    refuse::check(&line.tokens)?;
    let keyword = line.keyword().unwrap_or_default();
    if !line.opens_block {
        return simple(line, &keyword, labels, place, run_shapes).map(|piece| (piece, None));
    }
    if keyword == "let" {
        let (pattern, construct) = split_let(line)?;
        let inner = Line {
            tokens: construct,
            column: line.column,
            number: line.number,
            opens_block: true,
            span: line.span,
        };
        let inner_keyword = inner.keyword().unwrap_or_default();
        let (expr, shape) = construct_expr(&inner, &inner_keyword, &node.children, labels)?;
        return Ok((
            Piece::new(quote!(let #pattern = #expr?;), false),
            Some(shape),
        ));
    }
    match keyword.as_str() {
        "together" => {
            together(line, &node.children, labels).map(|(piece, shape)| (piece, Some(shape)))
        }
        "for" => {
            for_loop(line, &node.children, labels, place).map(|(piece, shape)| (piece, Some(shape)))
        }
        _ => construct_expr(line, &keyword, &node.children, labels)
            .map(|(tokens, shape)| (Piece::fallible(tokens), Some(shape))),
    }
}

/// Expand a block that yields a value: `each`, `within`, `retry`, `step`.
fn construct_expr(
    line: &Line,
    keyword: &str,
    children: &[Node],
    labels: &mut Labels,
) -> Result<(TokenStream, TokenStream)> {
    match keyword {
        "each" => each(line, children, labels),
        "within" => within(line, children, labels),
        "retry" => retry(line, children, labels),
        "step" => step(line, children, labels),
        _ => Err(Error::new(
            line.span,
            "unknown block; a line ending in `:` is one of `each`, `within`, `retry`, \
             `together`, `step`, `for`, `if`/`else`, or `let x = <block>:`",
        )),
    }
}

/// A line that opens no block: `give back`, `fail`, `let`, or plain Rust.
fn simple(
    line: &Line,
    keyword: &str,
    labels: &mut Labels,
    place: Place,
    run_shapes: &mut Vec<TokenStream>,
) -> Result<Piece> {
    let script = runtime();
    let tokens = &line.tokens;
    match keyword {
        "give" => {
            let rest = expect_words(line, &["give", "back"], "`give back <value>`")?;
            if place.nested {
                return Err(Error::new(
                    line.span,
                    "`give back` returns from the flow, and this line is inside a block that \
                     has its own value; make the value this block's last line instead",
                ));
            }
            let value = rewrite(rest, labels, run_shapes, line)?;
            run_shapes.push(shape_tokens("GiveBack", "", &text(tokens), line, &[]));
            Ok(Piece::leaving(
                quote!(return ::core::result::Result::Ok(#value);),
            ))
        }
        "fail" => {
            let (constructor, rest) = if tokens
                .get(1)
                .is_some_and(|token| is_ident(token, "transiently"))
            {
                (
                    quote!(transient),
                    expect_words(
                        line,
                        &["fail", "transiently", "with"],
                        "`fail transiently with <reason>`",
                    )?,
                )
            } else {
                (
                    quote!(failed),
                    expect_words(line, &["fail", "with"], "`fail with <reason>`")?,
                )
            };
            let reason = rewrite(rest, labels, run_shapes, line)?;
            run_shapes.push(shape_tokens("Fail", "", &text(tokens), line, &[]));
            Ok(Piece::leaving(
                quote!(return ::core::result::Result::Err(#script::FlowError::#constructor(#reason));),
            ))
        }
        "let" => {
            let rewritten = rewrite(tokens, labels, run_shapes, line)?;
            Ok(Piece::new(quote!(#rewritten;), false))
        }
        _ => match run_only(tokens, labels, run_shapes, line)? {
            Some(call) => Ok(Piece::fallible(call)),
            None => Ok(Piece::new(rewrite(tokens, labels, run_shapes, line)?, true)),
        },
    }
}

/// `if cond:` with its `else if cond:` and `else:` siblings.
fn if_chain(
    head: &Node,
    chain: &[&Node],
    labels: &mut Labels,
    place: Place,
    wants_value: bool,
) -> Result<(Piece, Vec<TokenStream>)> {
    let has_else = chain.last().is_some_and(|last| last.line.tokens.len() == 1);
    let branch_place = Place {
        nested: place.nested,
        wants_value: wants_value && has_else,
    };
    let mut shapes = Vec::new();
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
        let mut run_shapes = Vec::new();
        let condition = rewrite(condition_tokens, labels, &mut run_shapes, line)?;
        let body = block(&node.children, labels, branch_place)?;
        every_branch_leaves = every_branch_leaves && body.diverges;
        let (branch, mut inner) = body.into_block();
        run_shapes.append(&mut inner);
        let kind = if position == 0 { "If" } else { "Else" };
        shapes.push(shape_tokens(
            kind,
            "",
            &text(&line.tokens),
            line,
            &run_shapes,
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

/// `each <pattern> in <items>, at most <N> at once:`
fn each(line: &Line, children: &[Node], labels: &mut Labels) -> Result<(TokenStream, TokenStream)> {
    let tokens = line.tokens.get(1..).unwrap_or_default();
    let Some(in_at) = tokens.iter().position(|token| is_ident(token, "in")) else {
        return Err(Error::new(
            line.span,
            "`each` reads `each <name> in <items>, at most <N> at once:`",
        ));
    };
    let (pattern, after_in) = tokens.split_at(in_at);
    let after_in = after_in.get(1..).unwrap_or_default();
    let bound_at = after_in.windows(3).rposition(|window| {
        matches!(*window, [ref comma, ref at, ref most] if is_punct(comma, ',') && is_ident(at, "at") && is_ident(most, "most"))
    });
    let Some(bound_at) = bound_at else {
        return Err(Error::new(
            line.span,
            "`each` needs a bound: `each x in xs, at most N at once:`, where N is how many \
             run together. Unbounded fan-out is the defect this block exists to prevent",
        ));
    };
    let (items, clause) = after_in.split_at(bound_at);
    let limit_tokens = match *clause.get(3..).unwrap_or_default() {
        [ref limit @ .., ref at, ref once]
            if is_ident(at, "at") && is_ident(once, "once") && !limit.is_empty() =>
        {
            limit
        }
        _ => {
            return Err(Error::new(
                line.span,
                "the bound reads `, at most N at once`",
            ));
        }
    };
    if pattern.is_empty() || items.is_empty() {
        return Err(Error::new(
            line.span,
            "`each` reads `each <name> in <items>, at most <N> at once:`",
        ));
    }
    let limit = bound(limit_tokens, line, "at most", 65_536)?;
    let pattern_text = text(pattern);
    let label = labels.next(&format!("each:{pattern_text}"));
    let mut run_shapes = Vec::new();
    let items = rewrite(items, labels, &mut run_shapes, line)?;
    let pattern: TokenStream = pattern.iter().cloned().collect();
    let (body, mut inner) = nested_body(children)?;
    run_shapes.append(&mut inner);
    let script = runtime();
    let expr = quote! {
        #script::each(scope, #label, #script::at_most(#limit)?, #items, async |scope: #script::Scope, #pattern| {
            let scope = &scope;
            #body
        }).await
    };
    Ok((
        expr,
        shape_tokens(
            "Each",
            &text(limit_tokens),
            &text(&line.tokens),
            line,
            &run_shapes,
        ),
    ))
}

/// `within <duration>:`
fn within(
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
        shape_tokens("Within", &text(spec), &text(&line.tokens), line, &inner),
    ))
}

/// `retry up to <N> times[, waiting <duration>]:`
fn retry(
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
        #script::retry(scope, #label, #script::attempts(#count)?, #waiting, async |scope: &#script::Scope, attempt: u32| {
            let _ = attempt;
            #body
        }).await
    };
    Ok((
        expr,
        shape_tokens("Retry", &subject, &text(&line.tokens), line, &inner),
    ))
}

/// `step <name>:`
fn step(line: &Line, children: &[Node], labels: &mut Labels) -> Result<(TokenStream, TokenStream)> {
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
        shape_tokens("Step", &name.to_string(), &text(&line.tokens), line, &inner),
    ))
}

/// `together:` and its branches, run concurrently on this task.
fn together(line: &Line, children: &[Node], labels: &mut Labels) -> Result<(Piece, TokenStream)> {
    if line.tokens.len() != 1 {
        return Err(Error::new(
            line.span,
            "`together:` takes nothing before its `:`",
        ));
    }
    let mut patterns = Vec::new();
    let mut branches = Vec::new();
    let mut shapes = Vec::new();
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
        shape_tokens("Together", &count, "together", line, &shapes),
    ))
}

/// `for <pattern> in <items>:`, sequential, one scope per iteration.
fn for_loop(
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
    let mut run_shapes = Vec::new();
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
    let (body, mut inner) = body.into_block();
    run_shapes.append(&mut inner);
    let tokens = quote! {
        for (lgwks_script_index, #pattern) in ::core::iter::IntoIterator::into_iter(#items).enumerate() {
            let scope = &scope.item(#label, lgwks_script_index)?;
            #body
        }
    };
    Ok((
        Piece::new(tokens, false),
        shape_tokens("For", &label, &text(&line.tokens), line, &run_shapes),
    ))
}

/// The body of a construct with its own async block: a fresh scope for labels,
/// `give back` refused, and the last line as the value.
fn nested_body(children: &[Node]) -> Result<(TokenStream, Vec<TokenStream>)> {
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
fn split_let(line: &Line) -> Result<(TokenStream, Vec<TokenTree>)> {
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

/// Pass Rust through, turning `run name(args)` into a call in this scope.
fn rewrite(
    tokens: &[TokenTree],
    labels: &mut Labels,
    shapes: &mut Vec<TokenStream>,
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
fn run_call(
    tokens: &[TokenTree],
    labels: &mut Labels,
    shapes: &mut Vec<TokenStream>,
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
fn expect_words<'line>(
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
        return Err(Error::new(line.span, format!("this line reads {form}")));
    }
    Ok(rest)
}

/// A bound: a literal checked here against `1..=max`, or an expression the
/// runtime checks.
fn bound(tokens: &[TokenTree], line: &Line, clause: &str, max: u64) -> Result<TokenStream> {
    if let [TokenTree::Literal(ref literal)] = *tokens {
        let digits: String = literal
            .to_string()
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        let value: u64 = digits
            .parse()
            .map_err(|_| Error::new(literal.span(), format!("`{clause}` takes a whole number")))?;
        if value == 0 || value > max {
            return Err(Error::new(
                literal.span(),
                format!("`{clause} {value}` is outside 1..={max}"),
            ));
        }
        let literal = Literal::u64_unsuffixed(value);
        return Ok(literal.into_token_stream());
    }
    if tokens.is_empty() {
        return Err(Error::new(line.span, format!("`{clause}` needs a number")));
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
const UNITS: [(&str, u128); 4] = [
    ("ms", 1_000_000),
    ("s", 1_000_000_000),
    ("m", 60_000_000_000),
    ("h", 3_600_000_000_000),
];

/// A duration: `250ms`, `2s`, `1.5s`, `5m`, `1h`, or `(expr)` of type `Duration`.
fn duration(tokens: &[TokenTree], line: &Line) -> Result<TokenStream> {
    match *tokens {
        [TokenTree::Literal(ref literal)] => {
            let written = literal.to_string().replace('_', "");
            let split =
                written.find(|character: char| !(character.is_ascii_digit() || character == '.'));
            let (number, unit) = written.split_at(split.unwrap_or(written.len()));
            let Some(&(_, scale)) = UNITS.iter().find(|&&(name, _)| name == unit) else {
                return Err(Error::new(
                    literal.span(),
                    "a duration is a number with a unit: `250ms`, `2s`, `5m`, `1h`",
                ));
            };
            let nanos = scaled(number, scale).ok_or_else(|| {
                Error::new(literal.span(), "this duration is too large or not a number")
            })?;
            if nanos == 0 {
                return Err(Error::new(
                    literal.span(),
                    "a zero duration is no deadline; give a positive one",
                ));
            }
            let nanos = u64::try_from(nanos)
                .map_err(|_| Error::new(literal.span(), "this duration is too large"))?;
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
fn scaled(number: &str, scale: u128) -> Option<u128> {
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
        place = place.checked_div(10)?;
        let value = u128::from(digit.to_digit(10)?);
        total = total.checked_add(value.checked_mul(place)?)?;
    }
    Some(total)
}

/// A `StepShape::new(..)` constant for the architecture map.
fn shape_tokens(
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
fn line_literal(line: &Line) -> Literal {
    Literal::u32_unsuffixed(u32::try_from(line.number).unwrap_or(u32::MAX))
}

/// Tokens as a person would write them.
fn text_of(stream: &TokenStream) -> String {
    let tokens: Vec<TokenTree> = stream.clone().into_iter().collect();
    text(&tokens)
}

/// Whether any identifier in `stream`, at any depth, is one of `words`.
fn mentions(stream: &TokenStream, words: &[&str]) -> bool {
    stream.clone().into_iter().any(|token| match token {
        TokenTree::Ident(ref word) => words.iter().any(|wanted| word == wanted),
        TokenTree::Group(ref inner) => mentions(&inner.stream(), words),
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}

/// A line that is exactly `run name(args)`: the call as a flow `Result`,
/// without the `?` a call inside a larger expression needs.
fn run_only(
    tokens: &[TokenTree],
    labels: &mut Labels,
    shapes: &mut Vec<TokenStream>,
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
