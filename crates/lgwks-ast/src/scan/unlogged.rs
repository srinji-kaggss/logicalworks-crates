//! unlogged-err-return: a `return Err(..)` that no log emission runs before.
//!
//! The evidence is a log statement that executes unconditionally on the way
//! into the return: earlier in the same block, or in an enclosing block before
//! the statement that holds the return. A comment naming a log macro emits
//! nothing (comments are not statements), a string spelling one is a literal,
//! not a macro, and a log on a branch the return is not on does not seal the
//! block after it. Line numbers identify a return once its evidence is decided
//! from structure, so reflowing a program cannot change its verdict.
//!
//! A return is exempt inside a function marked `test` or `test_*`, the
//! innermost enclosing one deciding.

use std::collections::HashSet;

use super::{
    ExprPath, Hit, Node, Owner, block_statements, first_named, is_test_attr, item_attributes,
    line_of, owner_of, statement_expression,
};

/// Macro and path spellings that count as a log emission, matched against a
/// macro statement's rendered path (`tracing::warn!(`).
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

/// The finding's fixed text: CI printers key remediation off the rule name, and
/// a stable snippet keeps that mapping total.
const SNIPPET: &str = "return Err(...) with no log emission that runs before it — the caller sees only a value, not a signal. Log the error with enough context to diagnose it. Wording, line wrapping, and formatting do not matter: the check reads parsed statements, never physical lines.";

/// One `return Err(..)` as the walk met it.
struct ErrReturn {
    /// The line of its `return`.
    line: usize,
    /// A log statement certainly ran before it.
    sealed: bool,
    /// The innermost enclosing function is a test or test helper.
    exempt: bool,
}

/// Every unlogged-err-return finding under `root`.
///
/// Evidence is keyed by line, as the `syn` scan keyed it: a sealed return
/// vouches for every return on its line.
pub(super) fn detect(root: &Node<'_>, hits: &mut Vec<Hit>) {
    let mut returns = Vec::new();
    walk(root, false, false, &mut returns);
    let evidenced: HashSet<usize> = returns
        .iter()
        .filter(|found| found.sealed)
        .map(|found| found.line)
        .collect();
    for found in returns {
        if !found.exempt && !evidenced.contains(&found.line) {
            hits.push(Hit {
                rule: "unlogged-err-return",
                line: found.line,
                snippet: SNIPPET.to_owned(),
            });
        }
    }
}

/// Collect every `return Err(..)` under `node` with whether a log has run
/// before it (`sealed`) and whether its function is exempt.
///
/// A free function or `impl` method starts unsealed: a log in an enclosing
/// block did not run inside a function callable from anywhere. A trait method
/// keeps the enclosing state and scope, as the `syn` walk did. A closure body
/// inherits both.
fn walk(node: &Node<'_>, sealed: bool, exempt: bool, returns: &mut Vec<ErrReturn>) {
    match &*node.kind() {
        "function_item" if owner_of(node) != Owner::Trait => {
            let exempt = is_test_attr(&item_attributes(node));
            for child in node.children() {
                walk(&child, false, exempt, returns);
            }
        }
        "block" => {
            // A log that has run stays run for every later statement of the
            // block; a log inside a branch is a statement of the branch's own
            // block and does not leak out.
            let mut log_seen = sealed;
            for statement in block_statements(node) {
                if is_log(&statement) {
                    log_seen = true;
                    continue;
                }
                walk(&statement, log_seen, exempt, returns);
            }
        }
        "return_expression" => {
            if first_named(node).is_some_and(|value| is_err_construction(&value)) {
                returns.push(ErrReturn {
                    line: line_of(node),
                    sealed,
                    exempt,
                });
            }
            for child in node.children() {
                walk(&child, sealed, exempt, returns);
            }
        }
        _ => {
            for child in node.children() {
                walk(&child, sealed, exempt, returns);
            }
        }
    }
}

/// True for a call whose callee path ends in `Err`.
fn is_err_construction(value: &Node<'_>) -> bool {
    value.kind() == "call_expression"
        && value
            .field("function")
            .and_then(|callee| ExprPath::read(&callee))
            .is_some_and(|path| path.last() == Some("Err"))
}

/// True when a statement is itself a log macro, with or without its `;`.
fn is_log(statement: &Node<'_>) -> bool {
    let Some(invocation) =
        statement_expression(statement).filter(|inner| inner.kind() == "macro_invocation")
    else {
        return false;
    };
    let Some(path) = invocation
        .field("macro")
        .and_then(|name| ExprPath::read(&name))
    else {
        return false;
    };
    let rendered = format!("{}!(", path.render());
    LOG_MARKERS.iter().any(|marker| rendered.contains(marker))
}
