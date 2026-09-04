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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
        match self {
            Self::Unparseable { path, cause } => write!(f, "{path}: cannot parse Rust: {cause}"),
        }
    }
}

impl Error for ScanError {}

/// Scans one Rust source text with all five detectors, ordered by line.
pub fn scan_source(source: &str, path: &str) -> Result<Vec<Hit>, ScanError> {
    let file = syn::parse_file(source).map_err(|cause| ScanError::Unparseable {
        path: path.to_string(),
        cause: cause.to_string(),
    })?;
    let mut hits = Vec::new();
    detect_error_swallow(source, &file, &mut hits);
    detect_unlogged_err(source, &file, &mut hits);
    detect_allow_silence(&file, &mut hits);
    detect_long_try_chain(&file, &mut hits);
    detect_tautological_doc(&file, &mut hits);
    hits.sort_by(|a, b| (a.line, a.rule).cmp(&(b.line, b.rule)));
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

fn collect_cfg_atoms(meta: &syn::Meta, atoms: &mut HashSet<String>) {
    match meta {
        syn::Meta::Path(path) if path.is_ident("test") => {}
        syn::Meta::List(list)
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
fn atom_key(meta: &syn::Meta) -> String {
    match meta {
        syn::Meta::Path(path) => path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
        syn::Meta::List(list) => {
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
        syn::Meta::NameValue(named) => {
            format!(
                "{}={}",
                atom_key_path(&named.path),
                named.value.to_token_string()
            )
        }
    }
}

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
    fn to_token_string(&self) -> String;
}

impl TokenString for syn::Expr {
    fn to_token_string(&self) -> String {
        match self {
            syn::Expr::Lit(lit) => match &lit.lit {
                syn::Lit::Str(s) => format!("{:?}", s.value()),
                syn::Lit::ByteStr(_) | syn::Lit::CStr(_) => "b\"..\"".to_string(),
                syn::Lit::Byte(_) => "b'..'".to_string(),
                syn::Lit::Char(c) => format!("{:?}", c.value()),
                syn::Lit::Int(i) => i.base10_digits().to_string(),
                syn::Lit::Float(f) => f.base10_digits().to_string(),
                syn::Lit::Bool(b) => b.value.to_string(),
                _ => "..".to_string(),
            },
            _ => "..".to_string(),
        }
    }
}

fn cfg_children(
    list: &syn::MetaList,
) -> Option<syn::punctuated::Punctuated<syn::Meta, syn::Token![,]>> {
    syn::punctuated::Punctuated::parse_terminated
        .parse2(list.tokens.clone())
        .ok()
}

fn eval_cfg(meta: &syn::Meta, test: bool, values: &HashMap<&str, bool>) -> bool {
    match meta {
        syn::Meta::Path(path) if path.is_ident("test") => test,
        syn::Meta::List(list) if list.path.is_ident("all") => eval_cfg_all(list, test, values),
        syn::Meta::List(list) if list.path.is_ident("any") => eval_cfg_any(list, test, values),
        syn::Meta::List(list) if list.path.is_ident("not") => {
            eval_cfg_not(meta, list, test, values)
        }
        _ => atom_value(meta, values),
    }
}

fn eval_cfg_all(list: &syn::MetaList, test: bool, values: &HashMap<&str, bool>) -> bool {
    cfg_children(list)
        .is_some_and(|children| children.iter().all(|child| eval_cfg(child, test, values)))
}

fn eval_cfg_any(list: &syn::MetaList, test: bool, values: &HashMap<&str, bool>) -> bool {
    cfg_children(list)
        .is_some_and(|children| children.iter().any(|child| eval_cfg(child, test, values)))
}

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

fn atom_value(meta: &syn::Meta, values: &HashMap<&str, bool>) -> bool {
    values.get(atom_key(meta).as_str()).copied().unwrap_or(true)
}

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

struct ErrorSwallowVisitor<'a> {
    hits: &'a mut Vec<Hit>,
}

fn discarded_error_closure(node: &syn::ExprMethodCall) -> Option<&'static str> {
    let method = match node.method.to_string().as_str() {
        "unwrap_or_else" => "unwrap_or_else",
        "map_err" => "map_err",
        "or_else" => "or_else",
        _ => return None,
    };
    let syn::Expr::Closure(closure) = node.args.first()? else {
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
fn error_binding_is_dropped(parameter: &syn::Pat, body: &syn::Expr) -> bool {
    match parameter {
        syn::Pat::Wild(_) => true,
        syn::Pat::Ident(bound) => !body_reads_ident(body, &bound.ident),
        _ => false,
    }
}

fn body_reads_ident(body: &syn::Expr, name: &syn::Ident) -> bool {
    let mut reader = IdentReader { name, found: false };
    reader.visit_expr(body);
    reader.found
}

struct IdentReader<'a> {
    name: &'a syn::Ident,
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
    fn record_discarded_ok(&mut self, statement: &syn::Stmt) {
        let syn::Stmt::Expr(syn::Expr::MethodCall(call), Some(_)) = statement else {
            return;
        };
        if call.method == "ok" {
            self.hits.push(Hit {
                rule: "ERROR-SWALLOW",
                line: call.method.span().start().line.max(1),
                snippet: ".ok(); discards Result value".to_string(),
            });
        }
    }

    fn record_wildcard_binding(&mut self, statement: &syn::Stmt) -> bool {
        let syn::Stmt::Local(local) = statement else {
            return false;
        };
        if !matches!(local.pat, syn::Pat::Wild(_)) {
            return false;
        }
        let Some(init) = &local.init else {
            return false;
        };
        self.hits.push(Hit {
            rule: "ERROR-SWALLOW",
            line: local.let_token.span().start().line.max(1),
            snippet: "let _ = ...; binds a value to `_` without inspecting it".to_string(),
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
                snippet: ".unwrap_or_default() silently drops error".to_string(),
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

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

/// True for a direct `Err(...)` construction: a call whose function path ends
/// in the `Err` segment.
fn is_direct_err_construction(expr: &syn::Expr) -> bool {
    let syn::Expr::Call(call_expr) = expr else {
        return false;
    };
    let syn::Expr::Path(func_path) = &*call_expr.func else {
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
        let current_line = index + 1;
        if current_line >= start_line
            && current_line <= line_number
            && contains_any(line, LOG_MARKERS)
        {
            return true;
        }
    }
    false
}

fn unlogged_err_hit(line_number: usize) -> Hit {
    Hit {
        rule: "unlogged-err-return",
        line: line_number,
        snippet: "return Err(...) with no log emission before it in the same block — the caller sees only a value, not a signal. Log the error with enough context to diagnose it. Line wrapping does not matter: the check is on statements, not physical lines.".to_string(),
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

fn stmt_is_log(stmt: &syn::Stmt) -> bool {
    let mac = match stmt {
        syn::Stmt::Macro(stmt_macro) => &stmt_macro.mac,
        syn::Stmt::Expr(syn::Expr::Macro(expr_macro), _) => &expr_macro.mac,
        _ => return false,
    };
    contains_any(&macro_marker_text(mac), LOG_MARKERS)
}

/// Control flow severs adjacency: a log before an `if` does not vouch for an
/// `Err` return after it.
fn stmt_is_control_flow(stmt: &syn::Stmt) -> bool {
    let syn::Stmt::Expr(expr, _) = stmt else {
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

fn stmt_direct_err_return_line(stmt: &syn::Stmt) -> Option<usize> {
    let syn::Stmt::Expr(syn::Expr::Return(ret), _) = stmt else {
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
fn block_logged_return_lines(file: &syn::File) -> HashSet<usize> {
    let mut walker = BlockWalker {
        logged_returns: HashSet::new(),
    };
    walker.visit_file(file);
    walker.logged_returns
}

struct BlockWalker {
    logged_returns: HashSet<usize>,
}

impl<'ast> Visit<'ast> for BlockWalker {
    fn visit_block(&mut self, block: &'ast syn::Block) {
        self.scan_statements(block);
        visit::visit_block(self, block);
    }
}

impl BlockWalker {
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

fn is_test_attr(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().segments.last().is_some_and(|segment| {
            let segment_name = segment.ident.to_string();
            segment_name == "test" || segment_name.starts_with("test_")
        })
    })
}

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

struct ErrReturnWalker<'a> {
    source: &'a str,
    hits: &'a mut Vec<Hit>,
    test_scope_stack: Vec<bool>,
    logged_returns: &'a HashSet<usize>,
}

impl ErrReturnWalker<'_> {
    fn enter_scope(&mut self, attrs: &[syn::Attribute]) {
        self.test_scope_stack.push(is_test_attr(attrs));
    }

    fn leave_scope(&mut self) {
        self.test_scope_stack.pop();
    }

    fn is_scope_disabled(&self) -> bool {
        *self.test_scope_stack.last().unwrap_or(&false)
    }

    fn check_return_expr(&mut self, node: &syn::ExprReturn) {
        if self.is_scope_disabled() {
            return;
        }
        let Some(inner_expr) = &node.expr else {
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

fn detect_allow_silence(file: &syn::File, hits: &mut Vec<Hit>) {
    let mut visitor = AllowSilenceVisitor { hits };
    visitor.visit_file(file);
}

struct AllowSilenceVisitor<'a> {
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

fn clippy_lints_allowed_by(attribute: &syn::Attribute) -> Vec<String> {
    if !attribute.path().is_ident("allow") {
        return Vec::new();
    }
    let syn::Meta::List(list) = &attribute.meta else {
        return Vec::new();
    };
    let Ok(lints) = list.parse_args_with(
        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
    ) else {
        return Vec::new();
    };
    lints.into_iter().filter_map(clippy_lint_name).collect()
}

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

const MAX_TRY_CHAIN: usize = 3;

fn detect_long_try_chain(file: &syn::File, hits: &mut Vec<Hit>) {
    let mut walker = TryChainWalker {
        hits,
        current_fn_line: None,
        current_fn_skipped: false,
    };
    walker.visit_file(file);
}

struct TryChainWalker<'a> {
    hits: &'a mut Vec<Hit>,
    current_fn_line: Option<usize>,
    current_fn_skipped: bool,
}

struct TryCounter {
    count: usize,
}

impl<'ast> Visit<'ast> for TryCounter {
    fn visit_expr_try(&mut self, node: &'ast syn::ExprTry) {
        self.count += 1;
        visit::visit_expr_try(self, node);
    }

    fn visit_expr_closure(&mut self, _closure: &'ast syn::ExprClosure) {}
}

fn count_tries_in_stmt(statement: &syn::Stmt) -> usize {
    let mut counter = TryCounter { count: 0 };
    match statement {
        syn::Stmt::Expr(expr, _) => counter.visit_expr(expr),
        syn::Stmt::Local(_) => {}
        _ => visit::visit_stmt(&mut counter, statement),
    }
    counter.count
}

impl TryChainWalker<'_> {
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

const DOC_STOP_WORDS: &[&str] = &[
    "a", "an", "and", "at", "be", "by", "do", "for", "from", "if", "in", "is", "it", "of", "on",
    "or", "s", "that", "the", "this", "to", "was", "will", "with",
];

fn tokenize_name(name: &str) -> Vec<String> {
    name.split('_')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

fn tokenize_doc(doc: &str) -> Vec<String> {
    split_words(doc)
        .into_iter()
        .filter(|s| !DOC_STOP_WORDS.contains(&s.as_str()))
        .collect()
}

fn split_words(doc: &str) -> Vec<String> {
    doc.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

fn shared_stem_count(doc_tokens: &[String], name_tokens: &[String]) -> usize {
    doc_tokens
        .iter()
        .filter(|t| {
            name_tokens
                .iter()
                .any(|n| t.starts_with(n) || n.starts_with(t.as_str()))
        })
        .count()
}

fn is_public(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Public(_))
}

fn doc_attr_value(attr: &syn::Attribute) -> Option<String> {
    if !attr.path().is_ident("doc") {
        return None;
    }
    let syn::Meta::NameValue(named) = &attr.meta else {
        return None;
    };
    let syn::Expr::Lit(lit) = &named.value else {
        return None;
    };
    let syn::Lit::Str(text) = &lit.lit else {
        return None;
    };
    Some(text.value())
}

fn extract_doc_text(attrs: &[syn::Attribute]) -> String {
    attrs
        .iter()
        .filter_map(doc_attr_value)
        .collect::<Vec<_>>()
        .join(" ")
}

fn detect_tautological_doc(file: &syn::File, hits: &mut Vec<Hit>) {
    let mut walker = TautWalker { hits };
    walker.visit_file(file);
}

struct TautWalker<'a> {
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
        let ratio = shared as f64 / tokens.len() as f64;
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

    fn scan(text: &str) -> Vec<Hit> {
        scan_source(text, "test.rs").expect("test source parses")
    }

    fn rules(hits: &[Hit]) -> Vec<&'static str> {
        hits.iter().map(|hit| hit.rule).collect()
    }

    // ERROR-SWALLOW — RED then GREEN per spelling.

    #[test]
    fn wildcard_map_err_is_swallow() {
        let hits = scan("fn f() -> Result<(), E> { g().map_err(|_| E::Flat)?; Ok(()) }\n");
        assert!(rules(&hits).contains(&"ERROR-SWALLOW"), "{hits:?}");
    }

    #[test]
    fn named_but_unread_map_err_is_swallow() {
        let hits = scan("fn f() -> Result<(), E> { g().map_err(|cause| E::Flat)?; Ok(()) }\n");
        assert!(rules(&hits).contains(&"ERROR-SWALLOW"), "{hits:?}");
    }

    #[test]
    fn named_and_formatted_map_err_is_clean() {
        let hits = scan(
            "fn f() -> Result<(), E> { g().map_err(|cause| E::Msg(format!(\"{cause}\")))?; Ok(()) }\n",
        );
        assert!(!rules(&hits).contains(&"ERROR-SWALLOW"), "{hits:?}");
    }

    #[test]
    fn unwrap_or_default_on_result_is_swallow() {
        let hits = scan("fn f() -> u32 { g().unwrap_or_default() }\n");
        assert!(rules(&hits).contains(&"ERROR-SWALLOW"), "{hits:?}");
    }

    #[test]
    fn discarded_ok_is_swallow() {
        let hits = scan("fn f() { g().ok(); }\n");
        assert!(rules(&hits).contains(&"ERROR-SWALLOW"), "{hits:?}");
    }

    #[test]
    fn let_underscore_binding_is_swallow() {
        let hits = scan("fn f() { let _ = g(); }\n");
        assert!(rules(&hits).contains(&"ERROR-SWALLOW"), "{hits:?}");
    }

    #[test]
    fn swallow_inside_test_fn_is_exempt() {
        let hits = scan("#[test]\nfn f() { g().map_err(|_| E::Flat).unwrap(); }\n");
        assert!(!rules(&hits).contains(&"ERROR-SWALLOW"), "{hits:?}");
    }

    // unlogged-err-return — RED then GREEN.

    #[test]
    fn bare_err_return_without_log_is_flagged() {
        let hits = scan(
            "fn f(x: bool) -> Result<(), E> {\n    if x {\n        return Err(E::Flat);\n    }\n    Ok(())\n}\n",
        );
        assert!(rules(&hits).contains(&"unlogged-err-return"), "{hits:?}");
    }

    #[test]
    fn err_return_after_log_in_block_is_clean() {
        let hits = scan(
            "fn f(x: bool) -> Result<(), E> {\n    if x {\n        log::warn!(\"bad\");\n        return Err(E::Flat);\n    }\n    Ok(())\n}\n",
        );
        assert!(!rules(&hits).contains(&"unlogged-err-return"), "{hits:?}");
    }

    #[test]
    fn err_return_after_eprintln_is_clean() {
        let hits = scan(
            "fn f(x: bool) -> Result<(), E> {\n    if x {\n        eprintln!(\"bad\");\n        return Err(E::Flat);\n    }\n    Ok(())\n}\n",
        );
        assert!(!rules(&hits).contains(&"unlogged-err-return"), "{hits:?}");
    }

    // ALLOW-SILENCE — RED then GREEN.

    #[test]
    fn clippy_allow_is_silence() {
        let hits = scan("#[allow(clippy::too_many_arguments)]\npub fn f() {}\n");
        assert_eq!(rules(&hits), vec!["ALLOW-SILENCE"]);
    }

    #[test]
    fn rustc_allow_is_not_silence() {
        assert!(scan("#[allow(dead_code)]\nstruct Unused;\n").is_empty());
    }

    // long-try-chain — RED then GREEN.

    #[test]
    fn four_try_in_one_statement_is_a_chain() {
        let hits = scan("fn f() -> Result<(), E> {\n    g()?.h()?.i()?.j()?;\n    Ok(())\n}\n");
        assert!(rules(&hits).contains(&"long-try-chain"), "{hits:?}");
    }

    #[test]
    fn three_try_in_one_statement_is_clean() {
        let hits = scan("fn f() -> Result<(), E> {\n    g()?.h()?.i()?;\n    Ok(())\n}\n");
        assert!(!rules(&hits).contains(&"long-try-chain"), "{hits:?}");
    }

    #[test]
    fn try_in_let_is_not_counted() {
        let hits = scan(
            "fn f() -> Result<(), E> {\n    let a = g()?;\n    let b = a.h()?;\n    let c = b.i()?;\n    let d = c.j()?;\n    Ok(())\n}\n",
        );
        assert!(!rules(&hits).contains(&"long-try-chain"), "{hits:?}");
    }

    // tautological-doc — RED then GREEN.

    #[test]
    fn paraphrase_doc_is_tautological() {
        let hits = scan("/// User\npub fn user() {}\n");
        assert!(rules(&hits).contains(&"tautological-doc"), "{hits:?}");
    }

    #[test]
    fn behavioral_doc_is_clean() {
        let hits =
            scan("/// Opens the vault, creating it when `create` is set.\npub fn user() {}\n");
        assert!(!rules(&hits).contains(&"tautological-doc"), "{hits:?}");
    }

    // Span lines land on real lines.

    #[test]
    fn finding_lines_match_source_lines() {
        let hits = scan(
            "fn a() {}\nfn b() -> Result<(), E> {\n    g().map_err(|_| E::Flat)?;\n    Ok(())\n}\n",
        );
        let hit = hits
            .iter()
            .find(|hit| hit.rule == "ERROR-SWALLOW")
            .expect("one swallow");
        assert_eq!(hit.line, 3);
    }
}
