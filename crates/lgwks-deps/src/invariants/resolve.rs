//! Reference resolution: does each registered claim name a mechanism that
//! exists in *this* repository — a lint declared at a level that can fail, a
//! file that declares a test a runner selects, a module some `mod` reaches —
//! and the entry point that reads the register and resolves it.
//!
//! Split out of `invariants.rs` by responsibility; `audit` and `check` are
//! re-exported there, so their public paths are unchanged.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::{
    Audit, EnforcedBy, Enforcement, Entry, INVARIANTS_PATH, InvariantError, LintNamespace, Outcome,
    Refusal, Register, Status, binding, valid_lint_name,
};

/// Whether a register id follows the workspace's `INV-<SCOPE>-<SLUG>` form.
fn valid_id(id: &str) -> bool {
    let mut parts = id.split('-');
    parts.next() == Some("INV")
        && parts.next().is_some_and(valid_id_word)
        && parts.next().is_some_and(valid_id_word)
        && parts.all(valid_id_word)
}

/// Whether one scope or slug component uses the uppercase identifier alphabet.
fn valid_id_word(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
}

/// Cargo treats hyphens and underscores as equivalent in package names.
fn normalise(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('-', "_")
}

/// One lint a manifest declares, with the level it declares.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DeclaredLint {
    /// Namespace the lint belongs to, from the `[lints]` table name.
    namespace: LintNamespace,
    /// Lint name, without the namespace.
    name: String,
    /// Level the manifest declares, verbatim.
    level: String,
}

/// Whether a declared level can fail a build.
///
/// `allow` cannot, and neither can a level this gate does not recognise —
/// fail-closed reads an unknown level as unable to fail, which is the reading
/// that refuses rather than the one that passes.
fn is_active_level(level: &str) -> bool {
    matches!(level, "forbid" | "deny" | "warn")
}

/// Collects every lint declared by the root manifest and every member manifest.
///
/// Lints are collected from the whole workspace rather than from the scoped
/// package alone, because Cargo's `[lints] workspace = true` inheritance means
/// the root table applies to members, and a member's own table applies to
/// itself. A manifest that cannot be read contributes nothing: an unreadable
/// manifest therefore makes its lints *not* declared, which refuses rather than
/// passes.
fn declared_lints(root: &Path, members: &[crate::metadata::Member]) -> Vec<DeclaredLint> {
    let mut lints = Vec::new();
    let mut manifests = vec![root.join("Cargo.toml")];
    for member in members {
        manifests.push(member.manifest_dir.join("Cargo.toml"));
    }
    for manifest in manifests {
        if let Ok(text) = std::fs::read_to_string(&manifest) {
            collect_lints(&text, &mut lints);
        }
    }
    lints
}

/// Reads `[lints.<namespace>]` and `[workspace.lints.<namespace>]` tables.
fn collect_lints(text: &str, out: &mut Vec<DeclaredLint>) {
    let mut namespace = None;
    for line in text.lines() {
        let trimmed = strip_comment(line).trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(header) = trimmed
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            namespace = lint_table(header.trim());
            continue;
        }
        let Some(namespace) = namespace else { continue };
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let name = key.trim();
        if !valid_lint_name(name) {
            continue;
        }
        out.push(DeclaredLint {
            namespace,
            name: name.to_owned(),
            level: lint_level(value.trim()),
        });
    }
}

/// Maps a table header onto the lint namespace it declares.
fn lint_table(header: &str) -> Option<LintNamespace> {
    // `[workspace.lints.clippy]` and `[lints.clippy]` declare one namespace, the
    // second inherited by every member: reading the table name off either
    // spelling is the equivalence, and both arms are written because a manifest
    // may use either.
    let rest = match header.strip_prefix("workspace.") {
        Some(rest) => rest,
        None => header,
    };
    LintNamespace::parse_table(rest.strip_prefix("lints.")?)
}

/// Extracts a declared level from either the shorthand or the table form.
///
/// `unwrap_used = "forbid"` and `unwrap_used = { level = "forbid" }` are both
/// legal Cargo, and a reader that understood only the first would report a
/// declared lint as missing. An unrecognised shape yields the empty string,
/// which [`is_active_level`] reads as unable to fail.
fn lint_level(value: &str) -> String {
    let unquoted = value.trim().trim_matches('"');
    let Some(table) = unquoted.strip_prefix('{') else {
        return unquoted.to_owned();
    };
    let Some(after) = table.split_once("level") else {
        return String::new();
    };
    let Some((_, tail)) = after.1.split_once('=') else {
        return String::new();
    };
    let tail = tail.trim();
    // The level ends at the first delimiter, or runs to the end of the tail when
    // the writer closed the table on the same line: both are values Cargo wrote.
    let end = match tail.find([',', '}']) {
        Some(at) => at,
        None => tail.len(),
    };
    tail[..end].trim().trim_matches('"').to_owned()
}

/// Returns `line` up to a `#` that is outside a quoted string.
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    for (index, character) in line.char_indices() {
        match character {
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..index],
            _ => {}
        }
    }
    line
}

/// Resolves each registered invariant against the repository.
///
/// This is a reference-resolution pass and nothing more. It executes no lint
/// and no test; see [`SCOPE`](super::SCOPE) for the sentence every caller must
/// report beside a verdict.
#[must_use]
pub fn audit(register: &Register, root: &Path, members: &[crate::metadata::Member]) -> Audit {
    let lints = declared_lints(root, members);
    let mut refusals = Vec::new();
    let mut outcomes = Vec::with_capacity(register.entries.len());
    let mut seen_ids = BTreeSet::new();
    for entry in &register.entries {
        let before = refusals.len();
        if !valid_id(&entry.id) {
            refusals.push(Refusal::MalformedId {
                id: entry.id.clone(),
                line: entry.line,
            });
        }
        if !seen_ids.insert(entry.id.clone()) {
            refusals.push(Refusal::DuplicateId {
                id: entry.id.clone(),
                line: entry.line,
            });
        }
        check_scope(entry, members, &mut refusals);
        check_enforcer(entry, root, &lints, &mut refusals);
        // Only an entry that is otherwise sound is asked to bind: a run recorded
        // against an enforcer that does not resolve is already refused, and a
        // second refusal for the same entry would hide which one to fix first.
        if let Some(ref evidence) = entry.evidence
            && refusals.len() == before
            && let Err(gap) = binding::bind(root, evidence.revision(), entry.enforced_by.as_ref())
        {
            refusals.push(Refusal::EvidenceNotBound {
                id: entry.id.clone(),
                revision: evidence.revision().to_owned(),
                gap,
                line: entry.line,
            });
        }
        let status = match refusals.len() {
            len if len > before => Status::Refused,
            _ if entry.evidence.is_some() => Status::Attested,
            _ => Status::Resolved,
        };
        outcomes.push(Outcome {
            id: entry.id.clone(),
            status,
            line: entry.line,
        });
    }
    refusals.sort_by_key(ToString::to_string);
    Audit { refusals, outcomes }
}

/// Refuses a scope that is not a workspace package, or names a module no `mod`
/// item in that package reaches.
fn check_scope(entry: &Entry, members: &[crate::metadata::Member], refusals: &mut Vec<Refusal>) {
    // `split_once` states both shapes: a scope with no `::` is a crate name and
    // carries no module path, and one with `::` names the crate before it.
    let (crate_name, modules) = match entry.scope.split_once("::") {
        Some((crate_name, modules)) => (crate_name, modules),
        None => (entry.scope.as_str(), ""),
    };
    let Some(member) = members
        .iter()
        .find(|member| normalise(&member.name) == normalise(crate_name))
    else {
        refusals.push(Refusal::ScopeNotInWorkspace {
            id: entry.id.clone(),
            scope: entry.scope.clone(),
            line: entry.line,
        });
        return;
    };
    let modules: Vec<&str> = modules
        .split("::")
        .filter(|segment| !segment.is_empty())
        .collect();
    if modules.is_empty() {
        return;
    }
    if resolve_module(&member.manifest_dir, &modules).is_none() {
        refusals.push(Refusal::ScopeModuleNotFound {
            id: entry.id.clone(),
            scope: entry.scope.clone(),
            module: modules.join("::"),
            line: entry.line,
        });
    }
}

/// Whether the enforcer file at `path` declares a test a runner selects.
///
/// A file that exists but cannot be read declares nothing this gate can see,
/// so it is answered as declaring no test — the refusing direction — and the
/// read error is traced so the refusal can be told apart from an empty file.
fn file_declares_active_test(path: &Path) -> bool {
    match std::fs::read_to_string(path) {
        Ok(source) => declares_active_test(&source),
        Err(error) => {
            lgwks_std::trace::debug!(error = ?error, path = ?path, "file_declares_active_test: the enforcer could not be read");
            false
        }
    }
}

/// Refuses an enforcement reference that does not resolve to a real mechanism.
fn check_enforcer(entry: &Entry, root: &Path, lints: &[DeclaredLint], refusals: &mut Vec<Refusal>) {
    match entry.enforced_by {
        None => refusals.push(Refusal::MissingEnforcedBy {
            id: entry.id.clone(),
            line: entry.line,
        }),
        Some(EnforcedBy::Lint {
            namespace,
            ref name,
        }) => {
            let selector = format!("{namespace}::{name}");
            let declared = lints
                .iter()
                .find(|lint| lint.namespace == namespace && lint.name == *name);
            match declared {
                None => refusals.push(Refusal::LintNotDeclared {
                    id: entry.id.clone(),
                    lint: selector,
                    line: entry.line,
                }),
                Some(lint) if !is_active_level(&lint.level) => {
                    refusals.push(Refusal::LintDisabled {
                        id: entry.id.clone(),
                        lint: selector,
                        level: lint.level.clone(),
                        line: entry.line,
                    });
                }
                Some(_) => {}
            }
        }
        Some(EnforcedBy::File(ref relative)) => {
            let path = root.join(relative.as_path());
            if !path.is_file() {
                refusals.push(Refusal::EnforcedByNotFound {
                    id: entry.id.clone(),
                    enforced_by: relative.to_string(),
                    path,
                    line: entry.line,
                });
            } else if entry.enforcement == Enforcement::StaticCheck
                && !file_declares_active_test(&path)
            {
                // A file is only a check if it declares a test item a runner
                // would select. A monitor is proved by its recorded run, which
                // `build_evidence` has already required.
                refusals.push(Refusal::EnforcerIsNotATest {
                    id: entry.id.clone(),
                    path,
                    line: entry.line,
                });
            }
        }
    }
}

/// Resolves `segments` to the source file backing them, or `None`.
///
/// The crate root must exist, segment zero must be a module of the crate root,
/// and each later segment a module of the one before it. Requiring the `mod`
/// declaration as well as the file is the point: a stray `.rs` file that
/// nothing reaches is not a module a scope can claim authority over.
fn resolve_module(member_dir: &Path, segments: &[&str]) -> Option<PathBuf> {
    let mut current = crate_root(member_dir)?;
    for (index, segment) in segments.iter().enumerate() {
        let directory = if index == 0 {
            member_dir.join("src")
        } else {
            module_directory(&current)
        };
        let candidate = module_file(&directory, segment)?;
        let declaring = std::fs::read_to_string(&current).ok()?;
        if !declares_module(&declaring, segment) {
            return None;
        }
        current = candidate;
    }
    Some(current)
}

/// The crate root source file, preferring the library over the binary.
fn crate_root(member_dir: &Path) -> Option<PathBuf> {
    ["src/lib.rs", "src/main.rs"]
        .iter()
        .map(|relative| member_dir.join(relative))
        .find(|path| path.is_file())
}

/// The directory a module's own children live in.
///
/// A source path with no parent is a bare file name, whose children live
/// beside it in the current directory: the empty path is that directory.
fn module_directory(source: &Path) -> PathBuf {
    let parent = match source.parent() {
        Some(parent) => parent.to_path_buf(),
        None => PathBuf::new(),
    };
    match source.file_stem() {
        Some(stem) if stem != OsStr::new("mod") => parent.join(stem),
        _ => parent,
    }
}

/// The file backing a child module, when one exists.
fn module_file(directory: &Path, segment: &str) -> Option<PathBuf> {
    let flat = directory.join(format!("{segment}.rs"));
    if flat.is_file() {
        return Some(flat);
    }
    let nested = directory.join(segment).join("mod.rs");
    if nested.is_file() {
        return Some(nested);
    }
    None
}

/// Whether `source` declares `name` as a module.
pub(super) fn declares_module(source: &str, name: &str) -> bool {
    source.lines().any(|line| {
        let trimmed = strip_item_prefix(line.trim());
        let Some(rest) = trimmed.strip_prefix("mod") else {
            return false;
        };
        if !rest.starts_with(char::is_whitespace) {
            return false;
        }
        let Some(rest) = rest.trim_start().strip_prefix(name) else {
            return false;
        };
        let rest = rest.trim_start();
        rest.starts_with(';') || rest.starts_with('{')
    })
}

/// Strips the attributes and item qualifiers a declaration may carry.
///
/// `pub mod verb;` and `pub(crate) fn helper()` are ordinary Rust, and a scan
/// that required the bare keyword would report an enforcing module as absent.
/// `#[path = "x.rs"] mod ghost;` is the same defect one attribute further out,
/// so a leading attribute group is stepped over too. String and byte-string
/// literals are stepped over so a `pub` inside one does not confuse the walk.
fn strip_item_prefix(line: &str) -> &str {
    let mut rest = line.trim_start();
    loop {
        if let Some((_, after)) = split_attribute(rest) {
            rest = after.trim_start();
            continue;
        }
        if let Some(after) = rest.strip_prefix("pub") {
            let after = after.trim_start();
            if let Some(inside) = after.strip_prefix('(') {
                match inside.find(')') {
                    Some(close) => {
                        rest = inside[close.saturating_add(1)..].trim_start();
                        continue;
                    }
                    None => return rest,
                }
            }
            rest = after;
            continue;
        }
        if let Some((after, _width)) = skip_keyword(rest, "extern") {
            // `extern "C" fn f()` names an ABI and `extern fn f()` does not; both
            // are steppable, and an ABI this scanner cannot read is stepped past
            // as one word rather than left in the prefix.
            rest = match skip_literal(after) {
                Some(after_literal) => after_literal,
                None => after,
            }
            .trim_start();
            continue;
        }
        let mut advanced = false;
        for keyword in ["unsafe", "async", "const", "default", "move"] {
            if let Some(after) = rest.strip_prefix(keyword)
                && after.starts_with(char::is_whitespace)
            {
                rest = after.trim_start();
                advanced = true;
            }
        }
        if !advanced {
            return rest;
        }
    }
}

/// Splits `keyword` off `rest` when it is a whole word, with its byte length.
fn skip_keyword<'a>(rest: &'a str, keyword: &str) -> Option<(&'a str, usize)> {
    let after = rest.strip_prefix(keyword)?;
    if after.starts_with(char::is_whitespace) {
        Some((after.trim_start(), keyword.len()))
    } else {
        None
    }
}

/// Steps over a leading string or byte-string literal.
fn skip_literal(rest: &str) -> Option<&str> {
    let body = rest
        .strip_prefix('"')
        .or_else(|| rest.strip_prefix("r\""))?;
    let close = body.find('"')?;
    Some(&body[close.saturating_add(1)..])
}

/// Whether a file declares a test item a runner would actually select.
///
/// A `#[test]` under `#[ignore]` never runs, and an attribute group that
/// introduces an item other than a function — the `#[cfg(test)] mod tests`
/// wrapper itself — is not a test. A condition the gate cannot evaluate is
/// treated as unsatisfied: `#[cfg(feature = "never")]` above or beside the
/// `#[test]` may compile the function out of every configuration CI builds, so
/// the honest answer is that no run is known to select it. A file with no
/// selectable test item is not a check that can fail, which is what makes it
/// refuse rather than pass.
pub(super) fn declares_active_test(source: &str) -> bool {
    let lines: Vec<&str> = source.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let Some(marker) = attribute_group(line) else {
            continue;
        };
        if !is_test_marker(&marker) {
            continue;
        }
        let mut unselected = is_unselected_marker(&marker);
        // Conditions stacked above the marker decide selection just as the ones
        // below it do: `#[cfg(feature = "never")]` is conventionally written
        // immediately above the item it gates.
        let mut above = index;
        while let Some(previous) = above.checked_sub(1).and_then(|at| lines.get(at)) {
            let Some(group) = attribute_group(previous) else {
                break;
            };
            if is_unselected_marker(&group) {
                unselected = true;
            }
            above = above.saturating_sub(1);
        }
        let mut cursor = index.saturating_add(1);
        while let Some(next) = lines.get(cursor).and_then(|next| attribute_group(next)) {
            if is_ignore_marker(&next) || is_unselected_marker(&next) {
                unselected = true;
            }
            cursor = cursor.saturating_add(1);
        }
        let Some(item) = lines.get(cursor) else {
            continue;
        };
        if unselected {
            continue;
        }
        let item = strip_item_prefix(item.trim());
        if item.starts_with("fn") && item["fn".len()..].starts_with(char::is_whitespace) {
            return true;
        }
    }
    false
}

/// The text inside a leading `#[...]` attribute, brackets balanced.
fn attribute_group(line: &str) -> Option<String> {
    split_attribute(line).map(|(inner, _)| inner.trim().to_owned())
}

/// The character that closes an attribute group. One byte, so the byte after the
/// `]` `split_attribute` finds is the start of the text that follows it.
const CLOSE_BRACKET: char = ']';

/// Splits a leading `#[...]` attribute group off `line`.
///
/// Returns the group's body and the text after the closing bracket, so a caller
/// that must step over an attribute and one that must read it share one
/// bracket-depth-aware scanner rather than two that can drift.
fn split_attribute(line: &str) -> Option<(&str, &str)> {
    let body = line.trim_start().strip_prefix("#[")?;
    let mut depth = 1usize;
    for (offset, character) in body.char_indices() {
        match character {
            '[' => depth = depth.saturating_add(1),
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    // `char_indices` yields only boundary offsets and `]` is one
                    // byte, so both cuts are on a boundary by construction: the
                    // split is total and needs no substituted empty slice.
                    let (inner, after) = body.split_at(offset);
                    return Some((inner, &after[CLOSE_BRACKET.len_utf8()..]));
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether an attribute group marks a test item.
fn is_test_marker(text: &str) -> bool {
    text == "test"
        || text.ends_with("::test")
        || (text.starts_with("cfg(") && text.contains("test"))
}

/// Whether an attribute group gates an item on a condition this gate cannot
/// evaluate.
///
/// Only the predicate `test` is known to hold — the test profile is what runs
/// the item — and it is accepted bare or as a single-argument `all`/`any`.
/// Everything else is read as unsatisfied, including predicates that merely
/// mention `test`: `cfg(not(test))` is the *opposite* of selected, and a
/// feature named `test` is a feature. Reading the whole predicate rather than a
/// token inside it is what makes those distinguishable. The price is a
/// conjunctive predicate such as `cfg(all(test, unix))`, refused although a
/// Unix test profile would select it; refusing is the direction this gate is
/// allowed to err in.
fn is_unselected_marker(text: &str) -> bool {
    let Some(predicate) = text
        .strip_prefix("cfg(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return false;
    };
    let compact: String = predicate
        .chars()
        .filter(|character: &char| !character.is_whitespace())
        .collect();
    !matches!(compact.as_str(), "test" | "all(test)" | "any(test)")
}

/// Whether an attribute group keeps a test item out of a default run.
fn is_ignore_marker(text: &str) -> bool {
    text == "ignore"
        || text.ends_with("::ignore")
        || text.starts_with("ignore(")
        || text.starts_with("ignore=")
}

// ── Entry point ─────────────────────────────────────────────────────────────

/// Audits the optional invariant register at `root`.
///
/// Absence is deliberately `Ok(None)`: adding this register must not change
/// the existing dependency gate for repositories that have not authored one.
///
/// Workspace members come from Cargo metadata, so an unconfirmed capture
/// cleanup after a complete read is carried beside the audit; see
/// [`crate::metadata::Collected`].
pub fn check(
    root: &Path,
) -> Result<Option<crate::metadata::Collected<(Register, Audit)>>, InvariantError> {
    let path = root.join(INVARIANTS_PATH);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(cause) => {
            let refusal = Err(InvariantError::Unreadable { path, cause });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "check: returning an error to the caller");
            return refusal;
        }
    };
    let register = Register::parse(&text)?;
    let members = crate::metadata::workspace_members(root).map_err(InvariantError::Metadata)?;
    Ok(Some(members.map(|members| {
        let audit = audit(&register, root, &members);
        (register, audit)
    })))
}
