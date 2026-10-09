//! From lines to the script tree: every word read, every refusal decided.
//!
//! This is the whole of the language's reading. `lgwks_macros` generates code
//! from the tree it returns and refuses nothing of its own, so a tool that
//! calls [`parse`](super::parse) refuses exactly what `cargo build` refuses,
//! with the same message at the same token.
//!
//! # Step labels are structural
//!
//! A step's key is the hash of its tenant and its path, and the path is built
//! from the labels chosen here. Labels come from the structure of the script
//! (the flow name, `each:<pattern>`, `retry`, a `step` name, a `run` callee),
//! numbered `~2`, `~3` among siblings of one scope. They never contain a line
//! number: an unrelated edit above a step must not change its key, or a
//! restart after a deploy would repeat every effect the previous process had
//! already made.

use std::collections::HashMap;

use lgwks_deps::proc_macro2::{Delimiter, TokenStream, TokenTree};

use super::lexicon::{self, Kind};
use super::lines::{Line, Node, group, ident, is_ident, is_punct, text};
use super::refuse;
use super::tree::{Block, Construct, Flow, FlowShape, Script, Statement, StepShape};
use super::{Refusal, Result};

mod blocks;
mod words;

use blocks::{each, for_loop, if_chain, retry, split_let, step, together, within};
use words::{expect_words, rewrite, run_only, shape, text_of};

/// Ordinals for sibling labels within one scope.
#[derive(Default, Clone)]
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

/// Where a block sits, which decides what may appear in it.
#[derive(Clone, Copy)]
struct Place {
    /// Inside the body of `each`, `within`, `retry`, `together` or `step`,
    /// where `return` would leave that block rather than the flow.
    nested: bool,
    /// Whether the block's last expression is its value.
    wants_value: bool,
}

/// Read a whole script: every flow, with the attributes written above it.
///
/// # Errors
///
/// The first line that is not valid script.
pub(super) fn script(nodes: Vec<Node>) -> Result<Script> {
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
        flows.push(flow(node, std::mem::take(&mut attributes))?);
    }
    if let Some(stray) = attributes.first() {
        let refusal = Err(Refusal::new(
            stray.span(),
            "this attribute is not followed by a flow",
        ));
        tracing::debug!(error = ?refusal.as_ref().err(), "script: returning an error to the caller");
        return refusal;
    }
    Ok(Script { flows })
}

/// Read one `flow name(params) -> Output:` and its body.
fn flow(node: Node, attributes: Vec<TokenTree>) -> Result<Flow> {
    let Node { line, children } = node;
    let tokens = &line.tokens;
    let at_flow = tokens
        .iter()
        .position(|token| lexicon::is(token, Kind::Flow));
    let (Some(at_flow), true) = (at_flow, line.opens_block) else {
        let refusal = Err(Refusal::new(
            line.span,
            "a script is a list of flows: `flow name(param: Type) -> Output:` followed by \
         its indented body",
        ));
        tracing::debug!(error = ?refusal.as_ref().err(), "flow: returning an error to the caller");
        return refusal;
    };
    let visibility: TokenStream = tokens.iter().take(at_flow).cloned().collect();
    let mut rest = tokens.iter().skip(at_flow.saturating_add(1));
    let (Some(name), Some(params)) = (rest.next().and_then(ident), rest.next().and_then(group))
    else {
        let refusal = Err(Refusal::new(
            line.span,
            "expected `flow name(params)`; a flow with no inputs is `flow name():`",
        ));
        tracing::debug!(error = ?refusal.as_ref().err(), "flow: returning an error to the caller");
        return refusal;
    };
    if params.delimiter() != Delimiter::Parenthesis {
        let refusal = Err(Refusal::new(params.span(), "a flow's inputs are in `( )`"));
        tracing::debug!(error = ?refusal.as_ref().err(), "flow: returning an error to the caller");
        return refusal;
    }
    let after: Vec<TokenTree> = rest.cloned().collect();
    let output = match *after.as_slice() {
        [] => None,
        [ref dash, ref arrow, ref ty @ ..]
            if is_punct(dash, '-') && is_punct(arrow, '>') && !ty.is_empty() =>
        {
            Some(ty.iter().cloned().collect::<TokenStream>())
        }
        _ => {
            let refusal = Err(Refusal::new(
                line.span,
                "after a flow's inputs comes `-> Output:` or just `:`",
            ));
            tracing::debug!(error = ?refusal.as_ref().err(), "flow: returning an error to the caller");
            return refusal;
        }
    };
    let inputs = params.stream();
    refuse::check_stream(&inputs)?;
    if let Some(ref ty) = output {
        refuse::check_stream(ty)?;
    }

    let mut labels = Labels::default();
    let place = Place {
        nested: false,
        wants_value: output.is_some(),
    };
    let (body, steps) = block(&children, &mut labels, place)?;
    if output.is_some() && body.tail().is_none() && !body.diverges() {
        let span = children.last().map_or(line.span, |last| last.line.span);
        let refusal = Err(Refusal::new(
            span,
            "this flow promises an output (`-> ..`) but its last line produces no value; \
         end it with `give back <value>` or with the value itself",
        ));
        tracing::debug!(error = ?refusal.as_ref().err(), "flow: returning an error to the caller");
        return refusal;
    }

    let name_text = name.to_string();
    let signature = match output {
        Some(ref ty) => format!("{name_text}({}) -> {}", text_of(&inputs), text_of(ty)),
        None => format!("{name_text}({})", text_of(&inputs)),
    };
    let shape = FlowShape {
        name: name_text,
        signature,
        line: line.number,
        steps,
    };
    Ok(Flow {
        attributes: attributes.into_iter().collect(),
        visibility,
        name: name.clone(),
        inputs,
        output,
        body,
        shape,
    })
}

/// Read a block of sibling nodes, and the map entries they leave.
fn block(nodes: &[Node], labels: &mut Labels, place: Place) -> Result<(Block, Vec<StepShape>)> {
    let mut statements: Vec<Statement> = Vec::new();
    let mut shapes: Vec<StepShape> = Vec::new();
    let mut index: usize = 0;
    while let Some(node) = nodes.get(index) {
        let kind = lexicon::kind_of(&node.line);
        if kind == Some(Kind::Else) {
            let refusal = Err(Refusal::new(
                node.line.span,
                "`else:` must follow an `if ..:` block at the same indentation",
            ));
            tracing::debug!(error = ?refusal.as_ref().err(), "block: returning an error to the caller");
            return refusal;
        }
        let consumed = if kind == Some(Kind::If) && node.line.opens_block {
            let chain: Vec<&Node> = nodes
                .iter()
                .skip(index.saturating_add(1))
                .take_while(|sibling| lexicon::kind_of(&sibling.line) == Some(Kind::Else))
                .collect();
            let last_in_block = index.saturating_add(chain.len()).saturating_add(1) == nodes.len();
            let wants_value = place.wants_value && last_in_block;
            let (statement, chain_shapes) = if_chain(node, &chain, labels, place, wants_value)?;
            statements.push(statement);
            shapes.extend(chain_shapes);
            chain.len().saturating_add(1)
        } else {
            let (statement, shape) = statement(node, labels, place, &mut shapes)?;
            statements.push(statement);
            shapes.extend(shape);
            1
        };
        index = index.saturating_add(consumed);
    }
    let block = Block {
        statements,
        wants_value: place.wants_value,
    };
    Ok((block, shapes))
}

/// Read one line and the block it opens.
fn statement(
    node: &Node,
    labels: &mut Labels,
    place: Place,
    run_shapes: &mut Vec<StepShape>,
) -> Result<(Statement, Option<StepShape>)> {
    let line = &node.line;
    refuse::check(&line.tokens)?;
    if !line.opens_block {
        return simple(line, labels, place, run_shapes).map(|statement| (statement, None));
    }
    // The word is looked up in the lexicon rather than spelled here: a line
    // whose first token is no word (punctuation, a name) is plain Rust, and
    // `construct` is the one place that refuses it as a block.
    match lexicon::kind_of(line) {
        Some(Kind::Let) => {
            let (pattern, value) = split_let(line)?;
            let inner = Line {
                tokens: value,
                column: line.column,
                number: line.number,
                opens_block: true,
                span: line.span,
            };
            let (block, shape) = construct(&inner, &node.children, labels)?;
            Ok((Statement::Bind { pattern, block }, Some(shape)))
        }
        Some(Kind::Together) => together(line, &node.children, labels)
            .map(|(statement, shape)| (statement, Some(shape))),
        Some(Kind::For) => for_loop(line, &node.children, labels, place)
            .map(|(statement, shape)| (statement, Some(shape))),
        _ => {
            let (block, shape) = construct(line, &node.children, labels)?;
            Ok((Statement::Construct(block), Some(shape)))
        }
    }
}

/// Read a block that yields a value: `each`, `within`, `retry`, `step`.
fn construct(
    line: &Line,
    children: &[Node],
    labels: &mut Labels,
) -> Result<(Construct, StepShape)> {
    match lexicon::kind_of(line) {
        Some(Kind::Each) => each(line, children, labels),
        Some(Kind::Within) => within(line, children, labels),
        Some(Kind::Retry) => retry(line, children, labels),
        Some(Kind::Step) => step(line, children, labels),
        _ => {
            let refusal = Err(Refusal::new(
                line.span,
                format!(
                    "unknown block; a line ending in `:` is one of {}",
                    lexicon::block_words()
                ),
            ));
            tracing::debug!(error = ?refusal.as_ref().err(), "construct: returning an error to the caller");
            refusal
        }
    }
}

/// A line that opens no block: `give back`, `fail`, `let`, or plain Rust.
fn simple(
    line: &Line,
    labels: &mut Labels,
    place: Place,
    run_shapes: &mut Vec<StepShape>,
) -> Result<Statement> {
    let tokens = &line.tokens;
    match lexicon::kind_of(line) {
        Some(Kind::GiveBack) => give_back(line, place, labels, run_shapes),
        Some(Kind::Fail) => fail_line(line, labels, run_shapes),
        Some(Kind::Let) => Ok(Statement::Let(rewrite(tokens, labels, run_shapes, line))),
        _ => Ok(match run_only(tokens, labels, run_shapes, line) {
            Some(call) => Statement::Run(call),
            None => Statement::Rust(rewrite(tokens, labels, run_shapes, line)),
        }),
    }
}

/// `give back <value>`: leaves the flow with that value.
fn give_back(
    line: &Line,
    place: Place,
    labels: &mut Labels,
    run_shapes: &mut Vec<StepShape>,
) -> Result<Statement> {
    let rest = expect_words(
        line,
        &[Kind::GiveBack.spelling(), "back"],
        "`give back <value>`",
    )?;
    if place.nested {
        let refusal = Err(Refusal::new(
            line.span,
            "`give back` returns from the flow, and this line is inside a block that \
         has its own value; make the value this block's last line instead",
        ));
        tracing::debug!(error = ?refusal.as_ref().err(), "give_back: returning an error to the caller");
        return refusal;
    }
    let value = rewrite(rest, labels, run_shapes, line);
    run_shapes.push(shape(
        Kind::GiveBack,
        String::new(),
        text(&line.tokens),
        line,
        Vec::new(),
    ));
    Ok(Statement::GiveBack(value))
}

/// `fail [transiently] with <reason>`: leaves the flow with a typed error.
fn fail_line(
    line: &Line,
    labels: &mut Labels,
    run_shapes: &mut Vec<StepShape>,
) -> Result<Statement> {
    let tokens = &line.tokens;
    let transient = tokens
        .get(1)
        .is_some_and(|token| is_ident(token, "transiently"));
    let rest = if transient {
        expect_words(
            line,
            &[Kind::Fail.spelling(), "transiently", "with"],
            "`fail transiently with <reason>`",
        )?
    } else {
        expect_words(
            line,
            &[Kind::Fail.spelling(), "with"],
            "`fail with <reason>`",
        )?
    };
    let reason = rewrite(rest, labels, run_shapes, line);
    run_shapes.push(shape(
        Kind::Fail,
        String::new(),
        text(tokens),
        line,
        Vec::new(),
    ));
    Ok(Statement::Fail { transient, reason })
}
