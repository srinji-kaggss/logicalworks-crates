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
//! What a log names is read from its own arguments: an identifier among them
//! that is not a label (`error` in `error = %e`), and a capture inside its format
//! string (`e` in `"{e}"`). Its macro path (`tracing::error!`) and the prose of
//! its message name nothing, so `error!("request failed")` does not excuse a
//! `return Err(error)`. A literal beside the message (`"failed {}", 0`) is not
//! context either.
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
use super::syntax::{self, Path, Tok};

/// Macro names that emit a log line, reached through any path: `warn!`,
/// `tracing::warn!`, `lgwks_std::trace::warn!`. A name absent from this list is a
/// macro the detector does not treat as a signal, and a longer name that merely
/// ends in one (`my_info!`) is a different macro.
const LOG_MACROS: &[&str] = &[
    "info", "warn", "error", "debug", "trace", "event", "eprintln", "eprint",
];

/// Crates every macro of which is a log emission (`tracing::span!` aside, which
/// emits nothing, none of their macros is anything else). Matched as the first
/// whole segment of the macro's path, so `catalog::emit!` is not under `log::`.
const LOG_CRATES: &[&str] = &["tracing", "log"];

/// Words that are keywords or literals in a log's arguments, never a value the
/// log names, though the token reader spells them as identifiers.
const NOT_VALUES: &[&str] = &[
    "true", "false", "as", "mut", "ref", "move", "if", "else", "match", "in", "for", "while",
    "loop", "let", "return", "unsafe", "dyn", "async", "await", "crate", "super", "const",
    "static", "fn",
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

/// One log statement that has run on the path: the values it names, and whether
/// it carries any context beyond a bare format string.
struct SeenLog {
    /// Every value the log names: an identifier among its arguments that is not
    /// a label, and each capture in its format string. Never its macro path and
    /// never the prose of its message.
    names: Vec<String>,
    /// True when the macro passes a value of its own beside its format string:
    /// `debug!(index, "..")` and `error!(%cause, "..")` do, `eprintln!("zero")`,
    /// `eprintln!("saw {other}")` and `eprintln!("failed {}", 0)` do not.
    carries_context: bool,
}

/// Whether a macro path is a log emission: a log macro by name, or any macro of
/// a logging crate.
fn emits_log(path: &Path) -> bool {
    let by_name = path.last().is_some_and(|name| LOG_MACROS.contains(&name));
    let by_crate = path.segments.len() > 1
        && path
            .segments
            .first()
            .is_some_and(|first| LOG_CRATES.contains(&first.as_str()));
    by_name || by_crate
}

/// A block statement that is itself a log emission, read for what it names: a
/// macro invocation whose path [`emits_log`], written as a statement or as an
/// expression statement.
fn seen_log(source: &str, statement: Node<'_>) -> Option<SeenLog> {
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
    if !emits_log(&path) {
        return None;
    }
    let reading = match syntax::macro_tokens(invocation) {
        Some(tree) => read_arguments(&syntax::tokens(source, tree)),
        None => Reading::default(),
    };
    Some(SeenLog {
        names: reading.names,
        carries_context: reading.context,
    })
}

/// What the arguments of one log macro name.
#[derive(Default)]
struct Reading {
    /// Every value named, in source order, with repeats.
    names: Vec<String>,
    /// Whether an argument other than a format string names a value.
    context: bool,
}

/// Read a log's arguments, split at their top-level commas. The macro path is
/// not among them, and an argument's label (`error` in `error = %e`) is not a
/// value it names.
fn read_arguments(tokens: &[Tok]) -> Reading {
    let mut reading = Reading::default();
    for argument in tokens.split(|token| token.punct() == Some(",")) {
        collect(after_label(argument), &mut reading);
    }
    reading
}

/// The tokens of an argument after its `label =` or `label:` (`error = %e`,
/// `error.message = %e`, `target: "x"`), which are all of it when it has none.
fn after_label(argument: &[Tok]) -> &[Tok] {
    let mut expect_name = true;
    for (at, token) in argument.iter().enumerate() {
        if expect_name {
            if token.ident().is_none() {
                break;
            }
            expect_name = false;
            continue;
        }
        match token.punct() {
            Some(".") => expect_name = true,
            Some("=" | ":") => {
                return match argument.get(at.saturating_add(1)..) {
                    Some(value) => value,
                    None => &[],
                };
            }
            _ => break,
        }
    }
    argument
}

/// Gather what a run of tokens names: each identifier that is a value, and each
/// capture in a literal's format string, descending into groups.
fn collect(tokens: &[Tok], reading: &mut Reading) {
    for (at, token) in tokens.iter().enumerate() {
        if let Some(name) = token.ident() {
            if is_a_value(tokens, at, name) {
                reading.names.push(name.to_owned());
                reading.context = true;
            }
        } else if let Some(text) = token.literal() {
            captures(text, &mut reading.names);
        } else if let Some(inner) = token.group() {
            collect(inner, reading);
        }
    }
}

/// Whether the identifier at `at` is a value the log names: a binding or `self`,
/// not a keyword, a path segment (`Level::ERROR`), a field or method after a dot,
/// a macro (`format!`), a label before `=`, or a callee. A `(..)` or `{..}` after
/// an identifier makes it a callee; a `[..]` is an index, so `items` in
/// `items[0]` is a value.
fn is_a_value(tokens: &[Tok], at: usize, name: &str) -> bool {
    let binding =
        name == "self" || name.starts_with(|first: char| first.is_lowercase() || first == '_');
    if !binding || NOT_VALUES.contains(&name) {
        return false;
    }
    let before = at
        .checked_sub(1)
        .and_then(|index| tokens.get(index))
        .and_then(Tok::punct);
    if matches!(before, Some("." | "::")) {
        return false;
    }
    match tokens.get(at.saturating_add(1)) {
        Some(next) => !next.is_call() && !matches!(next.punct(), Some("::" | "!" | "=")),
        None => true,
    }
}

/// Append the names a format string captures: `e` in `{e}` and `{e:?}`, and a
/// width or precision argument (`w` in `{:w$}`). Doubled braces are text, a
/// positional field (`{}`, `{0}`) names nothing, and a brace never closed is not
/// a field.
fn captures(literal: &str, names: &mut Vec<String>) {
    let mut chars = literal.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '{' {
            continue;
        }
        if chars.peek() == Some(&'{') {
            chars.next();
            continue;
        }
        let mut field = String::new();
        let mut closed = false;
        for inner in chars.by_ref() {
            if inner == '}' {
                closed = true;
                break;
            }
            field.push(inner);
        }
        if !closed {
            return;
        }
        let (argument, spec) = match field.split_once(':') {
            Some(pair) => pair,
            None => (field.as_str(), ""),
        };
        push_identifier(argument.trim(), names);
        let mut parts = spec.split('$').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_some() {
                push_identifier(trailing_word(part), names);
            }
        }
    }
}

/// The identifier characters that end `text`.
fn trailing_word(text: &str) -> &str {
    let mut start = text.len();
    for (index, character) in text.char_indices().rev() {
        if !(character.is_alphanumeric() || character == '_') {
            break;
        }
        start = index;
    }
    let Some(word) = text.get(start..) else {
        return "";
    };
    word
}

/// Append `text` when it spells an identifier (not a number, not empty).
fn push_identifier(text: &str, names: &mut Vec<String>) {
    let mut characters = text.chars();
    let starts = characters
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_');
    if starts && characters.all(|rest| rest.is_alphanumeric() || rest == '_') {
        names.push(text.to_owned());
    }
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
    /// Every log statement that certainly runs before the node currently being
    /// visited, on every path that reaches it, with the values it names and
    /// whether it carries context. A stack rather than a flag, because the
    /// evidence a return needs is what the log says: it has to say something
    /// about the value the error carries (#699).
    logs: Vec<SeenLog>,
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
        self.logs
            .iter()
            .any(|log| log.carries_context || carried.iter().any(|name| log.names.contains(name)))
    }

    /// A block inherits the evidence of the statement that encloses it and grows
    /// it as its own statements are read. A log statement that has already run
    /// stays run for every later statement of the block, whatever sits between
    /// it and the return. A log inside a branch does not leak out, because the
    /// branch is a nested block whose entries are dropped when it ends.
    fn walk_block(&mut self, block: Node<'a>) {
        let depth = self.logs.len();
        for statement in syntax::named_kids(block) {
            if is_not_statement(statement) {
                continue;
            }
            match seen_log(self.source, statement) {
                Some(log) => self.logs.push(log),
                None => self.walk(statement),
            }
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
