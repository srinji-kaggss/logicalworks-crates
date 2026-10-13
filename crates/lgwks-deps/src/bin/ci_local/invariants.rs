//! The `invariants` lane: every `enforced by:` reference in the invariant
//! registers resolves to a real test, module, lane or script.
//!
//! An invariant that names its own enforcement is a claim a reader can check.
//! Nothing verified it: a renamed or deleted test left the sentence reading
//! exactly as authoritative as one pointing at a passing test. The references
//! are checked here against the module paths and items the sources define.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::builtins::{crate_directories, head, pass, refuse, tail};
use super::lanes::is_executable_file;
use super::modules::{Modules, collect_module_paths};
use super::toml_subset::{read_tables, text};
use super::{Context, GateError, Verdict, read};

/// Compares crate and module paths the way Cargo compares crate names.
///
/// A crate directory is `lgwks-std` while every Rust path spells it `lgwks_std`;
/// treating those as different modules would refuse every reference.
fn normalise_path(path: &str) -> String {
    path.to_lowercase().replace('-', "_")
}

/// The last `::` segment of a reference.
fn leaf_of(reference: &str) -> &str {
    match reference.rsplit("::").next() {
        Some(leaf) if !leaf.is_empty() => leaf,
        Some(_) | None => "",
    }
}

/// Shallowest module path first, then alphabetical.
fn by_depth(left: &str, right: &str) -> std::cmp::Ordering {
    left.matches("::")
        .count()
        .cmp(&right.matches("::").count())
        .then_with(|| left.cmp(right))
}

/// Module paths at which `reference` could resolve, shallowest first.
///
/// A qualified reference names its module path, so the leaf is resolved only
/// against a module path whose tail *is* the path the reference named. A bare name
/// carries no path to check, so it is matched anywhere.
fn resolution_candidates(modules: &Modules, reference: &str) -> Vec<String> {
    let leaf = leaf_of(reference);
    let wanted = match reference.rsplit_once("::") {
        Some((front, _)) => normalise_path(front),
        None => String::new(),
    };
    let bare = !reference.contains("::");
    let mut matches: Vec<String> = modules
        .iter()
        .filter(|entry| entry.1.contains(leaf))
        .filter(|entry| {
            let normalised = normalise_path(entry.0);
            bare || normalised == wanted || normalised.ends_with(&format!("::{wanted}"))
        })
        .map(|entry| entry.0.clone())
        .collect();
    matches.sort_by(|left, right| by_depth(left, right));
    matches
}

/// The register's files: the root `INVARIANTS.md`, then each crate's own.
///
/// One register split by crate under the 120-line `.md` law: a crate file the lane
/// did not read would be a rule nobody checks.
fn invariant_registers(root: &Path) -> Vec<PathBuf> {
    let mut files = vec![root.join("INVARIANTS.md")];
    files.extend(
        crate_directories(root)
            .into_iter()
            .map(|directory| directory.join("INVARIANTS.md"))
            .filter(|path| path.is_file()),
    );
    files
}

/// The invariant ids and bodies of `text`: each bullet runs to the next bullet or blank line.
fn bullets(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut at = 0_usize;
    while at < text.len() {
        let rest = tail(text, at);
        let on_line_start = at == 0 || head(text, at).ends_with('\n');
        let marker = rest.strip_prefix("- **INV-").filter(|_| on_line_start);
        let Some(after_marker) = marker else {
            match rest.find('\n') {
                Some(newline) => {
                    at = at.saturating_add(newline).saturating_add(1);
                    continue;
                }
                None => break,
            }
        };
        let run: String = after_marker
            .chars()
            .take_while(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || *ch == '-')
            .collect();
        let after_run = tail(after_marker, run.len());
        if run.is_empty() || !after_run.starts_with("**") {
            at = at.saturating_add(1);
            continue;
        }
        let body_start = text.len().saturating_sub(after_run.len()).saturating_add(2);
        let body_rest = tail(text, body_start);
        let end = [body_rest.find("\n- **"), body_rest.find("\n\n")].into_iter().flatten().min();
        let body = match end {
            Some(stop) => head(body_rest, stop),
            None => body_rest,
        };
        found.push((format!("INV-{run}"), body.to_owned()));
        at = body_start.saturating_add(body.len());
    }
    found
}

/// The ids of every `id = "…"` in the lane table text.
fn lane_ids(table: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for (index, _) in table.match_indices("id = \"") {
        let rest = tail(table, index.saturating_add(6));
        if let Some((name, _)) = rest.split_once('"')
            && !name.is_empty()
        {
            ids.insert(name.to_owned());
        }
    }
    ids
}

/// The backticked items of `clause`, in order.
fn backticked(clause: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut rest = clause;
    while let Some(open) = rest.find('`') {
        let after = tail(rest, open.saturating_add(1));
        let Some(close) = after.find('`') else {
            break;
        };
        if close == 0 {
            rest = after;
            continue;
        }
        items.push(head(after, close).to_owned());
        rest = tail(after, close.saturating_add(1));
    }
    items
}

/// Whether the text after `at` in `text` does not continue a word.
fn word_ends_at(text: &str, at: usize) -> bool {
    !tail(text, at).chars().next().is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
}

/// Whether the clause writes `` `reference` lane ``.
fn names_lane(clause: &str, reference: &str) -> bool {
    let needle = format!("`{reference}`");
    clause.match_indices(&needle).any(|(index, _)| {
        let after = tail(clause, index.saturating_add(needle.len()));
        let spaced = after.trim_start();
        spaced.len() < after.len() && spaced.starts_with("lane") && word_ends_at(spaced, 4)
    })
}

/// Whether the clause has a backtick followed by `lane` anywhere.
fn has_lane_marker(clause: &str) -> bool {
    clause.match_indices('`').any(|(index, _)| {
        let spaced = tail(clause, index.saturating_add(1)).trim_start();
        spaced.starts_with("lane") && word_ends_at(spaced, 4)
    })
}

/// Whether every character of `text` satisfies `allowed`, and there is at least one.
fn all_of(text: &str, allowed: impl Fn(char) -> bool) -> bool {
    !text.is_empty() && text.chars().all(allowed)
}

/// Whether `text` is `[A-Za-z0-9_]+(::[A-Za-z0-9_]+)+`.
fn is_qualified_path(text: &str) -> bool {
    let parts: Vec<&str> = text.split("::").collect();
    parts.len() >= 2 && parts.iter().all(|part| all_of(part, |ch| ch.is_ascii_alphanumeric() || ch == '_'))
}

/// Whether `text` is `[a-z0-9_]+`.
fn is_bare_name(text: &str) -> bool {
    all_of(text, |ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

/// Whether `text` is `[a-z0-9-]+`.
fn is_dashed_name(text: &str) -> bool {
    all_of(text, |ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

/// The `scripts/...` path of a script reference, with an optional interpreter.
fn script_path(reference: &str) -> Option<&str> {
    let path = match reference.strip_prefix("python3") {
        Some(after) => {
            let spaced = after.trim_start();
            if spaced.len() == after.len() {
                return None;
            }
            spaced
        }
        None => reference,
    };
    let file = path.strip_prefix("scripts/")?;
    if all_of(file, |ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-')) {
        Some(path)
    } else {
        None
    }
}

/// The `tests/...rs` or `src/...rs` path of a test-file reference.
fn test_file_path(reference: &str) -> Option<&str> {
    let inner = reference.strip_prefix("tests/").or_else(|| reference.strip_prefix("src/"))?;
    if inner.ends_with(".rs") && all_of(inner, |ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '/' | '-')) {
        Some(reference)
    } else {
        None
    }
}

/// Whether `text` holds a word of 7 to 40 lowercase hex digits, as a commit id does.
fn has_commit_id(text: &str) -> bool {
    text.split(|ch: char| !(ch.is_alphanumeric() || ch == '_')).any(|word| {
        (7..=40).contains(&word.len()) && word.chars().all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch))
    })
}

/// What the lane has learned while checking references.
struct Tally {
    /// How many references resolved or were checked.
    referenced: usize,
    /// The unresolved ones, as reported.
    missing: Vec<String>,
}

impl Tally {
    /// Counts one more checked reference.
    fn count(&mut self) {
        self.referenced = self.referenced.saturating_add(1);
    }
}

/// What a lane check reads: the tree, the lanes and the module index.
struct Evidence<'a> {
    /// The repository root.
    root: &'a Path,
    /// The lane ids.
    lanes: &'a BTreeSet<String>,
    /// The module index.
    modules: &'a Modules,
    /// The names of the binaries the workspace builds.
    binaries: &'a BTreeSet<String>,
}

/// The names of every `[[bin]]` any crate declares.
fn declared_binaries(root: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for directory in crate_directories(root) {
        let Ok(manifest) = std::fs::read_to_string(directory.join("Cargo.toml")) else {
            continue;
        };
        for table in read_tables(&manifest).iter().filter(|table| table.path == "bin" && table.array) {
            if let Some(name) = text(table, "name") {
                names.insert(name.to_owned());
            }
        }
    }
    names
}

/// Whether `script` is a retired Python script whose Rust binary of the same name
/// exists. The ban on Python replaced each gate script with a binary named after
/// it, so a register line that still cites `scripts/check-x.py` is citing the
/// binary `check-x`, which a lane runs.
fn has_successor_binary(evidence: &Evidence<'_>, script: &str) -> bool {
    let Some(file) = script.rsplit('/').next() else {
        return false;
    };
    match file.strip_suffix(".py") {
        Some(stem) => evidence.binaries.contains(stem),
        None => false,
    }
}

/// Reports a qualified or bare reference that names no item in its module path.
fn unresolved_item(evidence: &Evidence<'_>, name: &str, reference: &str) -> String {
    let leaf = leaf_of(reference);
    let mut elsewhere: Vec<&String> = evidence
        .modules
        .iter()
        .filter(|entry| entry.1.contains(leaf))
        .map(|entry| entry.0)
        .collect();
    elsewhere.sort_by(|left, right| by_depth(left, right));
    let shown: Vec<&str> = elsewhere.iter().take(4).map(|path| path.as_str()).collect();
    let detail = if shown.is_empty() {
        String::new()
    } else {
        format!("; it is defined in {}", shown.join(", "))
    };
    format!("{name}: `{reference}` names no item in the module path it names{detail}")
}

/// Checks one backticked `reference` of the invariant `name`.
fn check_reference(evidence: &Evidence<'_>, name: &str, reference: &str, clause: &str, tally: &mut Tally) {
    if evidence.lanes.contains(reference) && names_lane(clause, reference) {
        tally.count();
        return;
    }
    if has_lane_marker(clause) && is_dashed_name(reference) {
        tally.missing.push(format!("{name}: `{reference}` is named as a lane but is not in scripts/gate-lanes.toml"));
        return;
    }
    if is_qualified_path(reference) || is_bare_name(reference) {
        tally.count();
        if !evidence.lanes.contains(reference) && resolution_candidates(evidence.modules, reference).is_empty() {
            tally.missing.push(unresolved_item(evidence, name, reference));
        }
        return;
    }
    if let Some(lane) = reference.strip_suffix(" lane").filter(|front| is_dashed_name(front)) {
        tally.count();
        if !evidence.lanes.contains(lane) {
            tally.missing.push(format!("{name}: `{lane}` is not a lane in scripts/gate-lanes.toml"));
        }
        return;
    }
    if let Some(script) = script_path(reference) {
        tally.count();
        let target = evidence.root.join(script);
        if !target.exists() {
            if !has_successor_binary(evidence, script) {
                tally.missing.push(format!("{name}: {script} does not exist"));
            }
        } else if !is_executable_file(&target) {
            tally.missing.push(format!("{name}: {script} is not executable, so the gate cannot run it"));
        }
        return;
    }
    if let Some(suffix) = test_file_path(reference) {
        tally.count();
        if !crate_directories(evidence.root).iter().any(|directory| directory.join(suffix).exists()) {
            tally.missing.push(format!("{name}: {suffix} matches no test file under crates/"));
        }
        return;
    }
    let first: String = reference
        .chars()
        .take_while(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || *ch == '-')
        .collect();
    let spaced = tail(reference, first.len()).chars().next().is_some_and(char::is_whitespace);
    if !first.is_empty() && spaced && evidence.lanes.contains(&first) {
        tally.count();
    }
}

/// Every `**INV-…**` id in `text`.
fn ids_between_stars(text: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for (index, _) in text.match_indices("**INV-") {
        let rest = tail(text, index.saturating_add(6));
        let run: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || *ch == '-')
            .collect();
        if !run.is_empty() && tail(rest, run.len()).starts_with("**") {
            ids.insert(format!("INV-{run}"));
        }
    }
    ids
}

/// The ids the authored register `contract/INVARIANTS.toml` declares.
fn authored_ids(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| line.strip_prefix("id = \"").and_then(|rest| rest.split_once('"')))
        .filter(|found| found.0.starts_with("INV-"))
        .map(|found| found.0.to_owned())
        .collect()
}

/// Every `enforced by:` reference resolves to a real test, module, lane or script.
pub(super) fn invariants(context: &Context<'_>) -> Result<Verdict, GateError> {
    let root = context.root;
    let mut registers = Vec::new();
    for path in invariant_registers(root) {
        registers.push(read(&path)?);
    }
    let text = registers.join("\n\n");
    let modules = collect_module_paths(root)?;
    let lanes = lane_ids(&read(&root.join("scripts/gate-lanes.toml"))?);
    let binaries = declared_binaries(root);
    let evidence = Evidence {
        root,
        lanes: &lanes,
        modules: &modules,
        binaries: &binaries,
    };
    let mut tally = Tally {
        referenced: 0,
        missing: Vec::new(),
    };
    for (name, body) in bullets(&text) {
        // The prose wraps, so `enforced by:` can be split as `enforced\n  by:`.
        let flattened = body.split_whitespace().collect::<Vec<_>>().join(" ");
        let clause = match flattened.split_once("enforced by: ") {
            Some((_, rest)) => rest.to_owned(),
            None => String::new(),
        };
        if clause.trim().is_empty() && !has_commit_id(&flattened) {
            tally.missing.push(format!("{name}: no `enforced by:` clause and no `why:` commit to check"));
            continue;
        }
        for reference in backticked(&clause) {
            check_reference(&evidence, &name, reference.trim(), &clause, &mut tally);
        }
    }
    let authored = authored_ids(&read(&root.join("contract/INVARIANTS.toml"))?);
    let prose: BTreeSet<String> = ids_between_stars(&text)
        .into_iter()
        .filter(|id| id != "INV-DEP-EDGE-OWNED")
        .collect();
    let unregistered = prose.difference(&authored).count();
    let note = if unregistered == 0 {
        format!("all {} prose invariants are in contract/INVARIANTS.toml", prose.len())
    } else {
        format!(
            "{unregistered} of {} prose invariants are not yet in contract/INVARIANTS.toml, so no gate checks them",
            prose.len()
        )
    };
    if !tally.missing.is_empty() {
        return Ok(refuse(format!("INVARIANTS.md enforcement references do not resolve:\n{}", tally.missing.join("\n"))));
    }
    if tally.referenced == 0 {
        return Ok(refuse("INVARIANTS.md parsed zero enforcement references; the parser is broken".to_owned()));
    }
    Ok(pass(&format!(
        "{} INVARIANTS.md enforcement references resolve to real tests; {note}",
        tally.referenced
    )))
}

#[cfg(test)]
mod tests {
    use super::{backticked, bullets, has_commit_id, invariants, lane_ids, resolution_candidates};
    use crate::fixture::Fixture;
    use crate::{Context, Verdict};
    use crate::modules::collect_module_paths;
    use crate::process::Children;
    use std::collections::BTreeMap;
    use std::error::Error;
    use std::fs;
    use std::path::Path;

    type TestResult = Result<(), Box<dyn Error>>;

    /// The repository this binary was built from.
    fn repo() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// Writes one fixture file, making its directory.
    fn write_fixture(root: &Path, relative_path: &str, text: &str) -> Result<(), std::io::Error> {
        let path = root.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, text)
    }

    /// Runs the invariants lane against `root`.
    fn lane(root: &Path) -> Result<Verdict, Box<dyn Error>> {
        let children = Children::new(false);
        let env = BTreeMap::new();
        let context = Context {
            root,
            env: &env,
            children: &children,
            out: None,
        };
        Ok(invariants(&context)?)
    }

    /// A scratch tree with the given root register, empty lane and authored
    /// tables, and the given further files.
    fn tree_of(register: &str, files: &[(&str, &str)]) -> Result<Fixture, Box<dyn Error>> {
        let fixture = Fixture::claim(&std::env::temp_dir(), "lgwks-invariants-test")?;
        for entry in [("INVARIANTS.md", register), ("scripts/gate-lanes.toml", ""), ("contract/INVARIANTS.toml", "")] {
            write_fixture(fixture.path(), entry.0, entry.1)?;
        }
        for entry in files {
            write_fixture(fixture.path(), entry.0, entry.1)?;
        }
        Ok(fixture)
    }

    #[test]
    fn the_honest_register_resolves() -> TestResult {
        let verdict = lane(&repo())?;
        assert_eq!(verdict.code, 0, "the shipped INVARIANTS.md must resolve: {}", verdict.detail);
        Ok(())
    }

    #[test]
    fn a_real_function_in_the_wrong_module_is_refused() -> TestResult {
        let modules = collect_module_paths(&repo())?;
        assert_eq!(
            resolution_candidates(&modules, "lgwks_deps::metadata::lexical_join"),
            vec!["lgwks-deps::metadata".to_owned()],
            "a correct reference resolves"
        );
        assert!(
            resolution_candidates(&modules, "lgwks_deps::scan::cleanup_error").is_empty(),
            "a real leaf in the wrong module does not certify"
        );
        Ok(())
    }

    #[test]
    fn crate_directory_and_rust_path_spellings_are_one_module() -> TestResult {
        let modules = collect_module_paths(&repo())?;
        assert!(!resolution_candidates(&modules, "lgwks_std::hex::decode_into").is_empty(), "both spellings");
        assert!(
            !resolution_candidates(&modules, "strict_refuses_the_first_unreadable_directory").is_empty(),
            "a bare name"
        );
        Ok(())
    }

    #[test]
    fn an_unresolvable_reference_in_a_crate_register_fails_the_lane() -> TestResult {
        let fixture = tree_of(
            "",
            &[
                ("crates/demo/src/lib.rs", "fn real_case() {}\n"),
                ("crates/demo/INVARIANTS.md", "- **INV-DEMO-1** A rule. · enforced by: `no_such_case_anywhere`\n"),
            ],
        )?;
        let refused = lane(fixture.path())?;
        assert_eq!(refused.code, 1, "{}", refused.detail);
        assert!(refused.detail.contains("INV-DEMO-1: `no_such_case_anywhere`"), "{}", refused.detail);
        write_fixture(
            fixture.path(),
            "crates/demo/INVARIANTS.md",
            "- **INV-DEMO-1** A rule. · enforced by: `real_case`\n",
        )?;
        let resolved = lane(fixture.path())?;
        assert_eq!(resolved.code, 0, "{}", resolved.detail);
        Ok(())
    }

    #[test]
    fn a_retired_python_script_resolves_to_its_binary_only() -> TestResult {
        let fixture = tree_of(
            "- **INV-DEMO-2** A rule. · enforced by: `python3 scripts/check-demo.py`\n",
            &[("crates/demo/src/lib.rs", ""), ("crates/demo/Cargo.toml", "[package]\nname = \"demo\"\n")],
        )?;
        let without = lane(fixture.path())?;
        assert_eq!(without.code, 1, "no binary of that name: {}", without.detail);
        write_fixture(
            fixture.path(),
            "crates/demo/Cargo.toml",
            "[package]\nname = \"demo\"\n\n[[bin]]\nname = \"check-demo\"\npath = \"src/main.rs\"\n",
        )?;
        let with = lane(fixture.path())?;
        assert_eq!(with.code, 0, "the binary replaced the script: {}", with.detail);
        Ok(())
    }

    #[test]
    fn bullets_split_at_the_next_bullet_or_blank_line() {
        let found = bullets("- **INV-A-1** one\n  continued\n- **INV-B-2** two\n\nprose\n- **INV-C-3** three");
        let names: Vec<&str> = found.iter().map(|entry| entry.0.as_str()).collect();
        assert_eq!(names, vec!["INV-A-1", "INV-B-2", "INV-C-3"], "three bullets");
        assert_eq!(found.first().map(|entry| entry.1.contains("continued")), Some(true), "a wrapped body stays whole");
        assert_eq!(found.get(1).map(|entry| entry.1.contains("prose")), Some(false), "a blank line ends the body");
    }

    #[test]
    fn lanes_commits_and_backticks_are_read_as_the_lane_reads_them() {
        assert_eq!(lane_ids("id = \"a-b\"\nci_step = \"x\"\nid = \"c\"\n").len(), 2, "two lane ids");
        assert!(has_commit_id("fixed in 1a2b3c4d"), "a commit id");
        assert!(!has_commit_id("issue #160 and v1.2"), "not a commit id");
        assert_eq!(
            backticked("a `one` b `` c `two`"),
            vec!["one", " c "],
            "an empty pair is skipped and the scan resumes at its second tick, as a regex would"
        );
    }
}
