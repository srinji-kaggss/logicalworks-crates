//! `scan` owns the estate's zero-gate source detectors and enforces
//! INV-SCAN-ZERO: a file the estate ships carries no silenced error, no
//! unlogged error return, no lint allowance, no overlong try chain, and no
//! paraphrase docstring.
//!
//! Ported from keel's `keel-scan` detectors (`keel-core/src/hollowness.rs`,
//! `observability_gap.rs`, `allow_silence.rs`, `debuggability.rs`,
//! `interpretability.rs`) so every estate repo runs the same verdicts from
//! this one binary instead of depending on the keel gate fleet. Rule names,
//! messages, thresholds, and exemptions match keel exactly; any intentional
//! divergence is marked `DIVERGENCE` with its reason. The port reads syntax
//! with `syn` — a Rust grammar is the only honest oracle for Rust source,
//! which is why `syn` is this crate's one non-facade dependency.
//!
//! DIVERGENCE (cfg atom keys): keel keys SAT atoms by token-stream text via
//! `quote::ToTokens`. This port keys them by structural rendering, which is
//! canonical where token text is incidental (whitespace, trailing commas).
//! Both sides of every comparison use the same function, so verdicts agree.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::path::Path;

use syn::parse::Parser;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};

// ── Findings ────────────────────────────────────────────────────────────────

/// One detector finding, in the shape CI printers already read.
///
/// Non-exhaustive because the detectors below are its only constructors: a
/// downstream consumer must destructure a `Hit` with `..`, so a later evidence
/// field cannot be added by a caller and cannot break one either.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Hit {
    /// Detector rule name, verbatim (`ERROR-SWALLOW`, `unlogged-err-return`,
    /// `ALLOW-SILENCE`, `long-try-chain`, `tautological-doc`).
    pub rule: &'static str,
    /// 1-based line number.
    pub line: usize,
    /// Human-readable evidence.
    pub snippet: String,
}

/// Why a file could not be scanned. Unparseable source is a refusal, not a
/// pass — a gate that passes what it cannot read reports success for the one
/// condition it exists to catch.
///
/// Non-exhaustive so a caller cannot treat a future failure mode as impossible
/// by matching every variant: a new reason to refuse must not silently become a
/// compile error inside someone else's exhaustive `match`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScanError {
    /// The file is not parseable Rust.
    Unparseable {
        /// File that could not be parsed.
        path: String,
        /// The parse failure.
        cause: String,
    },
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Unparseable {
                ref path,
                ref cause,
            } => write!(f, "{path}: cannot parse Rust: {cause}"),
        }
    }
}

impl Error for ScanError {}

/// Scans one Rust source text with all five detectors, ordered by line.
pub fn scan_source(source: &str, path: &str) -> Result<Vec<Hit>, ScanError> {
    let file = syn::parse_file(source).map_err(|cause| ScanError::Unparseable {
        path: path.to_owned(),
        cause: cause.to_string(),
    })?;
    let mut hits = Vec::new();
    detect_error_swallow(source, &file, &mut hits);
    detect_unlogged_err(source, &file, &mut hits);
    detect_allow_silence(&file, &mut hits);
    detect_long_try_chain(&file, &mut hits);
    detect_tautological_doc(&file, &mut hits);
    hits.sort_by(|left, right| (left.line, left.rule).cmp(&(right.line, right.rule)));
    Ok(hits)
}

/// Scans one `.rs` file from disk.
pub fn scan_path(path: &Path) -> Result<Vec<Hit>, ScanError> {
    let source = std::fs::read_to_string(path).map_err(|cause| ScanError::Unparseable {
        path: path.display().to_string(),
        cause: cause.to_string(),
    })?;
    scan_source(&source, &path.display().to_string())
}

// ── Shared test-attribute semantics ─────────────────────────────────────────

/// True for `#[test]`, `#[test_case]`, `#[rstest]`, or a `#[cfg]` that can
/// only hold under `test` (decided by exhaustive SAT over its atoms).
fn is_test_fn(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(is_test_attribute)
}

/// True when one attribute marks its item as test-only: a test harness macro
/// (`test`, `test_case`, `rstest`) or a `cfg` that cannot hold outside `test`.
///
/// The `cfg` arm parses its argument as a single `syn::Meta`; an attribute
/// whose arguments are not a well-formed meta list — a bare `#[cfg]`, or one
/// carrying token soup — is not a test marker and answers `false`.
fn is_test_attribute(attr: &syn::Attribute) -> bool {
    let name = attr
        .path()
        .segments
        .last()
        .map(|segment| segment.ident.to_string());
    if name
        .as_deref()
        .is_some_and(|name| matches!(name, "test" | "test_case" | "rstest"))
    {
        return true;
    }
    attr.path().is_ident("cfg")
        && attr
            .parse_args::<syn::Meta>()
            .is_ok_and(|meta| cfg_requires_test(&meta))
}

/// True when no assignment of the cfg atoms outside `test` can satisfy the
/// expression — i.e. the item exists only under `cfg(test)`.
fn cfg_requires_test(meta: &syn::Meta) -> bool {
    let mut atoms = HashSet::new();
    collect_cfg_atoms(meta, &mut atoms);
    if atoms.len() > 12 {
        return false;
    }
    let atoms = atoms.into_iter().collect::<Vec<_>>();
    !(0..(1usize << atoms.len())).any(|assignment| {
        let values = atoms
            .iter()
            .enumerate()
            .map(|(index, atom)| (atom.as_str(), assignment & (1 << index) != 0))
            .collect::<HashMap<_, _>>();
        eval_cfg(meta, false, &values)
    })
}

/// Collects every free atom of a `cfg` expression into `atoms`, recursing
/// through `all` / `any` / `not`.
///
/// `test` is deliberately NOT collected: it is supplied by the SAT solver as a
/// fixed `false`, so treating it as a free variable would let the solver
/// satisfy an expression it cannot actually satisfy in a non-test build.
/// A combinator whose arguments do not parse falls through to the leaf arm and
/// is recorded by its own text, which keeps the expression unsolvable rather
/// than silently dropping the atom.
fn collect_cfg_atoms(meta: &syn::Meta, atoms: &mut HashSet<String>) {
    match *meta {
        syn::Meta::Path(ref path) if path.is_ident("test") => {}
        syn::Meta::List(ref list)
            if matches!(
                list.path.get_ident().map(ToString::to_string).as_deref(),
                Some("all" | "any" | "not")
            ) =>
        {
            if let Some(children) = cfg_children(list) {
                for child in children {
                    collect_cfg_atoms(&child, atoms);
                }
            } else {
                atoms.insert(atom_key(meta));
            }
        }
        _ => {
            atoms.insert(atom_key(meta));
        }
    }
}

/// Structural rendering of a cfg atom. DIVERGENCE from keel (see module
/// docs): canonical where token text is incidental; identity-consistent
/// within each SAT problem, which is all the solver requires.
///
/// Two structurally different atoms never render the same string, and the same
/// atom always renders the same string — that identity is what makes the
/// `HashMap` keyed by this text a faithful model of the cfg expression.
fn atom_key(meta: &syn::Meta) -> String {
    match *meta {
        syn::Meta::Path(ref path) => path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
        syn::Meta::List(ref list) => {
            let head = list
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");
            match cfg_children(list) {
                Some(children) => {
                    let inner = children.iter().map(atom_key).collect::<Vec<_>>().join(",");
                    format!("{head}({inner})")
                }
                None => format!("{head}(?)"),
            }
        }
        syn::Meta::NameValue(ref named) => {
            format!(
                "{}={}",
                atom_key_path(&named.path),
                named.value.to_token_string()
            )
        }
    }
}

/// Joins a path's segments with `::`, the same rendering [`atom_key`] uses, so
/// the left-hand side of a name-value atom keys consistently with a path atom
/// of the same spelling.
fn atom_key_path(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// Token rendering without the `quote` crate: the token stream's own
/// display. Used only for cfg name-value atoms, never for verdicts.
trait TokenString {
    /// Renders this expression as the text a cfg atom key would carry.
    /// Non-literal expressions render as `..`, which merges every such
    /// expression into one atom — a deliberate over-approximation that can
    /// only make an expression harder to satisfy than it really is.
    fn to_token_string(&self) -> String;
}

impl TokenString for syn::Expr {
    fn to_token_string(&self) -> String {
        match *self {
            syn::Expr::Lit(ref lit) => match lit.lit {
                syn::Lit::Str(ref text) => format!("{:?}", text.value()),
                syn::Lit::ByteStr(_) | syn::Lit::CStr(_) => "b\"..\"".to_owned(),
                syn::Lit::Byte(_) => "b'..'".to_owned(),
                syn::Lit::Char(ref character) => format!("{:?}", character.value()),
                syn::Lit::Int(ref integer) => integer.base10_digits().to_owned(),
                syn::Lit::Float(ref float) => float.base10_digits().to_owned(),
                syn::Lit::Bool(ref boolean) => boolean.value.to_string(),
                _ => "..".to_owned(),
            },
            _ => "..".to_owned(),
        }
    }
}

/// Parses a `cfg` list's arguments as a comma-separated meta list.
///
/// Returns `None` when the arguments are not well-formed metas; every caller
/// treats that as "unreadable, degrade conservatively" rather than guessing at
/// the contents. The parse is on a clone of the token stream, so the original
/// syntax tree the detector walks is never consumed.
fn cfg_children(
    list: &syn::MetaList,
) -> Option<syn::punctuated::Punctuated<syn::Meta, syn::Token![,]>> {
    syn::punctuated::Punctuated::parse_terminated
        .parse2(list.tokens.clone())
        .ok()
}

/// Evaluates a `cfg` expression under one assignment of its free atoms, with
/// the `test` atom fixed to the `test` argument.
///
/// A combinator whose arguments fail to parse folds to `false` via `is_some_and`
/// on the `all` / `any` arms; `not` falls back to [`atom_value`]. Unknown
/// paths evaluate through `values`, which defaults an unassigned atom to
/// `true` — the conservative direction, since a `true` atom makes an
/// expression easier to satisfy and so less likely to be called test-only.
fn eval_cfg(meta: &syn::Meta, test: bool, values: &HashMap<&str, bool>) -> bool {
    match *meta {
        syn::Meta::Path(ref path) if path.is_ident("test") => test,
        syn::Meta::List(ref list) if list.path.is_ident("all") => eval_cfg_all(list, test, values),
        syn::Meta::List(ref list) if list.path.is_ident("any") => eval_cfg_any(list, test, values),
        syn::Meta::List(ref list) if list.path.is_ident("not") => {
            eval_cfg_not(meta, list, test, values)
        }
        _ => atom_value(meta, values),
    }
}

/// True when every conjunct holds. An unparseable argument list answers
/// `false`: `all` of an unknown set cannot be shown to hold.
fn eval_cfg_all(list: &syn::MetaList, test: bool, values: &HashMap<&str, bool>) -> bool {
    cfg_children(list)
        .is_some_and(|children| children.iter().all(|child| eval_cfg(child, test, values)))
}

/// True when any disjunct holds. An unparseable argument list answers
/// `false` for the same reason as [`eval_cfg_all`].
fn eval_cfg_any(list: &syn::MetaList, test: bool, values: &HashMap<&str, bool>) -> bool {
    cfg_children(list)
        .is_some_and(|children| children.iter().any(|child| eval_cfg(child, test, values)))
}

/// Negates the single argument of a `not`. A `not` with zero or more than one
/// argument is malformed Rust; it degrades to [`atom_value`] rather than
/// guessing which argument was meant.
fn eval_cfg_not(
    meta: &syn::Meta,
    list: &syn::MetaList,
    test: bool,
    values: &HashMap<&str, bool>,
) -> bool {
    let Some(children) = cfg_children(list) else {
        return atom_value(meta, values);
    };
    let mut children = children.iter();
    match (children.next(), children.next()) {
        (Some(child), None) => !eval_cfg(child, test, values),
        _ => atom_value(meta, values),
    }
}

/// Looks up one atom's assigned value.
///
/// An atom absent from `values` answers `true`: [`collect_cfg_atoms`] inserts
/// every atom it walks, so an absent key means the walk and the evaluation
/// disagreed about the expression's shape, and `true` is the direction that
/// refuses to call the item test-only.
fn atom_value(meta: &syn::Meta, values: &HashMap<&str, bool>) -> bool {
    values.get(atom_key(meta).as_str()).copied().unwrap_or(true)
}

/// True when a module is itself test-only: `#[cfg(test)] mod tests`, or a
/// module gated by any attribute [`is_test_attribute`] recognises.
fn is_test_module(module: &syn::ItemMod) -> bool {
    is_test_fn(&module.attrs)
}

// ── Detector 1: ERROR-SWALLOW ───────────────────────────────────────────────
// Ported from keel-core/src/hollowness.rs `detect_error_swallow`.

/// Result-typed expressions whose error is silently discarded: `.ok()` on a
/// `Result`, `let _ = <fallible>`, `.unwrap_or_default()` on a `Result`, or
/// a one-argument closure to `unwrap_or_else` / `map_err` / `or_else` that
/// never reads the error it was handed.
///
/// Closure arity alone proves the receiver is a `Result`: `Option`'s
/// `unwrap_or_else` takes zero arguments, so a one-argument closure cannot be
/// an `Option` fallback, and `map_err` / `or_else` exist only on `Result`.
fn detect_error_swallow(_source: &str, file: &syn::File, hits: &mut Vec<Hit>) {
    let mut visitor = ErrorSwallowVisitor { hits };
    visitor.visit_file(file);
}

/// Walks a file looking for discarded `Result` errors, accumulating findings
/// into the caller's `hits`.
///
/// Test functions and test modules are skipped wholesale: a swallow inside a
/// test is the normal way to assert on an error, not an observability defect.
struct ErrorSwallowVisitor<'a> {
    /// Findings accumulated during the walk, in visit order.
    hits: &'a mut Vec<Hit>,
}

/// Names the method when a call passes a closure that drops the error it is
/// handed, and `None` otherwise.
///
/// Only one-argument closures count: `Option::unwrap_or_else` takes a
/// zero-argument closure and `map_err` / `or_else` do not exist on `Option`,
/// so an arity of exactly one is itself the proof that the receiver is a
/// `Result`. A closure taking a second argument (a `FnOnce(.., ..)`) is not
/// the shape being detected and is left alone.
fn discarded_error_closure(node: &syn::ExprMethodCall) -> Option<&'static str> {
    let method = match node.method.to_string().as_str() {
        "unwrap_or_else" => "unwrap_or_else",
        "map_err" => "map_err",
        "or_else" => "or_else",
        _ => return None,
    };
    let syn::Expr::Closure(ref closure) = *node.args.first()? else {
        return None;
    };
    let mut inputs = closure.inputs.iter();
    let (Some(parameter), None) = (inputs.next(), inputs.next()) else {
        return None;
    };
    error_binding_is_dropped(parameter, closure.body.as_ref()).then_some(method)
}

/// A `|_|` parameter drops the cause outright. A named parameter the body
/// never reads drops it just as completely.
///
/// Any pattern that is neither a wildcard nor a single identifier — a tuple, a
/// struct pattern, a slice — binds through a shape rather than a name; the
/// detector cannot tell a dropped binding from a used one there, so it refuses
/// to accuse and answers `false`.
fn error_binding_is_dropped(parameter: &syn::Pat, body: &syn::Expr) -> bool {
    match *parameter {
        syn::Pat::Wild(_) => true,
        syn::Pat::Ident(ref bound) => !body_reads_ident(body, &bound.ident),
        _ => false,
    }
}

/// True when `name` occurs anywhere in `body`, syntactically or inside a macro
/// body's token text.
fn body_reads_ident(body: &syn::Expr, name: &syn::Ident) -> bool {
    let mut reader = IdentReader { name, found: false };
    reader.visit_expr(body);
    reader.found
}

/// Visit state for [`body_reads_ident`]: the identifier being looked for and
/// whether it has been seen so far.
struct IdentReader<'a> {
    /// The parameter name whose use would count as reading the error.
    name: &'a syn::Ident,
    /// Set once a matching identifier is visited; never cleared, because one
    /// read anywhere in the body is enough to clear the closure.
    found: bool,
}

impl<'ast> Visit<'ast> for IdentReader<'_> {
    fn visit_ident(&mut self, node: &'ast syn::Ident) {
        if node == self.name {
            self.found = true;
        }
    }

    /// Macro bodies are matched as TEXT, not walked as syntax: an inline
    /// format capture spells the read inside a string literal, so
    /// `|err| panic!("...: {err}")` has no `err` ident in the AST. Text can
    /// only ever say "read" where syntax says nothing — failing toward
    /// silence, never toward a false accusation.
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if node.tokens.to_string().contains(&self.name.to_string()) {
            self.found = true;
        }
        visit::visit_macro(self, node);
    }
}

impl ErrorSwallowVisitor<'_> {
    /// Records a statement of the form `expr.ok();` — the `Result` is
    /// converted to an `Option` and then dropped, so the error is gone with no
    /// caller ever reading it.
    ///
    /// Only a statement whose semicolon is present counts. A trailing
    /// `.ok()` expression (a tail value) is not discarded, and an `Option`
    /// receiver is indistinguishable here — but `.ok()` on a `Result` is the
    /// shape this detector exists for, and the statement form is the one that
    /// provably throws the value away.
    fn record_discarded_ok(&mut self, statement: &syn::Stmt) {
        let syn::Stmt::Expr(syn::Expr::MethodCall(ref call), Some(_)) = *statement else {
            return;
        };
        if call.method == "ok" {
            self.hits.push(Hit {
                rule: "ERROR-SWALLOW",
                line: call.method.span().start().line.max(1),
                snippet: ".ok(); discards Result value".to_owned(),
            });
        }
    }

    /// Records a `let _ = <expr>;` binding and reports whether it matched.
    ///
    /// A `let _` with no initializer declares nothing fallible and is not a
    /// finding, so it answers `false` and the caller keeps walking the
    /// statement. When it does match, the initializer is visited here rather
    /// than by the caller, which is why the caller must not re-walk it: doing
    /// so would report the same initializer twice.
    fn record_wildcard_binding(&mut self, statement: &syn::Stmt) -> bool {
        let syn::Stmt::Local(ref local) = *statement else {
            return false;
        };
        if !matches!(local.pat, syn::Pat::Wild(_)) {
            return false;
        }
        let Some(ref init) = local.init else {
            return false;
        };
        self.hits.push(Hit {
            rule: "ERROR-SWALLOW",
            line: local.let_token.span().start().line.max(1),
            snippet: "let _ = ...; binds a value to `_` without inspecting it".to_owned(),
        });
        visit::visit_expr(self, init.expr.as_ref());
        true
    }
}

impl<'ast> Visit<'ast> for ErrorSwallowVisitor<'_> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !is_test_fn(&node.attrs) {
            visit::visit_item_fn(self, node);
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !is_test_fn(&node.attrs) {
            visit::visit_impl_item_fn(self, node);
        }
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !is_test_module(node) {
            visit::visit_item_mod(self, node);
        }
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let line = node.method.span().start().line.max(1);
        if node.method == "unwrap_or_default" {
            self.hits.push(Hit {
                rule: "ERROR-SWALLOW",
                line,
                snippet: ".unwrap_or_default() silently drops error".to_owned(),
            });
        }
        if let Some(method) = discarded_error_closure(node) {
            self.hits.push(Hit {
                rule: "ERROR-SWALLOW",
                line,
                snippet: format!(".{method}(|..| ..) never reads the error it was handed"),
            });
        }
        visit::visit_expr_method_call(self, node);
    }

    fn visit_stmt(&mut self, node: &'ast syn::Stmt) {
        self.record_discarded_ok(node);
        if !self.record_wildcard_binding(node) {
            visit::visit_stmt(self, node);
        }
    }
}

// ── Detector 2: unlogged-err-return ─────────────────────────────────────────
// Ported from keel-core/src/observability_gap.rs
// `detect_unlogged_err_construction`.

/// Macro and path prefixes that count as a log emission.
///
/// Both forms are listed because the detector matches two ways: the textual
/// window reads physical lines, where `tracing::warn!` appears with its path,
/// while the AST statement check renders a macro path as `warn!(`. A marker
/// absent from this list is a macro the detector does not treat as a signal.
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

/// True when any needle occurs as a substring of `haystack`. An empty needle
/// list answers `false`, so a caller that builds its needles at runtime cannot
/// accidentally match everything.
fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// True for a direct `Err(...)` construction: a call whose function path ends
/// in the `Err` segment.
fn is_direct_err_construction(expr: &syn::Expr) -> bool {
    let syn::Expr::Call(ref call_expr) = *expr else {
        return false;
    };
    let syn::Expr::Path(ref func_path) = *call_expr.func else {
        return false;
    };
    func_path
        .path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "Err")
}

/// True when a log marker appears within `window` physical lines above the
/// return. Covers returns nested inside expressions, which the statement
/// walk does not classify.
fn has_log_within(source: &str, line_number: usize, window: usize) -> bool {
    let start_line = line_number.saturating_sub(window);
    for (index, line) in source.lines().enumerate() {
        // `index` counts lines of an in-memory `&str`, so it is at most
        // `source.len() - 1` and `index + 1` cannot exceed `usize::MAX`; the
        // addition is saturating anyway so a caller-supplied source cannot
        // make this wrap in a release build.
        let current_line = index.saturating_add(1);
        if current_line >= start_line
            && current_line <= line_number
            && contains_any(line, LOG_MARKERS)
        {
            return true;
        }
    }
    false
}

/// Builds the finding for a `return Err(...)` that no log statement vouches
/// for. The snippet is fixed rather than interpolated: CI printers key their
/// remediation text off the rule name, and a stable snippet keeps that mapping
/// total across every hit.
fn unlogged_err_hit(line_number: usize) -> Hit {
    Hit {
        rule: "unlogged-err-return",
        line: line_number,
        snippet: "return Err(...) with no log emission before it in the same block — the caller sees only a value, not a signal. Log the error with enough context to diagnose it. Line wrapping does not matter: the check is on statements, not physical lines.".to_owned(),
    }
}

/// Renders a macro path the way [`LOG_MARKERS`] matches, so the AST check
/// and the textual check agree on what counts as a log.
fn macro_marker_text(mac: &syn::Macro) -> String {
    let path = mac
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::");
    format!("{path}!(")
}

/// True when a statement is itself a log emission, as a statement macro
/// (`log::warn!(..)`) or an expression-position macro (`log::warn!(..)` with a
/// trailing semicolon, which `syn` files under `Stmt::Expr`).
///
/// `syn` splits those two shapes across different variants for the same source
/// text, so both are checked; a statement that is neither shape is not a log.
fn stmt_is_log(stmt: &syn::Stmt) -> bool {
    let mac = match *stmt {
        syn::Stmt::Macro(ref stmt_macro) => &stmt_macro.mac,
        syn::Stmt::Expr(syn::Expr::Macro(ref expr_macro), _) => &expr_macro.mac,
        _ => return false,
    };
    contains_any(&macro_marker_text(mac), LOG_MARKERS)
}

/// Control flow severs adjacency: a log before an `if` does not vouch for an
/// `Err` return after it.
fn stmt_is_control_flow(stmt: &syn::Stmt) -> bool {
    let syn::Stmt::Expr(ref expr, _) = *stmt else {
        return false;
    };
    matches!(
        expr,
        syn::Expr::If(_)
            | syn::Expr::Match(_)
            | syn::Expr::While(_)
            | syn::Expr::ForLoop(_)
            | syn::Expr::Loop(_)
    )
}

/// The line of a statement that is exactly `return Err(..)` and `None` for
/// every other statement.
///
/// A bare `return;` has no expression and a `return Ok(..)` is not an error, so
/// both answer `None`; a `return` nested inside a larger expression is not a
/// statement and is handled by the physical-line window instead.
fn stmt_direct_err_return_line(stmt: &syn::Stmt) -> Option<usize> {
    let syn::Stmt::Expr(syn::Expr::Return(ref ret), _) = *stmt else {
        return None;
    };
    let inner = ret.expr.as_deref()?;
    if !is_direct_err_construction(inner) {
        return None;
    }
    Some(ret.span().start().line)
}

/// Lines of `return Err(...)` a log in the SAME BLOCK already explains.
/// Formatting-immune where the physical window is not: a wrapped log macro
/// is still one statement.
/// Collects the lines of every `return Err(..)` that a log statement earlier in
/// the same block already explains, across the whole file.
///
/// The result is a set of line numbers rather than a per-block value because
/// the physical-line window in [`ErrReturnWalker::check_return_expr`] looks at
/// the file as text and cannot see block structure. A line absent from the set
/// is a return no statement-level log vouches for.
fn block_logged_return_lines(file: &syn::File) -> HashSet<usize> {
    let mut walker = BlockWalker {
        logged_returns: HashSet::new(),
    };
    walker.visit_file(file);
    walker.logged_returns
}

/// Visit state for [`block_logged_return_lines`]: the set of return lines
/// already vouched for by a log in their own block.
struct BlockWalker {
    /// Line numbers of `return Err(..)` statements preceded by a log statement
    /// in the same block.
    logged_returns: HashSet<usize>,
}

impl<'ast> Visit<'ast> for BlockWalker {
    fn visit_block(&mut self, block: &'ast syn::Block) {
        self.scan_statements(block);
        visit::visit_block(self, block);
    }
}

impl BlockWalker {
    /// Walks one block's statements in order, remembering whether a log has
    /// been seen since the last control-flow statement.
    ///
    /// Control flow resets the flag: a log that ran in an earlier `if` branch
    /// says nothing about a return reached by a different path, so a return
    /// after an `if` / `match` / loop needs its own log to be explained.
    fn scan_statements(&mut self, block: &syn::Block) {
        let mut log_seen = false;
        for stmt in &block.stmts {
            if stmt_is_log(stmt) {
                log_seen = true;
            } else if let Some(line) = stmt_direct_err_return_line(stmt) {
                if log_seen {
                    self.logged_returns.insert(line);
                }
            } else if stmt_is_control_flow(stmt) {
                log_seen = false;
            }
        }
    }
}

/// True when an attribute names a test or a test helper: `test`, or any name
/// starting with `test_` (`test_helper`, `test_fixture`).
///
/// Deliberately looser than [`is_test_fn`]: this check only decides whether to
/// disable the unlogged-error rule, so a helper wrongly classified as test-only
/// costs a missed finding for that helper alone, never a false accusation on
/// production code.
fn is_test_attr(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().segments.last().is_some_and(|segment| {
            let segment_name = segment.ident.to_string();
            segment_name == "test" || segment_name.starts_with("test_")
        })
    })
}

/// Runs the unlogged-error detector over a whole file, accumulating findings
/// into the caller's `hits`.
///
/// The block-level log map is computed once up front and shared by reference
/// with every return the walk visits, so the two passes cannot disagree about
/// which returns are already explained.
fn detect_unlogged_err(source: &str, file: &syn::File, hits: &mut Vec<Hit>) {
    let logged_returns = block_logged_return_lines(file);
    let mut walker = ErrReturnWalker {
        source,
        hits,
        test_scope_stack: Vec::new(),
        logged_returns: &logged_returns,
    };
    walker.visit_file(file);
}

/// Walker state for [`detect_unlogged_err`]: the source text the physical-line
/// window reads, the accumulated findings, the enclosing fn test-ness stack,
/// and the pre-computed set of log-explained return lines.
struct ErrReturnWalker<'a> {
    /// Source text of the file being scanned, used for the line window.
    source: &'a str,
    /// Findings accumulated during the walk, in visit order.
    hits: &'a mut Vec<Hit>,
    /// One entry per enclosing function, `true` when that function is test-only.
    /// The innermost entry decides whether findings are suppressed; a stack is
    /// needed because an inner non-test fn inside a test module is still test
    /// code, and an inner test helper inside a production fn is still exempt.
    test_scope_stack: Vec<bool>,
    /// Lines whose `return Err(..)` a log in the same block already explains,
    /// computed once by [`block_logged_return_lines`].
    logged_returns: &'a HashSet<usize>,
}

impl ErrReturnWalker<'_> {
    /// Pushes the test-ness of a function whose body is about to be visited.
    fn enter_scope(&mut self, attrs: &[syn::Attribute]) {
        self.test_scope_stack.push(is_test_attr(attrs));
    }

    /// Pops the test-ness of the function just finished. Called exactly once
    /// per [`Self::enter_scope`], so the stack is empty again after each
    /// top-level item.
    fn leave_scope(&mut self) {
        self.test_scope_stack.pop();
    }

    /// True when the innermost enclosing function is test-only. A return
    /// outside any function — a `const` initializer, say — is not disabled,
    /// which is why an empty stack answers `false`.
    fn is_scope_disabled(&self) -> bool {
        *self.test_scope_stack.last().unwrap_or(&false)
    }

    /// Records a finding when a `return Err(..)` in production code has no log
    /// statement in its block and no log marker within three physical lines
    /// above it.
    ///
    /// Both checks run before the finding is pushed, so a return the block
    /// walker already accepted is never reported by the window check and vice
    /// versa — the two are alternatives, not cumulative evidence.
    fn check_return_expr(&mut self, node: &syn::ExprReturn) {
        if self.is_scope_disabled() {
            return;
        }
        let Some(ref inner_expr) = node.expr else {
            return;
        };
        if !is_direct_err_construction(inner_expr) {
            return;
        }
        let line_number = node.span().start().line;
        if self.logged_returns.contains(&line_number) {
            return;
        }
        if !has_log_within(self.source, line_number, 3) {
            self.hits.push(unlogged_err_hit(line_number));
        }
    }
}

impl<'ast> Visit<'ast> for ErrReturnWalker<'_> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.enter_scope(&node.attrs);
        visit::visit_item_fn(self, node);
        self.leave_scope();
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.enter_scope(&node.attrs);
        visit::visit_impl_item_fn(self, node);
        self.leave_scope();
    }

    fn visit_expr_return(&mut self, node: &'ast syn::ExprReturn) {
        self.check_return_expr(node);
        visit::visit_expr_return(self, node);
    }
}

// ── Detector 3: ALLOW-SILENCE ───────────────────────────────────────────────
// Ported from keel-core/src/allow_silence.rs: an `#[allow(clippy::*)]`
// annotation silences a lint with a mechanical fix. rustc's own lints are
// untouched by construction; `#[cfg_attr]` is out of scope (conditional).

/// Runs the ALLOW-SILENCE detector over a whole file, accumulating findings
/// into the caller's `hits`.
///
/// Every attribute in the file is visited, not only item-level ones, so an
/// allow on a `let`, a statement, or a match arm is caught by the same walk.
fn detect_allow_silence(file: &syn::File, hits: &mut Vec<Hit>) {
    let mut visitor = AllowSilenceVisitor { hits };
    visitor.visit_file(file);
}

/// Visit state for [`detect_allow_silence`]: the findings accumulated so far.
struct AllowSilenceVisitor<'a> {
    /// Findings accumulated during the walk, in visit order.
    hits: &'a mut Vec<Hit>,
}

impl<'ast> Visit<'ast> for AllowSilenceVisitor<'_> {
    fn visit_attribute(&mut self, attribute: &'ast syn::Attribute) {
        for lint_name in clippy_lints_allowed_by(attribute) {
            self.hits.push(Hit {
                rule: "ALLOW-SILENCE",
                line: attribute.span().start().line,
                snippet: format!("#[allow({lint_name})]"),
            });
        }
    }
}

/// Names every clippy lint an attribute silences, in source order.
///
/// An attribute that is not `allow`, that has no argument list (`#[allow]`),
/// or whose arguments are not a well-formed lint list answers empty: the
/// detector refuses to guess at token soup rather than reporting a lint name it
/// did not read.
fn clippy_lints_allowed_by(attribute: &syn::Attribute) -> Vec<String> {
    if !attribute.path().is_ident("allow") {
        return Vec::new();
    }
    let syn::Meta::List(ref list) = attribute.meta else {
        return Vec::new();
    };
    let Ok(lints) = list.parse_args_with(
        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
    ) else {
        return Vec::new();
    };
    lints.into_iter().filter_map(clippy_lint_name).collect()
}

/// Renders one entry of an `allow` list as a lint name, and `None` when the
/// entry is not a clippy lint.
///
/// The entry must be a bare path whose FIRST segment is `clippy`, so
/// `clippy::too_many_arguments` is reported and a rustc lint such as
/// `dead_code` is not. A nested `clippy::all` group is reported by its path
/// text, which is what the gate's remediation message quotes.
fn clippy_lint_name(lint: syn::Meta) -> Option<String> {
    let syn::Meta::Path(path) = lint else {
        return None;
    };
    if path
        .segments
        .first()
        .is_none_or(|first| first.ident != "clippy")
    {
        return None;
    }
    Some(
        path.segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
    )
}

// ── Detector 4: long-try-chain ──────────────────────────────────────────────
// Ported from keel-core/src/debuggability.rs `detect_long_try_chain`: more
// than three `?` operators in one non-`let` statement, outside test fns.

/// Above this many `?` operators in one non-`let` statement, the chain is a
/// finding. Three is keel's threshold and is contractual: a chain of exactly
/// this length passes, one longer fails.
const MAX_TRY_CHAIN: usize = 3;

/// Runs the long-try-chain detector over a whole file, accumulating findings
/// into the caller's `hits`.
fn detect_long_try_chain(file: &syn::File, hits: &mut Vec<Hit>) {
    let mut walker = TryChainWalker {
        hits,
        current_fn_line: None,
        current_fn_skipped: false,
    };
    walker.visit_file(file);
}

/// Walk state for [`detect_long_try_chain`]: the accumulated findings plus the
/// innermost function being visited and whether it is exempt.
struct TryChainWalker<'a> {
    /// Findings accumulated during the walk, in visit order.
    hits: &'a mut Vec<Hit>,
    /// Line of the innermost enclosing function's name, or `None` outside any
    /// function. The snippet names this line so the reader can find the chain.
    current_fn_line: Option<usize>,
    /// True when the innermost function was entered through an exempt
    /// attribute, in which case none of its statements are inspected.
    current_fn_skipped: bool,
}

/// Counts the `?` operators in one statement's expression tree.
struct TryCounter {
    /// `?` operators seen so far; saturates rather than wraps.
    count: usize,
}

impl<'ast> Visit<'ast> for TryCounter {
    fn visit_expr_try(&mut self, node: &'ast syn::ExprTry) {
        // A statement holds fewer `?` operators than the file has bytes, and
        // the file is an in-memory `&str` whose length is a `usize`, so the
        // counter cannot reach `usize::MAX`; saturating keeps a pathological
        // input from wrapping in a release build.
        self.count = self.count.saturating_add(1);
        visit::visit_expr_try(self, node);
    }

    /// Closure bodies are not descended into: a `?` inside a closure belongs
    /// to the closure's own control flow, not to the statement being counted.
    fn visit_expr_closure(&mut self, _closure: &'ast syn::ExprClosure) {}
}

/// Counts the `?` operators in a statement's own expression.
///
/// A `let` statement answers zero by construction: the detector's contract is
/// that binding an intermediate with `let` breaks the chain, so an initializer
/// full of `?` is the recommended shape rather than a finding. Any other
/// statement is walked generically, which covers `for` / `while` / `match`
/// statements whose headers can carry a `?`.
fn count_tries_in_stmt(statement: &syn::Stmt) -> usize {
    let mut counter = TryCounter { count: 0 };
    match *statement {
        syn::Stmt::Expr(ref expr, _) => counter.visit_expr(expr),
        syn::Stmt::Local(_) => {}
        _ => visit::visit_stmt(&mut counter, statement),
    }
    counter.count
}

impl TryChainWalker<'_> {
    /// Marks the start of a function body and returns the state it displaced,
    /// for [`Self::exit_function`] to restore.
    ///
    /// An exempt function answers `None` and leaves the current state alone, so
    /// statements inside it keep the enclosing function's coordinates rather
    /// than being attributed to a function that is never inspected. The wrapped
    /// line is `None` outside any function; it is flattened to `0` here and
    /// re-expanded on exit, so a top-level statement is not attributed to a
    /// function that does not exist.
    fn enter_function(
        &mut self,
        attrs: &[syn::Attribute],
        sig: &syn::Signature,
    ) -> Option<(usize, bool)> {
        if is_test_or_helper_attr(attrs) {
            return None;
        }
        let line_number = sig.ident.span().start().line;
        let previous = (self.current_fn_line, self.current_fn_skipped);
        self.current_fn_line = Some(line_number);
        self.current_fn_skipped = false;
        Some((previous.0.unwrap_or(0), previous.1))
    }

    /// Restores the state [`Self::enter_function`] displaced, re-expanding the
    /// `0` sentinel into `None` so "no enclosing function" survives a round
    /// trip while a real function on line 0 remains impossible (syn lines are
    /// 1-based).
    fn exit_function(&mut self, previous: Option<(usize, bool)>) {
        if let Some((line_number, allowed)) = previous {
            self.current_fn_line = if line_number == 0 {
                None
            } else {
                Some(line_number)
            };
            self.current_fn_skipped = allowed;
        }
    }

    /// Reports a finding when one statement carries more than
    /// [`MAX_TRY_CHAIN`] `?` operators.
    ///
    /// Does nothing when no inspectable function encloses the statement — code
    /// outside any function, or inside an exempt one — because the snippet
    /// names the enclosing function's line and there is none to name.
    fn inspect_statement(&mut self, statement: &syn::Stmt) {
        let fn_line = match self.current_fn_line {
            Some(line_number) if !self.current_fn_skipped => line_number,
            _ => return,
        };
        let try_count = count_tries_in_stmt(statement);
        if try_count > MAX_TRY_CHAIN {
            let stmt_line = statement.span().start().line;
            self.hits.push(Hit {
                rule: "long-try-chain",
                line: stmt_line,
                snippet: format!(
                    "fn near line {fn_line} — statement contains {try_count} `?` operators without an intermediate `let`. Break the chain with named intermediate values."
                ),
            });
        }
    }
}

/// True when an attribute exempts its function from the try-chain rule:
/// `test`, or any name starting with `test_`.
///
/// The prefix rule is what lets a suite factor a chain out into `test_fixture`
/// helpers without the detector firing on the fixture itself.
fn is_test_or_helper_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attribute| {
        attribute.path().segments.last().is_some_and(|segment| {
            let segment_name = segment.ident.to_string();
            segment_name == "test" || segment_name.starts_with("test_")
        })
    })
}

impl<'ast> Visit<'ast> for TryChainWalker<'_> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let previous = self.enter_function(&node.attrs, &node.sig);
        visit::visit_item_fn(self, node);
        self.exit_function(previous);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let previous = self.enter_function(&node.attrs, &node.sig);
        visit::visit_impl_item_fn(self, node);
        self.exit_function(previous);
    }

    fn visit_stmt(&mut self, node: &'ast syn::Stmt) {
        self.inspect_statement(node);
        visit::visit_stmt(self, node);
    }
}

// ── Detector 5: tautological-doc ────────────────────────────────────────────
// Ported from keel-core/src/interpretability.rs: a public fn whose docstring
// paraphrases its name (half or more of its ≤6 content tokens share stems
// with the name tokens).

/// Words ignored when deciding whether a docstring paraphrases its function
/// name. Articles, prepositions, and copulas carry no behavior, so counting
/// them as shared stems would call `/// Returns this value` on `fn value`
/// tautological when it says nothing about what is returned.
const DOC_STOP_WORDS: &[&str] = &[
    "a", "an", "and", "at", "be", "by", "do", "for", "from", "if", "in", "is", "it", "of", "on",
    "or", "s", "that", "the", "this", "to", "was", "will", "with",
];

/// Splits a function name on `_` into lowercase tokens.
///
/// Empty segments are dropped, so a leading or doubled underscore (`__init`)
/// yields `init` rather than empty tokens that could never match anything.
fn tokenize_name(name: &str) -> Vec<String> {
    name.split('_')
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.to_lowercase())
        .collect()
}

/// Splits a docstring into its content tokens, with stop words removed.
///
/// An all-stop-word docstring yields an empty vector, which
/// [`TautWalker::check`] treats as no evidence rather than as a paraphrase.
fn tokenize_doc(doc: &str) -> Vec<String> {
    split_words(doc)
        .into_iter()
        .filter(|word| !DOC_STOP_WORDS.contains(&word.as_str()))
        .collect()
}

/// Splits text into lowercase alphanumeric words, discarding punctuation.
///
/// Splitting on any non-alphanumeric character is what makes `` `create` ``
/// and `create,` yield the same token, so quoting a parameter in a docstring
/// does not hide it from the paraphrase check.
fn split_words(doc: &str) -> Vec<String> {
    doc.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| word.to_lowercase())
        .collect()
}

/// Counts doc tokens that share a stem with some name token.
///
/// The relation is prefix-based in EITHER direction, so `creates` matches
/// `create` and `user` matches `users`. It over-counts by design: the check
/// only fires at a 0.5 ratio over at most six tokens, so a generous match makes
/// the detector accuse more readily, which is the direction keel chose.
fn shared_stem_count(doc_tokens: &[String], name_tokens: &[String]) -> usize {
    doc_tokens
        .iter()
        .filter(|doc_token| {
            name_tokens.iter().any(|name_token| {
                doc_token.starts_with(name_token) || name_token.starts_with(doc_token.as_str())
            })
        })
        .count()
}

/// True only for `pub` items. `pub(crate)` and `pub(super)` are not this
/// detector's concern: a docstring paraphrase costs a crate-internal reader
/// nothing like what it costs a published API's reader.
fn is_public(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Public(_))
}

/// Extracts one `#[doc = "..."]` attribute's text, and `None` for any other
/// attribute shape.
///
/// The value must be a string literal: `#[doc = concat!(..)]` is legal Rust and
/// answers `None`, which drops it from the concatenated doc rather than
/// guessing at its content.
fn doc_attr_value(attr: &syn::Attribute) -> Option<String> {
    if !attr.path().is_ident("doc") {
        return None;
    }
    let syn::Meta::NameValue(ref named) = attr.meta else {
        return None;
    };
    let syn::Expr::Lit(ref lit) = named.value else {
        return None;
    };
    let syn::Lit::Str(ref text) = lit.lit else {
        return None;
    };
    Some(text.value())
}

/// Joins every doc line of an item into one string, in source order.
///
/// An item with no doc attributes yields an empty string, which
/// [`TautWalker::check`] rejects by length before any tokenizing happens.
fn extract_doc_text(attrs: &[syn::Attribute]) -> String {
    attrs
        .iter()
        .filter_map(doc_attr_value)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Runs the tautological-doc detector over a whole file, accumulating findings
/// into the caller's `hits`.
fn detect_tautological_doc(file: &syn::File, hits: &mut Vec<Hit>) {
    let mut walker = TautWalker { hits };
    walker.visit_file(file);
}

/// Visit state for [`detect_tautological_doc`]: the findings accumulated.
struct TautWalker<'a> {
    /// Findings accumulated during the walk, in visit order.
    hits: &'a mut Vec<Hit>,
}

impl<'ast> Visit<'ast> for TautWalker<'_> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.check(&node.vis, &node.attrs, &node.sig);
        visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.check(&node.vis, &node.attrs, &node.sig);
        visit::visit_impl_item_fn(self, node);
    }
}

impl TautWalker<'_> {
    /// Records a finding when a public function's docstring is a paraphrase of
    /// its own name.
    ///
    /// Nothing is reported unless the doc has at least three bytes of text,
    /// yields at least one content token, and has at most six of them: a long
    /// docstring inevitably reuses a name token somewhere, so the ceiling is
    /// what keeps the check from firing on real documentation. The ratio is
    /// then measured as "at least half the content tokens share a stem with the
    /// name".
    fn check(&mut self, vis: &syn::Visibility, attrs: &[syn::Attribute], sig: &syn::Signature) {
        let line = sig.ident.span().start().line;
        if !is_public(vis) || is_test_attr(attrs) {
            return;
        }
        let doc = extract_doc_text(attrs);
        let doc = doc.trim();
        if doc.len() < 3 {
            return;
        }
        let tokens = tokenize_doc(doc);
        if tokens.is_empty() {
            return;
        }
        let name_tokens = tokenize_name(&sig.ident.to_string());
        let shared = shared_stem_count(&tokens, &name_tokens);
        // Both counts are word counts of one docstring, and `shared` never
        // exceeds `tokens.len()`. Converting through `u32` makes the `f64`
        // conversion exact instead of lossy; a count too large to fit is proof
        // the docstring already broke the six-token ceiling checked below, so
        // this early return is the one that guard would have taken.
        let Ok(shared_count) = u32::try_from(shared) else {
            return;
        };
        let Ok(token_count) = u32::try_from(tokens.len()) else {
            return;
        };
        let ratio = f64::from(shared_count) / f64::from(token_count);
        if ratio < 0.5 || tokens.len() > 6 {
            return;
        }
        self.hits.push(Hit {
            rule: "tautological-doc",
            line,
            snippet: format!(
                "pub fn {} — docstring is a paraphrase of the function name ({} of {} tokens share stems). Replace it with substantive behavioral information.",
                sig.ident, shared, tokens.len()
            ),
        });
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The workspace forbids `unwrap`/`expect` outright, with no test
    /// exemption, so a test propagates its failure with `?` rather than
    /// aborting the run. The message is carried in the error rather than in an
    /// added `.expect`.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Scans one fixture string.
    ///
    /// Returns the `ScanError` rather than asserting it away: a fixture that
    /// stops parsing must fail the test that uses it loudly, and a `Vec`
    /// returned on the error path would make every `!contains` assertion below
    /// pass for the wrong reason.
    fn scan(text: &str) -> Result<Vec<Hit>, ScanError> {
        scan_source(text, "test.rs")
    }

    fn rules(hits: &[Hit]) -> Vec<&'static str> {
        hits.iter().map(|hit| hit.rule).collect()
    }

    // ERROR-SWALLOW — RED then GREEN per spelling.

    #[test]
    fn wildcard_map_err_is_swallow() -> TestResult {
        let hits = scan("fn f() -> Result<(), E> { g().map_err(|_| E::Flat)?; Ok(()) }\n")?;
        assert!(rules(&hits).contains(&"ERROR-SWALLOW"), "{hits:?}");
        Ok(())
    }

    #[test]
    fn named_but_unread_map_err_is_swallow() -> TestResult {
        let hits = scan("fn f() -> Result<(), E> { g().map_err(|cause| E::Flat)?; Ok(()) }\n")?;
        assert!(rules(&hits).contains(&"ERROR-SWALLOW"), "{hits:?}");
        Ok(())
    }

    #[test]
    fn named_and_formatted_map_err_is_clean() -> TestResult {
        let hits = scan(
            "fn f() -> Result<(), E> { g().map_err(|cause| E::Msg(format!(\"{cause}\")))?; Ok(()) }\n",
        )?;
        assert!(!rules(&hits).contains(&"ERROR-SWALLOW"), "{hits:?}");
        Ok(())
    }

    #[test]
    fn unwrap_or_default_on_result_is_swallow() -> TestResult {
        let hits = scan("fn f() -> u32 { g().unwrap_or_default() }\n")?;
        assert!(rules(&hits).contains(&"ERROR-SWALLOW"), "{hits:?}");
        Ok(())
    }

    #[test]
    fn discarded_ok_is_swallow() -> TestResult {
        let hits = scan("fn f() { g().ok(); }\n")?;
        assert!(rules(&hits).contains(&"ERROR-SWALLOW"), "{hits:?}");
        Ok(())
    }

    #[test]
    fn let_underscore_binding_is_swallow() -> TestResult {
        let hits = scan("fn f() { let _ = g(); }\n")?;
        assert!(rules(&hits).contains(&"ERROR-SWALLOW"), "{hits:?}");
        Ok(())
    }

    #[test]
    fn swallow_inside_test_fn_is_exempt() -> TestResult {
        let hits = scan("#[test]\nfn f() { g().map_err(|_| E::Flat).unwrap(); }\n")?;
        assert!(!rules(&hits).contains(&"ERROR-SWALLOW"), "{hits:?}");
        Ok(())
    }

    // unlogged-err-return — RED then GREEN.

    #[test]
    fn bare_err_return_without_log_is_flagged() -> TestResult {
        let hits = scan(
            "fn f(x: bool) -> Result<(), E> {\n    if x {\n        return Err(E::Flat);\n    }\n    Ok(())\n}\n",
        )?;
        assert!(rules(&hits).contains(&"unlogged-err-return"), "{hits:?}");
        Ok(())
    }

    #[test]
    fn err_return_after_log_in_block_is_clean() -> TestResult {
        let hits = scan(
            "fn f(x: bool) -> Result<(), E> {\n    if x {\n        log::warn!(\"bad\");\n        return Err(E::Flat);\n    }\n    Ok(())\n}\n",
        )?;
        assert!(!rules(&hits).contains(&"unlogged-err-return"), "{hits:?}");
        Ok(())
    }

    #[test]
    fn err_return_after_eprintln_is_clean() -> TestResult {
        let hits = scan(
            "fn f(x: bool) -> Result<(), E> {\n    if x {\n        eprintln!(\"bad\");\n        return Err(E::Flat);\n    }\n    Ok(())\n}\n",
        )?;
        assert!(!rules(&hits).contains(&"unlogged-err-return"), "{hits:?}");
        Ok(())
    }

    // ALLOW-SILENCE — RED then GREEN.

    #[test]
    fn clippy_allow_is_silence() -> TestResult {
        let hits = scan("#[allow(clippy::too_many_arguments)]\npub fn f() {}\n")?;
        assert_eq!(rules(&hits), vec!["ALLOW-SILENCE"]);
        Ok(())
    }

    #[test]
    fn rustc_allow_is_not_silence() -> TestResult {
        assert!(scan("#[allow(dead_code)]\nstruct Unused;\n")?.is_empty());
        Ok(())
    }

    // long-try-chain — RED then GREEN.

    #[test]
    fn four_try_in_one_statement_is_a_chain() -> TestResult {
        let hits = scan("fn f() -> Result<(), E> {\n    g()?.h()?.i()?.j()?;\n    Ok(())\n}\n")?;
        assert!(rules(&hits).contains(&"long-try-chain"), "{hits:?}");
        Ok(())
    }

    #[test]
    fn three_try_in_one_statement_is_clean() -> TestResult {
        let hits = scan("fn f() -> Result<(), E> {\n    g()?.h()?.i()?;\n    Ok(())\n}\n")?;
        assert!(!rules(&hits).contains(&"long-try-chain"), "{hits:?}");
        Ok(())
    }

    #[test]
    fn try_in_let_is_not_counted() -> TestResult {
        let hits = scan(
            "fn f() -> Result<(), E> {\n    let a = g()?;\n    let b = a.h()?;\n    let c = b.i()?;\n    let d = c.j()?;\n    Ok(())\n}\n",
        )?;
        assert!(!rules(&hits).contains(&"long-try-chain"), "{hits:?}");
        Ok(())
    }

    // tautological-doc — RED then GREEN.

    #[test]
    fn paraphrase_doc_is_tautological() -> TestResult {
        let hits = scan("/// User\npub fn user() {}\n")?;
        assert!(rules(&hits).contains(&"tautological-doc"), "{hits:?}");
        Ok(())
    }

    #[test]
    fn behavioral_doc_is_clean() -> TestResult {
        let hits =
            scan("/// Opens the vault, creating it when `create` is set.\npub fn user() {}\n")?;
        assert!(!rules(&hits).contains(&"tautological-doc"), "{hits:?}");
        Ok(())
    }

    // Span lines land on real lines.

    #[test]
    fn finding_lines_match_source_lines() -> TestResult {
        let hits = scan(
            "fn a() {}\nfn b() -> Result<(), E> {\n    g().map_err(|_| E::Flat)?;\n    Ok(())\n}\n",
        )?;
        let hit = hits
            .iter()
            .find(|hit| hit.rule == "ERROR-SWALLOW")
            .ok_or("a discarded-error closure must be reported")?;
        assert_eq!(hit.line, 3);
        Ok(())
    }
}
