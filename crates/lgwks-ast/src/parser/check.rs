//! The checked and unchecked parses, content detection and the parser's deadline.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use ast_grep_core::tree_sitter::LanguageExt;

use super::markdown::{MARKDOWN, validate_markdown_containers};
use super::walk::inspect_ast_with_pending;
use super::{
    AstGrep, ContentDetection, DEFAULT_PARSE_DEADLINE, InspectionStopReason, Language,
    MAX_AST_DEPTH, MAX_AST_NODES, MAX_DETECT_BYTES, MAX_MARKDOWN_CONTAINERS_PER_LINE,
    MAX_SOURCE_BYTES, ParseError, Parsed, StrDoc, TSLanguage,
};

/// Grammars one thread keeps a ready parser for before it starts over.
///
/// A parser is per thread and per grammar, so the cache is never shared and
/// never locked; the ceiling exists because a caller-registered grammar is a
/// key this crate does not choose, and an unbounded map keyed by it would grow
/// with whatever a long-lived thread was handed. Every compiled grammar fits
/// with room to spare, so the built-in set never evicts.
const MAX_CACHED_PARSERS: usize = 64;

thread_local! {
    /// This thread's parser per grammar, reset before every parse.
    static PARSERS: RefCell<HashMap<TSLanguage, tree_sitter::Parser>> =
        RefCell::new(HashMap::new());
}

/// Identify a language by file extension. `None` means the name does not claim
/// the file, so report unscanned, never clean.
///
/// Content sniffing is deliberately not part of this call: it costs one full
/// parse per distinct grammar. A caller that needs it opts in with
/// [`try_detect_content`] and names the few grammars it expects, rather than
/// trial-parsing every language this build carries.
#[must_use]
pub fn detect(path: &str) -> Option<Language> {
    Language::of_path(path)
}

/// Identify `source` by trial-parsing `candidates`, the opt-in content path.
///
/// Each distinct candidate is parsed once, so `source` is held to
/// [`MAX_DETECT_BYTES`] rather than [`MAX_SOURCE_BYTES`], and the caller, not
/// this crate, decides which grammars are plausible. The result distinguishes
/// no match, one clean candidate, and multiple clean candidates. A
/// parser-unavailable or node-budget refusal returns `Err`: an uninspected
/// required candidate cannot be treated as evidence against it.
pub fn try_detect_content(
    source: &str,
    candidates: &[Language],
) -> Result<ContentDetection, ParseError> {
    validate_source_size(source, MAX_DETECT_BYTES)?;
    detect_by_parsing(source, candidates)
}

/// Classify `source` across all distinct candidates, returning parser failures
/// because they leave the candidate set incompletely inspected.
///
/// `source` is assumed already size-checked by the caller: [`try_detect_content`]
/// is the only one, and it applies [`MAX_DETECT_BYTES`] before reaching here.
/// "Several clean readings" is a refusal rather than a tie-break: picking among
/// grammars that all accept the source would attach a rule set on no evidence,
/// which is the same defect as reporting a guess. Duplicate candidates are
/// discarded against the finite compiled grammar table before parsing.
fn detect_by_parsing(
    source: &str,
    candidates: &[Language],
) -> Result<ContentDetection, ParseError> {
    detect_candidates(candidates, |language| {
        try_parse(source, language).map(|_| ())
    })
}

/// Parse each distinct compiled candidate once and refuse incomplete inspection.
pub(super) fn detect_candidates(
    candidates: &[Language],
    mut parse_candidate: impl FnMut(Language) -> Result<(), ParseError>,
) -> Result<ContentDetection, ParseError> {
    let mut seen = vec![false; Language::ALL.len()];
    let mut unique_reading = None;
    let mut ambiguous = false;
    let mut incomplete = None;

    for &language in candidates {
        let Some(index) = Language::ALL
            .iter()
            .position(|&compiled| compiled == language)
        else {
            keep_first_incomplete(
                language,
                ParseError::ParserUnavailable {
                    language: language.name(),
                    detail: "candidate grammar is not present in this build".to_owned(),
                },
                &mut incomplete,
            );
            continue;
        };
        if std::mem::replace(&mut seen[index], true) {
            continue;
        }
        match parse_candidate(language) {
            Ok(()) => {
                if unique_reading.replace(language).is_some() {
                    ambiguous = true;
                }
            }
            Err(ParseError::InvalidSyntax { .. }) => {}
            Err(error) => keep_first_incomplete(language, error, &mut incomplete),
        }
    }

    if let Some((_, error)) = incomplete {
        let refusal = Err(error);
        tracing::debug!(error = ?refusal.as_ref().err(), "detect_candidates: returning an error to the caller");
        return refusal;
    }
    if ambiguous {
        Ok(ContentDetection::Ambiguous)
    } else if let Some(language) = unique_reading {
        Ok(ContentDetection::Unique(language))
    } else {
        Ok(ContentDetection::NoMatch)
    }
}

/// Keep the lexically first failed candidate so permutations report the same refusal.
fn keep_first_incomplete(
    language: Language,
    error: ParseError,
    incomplete: &mut Option<(Language, ParseError)>,
) {
    if incomplete
        .as_ref()
        .is_none_or(|&(recorded, _)| language.name() < recorded.name())
    {
        *incomplete = Some((language, error));
    }
}

/// Production boundary: refuse oversized bytes, stop a parse past
/// [`DEFAULT_PARSE_DEADLINE`], then reject recovery nodes and over-budget trees
/// in one traversal.
pub fn try_parse(code: &str, language: Language) -> Result<Parsed, ParseError> {
    try_parse_within(code, language, DEFAULT_PARSE_DEADLINE)
}

/// [`try_parse`] under a deadline the caller names.
///
/// The deadline bounds the parser, which is the work that is super-linear on
/// hostile input; the byte check before it and the validation walk after it are
/// linear and bounded by [`MAX_SOURCE_BYTES`] and [`MAX_AST_NODES`]. A parse
/// still running when `deadline` passes is stopped at the parser's next progress
/// check — every hundred parser operations — and answers
/// [`ParseError::TimedOut`]; one that finishes first answers exactly as
/// [`try_parse`] does. A source small enough to parse before the first check
/// therefore completes even under `Duration::ZERO`, because a parse that
/// finished has spent nothing the deadline could have saved.
///
/// A deeply nested source under `Duration::ZERO` answers [`ParseError::TimedOut`], and the
/// thread is free again: its parser starts the next parse clean. The unit test
/// `a_zero_deadline_stops_a_nested_parse_and_frees_the_thread` runs that.
pub fn try_parse_within(
    code: &str,
    language: Language,
    deadline: Duration,
) -> Result<Parsed, ParseError> {
    parse_bounded(
        code,
        &language.support_lang(),
        language.name(),
        Bounds {
            source_bytes: MAX_SOURCE_BYTES,
            ast_nodes: MAX_AST_NODES,
            ast_depth: MAX_AST_DEPTH,
            deadline,
        },
    )
}

/// Checked parse of a caller-registered grammar, held to the same byte bound,
/// deadline, node bound, depth bound, and recovery refusal as [`try_parse`].
pub fn try_parse_with<L: LanguageExt>(
    code: &str,
    language: &L,
    name: &'static str,
) -> Result<AstGrep<StrDoc<L>>, ParseError> {
    parse_bounded(
        code,
        language,
        name,
        Bounds {
            source_bytes: MAX_SOURCE_BYTES,
            ast_nodes: MAX_AST_NODES,
            ast_depth: MAX_AST_DEPTH,
            deadline: DEFAULT_PARSE_DEADLINE,
        },
    )
}

/// The policy one checked parse applies, named at the call site.
#[derive(Clone, Copy)]
pub(super) struct Bounds {
    /// Largest source handed to the parser.
    pub(super) source_bytes: usize,
    /// Largest tree the walk admits.
    pub(super) ast_nodes: usize,
    /// Deepest branch the walk admits.
    pub(super) ast_depth: usize,
    /// How long the parser may run.
    pub(super) deadline: Duration,
}

/// The one body behind [`try_parse`] and [`try_parse_with`]: byte bound, then
/// the tree under its deadline, then node bound, then depth bound, then
/// recovery refusal.
///
/// The bounds are parameters rather than constants read in place so both entry
/// points state the policy they apply at the call site, and so a later caller
/// with a different budget reuses this ordering instead of restating it. The
/// order is the point: the byte check runs before the parser allocates, the
/// node and depth checks before any walk of the tree reaches a caller, and both
/// before the recovery check, so a refusal never describes a tree this walk did
/// not finish inspecting.
///
/// The two tree bounds are separate refusals because they price different
/// shapes: [`ParseError::AstTooLarge`] is a wide tree, and
/// [`ParseError::AstTooDeep`] is a narrow one whose source is a few bytes per
/// nesting level, which the byte ceiling admits in the hundreds of thousands of
/// levels and the node ceiling admits in full.
///
/// `language` is borrowed and cloned into the `StrDoc` because a grammar may be
/// a registered [`CustomLang`] or a built-in [`Language`]; `name` is the stable
/// name the error variants carry, since they cannot hold the grammar itself.
pub(super) fn parse_bounded<L: LanguageExt>(
    code: &str,
    language: &L,
    name: &'static str,
    bounds: Bounds,
) -> Result<AstGrep<StrDoc<L>>, ParseError> {
    let Bounds {
        source_bytes: max_source_bytes,
        ast_nodes: max_ast_nodes,
        ast_depth: max_ast_depth,
        deadline,
    } = bounds;
    validate_source_size(code, max_source_bytes)?;
    // Before the parser, and only for the markdown grammar: its external scanner
    // serializes open block containers into a fixed buffer and asserts when they
    // do not fit, which ends the process rather than returning an error. Every
    // other grammar reaches `try_new` on any source inside the byte bound.
    if name == MARKDOWN {
        validate_markdown_containers(code, name, MAX_MARKDOWN_CONTAINERS_PER_LINE)?;
    }
    let tree = parse_tree(code, &language.get_ts_language(), name, deadline);
    let parsed = AstGrep::doc(StrDoc {
        src: code.to_owned(),
        lang: language.clone(),
        tree: tree?,
    });
    let (metrics, _, diagnostics, diagnostics_truncated) =
        inspect_ast_with_pending(&parsed.root(), Some(max_ast_nodes), Some(max_ast_depth));
    if metrics.stop_reason == Some(InspectionStopReason::DepthLimitExceeded) {
        let refusal = Err(ParseError::AstTooDeep {
            language: name,
            observed: metrics.max_depth,
            limit: max_ast_depth,
        });
        tracing::debug!(error = ?refusal.as_ref().err(), "parse_bounded: returning an error to the caller");
        return refusal;
    }
    if !metrics.complete {
        let refusal = Err(ParseError::AstTooLarge {
            language: name,
            observed: metrics.nodes,
            limit: max_ast_nodes,
        });
        tracing::debug!(error = ?refusal.as_ref().err(), "parse_bounded: returning an error to the caller");
        return refusal;
    }
    if metrics.has_syntax_issues {
        let refusal = Err(ParseError::InvalidSyntax {
            language: name,
            diagnostics,
            diagnostics_truncated,
        });
        tracing::debug!(error = ?refusal.as_ref().err(), "parse_bounded: returning an error to the caller");
        return refusal;
    }
    Ok(parsed)
}

/// Build `code`'s tree with this thread's parser for `grammar`, stopping it at
/// `deadline`.
///
/// This is the one place the crate names `tree-sitter` directly, and it does so
/// for the one thing `ast-grep-core` does not expose: its `parse_lang` builds the
/// parser privately and calls `Parser::parse` with no progress callback, so a
/// tree built through it cannot be stopped. The tree built here is the same
/// `tree_sitter::Tree` ast-grep would have built — same grammar, same source,
/// same parser defaults — and is handed to it through `StrDoc`'s public fields,
/// so a caller receives the identical [`Parsed`].
///
/// The parser is cached per thread and per grammar, as ast-grep's is. If the
/// cache is already borrowed on this thread, which nothing in this crate does,
/// the parse runs on a fresh parser rather than failing.
fn parse_tree(
    code: &str,
    grammar: &TSLanguage,
    name: &'static str,
    deadline: Duration,
) -> Result<tree_sitter::Tree, ParseError> {
    PARSERS.with(|cache| match cache.try_borrow_mut() {
        Ok(mut parsers) => {
            if parsers.len() >= MAX_CACHED_PARSERS && !parsers.contains_key(grammar) {
                parsers.clear();
            }
            let parser = match parsers.entry(grammar.clone()) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let fresh = new_parser(grammar, name);
                    entry.insert(fresh?)
                }
            };
            run_parser(parser, code, name, deadline, &mut Instant::now)
        }
        Err(busy) => {
            tracing::debug!(
                ?busy,
                "parse_tree: the parser cache is in use; parsing on a fresh parser"
            );
            let fresh = new_parser(grammar, name);
            run_parser(&mut fresh?, code, name, deadline, &mut Instant::now)
        }
    })
}

/// A parser for `grammar`, or the grammar's own refusal of it.
fn new_parser(grammar: &TSLanguage, name: &'static str) -> Result<tree_sitter::Parser, ParseError> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(grammar)
        .map(|()| parser)
        .map_err(|error| {
            let refusal = ParseError::ParserUnavailable {
                language: name,
                detail: error.to_string(),
            };
            tracing::debug!(error = ?refusal, "new_parser: returning an error to the caller");
            refusal
        })
}

/// Run one parse of `code` on `parser`, asking at every progress check whether
/// `deadline` has passed.
///
/// `clock` is read once when the parse starts and once at every progress check;
/// the callers pass `Instant::now`. It is an argument so the stopping rule can be
/// proved on a clock the proof controls: on the host's clock, how long a stopped
/// parse took measures the scheduler as much as the parser, and a 100 ms
/// deadline answered after 392 ms on a loaded CI runner although the parser
/// stopped at its first check past the deadline.
///
/// The parser is reset before and after. Before, because a parser from the
/// cache may hold state from whatever ran on it last; after, because a stopped
/// parse keeps its partial state *for resuming*, and a cached parser that kept
/// it would hand the next caller on this thread a continuation of somebody
/// else's source.
pub(super) fn run_parser(
    parser: &mut tree_sitter::Parser,
    code: &str,
    name: &'static str,
    deadline: Duration,
    clock: &mut impl FnMut() -> Instant,
) -> Result<tree_sitter::Tree, ParseError> {
    parser.reset();
    // `None` is a deadline past what the clock can represent, which never
    // arrives: the parse runs to completion, as it would with no bound at all.
    let stop_at = clock().checked_add(deadline);
    let mut expired = false;
    let mut progress = |_: &tree_sitter::ParseState| {
        if stop_at.is_some_and(|at| clock() >= at) {
            expired = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let bytes = code.as_bytes();
    let tree = parser.parse_with_options(
        // The parser reads the source through a closure that must answer with a
        // slice, so the end of the source is expressed as an empty one. Clamping
        // the offset to the source's length is what makes the split total: an
        // offset at or past the last byte then yields exactly the empty slice,
        // which is what the parser reads as the end of its input.
        &mut |offset, _| bytes.split_at(offset.min(bytes.len())).1,
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
    );
    parser.reset();
    tree.ok_or_else(|| {
        let refusal = if expired {
            ParseError::TimedOut {
                language: name,
                after: deadline,
            }
        } else {
            ParseError::ParserUnavailable {
                language: name,
                detail: "the parser returned no tree".to_owned(),
            }
        };
        tracing::debug!(error = ?refusal, "run_parser: returning an error to the caller");
        refusal
    })
}

/// Unchecked parse for diagnostics and tests that intentionally inspect
/// malformed trees. Production call sites use [`try_parse`].
///
/// **This path does not run [`MAX_MARKDOWN_CONTAINERS_PER_LINE`].** It returns a
/// [`Parsed`] rather than a `Result`, so a refusal has nowhere to go here, and a
/// markdown source past that bound will abort the process inside the grammar's
/// external scanner rather than return anything. It also runs neither
/// [`MAX_AST_NODES`] nor [`MAX_AST_DEPTH`]. A caller accepting untrusted input
/// uses [`try_parse`], which is the path every bound lives on.
#[must_use]
pub fn parse(code: &str, language: Language) -> Parsed {
    parse_with(code, &language.support_lang())
}

/// Unchecked parse of a caller-registered grammar. Production call sites use
/// [`try_parse_with`].
pub fn parse_with<L: LanguageExt>(code: &str, language: &L) -> AstGrep<StrDoc<L>> {
    language.ast_grep(code)
}

/// Refuse `source` over `limit` bytes, naming both numbers in the refusal.
///
/// The measure is bytes, not characters: the bound exists to keep parse work
/// linear in input, and what the parser is handed is bytes. `actual` travels
/// into the error rather than being dropped, so a caller can report how far
/// over the input was without measuring it a second time.
fn validate_source_size(source: &str, limit: usize) -> Result<(), ParseError> {
    let actual = source.len();
    (actual <= limit)
        .then_some(())
        .ok_or(ParseError::SourceTooLarge { actual, limit })
}
