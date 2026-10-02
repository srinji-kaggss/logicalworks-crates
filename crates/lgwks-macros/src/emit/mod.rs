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

use lgwks_deps::proc_macro2::{Delimiter, TokenStream, TokenTree};
use lgwks_deps::quote::quote;
use lgwks_deps::syn::{Error, Result};

use crate::lines::{Line, Node, group, ident, is_ident, is_punct, text};
use crate::refuse;

mod blocks;
mod words;

use blocks::{each, for_loop, if_chain, retry, split_let, step, together, within};
use words::{expect_words, line_literal, mentions, rewrite, run_only, shape_tokens, text_of};

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

/// The map entries one block leaves, in source order.
///
/// Every emitter appends to the `Shapes` of the block it is in; a nested block
/// collects its own and hands them up whole, so the map mirrors the nesting.
#[derive(Default)]
struct Shapes(Vec<TokenStream>);

impl Shapes {
    /// Record one entry.
    fn push(&mut self, shape: TokenStream) {
        self.0.push(shape);
    }

    /// Record entries a nested block collected.
    fn extend(&mut self, shapes: impl IntoIterator<Item = TokenStream>) {
        self.0.extend(shapes);
    }

    /// How many entries are recorded, to roll back a lookahead.
    fn len(&self) -> usize {
        self.0.len()
    }

    /// Forget entries recorded after `len`.
    fn truncate(&mut self, len: usize) {
        self.0.truncate(len);
    }

    /// The entries, for the parent's map entry.
    fn as_slice(&self) -> &[TokenStream] {
        &self.0
    }
}

impl IntoIterator for Shapes {
    type Item = TokenStream;
    type IntoIter = std::vec::IntoIter<TokenStream>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
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
    shapes: Shapes,
    /// Whether the last line leaves the block, so no value follows it.
    diverges: bool,
}

impl Body {
    /// `{ statements; Ok(tail) }` for an async body that returns a flow result.
    fn into_ok_block(self) -> (TokenStream, Shapes) {
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
    fn into_block(self) -> (TokenStream, Shapes) {
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
    lgwks_std::trace::warn!(
        operation = "script",
        "operation refused its request; the typed error carries the facts"
    );
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
    lgwks_std::trace::warn!(
        operation = "flow",
        "operation refused its request; the typed error carries the facts"
    );
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
    let steps = steps.as_slice();
    let shape = quote! {
        #script::FlowShape::new(#name_text, #signature, #line_number, &[#(#steps),*])
    };
    Ok((code, shape))
}

/// Expand a block of sibling nodes.
fn block(nodes: &[Node], labels: &mut Labels, place: Place) -> Result<Body> {
    lgwks_std::trace::warn!(
        operation = "block",
        "operation refused its request; the typed error carries the facts"
    );
    let mut pieces: Vec<Piece> = Vec::new();
    let mut shapes = Shapes::default();
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
    run_shapes: &mut Shapes,
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
    run_shapes: &mut Shapes,
) -> Result<Piece> {
    lgwks_std::trace::warn!(
        operation = "simple",
        "operation refused its request; the typed error carries the facts"
    );
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
