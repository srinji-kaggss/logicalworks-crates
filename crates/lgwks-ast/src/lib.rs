#![doc = include_str!("lexicon.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(
    not(feature = "parser"),
    doc = "Built without the `parser` feature, `lgwks_ast` is the typed-error derive \
           (`error`, re-exported at the root so `extern crate lgwks_ast as thiserror;` \
           derives `Error`) and the `diagnostic` types a tool reports with. The \
           multi-language AST parser is the `parser` feature, which every `lang-*` \
           feature and the default set enable."
)]
//!
//! ## Features
//!
//! | Feature | Default | Enables |
//! |---|---|---|
//! | `lang-rust`, `lang-python`, `lang-typescript`, `lang-javascript`, `lang-go`, `lang-java`, `lang-swift` | on | one tree-sitter grammar each, and `parser` |
//! | `lang-*` (the other 21, `lang-tsx` and `lang-c` among them) | off | one more grammar each, and `parser` |
//! | `parser` | on, through any `lang-*` | the multi-language parser and its grammar stack |
//! | `script` | off | `script::parse`, the reader of the `script!` language |
//! | `tool` | off | `Script::to_json` and the `lgwks-ast script` binary; implies `script` |
//! | `scan` | off | the zero-gate Rust source detectors and `lgwks-ast scan`; implies `lang-rust` |
//! | `full` | off | all 28 grammars |

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
pub use diagnostic::{Diagnostic, Pos, Severity, Span};

#[cfg(feature = "parser")]
mod parser;

#[cfg(feature = "parser")]
#[doc(hidden)]
pub use parser::*;

/// The supported parser surface: identify a language, parse under the checked
/// bounds, inspect the tree, and report a typed refusal.
///
/// These are the items a consumer compiles against. Everything else in the
/// parser is support code with no compatibility promise.
#[cfg(feature = "parser")]
pub use parser::{
    AstMetrics, AstNode, ContentDetection, Language, MAX_AST_DEPTH, MAX_AST_NODES,
    MAX_DETECT_BYTES, MAX_SOURCE_BYTES, ParseError, Parsed, SyntaxDiagnostic, SyntaxIssueKind,
    inspect_ast, parse, try_detect_content, try_parse, try_parse_with, try_parse_within,
};

/// The zero-gate source detectors (INV-SCAN-ZERO, #408), reading Rust through
/// the checked parse: `scan::scan_source` and `scan::scan_path`.
#[cfg(feature = "scan")]
pub mod scan;

/// The `script!` language's one reader (SL-2, #383): `script::parse` turns a
/// token stream into the typed script tree with every refusal decided, the
/// tree `lgwks_macros::script!` compiles and any tool reads.
#[cfg(feature = "script")]
pub mod script;
