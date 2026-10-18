//! The multi-language AST parser: language identification, grammar
//! selection, bounded parsing and traversal (the `parser` feature).
//!
//! Every `lang-*` feature enables `parser`, and so does the default set. A
//! build without it carries no grammar stack at all: only the typed-error
//! derive and the diagnostic types a tool reports with (#367). Everything here
//! is re-exported at the crate root, so the paths a consumer names are the
//! same either way.

use std::time::Duration;

use ast_grep_core::tree_sitter::LanguageExt;
pub use ast_grep_core::tree_sitter::{StrDoc, TSLanguage};
pub use ast_grep_core::{AstGrep, Node};
pub use ast_grep_language::SupportLang;

use crate::diagnostic::{self, Diagnostic, Severity};

/// The checked and unchecked parses, content detection and the parser's deadline.
mod check;
/// The grammar table, the shebang reader and the registered-grammar type.
mod language;
/// The markdown container guard the checked parse runs before the grammar sees a source.
mod markdown;
/// The cursor walk: inspection metrics, the retained recovery diagnostics and the name readers.
mod walk;

/// The checked parse, content detection and the bounds, under test.
#[cfg(all(test, feature = "lang-rust"))]
mod check_tests;
/// The deadline's stopping rule, on a clock the test drives.
#[cfg(all(test, feature = "lang-go"))]
mod deadline_tests;
/// What every build answers from its own compiled grammar table, and each grammar's fixtures.
#[cfg(test)]
mod feature_matrix_tests;
/// Extension and shebang resolution and registered grammars, under test.
#[cfg(all(test, feature = "lang-rust"))]
mod language_tests;
/// The walk, its bounds and the recovery reports, under test.
#[cfg(all(test, feature = "lang-rust"))]
mod tests;

#[doc(hidden)]
pub use self::check::{
    detect, parse, parse_with, try_detect_content, try_parse, try_parse_with, try_parse_within,
};
#[doc(hidden)]
pub use self::language::{CustomLang, Language};
#[doc(hidden)]
pub use self::markdown::markdown_containers;
#[doc(hidden)]
pub use self::walk::{
    callee_name, child_text_with_kind, definition_name, has_syntax_issues, inspect_ast, max_depth,
};

/// One parsed source file, owning its tree. Defaults to a built-in [`Language`];
/// a caller-registered grammar parses to `Parsed<CustomLang>`.
pub type Parsed<L = SupportLang> = AstGrep<StrDoc<L>>;

/// One node of a [`Parsed`] tree.
pub type AstNode<'t, L = SupportLang> = Node<'t, StrDoc<L>>;

/// Largest source admitted to the checked parser (2 MiB). This is the input
/// bound that keeps parse work linear in bytes.
pub const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;

/// Largest concrete syntax tree admitted; bounds downstream walks, which a
/// byte bound alone does not price. Measured on the tree after tree-sitter has
/// built it, so it does not cap the parser's own allocation. The walk is linear
/// in tree size, so this ceiling prices its *worst case*, not its running cost.
pub const MAX_AST_NODES: usize = 2_000_000;

/// Deepest branch a checked parse admits, root counting as 1.
///
/// [`MAX_AST_NODES`] prices a *wide* tree; a *deep* one is bounded by neither it
/// nor the byte ceiling in any useful way, because a source of `(((…` is a few
/// bytes per nesting level and yields one node per level plus the delimiters. The
/// node ceiling therefore admits a tree hundreds of thousands of levels deep,
/// and the traversal retains one frame per active ancestor, so the depth is
/// bounded explicitly instead.
///
/// The bound is on the checked parse ([`try_parse`], [`try_parse_with`]). The
/// public [`inspect_ast`] walk is uncapped in depth, because a caller inspecting
/// a tree on purpose may want the real depth of a malformed one; the ceiling's
/// only producer is the checked parse, which reports it as
/// [`ParseError::AstTooDeep`].
pub const MAX_AST_DEPTH: usize = 512;

/// How long a checked parse may hold its thread when the caller names no
/// deadline (10 s).
///
/// The byte ceiling bounds what the parser is handed, not how long it works on
/// it: tree-sitter's GLR parser is super-linear in nesting on some grammars, and
/// the parse-budget rig measured 97.5 s for 256 KiB of nested braces under the
/// Dart grammar and 29.8 s for a 512 KiB single line of Scala, with the 2 MiB
/// forms of both unfinished at 120 s (`bench/README.md`). This is the bound on
/// that: the parser is asked every hundred operations whether to continue, and
/// past the deadline it stops, releases its thread and the call answers
/// [`ParseError::TimedOut`].
///
/// Ten seconds is chosen from the same measurement. The slowest grammar on its
/// own valid source at the full byte ceiling is `javascript` at 1 MB/s p99,
/// about two seconds for 2 MiB, and the slowest *legitimate* shape measured —
/// one megabyte-and-a-half line of Ruby — is 5.6 s; ten seconds admits both on
/// the measuring host and stops every adversarial row the rig recorded. A
/// caller with a tighter budget, or a slower host, names its own through
/// [`try_parse_within`].
pub const DEFAULT_PARSE_DEADLINE: Duration = Duration::from_secs(10);

/// Largest number of block containers one line of markdown may declare (64).
///
/// **This is a process-safety bound, not a quality bound.** tree-sitter's
/// markdown grammar keeps its open block containers in an external scanner whose
/// state is serialized into a fixed 1 024-byte buffer, and it *asserts* — which
/// is `abort()`, not a catchable error — when that state does not fit. A source
/// that opens 255 containers does not return a `ParseError`: it ends the
/// process, with the caller inside it. `- ` repeated 255 times is 510 bytes.
///
/// The bound therefore sits well below the smallest measured abort. Bisected
/// from a child process across seventeen container shapes — unordered and
/// ordered list markers, blockquotes of one, two and three `>`, tabs,
/// list-in-quote, quote-in-list, list-in-quote-in-list, fenced and indented
/// code inside containers, and containers nested by indentation — **every
/// aborting shape reaches its abort at the same 255 open containers**, whether
/// it spells them one per repetition (255 repetitions), two per repetition
/// (128) or three (85). [`markdown_containers`]
/// counts that quantity directly, so the margin is `255 / 64`, about 4x, and
/// the count is additionally an over-estimate wherever indentation and markers
/// both contribute depth.
///
/// The cost of the margin is a refusal, not a crash: a document nested more
/// than 64 containers deep on one line is refused as
/// [`ParseError::ContainerNestingTooDeep`] before the scanner sees it. A
/// document nested 30 deep, which is already past what any real file contains,
/// still parses.
///
/// Only a grammar registered under the name `markdown` is measured against it,
/// and only on the checked parse. The count needs no grammar and no cargo
/// feature, so the rule is present in every build rather than appearing only
/// where `lang-md` happens to be on.
pub const MAX_MARKDOWN_CONTAINERS_PER_LINE: usize = 64;

/// Largest probe source admitted to [`try_detect_content`] (64 KiB). Content
/// detection costs one full parse per distinct candidate grammar, so it is
/// bounded far below [`MAX_SOURCE_BYTES`]; a language probe only needs enough bytes to
/// show one clean reading.
pub const MAX_DETECT_BYTES: usize = 64 * 1024;

/// Longest `#!` line [`Language::of_shebang`] will read (256 bytes).
///
/// A real shebang is a path and an interpreter, and the longest in ordinary use
/// is well under 128 bytes. The bound is what makes the read safe to point at
/// an arbitrary file: a binary whose first "line" is a megabyte of non-newline
/// bytes is refused in constant time rather than scanned.
pub const MAX_SHEBANG_BYTES: usize = 256;

/// Maximum recovery diagnostics retained from one checked parse.
pub const MAX_SYNTAX_DIAGNOSTICS: usize = 32;

/// A checked-parse refusal. None of these may be reported as clean.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseError {
    /// The source exceeds the byte bound.
    #[error("source is {actual} bytes; parser limit is {limit} bytes")]
    SourceTooLarge {
        /// Observed length in bytes.
        actual: usize,
        /// The applied bound.
        limit: usize,
    },
    /// The selected grammar could not produce a syntax tree.
    #[error("{language} parser could not produce a syntax tree: {detail}")]
    ParserUnavailable {
        /// The language name.
        language: &'static str,
        /// The parser's own detail string.
        detail: String,
    },
    /// The tree carries an `ERROR` or `MISSING` recovery node.
    #[error("{language} parser produced an ERROR or MISSING node")]
    InvalidSyntax {
        /// The language name.
        language: &'static str,
        /// At most [`MAX_SYNTAX_DIAGNOSTICS`] recovery nodes from the bounded walk.
        diagnostics: Vec<SyntaxDiagnostic>,
        /// Whether more recovery nodes were observed than retained.
        diagnostics_truncated: bool,
    },
    /// The tree exceeds the node bound.
    #[error("{language} AST exceeds {limit} nodes (observed at least {observed})")]
    AstTooLarge {
        /// The language name.
        language: &'static str,
        /// Nodes observed up to the bound.
        observed: usize,
        /// The applied bound.
        limit: usize,
    },
    /// The tree is nested deeper than the checked parse admits.
    ///
    /// A separate arm from [`ParseError::AstTooLarge`], because a caller acts on
    /// them differently: a node refusal says *this source is large*, and a depth
    /// refusal says *this source is shaped like the deeply-nested input a hostile
    /// parser input is made of*. Both name the bound and the observed value, and
    /// both are refusals of the whole file rather than located diagnostics.
    #[error("{language} AST nests deeper than {limit} levels (observed at least {observed})")]
    AstTooDeep {
        /// The language name.
        language: &'static str,
        /// Branch depth observed up to the bound.
        observed: usize,
        /// The applied bound.
        limit: usize,
    },
    /// One line declares more block containers than the checked parse admits.
    ///
    /// A *pre-parse* refusal, and a separate arm from
    /// [`ParseError::AstTooDeep`] because it measures something else about
    /// something else. `AstTooDeep` counts the depth of a tree that was built;
    /// this counts containers declared by the *text*, before any grammar runs,
    /// because the grammar cannot be run at all: the markdown scanner's own
    /// serialization buffer overflows first and takes the process with it. See
    /// [`MAX_MARKDOWN_CONTAINERS_PER_LINE`].
    ///
    /// Reported before the parse rather than after it, so a caller sees a
    /// refusal it can act on instead of a process that is no longer there.
    #[error(
        "{language} declares at least {observed} block containers on one line; \
         the limit is {limit}"
    )]
    ContainerNestingTooDeep {
        /// The language name.
        language: &'static str,
        /// Containers counted on the offending line.
        observed: usize,
        /// The applied bound.
        limit: usize,
    },
    /// The parser was still working when its deadline passed, and was stopped.
    ///
    /// A refusal of the whole file, like the size and shape refusals, and not a
    /// statement about its syntax: the source may be perfectly valid and merely
    /// expensive for this grammar. The thread that ran the parse is free again
    /// when this is returned, and its parser is reset, so the next parse on the
    /// same thread starts clean rather than resuming this one.
    #[error("{language} parse stopped at its deadline of {after:?}")]
    TimedOut {
        /// The language name.
        language: &'static str,
        /// The deadline that was applied.
        after: Duration,
    },
}

/// Every recovery node in `tree`, as a located diagnostic.
///
/// This is the form of [`diagnostic::diagnostics`] a caller should reach for.
/// The free function takes the source as an argument, and every span it reports
/// is a byte offset into *that* string — so handing it the wrong one yields
/// positions that are silently off by some amount rather than loudly wrong.
/// Here the source is read out of the tree, which is the only place it can be
/// correct by construction.
///
/// A [`Parsed`] owns its text, so this borrows the tree and nothing else. A
/// clean tree yields an empty `Vec`; a tree carrying recovery nodes yields one
/// [`Diagnostic`] per node, in source order.
///
/// `language` names the grammar in each message, and is the name the caller
/// parsed with — the same string [`try_parse_with`] takes. A caller of the
/// built-in grammars passes `language.name()`; ast-grep's own `Language` trait
/// carries no display name, so this crate cannot recover one from the tree.
///
/// A source whose second function is malformed reports its recovery on line 2, read from
/// the tree itself; the unit test `tree_diagnostics_reads_the_source_from_the_tree` runs that.
#[must_use]
pub fn tree_diagnostics<L: LanguageExt>(
    path: impl Into<std::path::PathBuf>,
    tree: &Parsed<L>,
    language: &str,
) -> Vec<Diagnostic> {
    diagnostic::diagnostics(path, &tree.root(), tree_source(tree), language)
}

/// How many recovery nodes `tree` carries.
///
/// The count behind [`has_syntax_issues`], which collapses the same question to
/// one sticky bit and cannot say where the damage is.
#[must_use]
pub fn tree_recovery_count<L: LanguageExt>(tree: &Parsed<L>) -> usize {
    diagnostic::recovery_count(&tree.root())
}

/// The text `tree` was parsed from.
///
/// [`Parsed`] is a type alias for ast-grep's own `Root`, whose `doc` field is
/// private, so this reaches the source through a node's public [`Doc`] surface.
/// The tree owns a copy of its text, so the result is never stale and never a
/// re-read from disk.
fn tree_source<L: LanguageExt>(tree: &Parsed<L>) -> &str {
    use ast_grep_core::Doc;
    tree.root().get_doc().get_source()
}

impl ParseError {
    /// This refusal as a located [`Diagnostic`], for a tool that reports every
    /// outcome in one shape.
    ///
    /// Every refusal is a [`Severity::Error`]. An [`InvalidSyntax`] refusal
    /// points at the earliest recovery node it retained, using that node's byte
    /// span in `source`, so a renderer underlines the text the grammar rejected.
    /// The other refusals are conditions of the whole file (a source too large,
    /// a grammar that produced no tree, a tree over the node bound), so their
    /// span is zero-width at the end of `source` rather than an invented line.
    /// `source` must be the text that was parsed; each span end is clamped into
    /// it.
    ///
    /// [`InvalidSyntax`]: ParseError::InvalidSyntax
    ///
    /// A whole-file refusal such as `SourceTooLarge` reports as an error with an empty span;
    /// the unit test `a_whole_file_refusal_reports_an_empty_error_span` runs that.
    #[must_use]
    pub fn to_diagnostic(&self, path: impl Into<std::path::PathBuf>, source: &str) -> Diagnostic {
        let span = match *self {
            Self::InvalidSyntax {
                ref diagnostics, ..
            } => match diagnostics.iter().min_by_key(|found| found.start_byte) {
                Some(first) => diagnostic::span_of(source, first.start_byte..first.end_byte),
                None => diagnostic::end_of(source),
            },
            _ => diagnostic::end_of(source),
        };
        Diagnostic::new(self.to_string(), span)
            .in_file(path)
            .with_severity(Severity::Error)
    }
}

/// Kind of tree-sitter recovery node reported by a checked parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SyntaxIssueKind {
    /// Tree-sitter inserted an error node for unparsed source.
    Error,
    /// Tree-sitter inserted a missing token to recover the parse.
    Missing,
}

/// A bounded recovery diagnostic with byte offsets into the original source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SyntaxDiagnostic {
    /// Whether the recovery node is `ERROR` or `MISSING`.
    pub kind: SyntaxIssueKind,
    /// Inclusive UTF-8 byte offset in the original source.
    pub start_byte: usize,
    /// Exclusive UTF-8 byte offset in the original source.
    pub end_byte: usize,
}

/// Why a bounded AST inspection stopped before visiting the full tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum InspectionStopReason {
    /// The walk observed one node beyond its configured node limit.
    NodeLimitExceeded,
    /// The walk descended one level beyond its configured depth limit.
    ///
    /// Only the checked parse configures a depth limit — see [`MAX_AST_DEPTH`] —
    /// and it turns this into [`ParseError::AstTooDeep`] rather than handing a
    /// caller a partial [`AstMetrics`], so the variant is recorded here rather
    /// than dropped so the metrics cannot report an incomplete walk with no
    /// reason.
    DepthLimitExceeded,
}

/// Complete content-detection result after every distinct candidate was checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContentDetection {
    /// No candidate parsed without recovery nodes.
    NoMatch,
    /// Exactly one candidate parsed without recovery nodes.
    Unique(Language),
    /// More than one distinct candidate parsed without recovery nodes.
    Ambiguous,
}

/// Node count, deepest depth, recovery state, and completeness from one traversal.
///
/// There is no `Default`: every value comes from a walk, so `complete` always
/// says whether that walk visited the whole tree. A default would be a value no
/// walk produced, with `complete == false` reading as a partial inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct AstMetrics {
    /// Nodes visited.
    pub nodes: usize,
    /// Deepest branch depth, root counting as 1.
    pub max_depth: usize,
    /// Whether an `ERROR` or `MISSING` node was seen.
    pub has_syntax_issues: bool,
    /// Whether the whole tree was visited.
    pub complete: bool,
    /// Node limit supplied to the traversal, if any.
    pub node_limit: Option<usize>,
    /// Why the walk stopped early, if inspection is incomplete.
    pub stop_reason: Option<InspectionStopReason>,
}
