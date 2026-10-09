//! Every `script!` invocation in one Rust source file, read by [`parse`].
//!
//! A tool that maps or checks scripts finds them the way the compiler hands
//! them to the macro. The file is lexed by the same `proc_macro2` tokenizer the
//! parser reads, whose `span-locations` keep each token's line and column in the
//! file, and every `script ! <group>` in token position is one invocation: at
//! any depth of nesting, under any path prefix (`lgwks_bot::script!`), with any
//! delimiter. A `script!` written inside a string, a comment or a doc comment is
//! not a token, so it is not found, just as the compiler would not expand it.
//!
//! The walk keeps its own stack of groups rather than recursing, so a file
//! nested deeper than any script costs heap in proportion to its nesting and
//! never the thread's stack.
//!
//! [`parse`]: super::parse

use std::str::FromStr;

use lgwks_deps::proc_macro2::{TokenStream, TokenTree};

use super::lines::{is_ident, is_punct};
use super::tree::Script;
use super::{Refusal, parse};

/// One `script!` invocation found in a source file, and what the parser read.
#[derive(Debug, Clone)]
pub struct Invocation {
    /// The 1-based line of the macro's name.
    line: usize,
    /// The 0-based column of the macro's name, in characters.
    column: usize,
    /// The script, or the refusal `cargo build` would report for it.
    read: Result<Script, Refusal>,
}

impl Invocation {
    /// The 1-based line of the macro's name, as the compiler reports it.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }

    /// The 0-based column of the macro's name, in characters.
    #[must_use]
    pub const fn column(&self) -> usize {
        self.column
    }

    /// The parsed script, or the refusal the macro reports at the same token.
    ///
    /// # Errors
    ///
    /// The refusal, when the language refuses this invocation.
    pub const fn read(&self) -> Result<&Script, &Refusal> {
        self.read.as_ref()
    }
}

/// What the walk does at one token.
enum Next {
    /// A `script ! <group>`: read it and step over all three tokens.
    Invocation(Invocation),
    /// A group that is not a script body: walk inside it, then step over it.
    Descend(TokenStream),
    /// Anything else: step over it.
    Step,
}

/// Read every `script!` invocation in `source`, in source order.
///
/// # Errors
///
/// The source is not Rust tokens (an unterminated string or comment, a stray
/// character), located where the tokenizer stopped. A refusal inside an
/// invocation is not an error: it is that invocation's [`Invocation::read`].
pub fn read_source(source: &str) -> Result<Vec<Invocation>, Refusal> {
    let tokens = match TokenStream::from_str(source) {
        Ok(tokens) => tokens,
        Err(error) => {
            let refusal = Refusal::new(
                error.span(),
                format!("this file is not Rust tokens, so no `script!` in it can be read: {error}"),
            );
            tracing::debug!(error = %refusal, "read_source: returning an error to the caller");
            return Err(refusal);
        }
    };
    let mut found = Vec::new();
    let mut frames: Vec<(Vec<TokenTree>, usize)> = vec![(tokens.into_iter().collect(), 0)];
    while let Some(frame) = frames.last_mut() {
        let &mut (ref trees, ref mut index) = frame;
        let Some(token) = trees.get(*index) else {
            frames.pop();
            continue;
        };
        let bang = trees
            .get(index.saturating_add(1))
            .is_some_and(|tree| is_punct(tree, '!'));
        let body = trees
            .get(index.saturating_add(2))
            .and_then(|tree| match *tree {
                TokenTree::Group(ref group) => Some(group),
                TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => None,
            })
            .filter(|_| bang && is_ident(token, "script"));
        let next = if let Some(body) = body {
            let start = token.span().start();
            Next::Invocation(Invocation {
                line: start.line,
                column: start.column,
                read: parse(body.stream()),
            })
        } else {
            match *token {
                TokenTree::Group(ref group) => Next::Descend(group.stream()),
                TokenTree::Ident(_) | TokenTree::Punct(_) | TokenTree::Literal(_) => Next::Step,
            }
        };
        match next {
            Next::Invocation(invocation) => {
                *index = index.saturating_add(3);
                found.push(invocation);
            }
            Next::Descend(inner) => {
                *index = index.saturating_add(1);
                frames.push((inner.into_iter().collect(), 0));
            }
            Next::Step => *index = index.saturating_add(1),
        }
    }
    Ok(found)
}
