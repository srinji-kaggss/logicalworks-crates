//! Spans, severities, and the diagnostics a tool reports on code.
//!
//! The refusal type is [`ParseError`]; this module is what a
//! tool actually renders. A [`Diagnostic`] carries the four things a report line
//! needs and the refusal type does not: which file, where in it, how bad, and
//! what to say.
//!
//! The positional data is not invented. ast-grep already computes a byte range
//! and a line/byte-column pair for every node (`Node::range`, `Node::start_pos`),
//! and nothing in this crate consumed it; that is the gap this module fills.
//! Spans are built from those values, and a span that would leave the source is
//! clamped rather than trusted, because a grammar is third-party code and its
//! arithmetic is not this crate's to vouch for.
//!
//! Line and column are **1-based** here, matching what an editor shows and what
//! every diagnostic printer assumes. ast-grep counts from zero; the conversion
//! happens once, here, so a caller never has to remember which convention a
//! given number is in. `byte` is always the raw zero-based offset, because that
//! is what [`str::get`] and slicing take.
//!
//! The walk is the same cursor walk [`inspect_ast`] uses, so collecting
//! diagnostics over a wide tree retains memory proportional to depth rather
//! than to fan-out.

use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::{AstNode, LanguageExt};

/// One position in a source file.
///
/// 1-based `line` and `column`, zero-based `byte`. `column` counts characters
/// rather than bytes, so a line containing multi-byte text reports the column a
/// person would count; `byte` is the offset a slice takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub struct Pos {
    /// 1-based line number.
    pub line: usize,
    /// 1-based character column.
    pub column: usize,
    /// Zero-based byte offset from the start of the file.
    pub byte: usize,
}

impl Pos {
    /// Build a position from zero-based counts, normalising to the 1-based
    /// convention this crate reports.
    ///
    /// A zero line or column becomes 1, because position zero is what the
    /// underlying grammar emits for the first line's first column, and a report
    /// reading "line 0" is a defect in the report rather than in the file.
    #[must_use]
    pub fn new(line: usize, column: usize, byte: usize) -> Self {
        Self {
            line: line.saturating_add(1),
            column: column.saturating_add(1),
            byte,
        }
    }
}

/// A half-open byte range, with the 1-based line/column of each end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Span {
    /// First position covered.
    pub start: Pos,
    /// One past the last position covered.
    pub end: Pos,
}

impl Span {
    /// The byte range this span covers, for slicing a source file.
    #[must_use]
    pub fn byte_range(&self) -> Range<usize> {
        self.start.byte..self.end.byte
    }

    /// A zero-width span at `pos`: the point a whole-file condition applies at.
    ///
    /// A refusal about a file's size is not a position inside the file, so it is
    /// reported at the end of the file rather than at some invented interior
    /// line. A renderer should not underline an empty span.
    #[must_use]
    pub fn at(pos: Pos) -> Self {
        Self {
            start: pos,
            end: pos,
        }
    }

    /// Whether this span covers no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.start.byte == self.end.byte
    }
}

/// How much a diagnostic matters.
///
/// The split is between *the parse was refused* and *the parse succeeded and
/// recovered*. A refusal is [`Severity::Error`] and there is no tree to inspect.
/// A recovery node is [`Severity::Warning`]: the tool got an answer, and the
/// answer may be wrong from that point on. Collapsing the two loses the only
/// information a reader needs to decide whether to trust the rest of the parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Severity {
    /// The source was refused; no usable tree exists.
    Error,
    /// The tree was produced but carries a recovery node.
    Warning,
}

impl Severity {
    /// The conventional single-character label for this severity.
    #[must_use]
    pub fn label(self) -> char {
        match self {
            Self::Error => 'E',
            Self::Warning => 'W',
        }
    }
}

/// One reportable finding against a source file.
///
/// The fields are readable but not publicly writable. A finding is a claim
/// about a source location: the message, the severity and the span have to
/// describe *one* thing at *one* place. Public mutable fields would let a
/// caller re-point `span` while leaving a message that describes the old
/// location, and the result still renders as well-formed output — a plausible
/// finding at an implausible place, which is worse than no finding. Build with
/// [`Diagnostic::new`]; [`Diagnostic::with_severity`] is the one adjustment this
/// type supports, and it exists because escalating or dropping a warning is a
/// decision a policy layer makes, not a field poke.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Diagnostic {
    /// The file the finding is against. This is a label carried through the
    /// report; this crate does not read the file and does not check that the
    /// bytes it parsed are the bytes this path holds now.
    file: PathBuf,
    /// How much the finding matters.
    severity: Severity,
    /// What to say, in one line.
    message: String,
    /// Where in the file the finding is.
    span: Span,
}

impl Diagnostic {
    /// A warning-level finding at `span`.
    #[must_use]
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            file: PathBuf::new(),
            severity: Severity::Warning,
            message: message.into(),
            span,
        }
    }

    /// The same finding, reported against `file`.
    ///
    /// A label only: this crate does not read the path, so nothing here checks
    /// that the bytes it parsed are the bytes that path holds now.
    #[must_use]
    pub fn in_file(mut self, file: impl Into<PathBuf>) -> Self {
        self.file = file.into();
        self
    }

    /// The same finding at a different severity.
    #[must_use]
    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }

    /// The file this finding is reported against.
    #[must_use]
    pub fn file(&self) -> &Path {
        &self.file
    }

    /// How much the finding matters.
    #[must_use]
    pub fn severity(&self) -> Severity {
        self.severity
    }

    /// What to say, in one line.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Where in the file the finding is.
    #[must_use]
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// The diagnostic rendered the way `rustc` renders one, without a trailing
    /// newline: `path:line:column: severity: message`.
    ///
    /// Built from the fields on every call rather than stored, so a caller that
    /// adjusts a field before rendering does not get the pre-edit text back.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "{}:{}:{}: {}: {}",
            self.file.display(),
            self.span.start.line,
            self.span.start.column,
            self.severity.label(),
            self.message
        )
    }
}

/// A walked node's line/column, resolved against the source.
///
/// ast-grep reports a zero-based line with a zero-based **byte** column, and
/// its own `column()` accessor is an O(n) scan of the source. Neither is what a
/// report wants: a byte column on a line containing multi-byte text counts
/// bytes a person cannot see. This indexes every line start once, so converting
/// a span is two binary-search steps and a slice of one line regardless of how
/// many findings the file holds.
struct LineIndex<'src> {
    /// The text spans are resolved against. Borrowed, never copied: the caller
    /// already holds it and copying it per file would double peak memory on a
    /// file near [`MAX_SOURCE_BYTES`].
    source: &'src str,
    /// Byte offset of the start of each line, always beginning with 0.
    line_starts: Vec<usize>,
}

impl<'src> LineIndex<'src> {
    /// Index every line start in `source`.
    fn new(source: &'src str) -> Self {
        let mut line_starts = vec![0];
        // `+ 1` is a saturating step past the newline, not a wrapping one: the
        // offset of a character is always strictly less than the length of the
        // string it indexes, so this cannot reach `usize::MAX`.
        line_starts.extend(
            source
                .match_indices('\n')
                .map(|(at, _)| at.saturating_add(1)),
        );
        Self {
            source,
            line_starts,
        }
    }

    /// The 1-based line number holding `byte`.
    ///
    /// The partition point is the first line start strictly after `byte`, so
    /// `byte` belongs to the line before it. A trailing newline puts a final
    /// start equal to `source.len()`, and the strict comparison keeps the last
    /// real line from being counted twice.
    fn line_of(&self, byte: usize) -> usize {
        self.line_starts.partition_point(|start| *start <= byte)
    }

    /// The 1-based position of `byte` within the file, clamped to the source
    /// and floored to a character boundary.
    ///
    /// A span can come from another copy of the text than the one passed here,
    /// so an offset inside a multi-byte character is possible; it resolves to
    /// that character's first byte rather than panicking on the slice below.
    fn position(&self, byte: usize) -> Pos {
        let byte = self.source.floor_char_boundary(byte);
        let line = self.line_of(byte);
        // `line_starts` always holds at least one entry and `line_of` returns a
        // count in `1..=line_starts.len()`, so the index is in range. A byte
        // landing exactly on a line start resolves to that line, and `saturating_sub`
        // covers the first line, whose start is the 0th entry.
        let line_start = self.line_starts[line.saturating_sub(1)];
        let column = self.source[line_start..byte].chars().count();
        Pos::new(line.saturating_sub(1), column, byte)
    }

    /// The span covering `range`, with every end clamped into the source.
    fn span(&self, range: Range<usize>) -> Span {
        Span {
            start: self.position(range.start),
            end: self.position(range.end),
        }
    }
}

/// Collect one diagnostic per recovery node in `root`'s tree.
///
/// A recovery node is an `ERROR` node or a `MISSING` one: the parser reached
/// text it could not fit the grammar to, and guessed. Every such node becomes a
/// [`Severity::Warning`] carrying the exact span of the offending text, so a
/// caller can underline it. A clean tree yields an empty `Vec`.
///
/// `language` names the grammar in each message; `recovery_message` is why it
/// is a parameter rather than something read off the node.
///
/// The reported order is source order, whatever shape the tree is, because a
/// reader scanning a list of findings expects to walk down the file.
///
/// ```
/// use lgwks_ast::{Language, Severity, diagnostic::diagnostics, parse};
///
/// let source = "fn main() { let x = 1; }\nfn broken( {\n";
/// let tree = parse(source, Language::Rust);
/// let found = diagnostics("src/main.rs", &tree.root(), source, "Rust");
///
/// // The recovery is reported, and it points at the line that carries it.
/// assert!(found.iter().all(|one| one.severity() == Severity::Warning));
/// assert!(found.iter().all(|d| d.span().start.line == 2));
/// ```
///
/// # Bounds
///
/// This is a scan of an *already parsed* tree and is deliberately not a bound a
/// caller can rely on: it is `O(nodes)` and unbounded, exactly like
/// [`inspect_ast`](crate::inspect_ast) with no limit. Reach for
/// [`try_parse`](crate::try_parse) first, which refuses a tree carrying recovery
/// nodes outright, and use this on a tree you already hold — for instance one
/// built by [`parse`](crate::parse), whose entire purpose is to inspect
/// malformed trees.
///
/// `source` must be the text `root` was parsed from. Every span here is a byte
/// offset into one specific string, and resolving an offset against a different
/// string yields a position that is *silently* wrong rather than loudly broken:
/// an off-by-one that reads as a real finding at a plausible line. The safe
/// entry point is [`tree_diagnostics`](crate::tree_diagnostics), which takes
/// the source from the tree and cannot be handed the wrong one.
#[must_use]
pub fn diagnostics<L: LanguageExt>(
    path: impl Into<PathBuf>,
    root: &AstNode<'_, L>,
    source: &str,
    language: &str,
) -> Vec<Diagnostic> {
    let file = labeled(path.into());
    let index = LineIndex::new(source);
    let mut found = Vec::new();
    visit_each_node(root, &mut |node| {
        if node.is_error || node.is_missing {
            found.push(
                Diagnostic::new(
                    recovery_message(node.is_missing, node.kind, language),
                    index.span(node.span.clone()),
                )
                .in_file(file.clone()),
            );
        }
    });
    // The walk already arrives in source order, and this sort keeps the
    // published order a property of `diagnostics` rather than of the traversal
    // that happens to feed it.
    found.sort_by_key(|diagnostic| (diagnostic.span.start.byte, diagnostic.span.end.byte));
    found
}

/// Whether a parsed tree carries recovery nodes, and where.
///
/// This is `diagnostics` for a caller that wants a count rather than a list,
/// and it is the honest form of the question `has_syntax_issues` answers: that
/// function reports a single sticky bit, which cannot say *how many* nodes the
/// parser gave up on or *where*. Use it to decide whether to keep going; use
/// this to tell a reader where the damage is.
#[must_use]
pub fn recovery_count<L: LanguageExt>(root: &AstNode<'_, L>) -> usize {
    let mut count = 0_usize;
    visit_each_node(root, &mut |node| {
        if node.is_error || node.is_missing {
            count = count.saturating_add(1);
        }
    });
    count
}

/// What one visited node looked like, detached from the cursor that reached it.
///
/// The visitor is handed facts rather than a node handle because a handle
/// borrows the cursor's position, and collecting handles would be the sibling
/// frontier this traversal exists not to build. Every field is read off the
/// node before the cursor moves on; `kind` borrows the tree rather than the
/// node, so it outlives the handle it came from.
struct Found<'tree> {
    /// Whether the node is an `ERROR` recovery node.
    is_error: bool,
    /// Whether the node is a `MISSING` recovery node.
    is_missing: bool,
    /// The node's grammar symbol name.
    kind: &'tree str,
    /// Inclusive start and exclusive end byte offsets in the source.
    span: Range<usize>,
}

/// Visit every node at or below `root` exactly once, depth-first, in bounded
/// memory.
///
/// The walk drives a tree-sitter cursor, whose only retained state is the stack
/// of ancestors it has descended through, mirroring `inspect_ast_with_pending`
/// (#277): one entry per *active* ancestor, so retained memory follows depth
/// rather than the root's fan-out, and a level is popped at the end of its
/// branch rather than left pending. A file with N top-level items therefore
/// costs O(depth) resident memory, not O(N).
///
/// Children are visited in source order, which is the order
/// [`inspect_ast`] now reports. Callers that present results in source order
/// get them in source order; the sort at the end of [`diagnostics`] is retained
/// so the published order does not depend on it.
///
/// The root is visited first, then every descendant. `inspect_ast` charges and
/// inspects the root too, so the two walks agree on which nodes exist: a tree
/// whose root is itself a recovery node is counted here exactly as it is
/// refused by [`try_parse`].
fn visit_each_node<L: LanguageExt>(root: &AstNode<'_, L>, visit: &mut dyn FnMut(&Found<'_>)) {
    visit_each_node_measuring(root, visit, None);
}

/// [`visit_each_node`], reporting the high-water mark of retained cursor depth.
///
/// The `peak` out-parameter is how INV-AST-1's resource claim is *tested*
/// against this walk rather than against a second copy of it. A test that
/// reimplemented the traversal would keep passing if this one regressed, which
/// is the failure mode a copied walk always has.
fn visit_each_node_measuring<L: LanguageExt>(
    root: &AstNode<'_, L>,
    visit: &mut dyn FnMut(&Found<'_>),
    mut peak: Option<&mut usize>,
) {
    let mut cursor = root.get_inner_node().walk();
    let mut cursor_depth = 0_usize;
    loop {
        let node = cursor.node();
        let range = node.range();
        visit(&Found {
            is_error: node.is_error(),
            is_missing: node.is_missing(),
            kind: node.kind(),
            span: range.start_byte..range.end_byte,
        });
        if let Some(peak) = peak.as_deref_mut() {
            *peak = (*peak).max(cursor_depth.saturating_add(1));
        }
        if cursor.goto_first_child() {
            cursor_depth = cursor_depth.saturating_add(1);
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
            cursor_depth = cursor_depth.saturating_sub(1);
        }
    }
}

/// Peak depth [`visit_each_node`] descends to while walking `root`.
///
/// One walk, run for its side effect of measuring. Test-only by construction:
/// it exists so INV-AST-1's resource claim is asserted against the walk that
/// actually ships rather than against a copy of it.
#[cfg(all(test, feature = "lang-rust"))]
#[must_use]
fn peak_retained_frames<L: LanguageExt>(root: &AstNode<'_, L>) -> usize {
    let mut peak = 0_usize;
    visit_each_node_measuring(root, &mut |_| {}, Some(&mut peak));
    peak
}

/// What to say about one recovery node.
///
/// An `ERROR` node is text the grammar could not parse. A `MISSING` node is
/// text the grammar expected and did not find, so it spans no source of its own
/// and reads differently.
///
/// The language is named by `language`, which the caller supplies rather than
/// this module reading off the node. ast-grep's `Language` trait has no `name()`
/// — the display name belongs to this crate's own [`Language`] enum, not to the
/// generic grammar — so a message built from the node alone could not say which
/// grammar it meant. Taking it as a parameter keeps the two facts in one place:
/// the caller named the grammar it parsed with, and the message names that.
fn recovery_message(node_kind_missing: bool, node_kind: &str, language: &str) -> String {
    if node_kind_missing {
        format!("{language} is missing {node_kind}")
    } else {
        format!("{language} could not parse this text")
    }
}

/// A file label that renders as something rather than nothing.
///
/// An empty path renders as `:1:1: E: …`, which reads as a truncated filename
/// instead of as "no file was given". The placeholder keeps the shape of the
/// line and says plainly that the file is unknown.
fn labeled(path: PathBuf) -> PathBuf {
    if path.as_os_str().is_empty() {
        PathBuf::from("<source>")
    } else {
        path
    }
}

/// The offset just past the last byte of `source`, as a zero-width span.
///
/// The end of a file is where a whole-file refusal is reported, so this is what
/// [`ParseError::to_diagnostic`](crate::ParseError::to_diagnostic) anchors its
/// size refusals to.
#[must_use]
pub fn end_of(source: &str) -> Span {
    LineIndex::new(source).span(source.len()..source.len())
}

/// The span of `range` in `source`, each end clamped into the source.
///
/// How a checked parse's [`SyntaxDiagnostic`] byte
/// offsets become the line and column a renderer points at.
pub(crate) fn span_of(source: &str, range: Range<usize>) -> Span {
    LineIndex::new(source).span(range)
}

// The fixtures here are Rust source, so the tests need the Rust grammar. The
// span arithmetic itself is language-independent and lives in `LineIndex`,
// which is exercised by the clamped-position test below without a grammar.
#[cfg(all(test, feature = "lang-rust"))]
mod tests {
    use super::*;
    use crate::{Language, parse};

    /// A file whose second function is malformed, so the recovery is confined to
    /// a known line and the byte offset is predictable.
    const BROKEN: &str = "fn main() { let x = 1; }\nfn broken( {\n";

    /// A wide, shallow tree: many top-level functions, each only a few nodes
    /// deep. This is the shape that separates a depth-proportional walk from a
    /// sibling-frontier one.
    fn wide_source(count: usize) -> String {
        (0..count)
            .map(|index| format!("fn f{index}() {{}}\n"))
            .collect::<String>()
    }

    #[test]
    fn a_clean_tree_reports_nothing() {
        let source = "fn main() {}\n";
        let tree = parse(source, Language::Rust);
        assert_eq!(
            diagnostics("ok.rs", &tree.root(), source, "Rust"),
            Vec::new(),
            "a clean tree has nothing to report"
        );
        assert_eq!(recovery_count(&tree.root()), 0);
    }

    #[test]
    fn a_recovery_is_reported_on_the_line_that_carries_it() {
        let tree = parse(BROKEN, Language::Rust);
        let found = diagnostics("bad.rs", &tree.root(), BROKEN, "Rust");
        assert_eq!(found.len(), 1, "one malformed function reports once");
        let first = &found[0];
        assert_eq!(
            first.span().start.line,
            2,
            "the recovery is on the second line, not the clean first one"
        );
        // The byte range is a real slice of the source, and it is exactly the
        // malformed text: not an approximation of it, and not an offset past it.
        let range = first.span().byte_range();
        assert!(
            range.start <= range.end && range.end <= BROKEN.len(),
            "span {range:?} is not inside a source of {} bytes",
            BROKEN.len()
        );
        assert_eq!(
            &BROKEN[range], "fn broken( {",
            "the span covers the malformed text and nothing else"
        );
    }

    #[test]
    fn a_recovery_is_never_reported_as_an_error() {
        let tree = parse(BROKEN, Language::Rust);
        let found = diagnostics("bad.rs", &tree.root(), BROKEN, "Rust");
        assert!(
            found.iter().all(|one| one.severity() == Severity::Warning),
            "a recovered parse is a warning; only a refusal is an error"
        );
        assert_eq!(recovery_count(&tree.root()), found.len());
    }

    #[test]
    fn findings_come_back_in_source_order() {
        let source = "fn a( {\nfn b( {\nfn c( {\n";
        let tree = parse(source, Language::Rust);
        let found = diagnostics("many.rs", &tree.root(), source, "Rust");
        assert!(found.len() > 1, "three broken functions report separately");
        let starts: Vec<usize> = found.iter().map(|one| one.span().start.byte).collect();
        let mut sorted = starts.clone();
        sorted.sort_unstable();
        assert_eq!(starts, sorted, "findings are not in source order");
    }

    #[test]
    fn lines_and_columns_are_one_based_and_bytes_are_zero_based() {
        let tree = parse(BROKEN, Language::Rust);
        let found = diagnostics("p.rs", &tree.root(), BROKEN, "Rust");
        let first = &found[0];
        assert!(
            first.span().start.line >= 1,
            "a report reading line 0 is a defect in the report"
        );
        assert!(
            first.span().start.column >= 1,
            "a report reading column 0 is a defect in the report"
        );
        // The 1-based convention is what makes the second line report 2. A
        // 0-based reading of the same byte offset would say 1, so this is the
        // assertion that pins the convention rather than the arithmetic.
        assert_eq!(
            first.span().start.line,
            BROKEN[..first.span().start.byte]
                .matches('\n')
                .count()
                .saturating_add(1),
            "the reported line is one more than the newlines before it"
        );
    }

    #[test]
    fn a_column_counts_characters_and_not_bytes() {
        // `é` is two bytes and one character. A report pointing after it must
        // not claim the reader typed two characters more than they did.
        let source = "// é\nfn broken( {\n";
        let tree = parse(source, Language::Rust);
        let found = diagnostics("u.rs", &tree.root(), source, "Rust");
        let first = &found[0];
        let line_start = source[..first.span().start.byte]
            .rfind('\n')
            .map_or(0, |at| at.saturating_add(1));
        let column_chars = source[line_start..first.span().start.byte].chars().count();
        assert_eq!(
            first.span().start.column,
            column_chars.saturating_add(1),
            "column is 1-based and counted in characters"
        );
        // The byte offset is genuinely further along than the character count,
        // which is the whole reason this distinction exists.
        assert!(
            first.span().start.byte > first.span().start.column,
            "the multi-byte line should separate byte and character offsets"
        );
    }

    #[test]
    fn an_end_of_file_position_is_the_last_line_and_zero_width() {
        let source = "fn ok() {}\n";
        let span = end_of(source);
        assert!(span.is_empty(), "a whole-file condition has no width");
        assert_eq!(span.start.byte, source.len());
        assert_eq!(
            span.start.line, 2,
            "the trailing newline opens the second line"
        );
    }

    #[test]
    fn an_unlabeled_file_renders_as_a_placeholder_and_not_a_bare_colon() {
        let tree = parse(BROKEN, Language::Rust);
        let found = diagnostics("", &tree.root(), BROKEN, "Rust");
        let rendered = found[0].render();
        assert!(
            rendered.starts_with("<source>:"),
            "an empty path rendered as {rendered}, which reads as a truncated filename"
        );
    }

    #[test]
    fn the_render_matches_the_documented_shape() {
        let tree = parse(BROKEN, Language::Rust);
        let rendered = diagnostics("src/a.rs", &tree.root(), BROKEN, "Rust")[0].render();
        assert!(rendered.starts_with("src/a.rs:"), "{rendered}");
        assert!(
            rendered.contains(": W: "),
            "a warning renders as ` W: `: {rendered}"
        );
        assert!(
            !rendered.ends_with('\n'),
            "render carries no trailing newline"
        );
    }

    #[test]
    fn a_message_names_the_grammar_it_came_from() {
        let tree = parse(BROKEN, Language::Rust);
        let found = diagnostics("m.rs", &tree.root(), BROKEN, "Rust");
        assert!(
            found[0].message().contains("Rust"),
            "the message names the grammar: {}",
            found[0].message()
        );
    }

    #[test]
    fn the_walk_retains_depth_and_not_the_sibling_frontier() {
        // The same resource invariant INV-AST-1 states for `inspect_ast`, held
        // by this walk: a wide tree must not accumulate one pending frame per
        // sibling, or a file with N top-level items costs O(N) resident memory.
        let source = wide_source(4_096);
        let tree = parse(&source, Language::Rust);
        assert_eq!(
            tree.root().children().len(),
            4_096,
            "the fixture presents 4,096 immediate siblings to the walk"
        );
        let peak = peak_retained_frames(&tree.root());
        assert!(
            peak < 64,
            "retained {peak} frames for 4,096 wide siblings; that is fan-out, not depth"
        );
    }

    #[test]
    fn a_deep_narrow_tree_still_retains_one_frame_per_ancestor() {
        // The other half of the same invariant: a deep tree does cost depth,
        // and the cost is the active depth rather than the total node count.
        let depth = 200;
        let source = format!(
            "{}fn f() {{}}{}",
            "fn f() {".repeat(depth),
            "}".repeat(depth)
        );
        let tree = parse(&source, Language::Rust);
        let peak = peak_retained_frames(&tree.root());
        assert!(
            peak > 1,
            "a {depth}-deep tree retained {peak} frames; the walk is not descending"
        );
        let nodes = crate::inspect_ast(&tree.root(), None).nodes;
        assert!(
            peak < nodes,
            "retained {peak} frames for a tree of {nodes} nodes; that is not depth-proportional"
        );
    }
}

/// The line arithmetic, with no grammar involved.
///
/// These hold at every feature set, so they are the tests that keep
/// [`LineIndex`] honest even in a build carrying no grammar at all — the case a
/// consumer of `--no-default-features` is in.
#[cfg(test)]
mod line_index_tests {
    use super::{LineIndex, Pos, Span};

    /// A grammar may report a range past the text it was given. This crate does
    /// not vouch for a third-party grammar's arithmetic, so the position is
    /// clamped into the source rather than reported as-is.
    #[test]
    fn a_range_past_the_source_is_clamped_rather_than_trusted() {
        let source = "fn broken( {";
        let index = LineIndex::new(source);
        let span = index.span(usize::MAX..usize::MAX);
        assert!(span.start.byte <= source.len(), "start clamped");
        assert!(span.end.byte <= source.len(), "end clamped");
    }

    #[test]
    fn an_offset_inside_a_character_is_floored_to_its_first_byte() {
        let source = "\u{e9}\u{e9}\nb";
        let index = LineIndex::new(source);
        let span = index.span(1..3);
        assert_eq!(span.start.byte, 0, "inside the first character");
        assert_eq!(span.start.column, 1, "columns are 1-based");
        assert_eq!(span.end.byte, 2, "inside the second character");
        assert_eq!(span.end.column, 2, "the second character's column");
    }

    #[test]
    fn a_range_entirely_past_the_source_resolves_to_the_last_line() {
        let source = "one\ntwo\nthree\n";
        let index = LineIndex::new(source);
        let span = index.span(9_000..9_001);
        assert_eq!(span.start.byte, source.len());
        assert_eq!(
            span.start.line, 4,
            "the trailing newline opens a fourth line"
        );
    }

    #[test]
    fn the_first_byte_of_the_file_is_line_one_column_one_byte_zero() {
        let source = "alpha\nbeta\n";
        let index = LineIndex::new(source);
        assert_eq!(
            index.position(0),
            Pos {
                line: 1,
                column: 1,
                byte: 0
            },
            "1-based line and column over a 0-based byte offset"
        );
    }

    #[test]
    fn a_byte_on_a_line_start_belongs_to_that_line_not_the_previous_one() {
        let source = "alpha\nbeta\n";
        let index = LineIndex::new(source);
        // Byte 6 is the `b` of `beta`, the first byte of the second line.
        assert_eq!(index.position(6).line, 2);
        assert_eq!(index.position(6).column, 1);
    }

    #[test]
    fn a_multi_byte_line_reports_characters_in_the_column_and_bytes_in_the_offset() {
        let source = "éé\n";
        let index = LineIndex::new(source);
        // Two two-byte characters, so byte 4 is the end of the line and is two
        // characters past its start.
        let pos = index.position(4);
        assert_eq!(pos.column, 3, "four bytes is two characters, one-based");
        assert_eq!(pos.byte, 4, "the byte offset is the byte offset");
    }

    #[test]
    fn an_empty_source_resolves_to_the_only_position_it_has() {
        let index = LineIndex::new("");
        let span = index.span(0..0);
        assert_eq!(span.start.byte, 0);
        assert_eq!(span.start.line, 1);
        assert!(span.is_empty());
    }

    #[test]
    fn a_span_covers_exactly_the_bytes_between_its_ends() {
        let source = "alpha\nbeta\ngamma\n";
        let index = LineIndex::new(source);
        let span = index.span(6..10);
        assert_eq!(&source[span.byte_range()], "beta");
        assert_eq!(span.start.line, 2);
        assert_eq!(span.end.line, 2);
    }

    #[test]
    fn a_zero_width_span_is_empty_and_keeps_its_position() {
        let source = "alpha\nbeta\n";
        let span = Span::at(index_position(source, 6));
        assert!(span.is_empty());
        assert_eq!(span.start, span.end);
        assert_eq!(span.start.line, 2);
    }

    /// The position of `byte` in `source`, through the same index the span
    /// helpers use.
    fn index_position(source: &str, byte: usize) -> Pos {
        LineIndex::new(source).position(byte)
    }
}
