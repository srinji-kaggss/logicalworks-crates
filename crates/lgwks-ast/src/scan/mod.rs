//! `scan` owns the zero-gate source detectors and enforces INV-SCAN-ZERO: a
//! file this workspace ships carries no silenced error, no unlogged error
//! return, no lint allowance, no overlong try chain, and no paraphrase
//! docstring.
//!
//! ALLOW-SILENCE reads every way a lint is silenced, in source and in a
//! manifest: `allow` and `expect` at any level, a rustc lint, a tool lint, a
//! group, `cfg_attr(.., allow(..))`, and a package's `[lints]` table (#699).
//! [`scan_manifest`] is the manifest half, because a lint lowered for a whole
//! package is the same act as an attribute written once per item.
//!
//! [`scan_source_with`] runs a selection of [`Rule`]s and [`scan_source`] runs
//! [`Rule::ALL`], so a consumer that gates one class of defect selects it by
//! name and pays for one parse (#699).
//!
//! The detectors read Rust through the one parser, [`crate::try_parse`], so the
//! gate and structural inspection cannot disagree about a file, and the gate
//! inherits the parser's bounds: a source past the byte ceiling, a tree past the
//! node or depth bound, and a parse past its deadline are typed refusals
//! ([`crate::scan::ScanError::Refused`]), never an abort. They replace five `syn::visit`
//! detectors (ADR 0001, #408) and answer the same verdicts: the rule names, the
//! evidence text, the thresholds and the exemptions are the reference
//! implementation's, held to an independent model by `sim_scan_detectors`.
//!
//! DIVERGENCE (cfg atom keys): the reference implementation keys SAT atoms by
//! token-stream text via `quote::ToTokens`. This port keys them by structural
//! rendering, which is canonical where token text is incidental (whitespace,
//! trailing commas). Both sides of every comparison use the same function, so
//! verdicts agree.
//!
//! [`scan_manifest`]: crate::scan::scan_manifest
//! [`scan_source_with`]: crate::scan::scan_source_with
//! [`scan_source`]: crate::scan::scan_source
//! [`Rule`]: crate::scan::Rule
//! [`Rule::ALL`]: crate::scan::Rule::ALL

mod allow;
mod binding;
mod cfg;
mod chain;
mod doc;
mod manifest;
mod swallow;
mod syntax;
mod unlogged;

pub use manifest::scan_manifest;

use std::error::Error;
use std::fmt;
use std::path::Path;

use tree_sitter::Node;

use crate::{Language, ParseError, try_parse};

/// One detector finding, in the shape CI printers already read.
///
/// Non-exhaustive because the detectors are its only constructors: a downstream
/// consumer must destructure a `Hit` with `..`, so a later evidence field cannot
/// be added by a caller and cannot break one either.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Hit {
    /// Detector rule name, verbatim (`ERROR-SWALLOW`, `unlogged-err-return`,
    /// `ALLOW-SILENCE`, `long-try-chain`, `tautological-doc`).
    rule: &'static str,
    /// 1-based line number.
    line: usize,
    /// Human-readable evidence.
    snippet: String,
}

impl Hit {
    /// A finding of `rule` at `line`.
    fn new(rule: &'static str, line: usize, snippet: String) -> Self {
        Self {
            rule,
            line,
            snippet,
        }
    }

    /// The detector rule name, verbatim.
    #[must_use]
    pub const fn rule(&self) -> &'static str {
        self.rule
    }

    /// The 1-based line the finding was observed on.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }

    /// The human-readable evidence the detector read at that line.
    ///
    /// Borrowed, so a consumer renders a finding without being able to edit what
    /// it was shown.
    #[must_use]
    pub fn snippet(&self) -> &str {
        &self.snippet
    }
}

/// Why a file could not be scanned. A file the scan cannot read is a refusal,
/// not a pass: a gate that passes what it cannot read reports success for the
/// one condition it exists to catch.
///
/// Non-exhaustive so a caller cannot treat a future failure mode as impossible
/// by matching every variant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScanError {
    /// The file is not parseable Rust, or could not be read at all.
    Unparseable {
        /// File that could not be parsed.
        path: String,
        /// The parse failure, with the line of the first damage when there is one.
        cause: String,
    },
    /// The checked parse refused the file for a bound it holds, not for its
    /// syntax: the byte ceiling, the node budget, the depth bound
    /// ([`ParseError::AstTooDeep`]) or the deadline.
    Refused {
        /// File that was refused.
        path: String,
        /// The bound, as the parser reports it.
        refusal: ParseError,
    },
}

impl fmt::Display for ScanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Unparseable {
                ref path,
                ref cause,
            } => write!(formatter, "{path}: cannot parse Rust: {cause}"),
            Self::Refused {
                ref path,
                ref refusal,
            } => write!(
                formatter,
                "{path}: refused before any detector ran: {refusal}"
            ),
        }
    }
}

impl Error for ScanError {}

/// One detector the scan can run, named by the rule its findings carry.
///
/// A consumer that wants one class of defect — a CI lane for swallowed errors
/// only, say — selects it here rather than post-filtering strings out of a
/// whole-file scan, so the rule set is the contract and a rule rename breaks a
/// build instead of silently matching nothing (#699).
///
/// Non-exhaustive so a later rule cannot break a caller that matches every
/// variant today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Rule {
    /// `ERROR-SWALLOW`: an error value that nothing reads.
    SwallowedError,
    /// `unlogged-err-return`: a return whose failure no log carries.
    UnloggedError,
    /// `ALLOW-SILENCE`: a lint silenced, in source or in a manifest.
    AllowSilence,
    /// `long-try-chain`: more `?` in one statement than the bound allows.
    LongTryChain,
    /// `tautological-doc`: a docstring that restates its own name.
    TautologicalDoc,
}

impl Rule {
    /// Every detector, in the order the scan runs them. Findings are ordered by
    /// line and then by rule name, whatever order the rules were selected in.
    pub const ALL: [Self; 5] = [
        Self::SwallowedError,
        Self::UnloggedError,
        Self::AllowSilence,
        Self::LongTryChain,
        Self::TautologicalDoc,
    ];

    /// The rule name a finding of this detector carries, verbatim.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SwallowedError => "ERROR-SWALLOW",
            Self::UnloggedError => "unlogged-err-return",
            Self::AllowSilence => "ALLOW-SILENCE",
            Self::LongTryChain => "long-try-chain",
            Self::TautologicalDoc => "tautological-doc",
        }
    }

    /// The detector a rule name selects, or `None` for a name no detector
    /// reports.
    ///
    /// Case-insensitive because a command line is typed by hand and the
    /// capitalisation of a rule name is not a fact a caller should have to
    /// remember.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|rule| rule.name().eq_ignore_ascii_case(name))
    }
}

/// Scans one Rust source text with all five detectors, ordered by line.
///
/// # Errors
///
/// [`ScanError::Unparseable`] when the text is not Rust, and
/// [`ScanError::Refused`] when it is past a bound of the checked parse.
pub fn scan_source(source: &str, path: &str) -> Result<Vec<Hit>, ScanError> {
    scan_source_with(source, path, &Rule::ALL)
}

/// Scans one Rust source text with `rules` and no other detector, ordered by
/// line.
///
/// A rule named twice runs once: the selection is deduplicated before any
/// detector runs, so a caller that concatenates rule lists cannot make a file
/// cost twice the work or report a finding twice. The text is parsed once
/// whatever is selected, so a narrow selection costs no extra parse.
///
/// # Errors
///
/// [`ScanError::Unparseable`] when the text is not Rust, and
/// [`ScanError::Refused`] when it is past a bound of the checked parse.
pub fn scan_source_with(source: &str, path: &str, rules: &[Rule]) -> Result<Vec<Hit>, ScanError> {
    let parsed =
        try_parse(source, Language::Rust).map_err(|refusal| refused(source, path, refusal))?;
    let root = parsed.root().get_inner_node();
    let mut selected = rules.to_vec();
    selected.sort_unstable();
    selected.dedup();
    let mut hits = Vec::new();
    for rule in selected {
        match rule {
            Rule::SwallowedError => swallow::detect(source, root, &mut hits),
            Rule::UnloggedError => unlogged::detect(source, root, &mut hits),
            Rule::AllowSilence => allow::detect(source, root, &mut hits),
            Rule::LongTryChain => chain::detect(source, root, &mut hits),
            Rule::TautologicalDoc => doc::detect(source, root, &mut hits),
        }
    }
    hits.sort_by(|left, right| (left.line, left.rule).cmp(&(right.line, right.rule)));
    Ok(hits)
}

/// Scans one `.rs` file from disk.
///
/// # Errors
///
/// [`ScanError::Unparseable`] when the file cannot be read, and everything
/// [`scan_source`] refuses.
pub fn scan_path(path: &Path) -> Result<Vec<Hit>, ScanError> {
    let named = path.display().to_string();
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(cause) => {
            let refusal = ScanError::Unparseable {
                path: named,
                cause: cause.to_string(),
            };
            tracing::debug!(error = %refusal, "scan_path: returning an error to the caller");
            return Err(refusal);
        }
    };
    scan_source(&source, &named)
}

/// The refusal a failed checked parse becomes: bad syntax is `Unparseable`
/// (with the line of the first damage), a bound is `Refused`.
fn refused(source: &str, path: &str, refusal: ParseError) -> ScanError {
    let error = match refusal {
        ParseError::InvalidSyntax { .. } => {
            let at = refusal.to_diagnostic(path, source).span().start;
            ScanError::Unparseable {
                path: path.to_owned(),
                cause: format!("{refusal} at line {}, column {}", at.line, at.column),
            }
        }
        _ => ScanError::Refused {
            path: path.to_owned(),
            refusal,
        },
    };
    tracing::debug!(error = %error, "refused: the scan will not read this file");
    error
}

/// Visit every node under `root` in source order with one cursor, so the state
/// held is the ancestors of the node in hand and nothing more.
fn each_node<'t>(root: Node<'t>, mut visit: impl FnMut(Node<'t>)) {
    let mut cursor = root.walk();
    loop {
        visit(cursor.node());
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}
