#![cfg_attr(feature = "parser", doc = include_str!("../README.md"))]
#![cfg_attr(feature = "parser", doc = include_str!("parser.md"))]
#![cfg_attr(
    not(feature = "parser"),
    doc = "Built without the `parser` feature, `lgwks_ast` is the typed-error derive \
           ([`error`], re-exported at the root so `extern crate lgwks_ast as thiserror;` \
           derives `Error`) and the [`diagnostic`] types a tool reports with. The \
           multi-language AST parser is the `parser` feature, which every `lang-*` \
           feature and the default set enable."
)]

// Lint contract (missing_docs deny, unsafe_code forbid, broken intra-doc
// links deny) comes from the workspace root.

/// Typed diagnostics: the shared error derive the parser's `ParseError` is built with.
pub mod error;

/// Spans, severities, and diagnostics: what a tool reports on code, as
/// opposed to what a parse refuses.
pub mod diagnostic;

/// Root re-export required by the `thiserror` derive's absolute expansion path.
///
/// `#[derive(Error)]` expands to `::thiserror::__private<N>::…`, resolved in
/// the *consuming* crate. A consumer with no `thiserror` Cargo edge makes that
/// path resolve here by naming this crate `thiserror`; the glob is what carries
/// the version-suffixed private module.
#[doc(hidden)]
pub use thiserror::*;

/// The diagnostic types a tool reports with, re-exported at the crate root.
///
/// A tool reporting on code needs [`Diagnostic`], [`Span`], [`Pos`] and
/// [`Severity`] in almost every signature it writes, and reaching into
/// `lgwks_ast::diagnostic::` for all four is a tax on the common case. The
/// module stays public for a caller who prefers the longer path.
pub use diagnostic::{Diagnostic, Pos, Severity, Span};

#[cfg(feature = "parser")]
mod parser;

#[cfg(feature = "parser")]
pub use parser::*;

/// The `script!` language's one reader (SL-2, #383): `script::parse` turns a
/// token stream into the typed script tree with every refusal decided, the
/// tree `lgwks_macros::script!` compiles and any tool reads.
#[cfg(feature = "script")]
pub mod script;
