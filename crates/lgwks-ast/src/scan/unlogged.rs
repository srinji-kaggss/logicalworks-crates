//! Detector unlogged-err-return: a `return Err(..)` in production code that no
//! log statement runs before.
//!
//! The evidence is a log *statement* that executes unconditionally on the way
//! into the return: either earlier in the same block, or in an enclosing block
//! before the statement that encloses the return. Three things are deliberately
//! not evidence:
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
        sealed: false,
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

/// True when a block statement is itself a log emission: a macro invocation
/// whose path spells one of [`LOG_MARKERS`], written as a statement or as an
/// expression statement.
fn statement_is_log(source: &str, statement: Node<'_>) -> bool {
    let invocation = match statement.kind() {
        "macro_invocation" => statement,
        "expression_statement" => match syntax::named_kids(statement).as_slice() {
            &[only] if only.kind() == "macro_invocation" => only,
            _ => return false,
        },
        _ => return false,
    };
    let Some(path) = invocation
        .by_field("macro")
        .and_then(|name| Path::of(source, name))
    else {
        return false;
    };
    let rendered = format!("{}!(", path.render());
    LOG_MARKERS.iter().any(|marker| rendered.contains(marker))
}

/// Visit state for the evidence pass.
struct EvidenceWalker<'a> {
    /// The text the tree was parsed from.
    source: &'a str,
    /// Lines of `return Err(..)` expressions a log has already run before.
    evidenced: BTreeSet<usize>,
    /// True when a log statement certainly runs before the node currently being
    /// visited, on every path that reaches it.
    sealed: bool,
}

impl EvidenceWalker<'_> {
    /// Visit a node and everything under it.
    fn walk(&mut self, node: Node<'_>) {
        match node.kind() {
            "block" => self.walk_block(node),
            "return_expression" => {
                if self.sealed
                    && let Some(line) = err_return_line(self.source, node)
                {
                    self.evidenced.insert(line);
                }
                self.walk_children(node);
            }
            // A function body starts unsealed: an enclosing block's log did not
            // run inside a function that may be called from anywhere. A trait's
            // default method is no exception in the source it is written in, but
            // the reference walk never reset for one, and neither does this.
            "function_item" if !syntax::is_in_trait(node) => {
                let saved = self.sealed;
                self.sealed = false;
                self.walk_children(node);
                self.sealed = saved;
            }
            _ => self.walk_children(node),
        }
    }

    /// Visit every named child of a node.
    fn walk_children(&mut self, node: Node<'_>) {
        for child in syntax::named_kids(node) {
            self.walk(child);
        }
    }

    /// A block inherits the seal of the statement that encloses it and raises it
    /// as its own statements are read. `sealed` never falls inside one block: a
    /// log statement that has already run stays run for every later statement,
    /// including one reached through a later `if`. A log inside a branch does not
    /// leak out, because the branch is a nested block with its own frame.
    fn walk_block(&mut self, block: Node<'_>) {
        let inherited = self.sealed;
        let mut log_seen = inherited;
        for statement in syntax::named_kids(block) {
            if is_not_statement(statement) {
                continue;
            }
            if statement_is_log(self.source, statement) {
                log_seen = true;
                continue;
            }
            self.sealed = log_seen;
            self.walk(statement);
            self.sealed = inherited;
        }
        self.sealed = inherited;
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
                self.test_scopes
                    .push(cfg::is_test_or_helper(self.source, node));
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
