//! Which items exist only under `cfg(test)`, and which attributes mark a test.
//!
//! The scan skips test code for the error rules, so it has to say what test
//! code is. `#[test]`, `#[test_case]` and `#[rstest]` are tests; a `#[cfg(..)]`
//! is test-only when no assignment of its other atoms can satisfy it with `test`
//! false, decided by exhaustive SAT over at most [`MAX_CFG_ATOMS`] atoms.

use std::collections::{BTreeSet, HashMap};

use tree_sitter::Node;

use super::syntax::{self, Path, Tok};

/// The most free atoms a `cfg` may have before the scan stops enumerating
/// assignments and declines to call it test-only.
const MAX_CFG_ATOMS: usize = 12;

/// One parsed meta item: `name`, `name(..)` or `name = value`.
#[derive(Clone, Debug)]
pub(super) enum Meta {
    /// A bare path.
    Path(Path),
    /// A path with a parenthesised list. `children` is `None` when the list is
    /// not a well-formed meta list; every reader degrades conservatively then.
    List {
        /// The list's path (`all`, `any`, `not`, `feature`).
        path: Path,
        /// The entries, or `None` when they do not parse.
        children: Option<Vec<Meta>>,
    },
    /// `path = value`, with the value rendered as an atom key would carry it.
    NameValue {
        /// The left-hand path.
        path: Path,
        /// The value: a literal's text, or `..` for any other expression.
        value: String,
    },
}

/// The identifier token at `at`.
fn ident_at(tokens: &[Tok], at: usize) -> Option<&str> {
    tokens.get(at).and_then(Tok::ident)
}

/// The punctuation token at `at`.
fn punct_at(tokens: &[Tok], at: usize) -> Option<&str> {
    tokens.get(at).and_then(Tok::punct)
}

/// A path at `at` in `tokens`, advancing `at` past it.
fn parse_path(tokens: &[Tok], at: &mut usize) -> Option<Path> {
    let mut path = Path::default();
    if punct_at(tokens, *at) == Some("::") {
        path.leading = true;
        *at = at.saturating_add(1);
    }
    loop {
        path.segments.push(ident_at(tokens, *at)?.to_owned());
        *at = at.saturating_add(1);
        let more =
            punct_at(tokens, *at) == Some("::") && ident_at(tokens, at.saturating_add(1)).is_some();
        if !more {
            return Some(path);
        }
        *at = at.saturating_add(1);
    }
}

/// One meta item at `at`, advancing `at` past it.
fn parse_meta(tokens: &[Tok], at: &mut usize) -> Option<Meta> {
    let path = parse_path(tokens, at)?;
    if let Some(inner) = tokens.get(*at).and_then(Tok::group) {
        *at = at.saturating_add(1);
        return Some(Meta::List {
            path,
            children: parse_list(inner),
        });
    }
    if punct_at(tokens, *at) != Some("=") {
        return Some(Meta::Path(path));
    }
    *at = at.saturating_add(1);
    let start = *at;
    while tokens.get(*at).is_some() && punct_at(tokens, *at) != Some(",") {
        *at = at.saturating_add(1);
    }
    let value = tokens.get(start..*at)?;
    let rendered = match value.len() {
        0 => return None,
        1 => match value.first().and_then(Tok::literal) {
            Some(literal) => literal.to_owned(),
            None => "..".to_owned(),
        },
        _ => "..".to_owned(),
    };
    Some(Meta::NameValue {
        path,
        value: rendered,
    })
}

/// A comma-separated meta list with an optional trailing comma, or `None` when
/// any entry does not parse.
pub(super) fn parse_list(tokens: &[Tok]) -> Option<Vec<Meta>> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        found.push(parse_meta(tokens, &mut at)?);
        if at >= tokens.len() {
            break;
        }
        if punct_at(tokens, at) != Some(",") {
            return None;
        }
        at = at.saturating_add(1);
    }
    Some(found)
}

/// The single meta item an attribute's arguments are, or `None` when they are
/// anything else (a bare `#[cfg]`, an empty list, token soup).
fn single_meta(tokens: &[Tok]) -> Option<Meta> {
    let mut at = 0;
    let meta = parse_meta(tokens, &mut at)?;
    (at == tokens.len()).then_some(meta)
}

/// What one `cfg` expression evaluates to under one assignment of its atoms.
///
/// `Unevaluated` is an answer of its own rather than a value substituted for a
/// missing one: an atom the assignment never recorded means the walk that
/// collected the atoms and the evaluation that reads them understood different
/// shapes out of one expression, which is this scanner disagreeing with itself.
/// That is not a value the expression has, and folding it into `Satisfied`
/// would make the disagreement invisible to every caller above it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// The expression holds under this assignment.
    Satisfied,
    /// The expression does not hold under this assignment.
    Unsatisfied,
    /// The expression could not be evaluated under this assignment.
    Unevaluated,
}

impl Verdict {
    /// Whether the expression can hold under some assignment. An expression
    /// this scanner could not evaluate is one it refuses to call test-only: an
    /// item kept in scope is a candidate for a finding, and an item dropped is a
    /// clean report for source nobody read.
    const fn may_hold(self) -> bool {
        matches!(self, Self::Satisfied | Self::Unevaluated)
    }

    /// `all` of two conjuncts: one unsatisfied conjunct is enough.
    const fn all(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unsatisfied, _) | (_, Self::Unsatisfied) => Self::Unsatisfied,
            (Self::Satisfied, Self::Satisfied) => Self::Satisfied,
            _ => Self::Unevaluated,
        }
    }

    /// `any` of two disjuncts: one satisfied disjunct is enough.
    const fn any(self, other: Self) -> Self {
        match (self, other) {
            (Self::Satisfied, _) | (_, Self::Satisfied) => Self::Satisfied,
            (Self::Unsatisfied, Self::Unsatisfied) => Self::Unsatisfied,
            _ => Self::Unevaluated,
        }
    }

    /// `not`: negating "unknown" is still unknown.
    const fn negate(self) -> Self {
        match self {
            Self::Satisfied => Self::Unsatisfied,
            Self::Unsatisfied => Self::Satisfied,
            Self::Unevaluated => Self::Unevaluated,
        }
    }
}

/// Structural rendering of a cfg atom: canonical where token text is
/// incidental, and identity-consistent within each SAT problem, which is all
/// the solver requires.
fn atom_key(meta: &Meta) -> String {
    match *meta {
        Meta::Path(ref path) => path.render(),
        Meta::List {
            ref path,
            ref children,
        } => {
            let head = path.render();
            match *children {
                Some(ref entries) => {
                    let inner = entries.iter().map(atom_key).collect::<Vec<_>>().join(",");
                    format!("{head}({inner})")
                }
                None => format!("{head}(?)"),
            }
        }
        Meta::NameValue {
            ref path,
            ref value,
        } => format!("{}={value}", path.render()),
    }
}

/// The combinator a list is, when its path is exactly that identifier.
fn combinator(path: &Path) -> Option<&str> {
    path.single()
        .filter(|name| matches!(*name, "all" | "any" | "not"))
}

/// Collect every free atom of a `cfg` expression, recursing through `all`,
/// `any` and `not`.
///
/// `test` is deliberately not collected: it is supplied as a fixed `false`, so
/// treating it as a free variable would let the solver satisfy an expression it
/// cannot satisfy in a non-test build. A combinator whose arguments do not parse
/// is recorded by its own text, which keeps the expression unsolvable rather
/// than silently dropping the atom.
fn collect_atoms(meta: &Meta, atoms: &mut BTreeSet<String>) {
    match *meta {
        Meta::Path(ref path) if path.single() == Some("test") => {}
        Meta::List {
            ref path,
            children: Some(ref entries),
        } if combinator(path).is_some() => {
            for entry in entries {
                collect_atoms(entry, atoms);
            }
        }
        _ => {
            atoms.insert(atom_key(meta));
        }
    }
}

/// One atom's value under the assignment, or `Unevaluated` when the assignment
/// never recorded it.
fn atom_verdict(meta: &Meta, values: &HashMap<&str, bool>) -> Verdict {
    match values.get(atom_key(meta).as_str()).copied() {
        Some(true) => Verdict::Satisfied,
        Some(false) => Verdict::Unsatisfied,
        None => Verdict::Unevaluated,
    }
}

/// Evaluate a `cfg` expression under one assignment of its free atoms, with
/// `test` fixed false.
fn evaluate(meta: &Meta, values: &HashMap<&str, bool>) -> Verdict {
    match *meta {
        Meta::Path(ref path) if path.single() == Some("test") => Verdict::Unsatisfied,
        Meta::List {
            ref path,
            ref children,
        } => match (combinator(path), children.as_deref()) {
            (Some("all"), Some(entries)) => {
                entries.iter().fold(Verdict::Satisfied, |held, entry| {
                    held.all(evaluate(entry, values))
                })
            }
            (Some("any"), Some(entries)) => {
                entries.iter().fold(Verdict::Unsatisfied, |held, entry| {
                    held.any(evaluate(entry, values))
                })
            }
            (Some("not"), Some(entries)) => match entries.first() {
                Some(only) if entries.len() == 1 => evaluate(only, values).negate(),
                _ => atom_verdict(meta, values),
            },
            (Some("all" | "any"), None) => Verdict::Unevaluated,
            _ => atom_verdict(meta, values),
        },
        _ => atom_verdict(meta, values),
    }
}

/// Whether no assignment of the atoms outside `test` can satisfy the
/// expression, i.e. the item exists only under `cfg(test)`.
fn requires_test(meta: &Meta) -> bool {
    let mut atoms = BTreeSet::new();
    collect_atoms(meta, &mut atoms);
    if atoms.len() > MAX_CFG_ATOMS {
        return false;
    }
    let atoms: Vec<String> = atoms.into_iter().collect();
    !(0..(1usize << atoms.len())).any(|assignment| {
        let values: HashMap<&str, bool> = atoms
            .iter()
            .enumerate()
            .map(|(index, atom)| (atom.as_str(), assignment & (1 << index) != 0))
            .collect();
        evaluate(meta, &values).may_hold()
    })
}

/// True when one attribute marks its item as test-only: a test harness macro
/// (`test`, `test_case`, `rstest`) or a `cfg` that cannot hold outside `test`.
///
/// The `cfg` arm reads its arguments as a single meta item; an attribute whose
/// arguments are not a well-formed one (a bare `#[cfg]`, or token soup) is not a
/// test marker and answers `false`.
fn is_test_attribute(source: &str, item: Node<'_>) -> bool {
    let Some(attr) = syntax::attribute(source, item) else {
        return false;
    };
    if attr
        .path
        .last()
        .is_some_and(|name| matches!(name, "test" | "test_case" | "rstest"))
    {
        return true;
    }
    attr.path.single() == Some("cfg")
        && attr
            .arguments
            .map(|tree| syntax::tokens(source, tree))
            .and_then(|tokens| single_meta(&tokens))
            .is_some_and(|meta| requires_test(&meta))
}

/// True for an item whose attributes mark it test-only.
pub(super) fn is_test_item(source: &str, item: Node<'_>) -> bool {
    syntax::attributes(item)
        .into_iter()
        .any(|attribute| is_test_attribute(source, attribute))
}

/// True when an attribute names a test or a test helper: `test`, or any name
/// starting with `test_`.
///
/// Deliberately looser than [`is_test_item`]: this decides only whether to
/// disable a rule, so a helper wrongly classified as test-only costs a missed
/// finding for that helper alone, never a false accusation on production code.
pub(super) fn is_test_or_helper(source: &str, item: Node<'_>) -> bool {
    syntax::attributes(item).into_iter().any(|attribute| {
        match syntax::attribute(source, attribute) {
            Some(attr) => attr
                .path
                .last()
                .is_some_and(|name| name == "test" || name.starts_with("test_")),
            None => false,
        }
    })
}
