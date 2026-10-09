//! Turning a token stream back into indented lines, and lines into blocks.
//!
//! A proc macro receives tokens, not text: whitespace and newlines are gone.
//! Every token still carries its source span, though, and a span knows its line
//! and column. That is enough to recover the layout a person wrote: a new line
//! starts where a token begins on a later line than the previous token ended,
//! and a line's indentation is the column of its first token. A bracketed group
//! is one token however many lines it spans, so a call split across lines stays
//! one logical line, which is Python's own rule for brackets.

use lgwks_deps::proc_macro2::{
    Delimiter, Group, Ident, LineColumn, Punct, Spacing, Span, TokenStream, TokenTree,
};
use lgwks_deps::syn::{Error, Result};

/// One logical line of a script.
pub(crate) struct Line {
    /// The line's tokens, with a trailing block `:` removed.
    pub(crate) tokens: Vec<TokenTree>,
    /// The column of the first token.
    pub(crate) column: usize,
    /// The source line of the first token.
    pub(crate) number: usize,
    /// Whether the line ended in `:`, which opens a block.
    pub(crate) opens_block: bool,
    /// The span of the first token, for errors about the line as a whole.
    pub(crate) span: Span,
}

/// A line and the block it opens, if any.
pub(crate) struct Node {
    /// The header, or the whole statement.
    pub(crate) line: Line,
    /// The lines indented beneath a header.
    pub(crate) children: Vec<Node>,
}

/// The tokens after the first `count` of `tokens`.
///
/// Every caller passes the tokens of a line whose leading words it has already
/// read — a keyword, an `in`, the `=`, the `, at most` of a bound — so `count` is
/// a number of tokens this slice has. Asking past the end is asking about a form
/// that has nothing after what it consumed, and each such caller already refuses
/// it in its own words (`if` needs a condition, `each` reads
/// `each <name> in <items>:`); reading "nothing after it" once here keeps the
/// fourteen callers from each deciding what an absent remainder means.
pub(crate) fn after(tokens: &[TokenTree], count: usize) -> &[TokenTree] {
    let mut rest = tokens;
    for _ in 0..count {
        let Some((_read, tail)) = rest.split_first() else {
            return rest;
        };
        rest = tail;
    }
    rest
}

/// Split a stream into logical lines.
pub(crate) fn split(stream: TokenStream) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut current: Vec<TokenTree> = Vec::new();
    let mut last_end: usize = 0;
    for token in stream {
        let start = token.span().start();
        if !current.is_empty()
            && start.line > last_end
            && let Some(line) = finish(std::mem::take(&mut current))
        {
            lines.push(line);
        }
        last_end = token.span().end().line;
        current.push(token);
    }
    if let Some(line) = finish(current) {
        lines.push(line);
    }
    lines
}

/// Close a line: record its position and strip a block-opening `:`.
///
/// `None` for a line with no tokens at all, which is not a line of the script:
/// its position, its line number and its span would all be invented, and a
/// [`Line`] whose span is invented reports an error against a place in the
/// source the author never wrote. [`split`] closes a line only around tokens it
/// has read, so it never asks for one of these.
fn finish(mut tokens: Vec<TokenTree>) -> Option<Line> {
    let span = tokens.first()?.span();
    let start = span.start();
    let opens_block = ends_in_block_colon(&tokens);
    if opens_block {
        tokens.pop();
    }
    Some(Line {
        tokens,
        column: start.column,
        number: start.line,
        opens_block,
        span,
    })
}

/// Whether the last token is a lone `:` rather than the second half of `::`.
fn ends_in_block_colon(tokens: &[TokenTree]) -> bool {
    let mut from_end = tokens.iter().rev();
    let Some(last) = from_end.next().and_then(punct) else {
        return false;
    };
    if last.as_char() != ':' {
        return false;
    }
    !from_end
        .next()
        .and_then(punct)
        .is_some_and(|previous| previous.as_char() == ':' && previous.spacing() == Spacing::Joint)
}

/// Arrange lines into a tree by indentation.
///
/// # Errors
///
/// A header with nothing indented beneath it, a line indented under something
/// that is not a header, and a dedent that lands between two block levels.
pub(crate) fn tree(lines: Vec<Line>) -> Result<Vec<Node>> {
    let mut queue = lines.into_iter().peekable();
    let Some(first_column) = queue.peek().map(|line| line.column) else {
        return Ok(Vec::new());
    };
    let nodes = block(&mut queue, first_column)?;
    if let Some(stray) = queue.next() {
        let refusal = Err(Error::new(
            stray.span,
            "this line is indented less than the first line of the script; \
         every flow starts at the same column",
        ));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "tree: returning an error to the caller");
        return refusal;
    }
    Ok(nodes)
}

/// Collect the nodes at `column`, recursing into deeper headers.
fn block(
    queue: &mut std::iter::Peekable<std::vec::IntoIter<Line>>,
    column: usize,
) -> Result<Vec<Node>> {
    let mut nodes = Vec::new();
    while let Some(next) = queue.peek() {
        if next.column < column {
            break;
        }
        if next.column > column {
            let refusal = Err(Error::new(
                next.span,
                "unexpected indent: only a line ending in `:` opens an indented block",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "block: returning an error to the caller");
            return refusal;
        }
        let Some(line) = queue.next() else {
            break;
        };
        let mut children = Vec::new();
        if line.opens_block {
            let Some(child_column) = queue.peek().map(|child| child.column) else {
                let refusal = Err(expected_block(&line));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "block: returning an error to the caller");
                return refusal;
            };
            if child_column <= column {
                let refusal = Err(expected_block(&line));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "block: returning an error to the caller");
                return refusal;
            }
            children = block(queue, child_column)?;
            if let Some(after) = queue.peek()
                && after.column > column
            {
                let refusal = Err(Error::new(
                    after.span,
                    "this dedent does not line up with any enclosing block",
                ));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "block: returning an error to the caller");
                return refusal;
            }
        }
        nodes.push(Node { line, children });
    }
    Ok(nodes)
}

/// The error for a header with no block beneath it.
fn expected_block(line: &Line) -> Error {
    Error::new(
        line.span,
        "this line ends in `:` and so opens a block, but nothing is indented beneath it",
    )
}

/// Whether `token` is the punctuation `character`.
pub(crate) fn is_punct(token: &TokenTree, character: char) -> bool {
    matches!(*token, TokenTree::Punct(ref punct) if punct.as_char() == character)
}

/// Whether `token` is the identifier `word`.
pub(crate) fn is_ident(token: &TokenTree, word: &str) -> bool {
    matches!(*token, TokenTree::Ident(ref ident) if ident == word)
}

/// Whether `token` is a parenthesised group.
pub(crate) fn is_parens(token: &TokenTree) -> bool {
    matches!(*token, TokenTree::Group(ref group) if group.delimiter() == Delimiter::Parenthesis)
}

/// Render tokens the way the person wrote them, for the architecture map.
///
/// Each token's own source text is laid out with a space wherever the source
/// had a gap, so `&'a Site`, `Vec<u32>` and `a < b` all come back as written.
/// A token that starts inside text already rendered is skipped: a lifetime's
/// `'` and its name can both carry the span of the whole lifetime. A token
/// without source text (one produced by another macro) falls back to the
/// compiler's rendering of it.
pub(crate) fn text(tokens: &[TokenTree]) -> String {
    let mut rendered = String::new();
    let mut previous_end: Option<LineColumn> = None;
    for token in tokens {
        let span = token.span();
        let start = span.start();
        if previous_end.is_some_and(|end| (start.line, start.column) < (end.line, end.column)) {
            continue;
        }
        let gap = previous_end.is_none_or(|end| end != start);
        if gap && !rendered.is_empty() {
            rendered.push(' ');
        }
        // Two different spellings, not one spelling with a stand-in: a token the
        // person wrote has the source text its span names, and a token another
        // macro produced has none, so the compiler's own rendering of it is the
        // closest text available.
        match span.source_text() {
            Some(source) => rendered.push_str(&source),
            None => rendered.push_str(&token.to_string()),
        }
        previous_end = Some(span.end());
    }
    rendered
}

/// The identifier `token` is, if it is one.
pub(crate) fn ident(token: &TokenTree) -> Option<&Ident> {
    match *token {
        TokenTree::Ident(ref ident) => Some(ident),
        TokenTree::Group(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => None,
    }
}

/// The group `token` is, if it is one.
pub(crate) fn group(token: &TokenTree) -> Option<&Group> {
    match *token {
        TokenTree::Group(ref group) => Some(group),
        TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => None,
    }
}

/// The punctuation `token` is, if it is one.
fn punct(token: &TokenTree) -> Option<&Punct> {
    match *token {
        TokenTree::Punct(ref punct) => Some(punct),
        TokenTree::Group(_) | TokenTree::Ident(_) | TokenTree::Literal(_) => None,
    }
}
