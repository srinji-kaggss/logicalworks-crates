//! Detector ERROR-SWALLOW: result-typed expressions whose error is silently
//! discarded: `.ok();` in statement position, `let _ = <fallible>;`,
//! `.unwrap_or_default()` on a `Result`, or a one-argument closure to
//! `unwrap_or_else` / `map_err` / `or_else` that never observes the error it was
//! handed.
//!
//! The rule's name is a claim (an error existed and was dropped), so the scanner
//! resolves *why it believes a fallible value is in hand* before it reports one,
//! from evidence stated in the file being scanned ([`FallibilityIndex`]). A
//! receiver that resolves to something which is not a `Result` is never
//! reported. A receiver that does not resolve at all is reported only where the
//! call's shape cannot be an `Option` operation, and the evidence line then says
//! the shape is all there is. Nothing about the verdict depends on a method's
//! arity alone, which is not a type and cannot be one.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::hash_map::Entry;

use tree_sitter::Node;

use super::syntax::Fielded;
use super::syntax::{self, Path};
use super::{Hit, binding, cfg, each_node};

/// The evidence line used when a call's shape is the only thing proving it
/// cannot be an `Option` operation.
const SHAPE_ONLY: &str = "the call shape alone, which `Option` cannot have";

/// Standard-library routines that return `Result`, by rendered path.
///
/// This table is the scanner's substitute for the type information a single file
/// does not carry: the parser reads syntax, and these are the call forms whose
/// fallibility is a fact about the standard library rather than an inference
/// from a name. It is deliberately finite and hand-checked, and it holds only
/// routines reached by calling them outright: an entry like `Command::output`
/// would be dead, because in `Command::new(..).output()` the receiver expression
/// is the `Command::new(..)` call, not `output`.
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

/// How many alias hops a written type may take before resolution gives up.
///
/// `type A = B; type B = A;` is not a `Result` and is not resolvable; this bound
/// is what makes the recursion total without carrying a visited set.
const MAX_ALIAS_DEPTH: u8 = 8;

/// Runs the error-swallow detector over a whole file.
pub(super) fn detect(source: &str, root: Node<'_>, hits: &mut Vec<Hit>) {
    let index = FallibilityIndex::of(source, root);
    let mut walker = SwallowWalker {
        source,
        hits,
        index: &index,
    };
    walker.walk(root);
}

/// What this file proves about the value a receiver expression yields.
///
/// Every witness is built from syntax in the scanned file, so a positive answer
/// is a fact about the file rather than a guess from a spelling. The three
/// answers are distinct for a reason: [`Self::NotFallible`] is evidence *against*
/// an error, [`Self::Unresolved`] is the absence of evidence, and only the first
/// licenses silence while the second licenses the shape-only report.
enum Fallibility {
    /// The receiver carries an error; the witness says why the scanner says so.
    Result(Witness),
    /// The receiver resolved to a value that does not carry an error.
    NotFallible,
    /// The receiver could not be resolved from this file.
    Unresolved,
}

/// Why a value is known to carry an error.
enum Witness {
    /// A function defined in this file declares a `Result` return, directly or
    /// through a type alias declared here.
    Function(String),
    /// A binding's written type annotation resolves to `Result`.
    Binding(String),
    /// A standard-library routine from [`STD_FALLIBLE_ROUTINES`].
    Routine(String),
}

impl Witness {
    /// Render the witness as the finding's evidence.
    fn describe(&self) -> String {
        match *self {
            Self::Function(ref name) => format!("the `Result` returned by fn `{name}`"),
            Self::Binding(ref name) => format!("the `Result` bound by `let {name}: ..`"),
            Self::Routine(ref path) => format!("the `Result` returned by `{path}`"),
        }
    }
}

/// A return type as the signature wrote it.
enum KnownReturn<'t> {
    /// The signature writes a return type.
    Written(Node<'t>),
    /// The signature writes none, so the return is `()`.
    Unit,
    /// Two definitions here share this bare name, so a call through it does not
    /// resolve to one signature.
    Ambiguous,
}

/// The fallibility witnesses one file states about itself.
///
/// The scan is single-file by construction, so this index is the whole semantic
/// source available, and the boundary is stated rather than papered over: a
/// receiver whose type is declared in another module is
/// [`Fallibility::Unresolved`], not a guess.
#[derive(Default)]
struct FallibilityIndex<'t> {
    /// Every free function defined here, by name, with the return type its
    /// signature writes. Methods are not recorded: a receiver reached as `x.f()`
    /// is resolved by the expression, never by the method's name, so recording
    /// `fn open` from an impl block would only make a *free* `fn open` ambiguous
    /// and cost a witness it had.
    functions: HashMap<String, KnownReturn<'t>>,
    /// Every `type X = ..;` alias defined here.
    aliases: HashMap<String, Node<'t>>,
    /// Every annotated `let name: Type = ..;` written here.
    bindings: HashMap<String, Node<'t>>,
}

impl<'t> FallibilityIndex<'t> {
    /// Collect every witness the file states, test code included.
    fn of(source: &str, root: Node<'t>) -> Self {
        let mut index = Self::default();
        let mut seen: HashSet<String> = HashSet::new();
        let mut ambiguous: HashSet<String> = HashSet::new();
        each_node(root, |node| match node.kind() {
            "function_item" if !syntax::is_in_impl(node) && !syntax::is_in_trait(node) => {
                index.record_signature(source, node);
            }
            "type_item" if !syntax::is_in_impl(node) => {
                if let (Some(name), Some(aliased)) = (node.by_field("name"), node.by_field("type"))
                {
                    index
                        .aliases
                        .insert(syntax::text(source, name).to_owned(), aliased);
                }
            }
            "let_declaration" => {
                index.record_binding(source, node, &mut seen, &mut ambiguous);
            }
            _ => {}
        });
        index
    }

    /// Record a free function's return type under its name.
    fn record_signature(&mut self, source: &str, function: Node<'t>) {
        let Some(name) = function.by_field("name") else {
            return;
        };
        let known = match function.by_field("return_type") {
            Some(written) => KnownReturn::Written(written),
            None => KnownReturn::Unit,
        };
        match self.functions.entry(syntax::text(source, name).to_owned()) {
            Entry::Vacant(slot) => {
                slot.insert(known);
            }
            // A second definition under one bare name does not resolve to one
            // signature, so a call through it stops being a witness instead of
            // silently keeping whichever was seen first.
            Entry::Occupied(mut slot) => {
                slot.insert(KnownReturn::Ambiguous);
            }
        }
    }

    /// Only an annotated `let` states a type; an inferred one states nothing,
    /// which is why `let value = ..;` never becomes a witness. A name written
    /// more than once no longer says which binding a later read of it means.
    fn record_binding(
        &mut self,
        source: &str,
        declaration: Node<'t>,
        seen: &mut HashSet<String>,
        ambiguous: &mut HashSet<String>,
    ) {
        let (Some(pattern), Some(written)) = (
            declaration.by_field("pattern"),
            declaration.by_field("type"),
        ) else {
            return;
        };
        let Some(name) = bound_name(source, pattern) else {
            return;
        };
        if ambiguous.contains(name) {
            return;
        }
        if seen.insert(name.to_owned()) {
            self.bindings.insert(name.to_owned(), written);
        } else {
            self.bindings.remove(name);
            ambiguous.insert(name.to_owned());
        }
    }

    /// Resolve a receiver expression, or the value of a `let _ = <expr>;`.
    ///
    /// Only a call or a named binding states anything here. A method call's
    /// *result* is not the fallible value this detector is about (the method
    /// already returned `T`) and the method call itself is reported by the
    /// call walk, so classifying one here would double-count it.
    fn classify(&self, source: &str, receiver: Node<'_>) -> Fallibility {
        match receiver.kind() {
            "call_expression" => match receiver.by_field("function") {
                Some(function) => self.classify_call(source, function),
                None => Fallibility::Unresolved,
            },
            "identifier" => self.classify_binding(source, receiver),
            _ => Fallibility::Unresolved,
        }
    }

    /// Classify a call target.
    ///
    /// Supported forms are a single-segment path (`read_config(..)`) and a
    /// `Self::`- or `self::`-qualified one. A path through a module
    /// (`crate::io::read_config(..)`) does not resolve: the scanner cannot see
    /// what that module re-exports, and resolving by the last segment would
    /// credit a different function that happens to share its name.
    fn classify_call(&self, source: &str, function: Node<'_>) -> Fallibility {
        let Some(path) = Path::of(source, function) else {
            return Fallibility::Unresolved;
        };
        let rendered = path.render();
        if STD_FALLIBLE_ROUTINES.contains(&rendered.as_str()) {
            return Fallibility::Result(Witness::Routine(rendered));
        }
        let Some(name) = callable_name(&path) else {
            return Fallibility::Unresolved;
        };
        match self.functions.get(name) {
            Some(&KnownReturn::Written(written)) if self.type_is_result(source, written, 0) => {
                Fallibility::Result(Witness::Function(name.to_owned()))
            }
            Some(&(KnownReturn::Written(_) | KnownReturn::Unit)) => Fallibility::NotFallible,
            Some(&KnownReturn::Ambiguous) | None => Fallibility::Unresolved,
        }
    }

    /// Classify a plain path used as a value, against the annotated `let`
    /// bindings this file writes.
    fn classify_binding(&self, source: &str, identifier: Node<'_>) -> Fallibility {
        let name = syntax::text(source, identifier);
        match self.bindings.get(name) {
            Some(&written) if self.type_is_result(source, written, 0) => {
                Fallibility::Result(Witness::Binding(name.to_owned()))
            }
            Some(_) => Fallibility::NotFallible,
            None => Fallibility::Unresolved,
        }
    }

    /// True when a written type is a `Result`, following this file's aliases.
    fn type_is_result(&self, source: &str, written: Node<'_>, depth: u8) -> bool {
        if depth >= MAX_ALIAS_DEPTH {
            return false;
        }
        let next = depth.saturating_add(1);
        match written.kind() {
            "reference_type" => written
                .by_field("type")
                .is_some_and(|inner| self.type_is_result(source, inner, next)),
            "type_identifier" | "scoped_type_identifier" | "generic_type" => {
                let Some(path) = Path::of(source, written) else {
                    return false;
                };
                match path.last() {
                    Some("Result") => true,
                    Some(last) => match self.aliases.get(last) {
                        Some(&aliased) => self.type_is_result(source, aliased, next),
                        None => false,
                    },
                    None => false,
                }
            }
            _ => false,
        }
    }
}

/// The name an annotated `let` binds, when its pattern is only a name (with
/// `mut` or `ref`, which are not part of what it is called).
fn bound_name<'s>(source: &'s str, pattern: Node<'_>) -> Option<&'s str> {
    match pattern.kind() {
        "identifier" => Some(syntax::text(source, pattern)),
        "mut_pattern" | "ref_pattern" => {
            let inner: Vec<Node<'_>> = syntax::named_kids(pattern)
                .into_iter()
                .filter(|kid| kid.kind() != "mutable_specifier")
                .collect();
            match inner.as_slice() {
                &[only] => bound_name(source, only),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The bare name a call's path resolves to in this file, when its own shape
/// allows one.
///
/// A qualified `Self::name` is the same free function as `name`; anything longer
/// is a path through a module, which does not resolve here.
fn callable_name(path: &Path) -> Option<&str> {
    if path.leading {
        return None;
    }
    match (
        path.segments.first(),
        path.segments.get(1),
        path.segments.get(2),
    ) {
        (Some(only), None, _) => Some(only.as_str()),
        (Some(first), Some(second), None) if first == "Self" || first == "self" => {
            Some(second.as_str())
        }
        _ => None,
    }
}

/// The method name, its node and its receiver, when a call is a method call:
/// `receiver.method(..)`, with or without a turbofish.
fn method_call<'s, 't>(source: &'s str, call: Node<'t>) -> Option<(&'s str, Node<'t>, Node<'t>)> {
    if call.kind() != "call_expression" {
        return None;
    }
    let function = call.by_field("function")?;
    let callee = if function.kind() == "generic_function" {
        function.by_field("function")?
    } else {
        function
    };
    if callee.kind() != "field_expression" {
        return None;
    }
    let method = callee.by_field("field")?;
    let receiver = callee.by_field("value")?;
    Some((syntax::text(source, method), method, receiver))
}

/// The parameters of a closure, `_` included.
fn closure_parameters(closure: Node<'_>) -> Vec<Node<'_>> {
    let Some(parameters) = closure.by_field("parameters") else {
        return Vec::new();
    };
    syntax::kids(parameters)
        .into_iter()
        .filter(|kid| !kid.is_extra() && !matches!(kid.kind(), "|" | ","))
        .collect()
}

/// The arguments of a call, attributes set apart.
fn call_arguments(call: Node<'_>) -> Vec<Node<'_>> {
    match call.by_field("arguments") {
        Some(arguments) => syntax::named_kids(arguments)
            .into_iter()
            .filter(|argument| argument.kind() != "attribute_item")
            .collect(),
        None => Vec::new(),
    }
}

/// Names the method when a call passes a closure that drops the error it is
/// handed, and `None` otherwise.
///
/// Only one-argument closures count. That arity is evidence about `Option`, not
/// about the receiver: `Option::unwrap_or_else` and `Option::or_else` take a
/// zero-argument closure, and `Option` has no `map_err` at all, so a call
/// passing exactly one closure argument cannot be any of those methods on an
/// `Option`. A user-defined type may still define the same names, which is why
/// this answer is only the *shape*: when the receiver does not resolve, the
/// finding says so, and when it resolves to a non-`Result` the call is not
/// reported at all. A closure taking a second argument is not the shape being
/// detected and is left alone.
fn discarded_error_closure(source: &str, call: Node<'_>, method: &str) -> Option<&'static str> {
    let name = match method {
        "unwrap_or_else" => "unwrap_or_else",
        "map_err" => "map_err",
        "or_else" => "or_else",
        _ => return None,
    };
    let first = *call_arguments(call).first()?;
    if first.kind() != "closure_expression" {
        return None;
    }
    let parameters = closure_parameters(first);
    let &[parameter] = parameters.as_slice() else {
        return None;
    };
    let body = first.by_field("body")?;
    error_binding_is_dropped(source, parameter, body).then_some(name)
}

/// A `|_|` parameter drops the cause outright. A named parameter the body never
/// reads drops it just as completely.
///
/// Any pattern that is neither a wildcard nor a single identifier (a tuple, a
/// struct pattern, a slice, a typed parameter) binds through a shape rather than
/// a name; the detector cannot tell a dropped binding from a used one there, so
/// it refuses to accuse and answers `false`.
fn error_binding_is_dropped(source: &str, parameter: Node<'_>, body: Node<'_>) -> bool {
    match bound_name(source, parameter) {
        Some(name) => !binding::observes(source, body, name),
        None => parameter.kind() == "_",
    }
}

/// Walks a file looking for discarded `Result` errors, accumulating findings
/// into the caller's `hits`.
///
/// Test functions and test modules are skipped wholesale: a swallow inside a test
/// is the normal way to assert on an error, not an observability defect.
struct SwallowWalker<'a, 't> {
    /// The text the tree was parsed from.
    source: &'a str,
    /// Findings accumulated during the walk, in visit order.
    hits: &'a mut Vec<Hit>,
    /// Witnesses collected from the file being walked.
    index: &'a FallibilityIndex<'t>,
}

impl SwallowWalker<'_, '_> {
    /// Visit a node and everything under it.
    fn walk(&mut self, node: Node<'_>) {
        match node.kind() {
            "function_item" => {
                if !syntax::is_in_trait(node) && cfg::is_test_item(self.source, node) {
                    return;
                }
                self.walk_children(node);
            }
            "mod_item" => {
                if !cfg::is_test_item(self.source, node) {
                    self.walk_children(node);
                }
            }
            "call_expression" => {
                self.record_method_call(node);
                self.walk_children(node);
            }
            "expression_statement" => {
                self.record_discarded_ok(node);
                self.walk_children(node);
            }
            "let_declaration" => {
                if !self.record_wildcard_binding(node) {
                    self.walk_children(node);
                }
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

    /// Build the evidence for a finding, or refuse to report one.
    ///
    /// `shape_is_option_impossible` is true only for a call whose spelling and
    /// argument shape a standard `Option` cannot have. A resolved witness always
    /// reports; a receiver resolved to something that is not a `Result` never
    /// does; an unresolved receiver reports only under that shape, and the
    /// evidence then says the shape is the whole of it rather than dressing an
    /// inference up as a proved defect.
    fn evidence_for(&self, receiver: Node<'_>, shape_is_option_impossible: bool) -> Option<String> {
        match self.index.classify(self.source, receiver) {
            Fallibility::Result(witness) => Some(witness.describe()),
            Fallibility::NotFallible => None,
            Fallibility::Unresolved if shape_is_option_impossible => Some(SHAPE_ONLY.to_owned()),
            Fallibility::Unresolved => None,
        }
    }

    /// Records `.unwrap_or_default()` on a `Result` and the discarded-error
    /// closures of `unwrap_or_else`, `map_err` and `or_else`.
    fn record_method_call(&mut self, call: Node<'_>) {
        let Some((method, at, receiver)) = method_call(self.source, call) else {
            return;
        };
        let line = syntax::line(at);
        if method == "unwrap_or_default" {
            // `Option::unwrap_or_default` carries this exact spelling and means
            // "absent, use the default", so the receiver has to resolve before
            // this can be reported as a lost error.
            if let Some(evidence) = self.evidence_for(receiver, false) {
                self.hits.push(Hit::new(
                    "ERROR-SWALLOW",
                    line,
                    format!(".unwrap_or_default() silently drops error from {evidence}"),
                ));
            }
        }
        if let Some(dropped) = discarded_error_closure(self.source, call, method)
            && let Some(evidence) = self.evidence_for(receiver, true)
        {
            self.hits.push(Hit::new(
                "ERROR-SWALLOW",
                line,
                format!(
                    ".{dropped}(|..| ..) never reads the error it was handed; evidence: {evidence}"
                ),
            ));
        }
    }

    /// Records a statement of the form `expr.ok();`: the `Result` is converted
    /// to an `Option` and then dropped, so the error is gone with no caller ever
    /// reading it.
    ///
    /// Only a statement whose semicolon is present counts: a trailing `.ok()`
    /// expression (a tail value) is not discarded. `Option` has no `ok`, so the
    /// spelling is the shape evidence; a receiver that resolves to a non-`Result`
    /// still refuses the finding.
    fn record_discarded_ok(&mut self, statement: Node<'_>) {
        if !syntax::kids(statement).iter().any(|kid| kid.kind() == ";") {
            return;
        }
        let Some(expression) = syntax::named_kids(statement).into_iter().next() else {
            return;
        };
        let Some((method, at, receiver)) = method_call(self.source, expression) else {
            return;
        };
        if method != "ok" {
            return;
        }
        let Some(evidence) = self.evidence_for(receiver, true) else {
            return;
        };
        self.hits.push(Hit::new(
            "ERROR-SWALLOW",
            syntax::line(at),
            format!(".ok(); discards {evidence} without reading it"),
        ));
    }

    /// Records a `let _ = <expr>;` binding and reports whether it matched.
    ///
    /// A `let _` with no initializer declares nothing fallible and is not a
    /// finding, so it answers `false` and the caller keeps walking the statement.
    /// When it does match, the initializer is visited here rather than by the
    /// caller, which is why the caller must not re-walk it: doing so would report
    /// the same initializer twice.
    ///
    /// The initializer is visited either way, because a value that is not
    /// fallible can still hold a `.ok()` or another swallow; but only a call or
    /// an annotated binding *witnesses* fallibility, so `let _ = 7usize;` and
    /// `let _ = option;` produce no finding.
    fn record_wildcard_binding(&mut self, declaration: Node<'_>) -> bool {
        let wildcard = declaration
            .by_field("pattern")
            .is_some_and(|pattern| pattern.kind() == "_");
        if !wildcard || declaration.by_field("type").is_some() {
            return false;
        }
        let Some(init) = declaration.by_field("value") else {
            return false;
        };
        if let Fallibility::Result(witness) = self.index.classify(self.source, init) {
            self.hits.push(Hit::new(
                "ERROR-SWALLOW",
                syntax::line(declaration),
                format!(
                    "let _ = ...; binds {} to `_` without inspecting it",
                    witness.describe()
                ),
            ));
        }
        self.walk(init);
        true
    }
}
