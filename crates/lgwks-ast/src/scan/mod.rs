//! The zero-gate source detectors (INV-SCAN-ZERO), read through this crate's
//! one Rust grammar (#408).
//!
//! A file this workspace ships carries no silenced error, no unlogged error
//! return, no lint allowance, no overlong try chain and no paraphrase
//! docstring; a file the scan cannot read is refused, never passed.
//!
//! The detectors read tree-sitter syntax. They used to read `syn` from
//! `lgwks_deps`, which made the gate the workspace's second Rust parser and
//! let one hostile file abort it: `syn` recursed without a bound, where the
//! checked parse here refuses past [`crate::MAX_AST_DEPTH`] and every walk
//! below is bounded by that same ceiling (ADR 0001). Rule names, messages,
//! thresholds and exemptions are kept verdict for verdict, and
//! `tests/it/sim_scan_detectors.rs` holds them to an independent model. Where
//! the two grammars shape one source differently, the place is marked
//! `DIVERGENCE` with its reason.
//!
//! Two grammar facts shape every detector. A method call has no node of its
//! own: `x.f(a)` is a `call_expression` whose callee is a `field_expression`.
//! And attributes and doc comments are siblings written before the item they
//! annotate, not children of it, so [`item_attributes`] reads them backwards.

mod binding;
mod cfg;
mod lints;
mod meta;
mod swallow;
mod unlogged;

use std::error::Error;
use std::fmt;
use std::io::Read;
use std::path::Path;

use crate::{AstNode, Language, MAX_SOURCE_BYTES, try_parse};

/// One node of a parsed Rust tree.
type Node<'tree> = AstNode<'tree>;

/// The most a scan reads from one file: one byte past the parser's own source
/// ceiling, so an oversized file reaches the parse and is refused there by
/// name rather than read whole into memory.
fn read_limit() -> u64 {
    match u64::try_from(MAX_SOURCE_BYTES) {
        Ok(ceiling) => ceiling.saturating_add(1),
        // A `usize` wider than 64 bits holds a ceiling no `u64` file length
        // can exceed, so the read is bounded by the file itself.
        Err(_) => u64::MAX,
    }
}

/// A finding, in the `{path}:{line}: [{rule}] {snippet}` shape the gate prints.
///
/// The detectors are its only constructors, which is why it is non-exhaustive
/// with private fields: a consumer reads it and cannot forge one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Hit {
    /// `ERROR-SWALLOW`, `unlogged-err-return`, `ALLOW-SILENCE`,
    /// `long-try-chain` or `tautological-doc`.
    rule: &'static str,
    /// Where the finding was observed, counting from 1.
    line: usize,
    /// The evidence the detector read there.
    snippet: String,
}

impl Hit {
    /// Which of the five detectors fired, by its stable rule name.
    #[must_use]
    pub const fn rule(&self) -> &'static str {
        self.rule
    }

    /// The source line the finding sits on, counting from 1.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }

    /// The evidence, as the gate prints it after the rule name.
    #[must_use]
    pub fn snippet(&self) -> &str {
        &self.snippet
    }
}

/// A file the scan could not read, which the gate refuses: passing what it
/// cannot read would report success for the one condition it exists to catch.
///
/// Non-exhaustive so a later reason to refuse cannot become a compile error in
/// a caller's exhaustive `match`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScanError {
    /// The file could not be read, or the checked parse refused it.
    Unparseable {
        /// The file, as the caller named it.
        path: String,
        /// The read failure or the parse refusal.
        cause: String,
    },
}

impl fmt::Display for ScanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Unparseable {
                ref path,
                ref cause,
            } => write!(formatter, "{path}: cannot parse Rust: {cause}"),
        }
    }
}

impl Error for ScanError {}

/// Every finding in one Rust source text, ordered by line and then rule.
///
/// # Errors
///
/// [`ScanError::Unparseable`] when the checked parse refuses the text:
/// invalid syntax, or the byte, node, depth or time ceiling.
pub fn scan_source(source: &str, path: &str) -> Result<Vec<Hit>, ScanError> {
    let parsed = try_parse(source, Language::Rust).map_err(|cause| {
        tracing::debug!(%cause, path, "scan: the checked parse refused a source");
        ScanError::Unparseable {
            path: path.to_owned(),
            cause: cause.to_string(),
        }
    })?;
    let root = parsed.root();
    let mut hits = Vec::new();
    swallow::detect(&root, &mut hits);
    unlogged::detect(&root, &mut hits);
    lints::allow_silence(&root, &mut hits);
    lints::long_try_chain(&root, &mut hits);
    lints::tautological_doc(&root, &mut hits);
    hits.sort_by(|left, right| (left.line, left.rule).cmp(&(right.line, right.rule)));
    Ok(hits)
}

/// Every finding in one `.rs` file, reading no more of it than the parser
/// would accept plus one byte.
///
/// # Errors
///
/// [`ScanError::Unparseable`] when the file cannot be opened, read or decoded
/// as UTF-8, and for every refusal [`scan_source`] makes.
pub fn scan_path(path: &Path) -> Result<Vec<Hit>, ScanError> {
    let shown = path.display().to_string();
    let mut source = String::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(read_limit()).read_to_string(&mut source))
        .map_err(|cause| {
            tracing::debug!(%cause, path = %shown, "scan: a source could not be read");
            ScanError::Unparseable {
                path: shown.clone(),
                cause: cause.to_string(),
            }
        })?;
    scan_source(&source, &shown)
}

// ── Shared syntax ───────────────────────────────────────────────────────────

/// The line a node starts on, counting from 1; tree-sitter counts from 0.
fn line_of(node: &Node<'_>) -> usize {
    node.start_pos().line().saturating_add(1)
}

/// True for a comment, which the grammar admits between any two tokens.
fn is_comment(node: &Node<'_>) -> bool {
    matches!(&*node.kind(), "line_comment" | "block_comment")
}

/// The first named child that is not a comment: the expression inside an
/// expression statement, the path of an attribute.
fn first_named<'tree>(node: &Node<'tree>) -> Option<Node<'tree>> {
    node.children()
        .find(|child| child.is_named() && !is_comment(child))
}

/// A block's statements in order, the tail expression included, as `syn`
/// listed them: delimiters, a label, comments, attributes and stray `;` are
/// not statements.
fn block_statements<'tree>(block: &Node<'tree>) -> Vec<Node<'tree>> {
    block
        .children()
        .filter(|child| {
            child.is_named()
                && !is_comment(child)
                && !matches!(
                    &*child.kind(),
                    "label" | "attribute_item" | "inner_attribute_item" | "empty_statement"
                )
        })
        .collect()
}

/// The expression a statement holds: an expression statement's inner
/// expression, or the statement itself when it is a bare tail expression.
fn statement_expression<'tree>(statement: &Node<'tree>) -> Option<Node<'tree>> {
    if statement.kind() == "expression_statement" {
        first_named(statement)
    } else {
        Some(statement.clone())
    }
}

/// The arguments a call passes, in order, without punctuation, comments or
/// argument attributes.
fn call_arguments<'tree>(arguments: &Node<'tree>) -> Vec<Node<'tree>> {
    arguments
        .children()
        .filter(|child| child.is_named() && !is_comment(child) && child.kind() != "attribute_item")
        .collect()
}

/// A method call `receiver.method(args)`, which the grammar spells as a call
/// whose callee is a field access, possibly under a turbofish.
struct MethodCall<'tree> {
    /// The expression the method is called on.
    receiver: Node<'tree>,
    /// The method's name token.
    method: Node<'tree>,
    /// The `arguments` node.
    arguments: Node<'tree>,
}

impl<'tree> MethodCall<'tree> {
    /// The method call `node` is, or `None` for any other node. A call of a
    /// tuple field (`x.0()`) or of a parenthesised field (`(x.f)()`) is a
    /// plain call in `syn`, and is one here too.
    fn of(node: &Node<'tree>) -> Option<Self> {
        if node.kind() != "call_expression" {
            return None;
        }
        let mut callee = node.field("function")?;
        if callee.kind() == "generic_function" {
            callee = callee.field("function")?;
        }
        if callee.kind() != "field_expression" {
            return None;
        }
        let method = callee.field("field")?;
        if method.kind() != "field_identifier" {
            return None;
        }
        Some(Self {
            receiver: callee.field("value")?,
            method,
            arguments: node.field("arguments")?,
        })
    }
}

/// Where a `fn` is declared, which decides how `syn` visited it: as a free
/// function (`ItemFn`, nested ones included), an `impl` method or a trait
/// method.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    /// At module level or inside a body.
    Free,
    /// In an `impl` block.
    Impl,
    /// In a `trait` block.
    Trait,
}

/// The owner of an item, read from the declaration list around it.
fn owner_of(item: &Node<'_>) -> Owner {
    let Some(list) = item
        .parent()
        .filter(|list| list.kind() == "declaration_list")
    else {
        return Owner::Free;
    };
    match list.parent() {
        Some(container) if container.kind() == "impl_item" => Owner::Impl,
        Some(container) if container.kind() == "trait_item" => Owner::Trait,
        _ => Owner::Free,
    }
}

/// A path in expression position, as `syn::ExprPath` held it: segment names
/// without generic arguments, whether it opened with `::`, whether any
/// segment carried generic arguments, and whether a `<T as Trait>::`
/// qualifier roots it.
struct ExprPath {
    /// Each segment's name, in order.
    segments: Vec<String>,
    /// The path opened with `::`.
    leading_colon: bool,
    /// A segment carried generic arguments (`f::<T>`).
    generic: bool,
    /// A bracketed qualifier roots the path, `syn`'s `qself`.
    qualified: bool,
}

impl ExprPath {
    /// The path an expression spells, or `None` for an expression that is
    /// not a path. A method turbofish (`x.f::<T>`) is a field access.
    fn read(node: &Node<'_>) -> Option<Self> {
        let mut path = Self {
            segments: Vec::new(),
            leading_colon: false,
            generic: false,
            qualified: false,
        };
        path.push(node)?;
        Some(path)
    }

    /// Appends `node`'s segments, or answers `None` when it is not a path.
    fn push(&mut self, node: &Node<'_>) -> Option<()> {
        match &*node.kind() {
            "identifier" | "type_identifier" | "self" | "super" | "crate" | "metavariable" => {
                self.segments.push(node.text().into_owned());
            }
            "scoped_identifier" => {
                match node.field("path") {
                    Some(prefix) => self.push(&prefix)?,
                    None => self.leading_colon = self.segments.is_empty(),
                }
                self.segments.push(node.field("name")?.text().into_owned());
            }
            "generic_function" => {
                self.generic = true;
                self.push(&node.field("function")?)?;
            }
            "generic_type" => {
                self.generic = true;
                self.push(&node.field("type")?)?;
            }
            "bracketed_type" => self.qualified = true,
            _ => return None,
        }
        Some(())
    }

    /// The last segment's name.
    fn last(&self) -> Option<&str> {
        self.segments.last().map(String::as_str)
    }

    /// The segments joined by `::`, without the leading colon.
    fn render(&self) -> String {
        self.segments.join("::")
    }

    /// The one name this path is, when it is a bare identifier: no `::`, no
    /// generic arguments, no qualifier.
    fn single(&self) -> Option<&str> {
        if self.leading_colon || self.generic || self.qualified {
            return None;
        }
        match *self.segments.as_slice() {
            [ref only] => Some(only.as_str()),
            _ => None,
        }
    }
}

// ── Attributes ──────────────────────────────────────────────────────────────

/// The attributes and doc comments that annotate `item`, in source order: the
/// outer ones written before it, then the inner ones heading its body, which
/// is the set `syn` gave an item as `attrs`. Plain comments between them are
/// skipped.
fn item_attributes<'tree>(item: &Node<'tree>) -> Vec<Node<'tree>> {
    let mut found = Vec::new();
    let mut cursor = item.prev();
    while let Some(sibling) = cursor {
        let keep = sibling.kind() == "attribute_item" || is_doc_comment(&sibling, Side::Outer);
        if !keep && !is_comment(&sibling) {
            break;
        }
        cursor = sibling.prev();
        if keep {
            found.push(sibling);
        }
    }
    found.reverse();
    if let Some(body) = item.field("body") {
        for child in body.children().filter(|child| child.is_named()) {
            let keep =
                child.kind() == "inner_attribute_item" || is_doc_comment(&child, Side::Inner);
            if !keep && !is_comment(&child) {
                break;
            }
            if keep {
                found.push(child);
            }
        }
    }
    found
}

/// Which item a doc comment documents.
#[derive(Clone, Copy)]
enum Side {
    /// `///` and `/** */`: the item after it.
    Outer,
    /// `//!` and `/*! */`: the item around it.
    Inner,
}

/// True for a doc comment of the given side.
fn is_doc_comment(node: &Node<'_>, side: Side) -> bool {
    let (field, marker) = match side {
        Side::Outer => ("outer", "outer_doc_comment_marker"),
        Side::Inner => ("inner", "inner_doc_comment_marker"),
    };
    is_comment(node)
        && (node.field(field).is_some() || node.children().any(|child| child.kind() == marker))
}

/// The `attribute` inside an `#[..]` or `#![..]`; `None` for a doc comment.
fn attribute_of<'tree>(item: &Node<'tree>) -> Option<Node<'tree>> {
    item.children().find(|child| child.kind() == "attribute")
}

/// The path an attribute names, read as an expression path.
fn attribute_path(attribute: &Node<'_>) -> Option<ExprPath> {
    ExprPath::read(&first_named(attribute)?)
}

/// True when an attribute's path is the bare identifier `word`, `syn`'s
/// `is_ident`.
fn attribute_is(attribute: &Node<'_>, word: &str) -> bool {
    attribute_path(attribute).is_some_and(|path| path.single() == Some(word))
}

/// True for `#[test]`, `#[test_case]`, `#[rstest]`, or a `#[cfg]` that can
/// only hold under `test`, decided by exhaustive satisfiability over its atoms.
fn is_test_fn(attributes: &[Node<'_>]) -> bool {
    attributes.iter().any(is_test_attribute)
}

/// True when one attribute marks its item test-only: a harness macro named by
/// its last path segment, or a `cfg` whose single predicate cannot hold
/// outside `test`. A `cfg` whose arguments are not one predicate is not a
/// test marker.
fn is_test_attribute(item: &Node<'_>) -> bool {
    let Some(attribute) = attribute_of(item) else {
        return false;
    };
    let named_test = attribute_path(&attribute).is_some_and(|path| {
        path.last()
            .is_some_and(|name| matches!(name, "test" | "test_case" | "rstest"))
    });
    if named_test {
        return true;
    }
    attribute_is(&attribute, "cfg")
        && attribute
            .field("arguments")
            .and_then(|arguments| meta::parse_single(&arguments))
            .is_some_and(|predicate| cfg::requires_test(&predicate))
}

/// True when an attribute names a test or a test helper: last segment `test`,
/// or any name starting `test_`.
///
/// Looser than [`is_test_fn`] on purpose: it only switches off the
/// unlogged-error, try-chain and tautology rules for that function, so a
/// helper wrongly taken for a test costs one missed finding there and never a
/// false accusation elsewhere.
fn is_test_attr(attributes: &[Node<'_>]) -> bool {
    attributes
        .iter()
        .filter_map(attribute_of)
        .filter_map(|attribute| attribute_path(&attribute))
        .any(|path| {
            path.last()
                .is_some_and(|name| name == "test" || name.starts_with("test_"))
        })
}
