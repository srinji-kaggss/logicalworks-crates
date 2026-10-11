//! ERROR-SWALLOW: a `Result` whose error is dropped unread.
//!
//! Four shapes: `.ok();` as a statement, `let _ = <fallible>;`,
//! `.unwrap_or_default()` on a `Result`, and a one-parameter closure to
//! `unwrap_or_else`, `map_err` or `or_else` that never reads the error it is
//! handed. The rule's name is a claim that an error existed, so the scanner
//! first resolves why it believes a fallible value is in hand, from evidence
//! the file states ([`Index`]). A receiver resolved to something that is not a
//! `Result` is never reported; one that does not resolve is reported only
//! where the call's shape cannot be an `Option` operation, and the evidence
//! then says the shape is all there is. Test functions and test modules are
//! skipped whole: swallowing an error is how a test asserts on one.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use super::binding::{bound_name, observes};
use super::{
    ExprPath, Hit, MethodCall, Node, Owner, block_statements, call_arguments, first_named,
    is_comment, is_test_fn, item_attributes, line_of, owner_of,
};

/// The rule name.
const RULE: &str = "ERROR-SWALLOW";

/// The evidence when a call's shape is the only proof it is not an `Option`.
const SHAPE_ONLY: &str = "the call shape alone, which `Option` cannot have";

/// Standard-library routines that return `Result`, by rendered path: the
/// stand-in for type information one file does not carry. Finite and checked
/// by hand, and only routines called outright: in `Command::new(..).output()`
/// the receiver is `Command::new(..)`, so a `Command::output` entry would be
/// dead.
const STD_FALLIBLE_ROUTINES: &[&str] = &[
    "File::create",
    "File::open",
    "String::from_utf8",
    "std::env::current_dir",
    "std::env::current_exe",
    "std::env::remove_var",
    "std::env::set_current_dir",
    "std::env::set_var",
    "std::env::var",
    "std::env::var_os",
    "std::fs::File::create",
    "std::fs::File::open",
    "std::fs::canonicalize",
    "std::fs::copy",
    "std::fs::create_dir",
    "std::fs::create_dir_all",
    "std::fs::metadata",
    "std::fs::read",
    "std::fs::read_dir",
    "std::fs::read_link",
    "std::fs::read_to_string",
    "std::fs::remove_dir",
    "std::fs::remove_dir_all",
    "std::fs::remove_file",
    "std::fs::rename",
    "std::fs::symlink_metadata",
    "std::fs::write",
    "std::net::TcpListener::bind",
    "std::net::TcpStream::connect",
    "std::net::UdpSocket::bind",
    "std::str::from_utf8",
];

/// Alias hops a written type may take before resolution stops, which makes
/// `type A = B; type B = A;` terminate without a visited set.
const MAX_ALIAS_DEPTH: u8 = 8;

/// Every ERROR-SWALLOW finding under `root`.
pub(super) fn detect(root: &Node<'_>, hits: &mut Vec<Hit>) {
    let index = Index::of(root);
    let mut walk = Walk {
        index: &index,
        hits,
    };
    walk.visit(root);
}

/// What the file proves about the value an expression yields.
enum Fallibility {
    /// It carries an error, for the reason given.
    Result(String),
    /// It resolved to a value that carries no error: evidence against.
    NotFallible,
    /// It did not resolve: no evidence either way.
    Unresolved,
}

/// A return type as a signature wrote it.
enum KnownReturn<'tree> {
    /// The signature writes this type.
    Written(Node<'tree>),
    /// The signature writes none: `()`.
    Unit,
    /// Two free functions here share the name, so a call through it does not
    /// resolve to one signature.
    Ambiguous,
}

/// The fallibility witnesses one file states about itself. The scan reads one
/// file, so a type declared in another module is unresolved, not guessed.
#[derive(Default)]
struct Index<'tree> {
    /// Free functions by name. Methods are not recorded: a receiver reached as
    /// `x.f()` resolves by its expression, and recording `fn open` from an
    /// `impl` would only make a free `fn open` ambiguous.
    functions: HashMap<String, KnownReturn<'tree>>,
    /// `type X = ..;` aliases; an `impl`'s associated types are not aliases.
    aliases: HashMap<String, Node<'tree>>,
    /// Annotated single-identifier `let name: Type` bindings, minus any name
    /// annotated more than once.
    bindings: HashMap<String, Node<'tree>>,
}

impl<'tree> Index<'tree> {
    /// Collect every witness under `root`, test code included.
    fn of(root: &Node<'tree>) -> Self {
        let mut index = Self::default();
        let mut seen = HashSet::new();
        let mut ambiguous = HashSet::new();
        for node in root.dfs() {
            match &*node.kind() {
                "function_item" if owner_of(&node) == Owner::Free => {
                    let Some(name) = node.field("name") else {
                        continue;
                    };
                    let known = match node.field("return_type") {
                        Some(written) => KnownReturn::Written(written),
                        None => KnownReturn::Unit,
                    };
                    match index.functions.entry(name.text().into_owned()) {
                        Entry::Vacant(slot) => {
                            slot.insert(known);
                        }
                        Entry::Occupied(mut slot) => {
                            slot.insert(KnownReturn::Ambiguous);
                        }
                    }
                }
                "type_item" if owner_of(&node) == Owner::Free => {
                    if let (Some(name), Some(aliased)) = (node.field("name"), node.field("type")) {
                        index.aliases.insert(name.text().into_owned(), aliased);
                    }
                }
                "let_declaration" => {
                    let (Some(pattern), Some(written)) =
                        (node.field("pattern"), node.field("type"))
                    else {
                        continue;
                    };
                    let Some(name) = bound_name(&pattern) else {
                        continue;
                    };
                    if ambiguous.contains(&name) {
                        continue;
                    }
                    if seen.insert(name.clone()) {
                        index.bindings.insert(name, written);
                    } else {
                        index.bindings.remove(&name);
                        ambiguous.insert(name);
                    }
                }
                _ => {}
            }
        }
        index
    }

    /// Resolve a receiver, or a value bound to `_`: only a plain call or a
    /// named binding states anything. A method call's result is already the
    /// `T`, and the method call itself is judged on its own.
    fn classify(&self, value: &Node<'_>) -> Fallibility {
        if value.kind() == "call_expression" {
            return match value.field("function") {
                Some(callee) => self.classify_call(&callee),
                None => Fallibility::Unresolved,
            };
        }
        match ExprPath::read(value) {
            Some(path) if !path.qualified => self.classify_binding(&path),
            _ => Fallibility::Unresolved,
        }
    }

    /// Resolve a call target: a listed routine, a bare name, or a `Self::` or
    /// `self::` name. A path through a module does not resolve; crediting a
    /// different function that shares the last segment would be a guess.
    fn classify_call(&self, callee: &Node<'_>) -> Fallibility {
        let Some(path) = ExprPath::read(callee).filter(|path| !path.qualified) else {
            return Fallibility::Unresolved;
        };
        let rendered = path.render();
        if STD_FALLIBLE_ROUTINES.contains(&rendered.as_str()) {
            return Fallibility::Result(format!("the `Result` returned by `{rendered}`"));
        }
        if path.leading_colon {
            return Fallibility::Unresolved;
        }
        let name = match *path.segments.as_slice() {
            [ref only] => only,
            [ref first, ref second] if first == "Self" || first == "self" => second,
            _ => return Fallibility::Unresolved,
        };
        let Some(known) = self.functions.get(name) else {
            return Fallibility::Unresolved;
        };
        match *known {
            KnownReturn::Written(ref written) if self.is_result(written, 0) => {
                Fallibility::Result(format!("the `Result` returned by fn `{name}`"))
            }
            KnownReturn::Written(_) | KnownReturn::Unit => Fallibility::NotFallible,
            KnownReturn::Ambiguous => Fallibility::Unresolved,
        }
    }

    /// Resolve a plain one-segment path against the annotated bindings.
    fn classify_binding(&self, path: &ExprPath) -> Fallibility {
        let name = match *path.segments.as_slice() {
            [ref only] if !path.leading_colon => only,
            _ => return Fallibility::Unresolved,
        };
        match self.bindings.get(name) {
            Some(written) if self.is_result(written, 0) => {
                Fallibility::Result(format!("the `Result` bound by `let {name}: ..`"))
            }
            Some(_) => Fallibility::NotFallible,
            None => Fallibility::Unresolved,
        }
    }

    /// True when a written type is a `Result`, through this file's aliases,
    /// references and parentheses.
    fn is_result(&self, written: &Node<'_>, depth: u8) -> bool {
        if depth >= MAX_ALIAS_DEPTH {
            return false;
        }
        let next = depth.saturating_add(1);
        match &*written.kind() {
            "reference_type" => written
                .field("type")
                .is_some_and(|inner| self.is_result(&inner, next)),
            // `(T)` is a one-element tuple type with no comma: `syn`'s paren.
            "tuple_type" => {
                let mut elements = written
                    .children()
                    .filter(|child| child.is_named() && !is_comment(child));
                let comma = written.children().any(|child| child.kind() == ",");
                match (elements.next(), elements.next(), comma) {
                    (Some(only), None, false) => self.is_result(&only, next),
                    _ => false,
                }
            }
            _ => match last_type_segment(written) {
                Some(name) if name == "Result" => true,
                Some(name) => self
                    .aliases
                    .get(&name)
                    .is_some_and(|aliased| self.is_result(aliased, next)),
                None => false,
            },
        }
    }
}

/// The last segment of a type path, generic arguments ignored; `None` for a
/// type that is not a path, or one rooted in `<T as Trait>::`.
fn last_type_segment(written: &Node<'_>) -> Option<String> {
    match &*written.kind() {
        "type_identifier" => Some(written.text().into_owned()),
        "scoped_type_identifier" => {
            let qualified = written
                .field("path")
                .is_some_and(|prefix| prefix.kind() == "bracketed_type");
            if qualified {
                return None;
            }
            written.field("name").map(|name| name.text().into_owned())
        }
        "generic_type" => last_type_segment(&written.field("type")?),
        _ => None,
    }
}

/// The swallow walk.
struct Walk<'index, 'tree, 'hits> {
    /// The file's witnesses.
    index: &'index Index<'tree>,
    /// Findings, in visit order.
    hits: &'hits mut Vec<Hit>,
}

impl Walk<'_, '_, '_> {
    /// Visit `node`, skipping test functions and test modules whole.
    fn visit(&mut self, node: &Node<'_>) {
        let kind = node.kind();
        let test_scope = match &*kind {
            "function_item" => owner_of(node) != Owner::Trait && is_test_fn(&item_attributes(node)),
            "mod_item" => is_test_fn(&item_attributes(node)),
            _ => false,
        };
        if test_scope {
            return;
        }
        if let Some(call) = MethodCall::of(node) {
            self.method_call(&call);
        }
        if kind == "block" {
            for statement in block_statements(node) {
                self.discarded_ok(&statement);
                if !self.wildcard_binding(&statement) {
                    self.visit(&statement);
                }
            }
            return;
        }
        for child in node.children() {
            self.visit(&child);
        }
    }

    /// The evidence for a finding, or `None` to stay silent: a resolved
    /// witness always reports, a non-`Result` never does, and an unresolved
    /// receiver reports only under a shape `Option` cannot have.
    fn evidence(&self, receiver: &Node<'_>, option_impossible: bool) -> Option<String> {
        match self.index.classify(receiver) {
            Fallibility::Result(witness) => Some(witness),
            Fallibility::NotFallible => None,
            Fallibility::Unresolved if option_impossible => Some(SHAPE_ONLY.to_owned()),
            Fallibility::Unresolved => None,
        }
    }

    /// `.unwrap_or_default()` on a resolved `Result`, and a fallback closure
    /// that drops the error it is handed.
    fn method_call(&mut self, call: &MethodCall<'_>) {
        let line = line_of(&call.method);
        // `Option::unwrap_or_default` has this exact spelling, so the
        // receiver has to resolve before this is a lost error.
        if call.method.text() == "unwrap_or_default"
            && let Some(evidence) = self.evidence(&call.receiver, false)
        {
            self.hits.push(Hit {
                rule: RULE,
                line,
                snippet: format!(".unwrap_or_default() silently drops error from {evidence}"),
            });
        }
        if let Some(method) = discarded_error_closure(call)
            && let Some(evidence) = self.evidence(&call.receiver, true)
        {
            self.hits.push(Hit {
                rule: RULE,
                line,
                snippet: format!(
                    ".{method}(|..| ..) never reads the error it was handed; evidence: {evidence}"
                ),
            });
        }
    }

    /// `expr.ok();` as a statement: the error becomes `None` and is dropped.
    /// A tail `.ok()` is a value, not a discard. `Option` has no `ok`, so the
    /// spelling is the shape evidence.
    fn discarded_ok(&mut self, statement: &Node<'_>) {
        if statement.kind() != "expression_statement" {
            return;
        }
        let Some(call) = first_named(statement).and_then(|inner| MethodCall::of(&inner)) else {
            return;
        };
        if call.method.text() != "ok" {
            return;
        }
        if let Some(evidence) = self.evidence(&call.receiver, true) {
            self.hits.push(Hit {
                rule: RULE,
                line: line_of(&call.method),
                snippet: format!(".ok(); discards {evidence} without reading it"),
            });
        }
    }

    /// `let _ = <expr>;`, reporting whether it matched. A match visits its
    /// value here, so the caller must not visit the statement again; a
    /// `let _: T = ..` is a typed pattern in `syn`, not a wildcard.
    fn wildcard_binding(&mut self, statement: &Node<'_>) -> bool {
        if statement.kind() != "let_declaration"
            || statement.field("type").is_some()
            || !statement
                .field("pattern")
                .is_some_and(|pattern| pattern.kind() == "_")
        {
            return false;
        }
        let Some(value) = statement.field("value") else {
            return false;
        };
        if let Fallibility::Result(witness) = self.index.classify(&value) {
            self.hits.push(Hit {
                rule: RULE,
                line: line_of(statement),
                snippet: format!("let _ = ...; binds {witness} to `_` without inspecting it"),
            });
        }
        self.visit(&value);
        true
    }
}

/// The method's name when a call passes a one-parameter closure that drops
/// the error it is handed.
///
/// The arity is evidence about `Option`, not about the receiver: `Option`'s
/// `unwrap_or_else` and `or_else` take a zero-parameter closure and it has no
/// `map_err`, so one closure parameter rules `Option` out. A user type may
/// still define these names, which is why this is only the shape.
fn discarded_error_closure(call: &MethodCall<'_>) -> Option<&'static str> {
    let method = match &*call.method.text() {
        "unwrap_or_else" => "unwrap_or_else",
        "map_err" => "map_err",
        "or_else" => "or_else",
        _ => return None,
    };
    let arguments = call_arguments(&call.arguments);
    let closure = arguments
        .first()
        .filter(|first| first.kind() == "closure_expression")?;
    let parameters: Vec<Node<'_>> = closure
        .field("parameters")?
        .children()
        .filter(|parameter| {
            (parameter.is_named() && !is_comment(parameter)) || parameter.kind() == "_"
        })
        .collect();
    let [ref parameter] = *parameters.as_slice() else {
        return None;
    };
    let dropped = match &*parameter.kind() {
        "_" => true,
        _ => match bound_name(parameter) {
            Some(name) => closure
                .field("body")
                .is_none_or(|body| !observes(&body, &name)),
            // A typed or destructuring parameter binds through a shape the
            // walk cannot follow, so it refuses to accuse.
            None => false,
        },
    };
    dropped.then_some(method)
}
