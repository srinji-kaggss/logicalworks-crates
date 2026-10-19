//! The `script!` language, read once (SL-2, #383).
//!
//! [`parse`] turns tokens into a [`Script`]: every flow, every step with its
//! word already read and its structural label chosen, and the architecture map.
//! `lgwks_macros::script!` is a shim over it that writes Rust for the tree, and
//! a tool that wants to read, map or check a script calls the same function, so
//! there is one reader of the language and no second one to drift from it.
//!
//! The function takes a `proc_macro2` token stream, which is what a macro
//! receives and what `TokenStream::from_str` makes of source text outside a
//! macro, with each token's line and column kept (`span-locations`). A tool
//! therefore gets the macro's tree, refusals and all, from plain text: a flow
//! with an `each` step and a `give back` maps to those two kinds in source
//! order, and a flow that ends the process is refused at the token's own line.
//! The tests `a_tool_reads_the_macros_map_from_plain_text` and
//! `a_refusal_is_located_at_its_token_as_the_compiler_reports_it` run both.
//!
//! Selected by the `script` feature, which reaches `proc_macro2` through the
//! `lgwks_deps` storefront and builds no tree-sitter, so the macro's build does
//! not grow by a grammar.
//!
//! [`read_source`] finds every `script!` in a Rust file and reads each through
//! [`parse`], which is how a tool maps or checks a whole repository without
//! compiling it. With the `tool` feature, `Script::to_json` writes the map as
//! the runtime's `Architecture::to_json` does, and the `lgwks-ast` binary runs
//! `lgwks-ast script map|check [--json] [PATH...]` over a tree (#384).
//!
//! [`parse`]: crate::script::parse
//! [`Script`]: crate::script::Script
//! [`read_source`]: crate::script::read_source

use lgwks_deps::proc_macro2::{Span, TokenStream};

mod find;
#[cfg(feature = "tool")]
mod json;
mod lexicon;
mod lines;
mod parse;
mod passthrough;
mod refuse;
mod tree;

#[cfg(test)]
mod lines_props;
#[cfg(test)]
mod sim_script;

pub use find::{Invocation, read_source};
pub use lexicon::{Axis, Evidence, Form, Guarantee, Kind, LEXICON, Position, Word};
pub use tree::{
    Block, Bound, Branch, BranchValue, Call, Code, Construct, Domain, Duration, Each, Flow,
    FlowShape, For, Fragment, IfBranch, IfChain, MapStep, Passthrough, Retry, Script, Site,
    Statement, Step, StepShape, Together, Within, write_map,
};

/// A script the language refuses, located at the token it is about.
///
/// Every refusal names what to write instead (SL-5). The message is the one
/// the macro reports through the compiler, at the same span, so a tool and
/// `cargo build` say the same thing in the same place.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct Refusal {
    /// The token the refusal is about.
    span: Span,
    /// What is refused and what to write instead.
    message: String,
}

impl Refusal {
    /// A refusal at `span`.
    pub(crate) fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }

    /// The token the refusal is about.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// What is refused and what to write instead.
    #[must_use]
    pub const fn message(&self) -> &str {
        self.message.as_str()
    }

    /// The 1-based source line of the token, as the compiler reports it.
    #[must_use]
    pub fn line(&self) -> usize {
        self.span.start().line
    }

    /// The 0-based column of the token, in characters.
    #[must_use]
    pub fn column(&self) -> usize {
        self.span.start().column
    }
}

/// A parse step's answer: its value, or the refusal that stops the script.
type Result<T> = std::result::Result<T, Refusal>;

/// Read a whole `script!` block.
///
/// # Errors
///
/// The first construct the language refuses, in source order, located at its
/// token and naming its replacement.
///
/// # Complexity
///
/// Linear in the number of tokens. The tree is owned, so the call allocates in
/// proportion to the script.
pub fn parse(tokens: TokenStream) -> std::result::Result<Script, Refusal> {
    lines::tree(lines::split(tokens)).and_then(parse::script)
}
