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
//! therefore gets the macro's tree, refusals and all, from plain text:
//!
//! ```
//! use std::str::FromStr;
//!
//! use lgwks_ast::script::{Kind, parse};
//! use lgwks_deps::proc_macro2::TokenStream;
//!
//! let source = "flow sizes(pages: Vec<String>) -> usize:\n    \
//!               let sizes = each page in pages:\n        page.len()\n    \
//!               give back sizes.len()\n";
//! let tokens = TokenStream::from_str(source).map_err(|error| error.to_string())?;
//! let script = parse(tokens).map_err(|refusal| refusal.to_string())?;
//! let flow = script.flows().first().ok_or("one flow")?;
//! assert_eq!(flow.shape().signature(), "sizes(pages: Vec<String>) -> usize");
//! let kinds: Vec<Kind> = flow.shape().steps().iter().map(|step| step.kind()).collect();
//! assert_eq!(kinds, [Kind::Each, Kind::GiveBack], "the map, in source order");
//!
//! let refused = TokenStream::from_str("flow f():\n    std::process::exit(1)\n")
//!     .map_err(|error| error.to_string())?;
//! let refusal = parse(refused).err().ok_or("exit is refused")?;
//! assert!(refusal.message().contains("ending the process"), "{refusal}");
//! assert_eq!(refusal.line(), 2, "located at the token, as the compiler reports it");
//! # Ok::<(), String>(())
//! ```
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
mod refuse;
mod tree;

#[cfg(test)]
mod lines_props;
#[cfg(test)]
mod sim_script;

pub use find::{Invocation, read_source};
pub use lexicon::{Axis, Evidence, Form, Guarantee, Kind, LEXICON, Position, Word};
pub use tree::{
    Block, Bound, Branch, BranchValue, Call, Code, Construct, Duration, Each, Flow, FlowShape, For,
    Fragment, IfBranch, IfChain, MapStep, Retry, Script, Statement, Step, StepShape, Together,
    Within, write_map,
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
pub fn parse(tokens: TokenStream) -> std::result::Result<Script, Refusal> {
    lines::tree(lines::split(tokens)).and_then(parse::script)
}
