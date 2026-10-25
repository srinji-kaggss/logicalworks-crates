//! Detector unlogged-err-return: a `return Err(..)` in production code that no
//! log statement runs before.
//!
//! The evidence is a log *statement* that runs unconditionally on the way into
//! the return — earlier in the same block, or in an enclosing block before the
//! statement that encloses the return — and that says something about the
//! failure it precedes (#699): it names a value the error carries, or it carries
//! context of its own beyond a bare message. `eprintln!("zero"); return Err(e);`
//! emits a word and nothing about the error, so it is not evidence;
//! `tracing::error!(%e)`, `tracing::error!(error = ?e)` and the
//! `debug!(index, "..")` a refusal is announced with are. An error that carries
//! no value (`Err(E::Flat)`, `Err("denied")`) has nothing to name, so the log on
//! the path is the whole signal and any log evidences it.
//!
//! Three things are deliberately not evidence:
//!
//! - Prose. A comment is an `extra` node and a string literal is a literal, not
//!   a macro, so `// tracing::warn!(..)` and `let s = "warn!(";` emit nothing and
//!   excuse nothing.
//! - A log on a path the return is not on. A log inside an untaken `if` branch
//!   is not a statement of the enclosing block and does not seal it.
//! - Physical proximity. Line numbers never enter the decision except as the
//!   identity of a return once its evidence has been established from structure,
//!   so reflowing the same program cannot change the verdict.
//!
//! The walk is conservative in the direction a gate must be: a log it cannot
//! place before the return leaves the return unevidenced, and an unevidenced
//! return is reported.

use std::collections::BTreeSet;

use tree_sitter::Node;

use super::Hit;
use super::cfg;
use super::syntax::Fielded;
use super::syntax::{self, Path};

/// Macro and path prefixes that count as a log emission.
///
/// Both the path prefix (`tracing::`) and the bare macro name (`warn!(`) are
/// listed because the one match site renders a macro path from the tree and then
/// looks for either spelling in that rendering. A marker absent from this list is
/// a macro the detector does not treat as a signal.
const LOG_MARKERS: &[&str] = &[
    "tracing::",
    "log::",
    "info!(",
    "warn!(",
    "error!(",
    "debug!(",
    "trace!(",
    "event!(",
    "eprintln!(",
    "eprint!(",
];

/// The fixed evidence text. CI printers key their remediation text off the rule
/// name, and a stable snippet keeps that mapping total across every hit.
const SNIPPET: &str = "return Err(...) with no log emission that runs before it — the caller sees only a value, not a signal. Log the error with enough context to diagnose it. Wording, line wrapping, and formatting do not matter: the check reads parsed statements, never physical lines.";

/// Runs the unlogged-error detector over a whole file.
///
/// The evidence set is computed once up front and shared with every return the
/// second walk visits, so the two passes cannot disagree about which returns are
/// already explained.
pub(super) fn detect(source: &str, root: Node<'_>, hits: &mut Vec<Hit>) {
    let mut evidence = EvidenceWalker {
        source,
        evidenced: BTreeSet::new(),
        logs: Vec::new(),
    };
    evidence.walk(root);
    let mut walker = ErrReturnWalker {
        source,
        hits,
        test_scopes: Vec::new(),
        evidenced: &evidence.evidenced,
    };
    walker.walk(root);
}

/// The kinds that are not statements of a block: attributes, its label, and a
/// stray semicolon.
fn is_not_statement(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "attribute_item" | "inner_attribute_item" | "label" | "empty_statement"
    )
}

/// The last identifier of a call's function path (`Err`, `Result::Err`), which
/// is how a direct `Err(..)` construction is recognised.
fn callee_name<'s>(source: &'s str, function: Node<'_>) -> Option<&'s str> {
    match function.kind() {
        "identifier" => Some(syntax::text(source, function)),
        "scoped_identifier" => function
            .by_field("name")
            .map(|name| syntax::text(source, name)),
        "generic_function" => function
            .by_field("function")
            .and_then(|inner| callee_name(source, inner)),
        _ => None,
    }
}

/// The line of a `return Err(..)` expression, and `None` for any other node.
fn err_return_line(source: &str, node: Node<'_>) -> Option<usize> {
    if node.kind() != "return_expression" {
        return None;
    }
    let value = syntax::named_kids(node).into_iter().next()?;
    if value.kind() != "call_expression" {
        return None;
    }
    let function = value.by_field("function")?;
    (callee_name(source, function) == Some("Err")).then(|| return_line(node))
}

/// The line a `return` expression starts on, which is where its first outer
/// attribute starts when it has any: `#[cfg(..)] return Err(..);` is one
/// expression to the reference reader, attribute first.
fn return_line(node: Node<'_>) -> usize {
    let anchor = match node.parent() {
        Some(parent) if parent.kind() == "expression_statement" => parent,
        _ => node,
    };
    match syntax::attributes(anchor).first() {
        Some(attribute) => syntax::line(*attribute),
        None => syntax::line(node),
    }
}

/// One log statement that has run on the path: its source text, and whether it
/// carries any context beyond a bare format string.
#[derive(Clone, Copy)]
struct SeenLog<'a> {
    /// The statement as written.
    text: &'a str,
    /// True when the macro passes a field, a capture or any argument beside its
    /// format string: `debug!(index, "..")` and `error!(%cause, "..")` do,
    /// `eprintln!("zero")` does not.
    carries_context: bool,
}

/// The source text of a block statement that is itself a log emission, and
/// whether it carries context: a macro invocation whose path spells one of
/// [`LOG_MARKERS`], written as a statement or as an expression statement.
fn seen_log<'s>(source: &'s str, statement: Node<'_>) -> Option<SeenLog<'s>> {
    let invocation = match statement.kind() {
        "macro_invocation" => statement,
        "expression_statement" => match syntax::named_kids(statement).as_slice() {
            &[only] if only.kind() == "macro_invocation" => only,
            _ => return None,
        },
        _ => return None,
    };
    let path = invocation
        .by_field("macro")
        .and_then(|name| Path::of(source, name))?;
    let rendered = format!("{}!(", path.render());
    if !LOG_MARKERS.iter().any(|marker| rendered.contains(marker)) {
        return None;
    }
    // The invocation's last named child is its token tree: the macro path comes
    // first and the arguments last, whatever spelling the path takes.
    let mut kids = syntax::named_kids(invocation);
    let args = match kids.pop() {
        Some(tree) => syntax::named_kids(tree),
        None => Vec::new(),
    };
    // A macro with no argument emits a fixed string; one string literal and
    // nothing else is a bare message that names no value, so it cannot say what
    // happened; anything further is context.
    let carries_context = match args.len() {
        0 => false,
        1 => args
            .first()
            .is_some_and(|only| only.kind() != "string_literal"),
        _ => true,
    };
    Some(SeenLog {
        text: syntax::text(source, invocation),
        carries_context,
    })
}

/// Whether `character` continues an identifier, which is how a mention is kept
/// from matching inside a longer word (`e` inside `zero`, `error` inside `errors`).
fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// Whether `text` names `name` as a whole identifier, so a format capture
/// (`"{e}"`), a field (`%e`) and a bare argument all count while prose inside a
/// longer word does not.
fn mentions(text: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(offset) = text.get(from..).and_then(|rest| rest.find(name)) {
        let start = from.saturating_add(offset);
        let end = start.saturating_add(name.len());
        let before = text.get(..start).and_then(|head| head.chars().next_back());
        let after = text.get(end..).and_then(|tail| tail.chars().next());
        if !before.is_some_and(is_word) && !after.is_some_and(is_word) {
            return true;
        }
        from = end;
    }
    false
}

/// Whether an identifier is a *name* rather than a value the error carries: the
/// callee of a call, the macro being invoked, a field, or a path segment. A name
/// is not carried to the caller, so a log that repeats it proves nothing about
/// the error (`format` in `Err(format!(..).into())`, `from` in `Arc::from(..)`).
fn is_a_name(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "call_expression" => parent
            .by_field("function")
            .is_some_and(|callee| callee.id() == node.id()),
        "macro_invocation" => parent
            .by_field("macro")
            .is_some_and(|name| name.id() == node.id()),
        "field_expression" => parent
            .by_field("field")
            .is_some_and(|field| field.id() == node.id()),
        "scoped_identifier" | "scoped_type_identifier" | "type_identifier" => true,
        _ => false,
    }
}

/// The identifiers a `return Err(..)` carries to its caller, in source order.
///
/// A type and a variant are not a carried value (`E`, `Flat`, `Refusal`), a field
/// name is the label on a value rather than the value, and a callee or a macro is
/// a name, so only identifiers that begin a value's name — lowercase or `_`, the
/// Rust spelling of a binding — are returned. A return that carries none
/// (`Err(E::Flat)`, `Err("denied")`) has nothing a log could name, and the caller
/// treats the log itself as the signal.
fn carried(source: &str, node: Node<'_>) -> Vec<String> {
    let Some(value) = syntax::named_kids(node).into_iter().next() else {
        return Vec::new();
    };
    let Some(arguments) = value.by_field("arguments") else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    super::each_node(arguments, |child| {
        if child.kind() != "identifier" || is_a_name(child) {
            return;
        }
        let text = syntax::text(source, child);
        if text.starts_with(|first: char| first.is_lowercase() || first == '_') {
            names.push(text.to_owned());
        }
    });
    names.sort();
    names.dedup();
    names
}

/// Visit state for the evidence pass.
struct EvidenceWalker<'a> {
    /// The text the tree was parsed from.
    source: &'a str,
    /// Lines of `return Err(..)` expressions a log has already run before.
    evidenced: BTreeSet<usize>,
    /// The source text of every log statement that certainly runs before the
    /// node currently being visited, on every path that reaches it, with whether
    /// it carries context. A stack rather than a flag, because the evidence a
    /// return needs is the log's text: the log has to say something about the
    /// value the error carries (#699).
    logs: Vec<SeenLog<'a>>,
}

impl<'a> EvidenceWalker<'a> {
    /// Visit a node and everything under it.
    fn walk(&mut self, node: Node<'a>) {
        match node.kind() {
            "block" => self.walk_block(node),
            "return_expression" => {
                if let Some(line) = err_return_line(self.source, node)
                    && self.observed(node)
                {
                    self.evidenced.insert(line);
                }
                self.walk_children(node);
            }
            // A function body starts with no evidence: an enclosing block's log
            // did not run inside a function that may be called from anywhere. A
            // trait's default method is no exception in the source it is written
            // in, but the reference walk never reset for one, and neither does
            // this.
            "function_item" if !syntax::is_in_trait(node) => {
                let saved = std::mem::take(&mut self.logs);
                self.walk_children(node);
                self.logs = saved;
            }
            _ => self.walk_children(node),
        }
    }

    /// Visit every named child of a node.
    fn walk_children(&mut self, node: Node<'a>) {
        for child in syntax::named_kids(node) {
            self.walk(child);
        }
    }

    /// Whether a log already on this path carries what this return emits.
    ///
    /// An error with no carried value has nothing for a log to name, so the log
    /// is the whole signal. An error that carries values is observed when either
    /// a log names one of them, or a log carries context of its own beyond a bare
    /// message: `debug!(index, "..")` says which frame failed, while
    /// `eprintln!("zero")` says nothing about the failure it precedes.
    fn observed(&self, node: Node<'_>) -> bool {
        if self.logs.is_empty() {
            return false;
        }
        let carried = carried(self.source, node);
        if carried.is_empty() {
            return true;
        }
        self.logs.iter().any(|log| {
            log.carries_context || carried.iter().any(|name| mentions(log.text, name.as_str()))
        })
    }

    /// A block inherits the evidence of the statement that encloses it and grows
    /// it as its own statements are read. A log statement that has already run
    /// stays run for every later statement, including one reached through a later
    /// `if`. A log inside a branch does not leak out, because the branch is a
    /// nested block whose entries are dropped when it ends.
    fn walk_block(&mut self, block: Node<'a>) {
        let depth = self.logs.len();
        for statement in syntax::named_kids(block) {
            if is_not_statement(statement) {
                continue;
            }
            if let Some(log) = seen_log(self.source, statement) {
                self.logs.push(log);
                continue;
            }
            self.walk(statement);
            self.logs.truncate(depth);
        }
        self.logs.truncate(depth);
    }
}

/// Walker state for the reporting pass.
struct ErrReturnWalker<'a> {
    /// The text the tree was parsed from.
    source: &'a str,
    /// Findings accumulated during the walk, in visit order.
    hits: &'a mut Vec<Hit>,
    /// One entry per enclosing function, `true` when that function is test-only.
    /// The innermost entry decides whether findings are suppressed; a stack is
    /// needed because an inner non-test fn inside a test module is still test
    /// code, and an inner test helper inside a production fn is still exempt.
    test_scopes: Vec<bool>,
    /// Lines whose `return Err(..)` a log statement runs before.
    evidenced: &'a BTreeSet<usize>,
}

impl ErrReturnWalker<'_> {
    /// Visit a node and everything under it.
    fn walk(&mut self, node: Node<'_>) {
        match node.kind() {
            "function_item" if !syntax::is_in_trait(node) => {
                self.open_scope(cfg::is_test_or_helper(self.source, node));
                self.walk_children(node);
                self.test_scopes.pop();
            }
            // A module under a `cfg` that cannot hold outside `test` is test
            // code for every item it holds: the exemption is a scope, not a
            // spelling, and the module doc says so (#699).
            "mod_item" => {
                self.open_scope(cfg::is_test_item(self.source, node));
                self.walk_children(node);
                self.test_scopes.pop();
            }
            "return_expression" => {
                self.check(node);
                self.walk_children(node);
            }
            _ => self.walk_children(node),
        }
    }

    /// Enter a scope that is test code when it says so itself or when any
    /// enclosing scope already is: an inner function inside a test module is
    /// still test code.
    fn open_scope(&mut self, own: bool) {
        let inherited = self.test_scopes.last().copied() == Some(true);
        self.test_scopes.push(inherited || own);
    }

    /// Visit every named child of a node.
    fn walk_children(&mut self, node: Node<'_>) {
        for child in syntax::named_kids(node) {
            self.walk(child);
        }
    }

    /// Records a finding when a `return Err(..)` in production code has no log
    /// emission that runs before it. A return outside any function (a `const`
    /// initializer, say) is not in a disabled scope.
    fn check(&mut self, node: Node<'_>) {
        if self.test_scopes.last().copied() == Some(true) {
            return;
        }
        let Some(line) = err_return_line(self.source, node) else {
            return;
        };
        if !self.evidenced.contains(&line) {
            self.hits
                .push(Hit::new("unlogged-err-return", line, SNIPPET.to_owned()));
        }
    }
}
