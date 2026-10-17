#![doc = include_str!("lexicon.md")]
#![cfg_attr(
    not(feature = "parser"),
    doc = "Built without the `parser` feature, `lgwks_ast` is the typed-error derive \
           (`error`, re-exported at the root so `extern crate lgwks_ast as thiserror;` \
           derives `Error`) and the `diagnostic` types a tool reports with. The \
           multi-language AST parser is the `parser` feature, which every `lang-*` \
           feature and the default set enable."
)]

// Lint contract (missing_docs deny, unsafe_code forbid, broken intra-doc
// links deny) comes from the workspace root.

// The README and the parser page describe the direct-use API, which is no longer
// the front page (#526: `script!` is the one supported interface). They stay
// compiled, so their examples are still built and run as doctests, on a hidden
// item at the root, where their links still resolve.
#[cfg(feature = "parser")]
#[doc(hidden)]
#[doc = include_str!("../README.md")]
#[doc = include_str!("parser.md")]
const _: () = ();

/// Typed diagnostics: the shared error derive the parser's `ParseError` is built with.
#[doc(hidden)]
pub mod error;

/// Spans, severities, and diagnostics: what a tool reports on code, as
/// opposed to what a parse refuses.
#[doc(hidden)]
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
#[doc(hidden)]
pub use diagnostic::{Diagnostic, Pos, Severity, Span};

#[cfg(feature = "parser")]
mod parser;

#[cfg(feature = "parser")]
#[doc(hidden)]
pub use parser::*;

/// The zero-gate source detectors (INV-SCAN-ZERO, #408), reading Rust through
/// the checked parse: `scan::scan_source` and `scan::scan_path`.
#[cfg(feature = "scan")]
#[doc(hidden)]
pub mod scan;

/// The `script!` language's one reader (SL-2, #383): `script::parse` turns a
/// token stream into the typed script tree with every refusal decided, the
/// tree `lgwks_macros::script!` compiles and any tool reads.
#[cfg(feature = "script")]
pub mod script;
