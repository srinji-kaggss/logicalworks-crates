//! One owner per check (#695): every gate in the estate has exactly one
//! implementation, named in `contract/GATE_OWNERSHIP.toml`.
//!
//! The table is data, and this test is the check that keeps it honest. For
//! every row it asserts three machine facts: the implementation file exists,
//! the entry is defined in exactly the files the rows naming it give (so no
//! second file anywhere under `crates/lgwks-deps/src` implements the same
//! check), and the binary is a declared `[[bin]]` target of `lgwks_deps`.
//! The `consumer` column is intent — the gate outside this repository that
//! calls the entry as a library — and is read by people, not by this test.
//!
//! Sources: the issue's acceptance (each check has exactly one implementation
//! in the estate, named in both repositories' docs) and the estate's own
//! `contract/CATALOG.toml` pattern (a register plus the lane that refuses a
//! row the tree no longer matches).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// What every test here returns.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// One row of the ownership table.
struct Row {
    /// The check's name.
    check: String,
    /// The single file implementing it, workspace-relative.
    file: String,
    /// The entry the binary and the library consumer share.
    entry: String,
    /// The `[[bin]]` target running it.
    binary: String,
}

/// A table row's value: `"..."` with no escapes, or a refusal naming the line.
fn value(line: &str, number: usize) -> Result<String, String> {
    let unquoted = line
        .split_once('=')
        .map(|(_, value)| value.trim())
        .filter(|value| value.starts_with('"') && value.ends_with('"') && value.len() >= 2)
        .ok_or_else(|| format!("line {number}: expected `key = \"value\"`, got {line:?}"))?;
    Ok(unquoted[1..unquoted.len().saturating_sub(1)].to_owned())
}

/// Reads the ownership rows: `[[gate]]` blocks of `check`, `file`, `entry`
/// and `binary` in that order, nothing else, no duplicates.
fn read_table(text: &str) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    let mut current: BTreeMap<String, String> = BTreeMap::new();
    let mut in_gate = false;
    for (offset, raw) in text.lines().enumerate() {
        let number = offset.saturating_add(1);
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[gate]]" {
            if in_gate {
                return refuse(format!("line {number}: a gate block opened inside one"));
            }
            in_gate = true;
            continue;
        }
        if !in_gate {
            return refuse(format!("line {number}: expected `[[gate]]`, got {line:?}"));
        }
        let (key, _) = line
            .split_once('=')
            .map(|(key, _)| (key.trim(), ()))
            .ok_or_else(|| format!("line {number}: expected `key = \"value\"`, got {line:?}"))?;
        if !["check", "file", "entry", "binary", "consumer"].contains(&key) {
            return refuse(format!("line {number}: unknown key {key:?}"));
        }
        if current.contains_key(key) {
            return refuse(format!("line {number}: duplicate key {key:?}"));
        }
        current.insert(key.to_owned(), value(line, number)?);
        if key == "consumer" {
            // `consumer` closes the row: intent is always stated, so a row
            // that forgets who calls the rule is incomplete rather than terse.
            let check = take(&mut current, "check", number)?;
            let file = take(&mut current, "file", number)?;
            let entry = take(&mut current, "entry", number)?;
            let binary = take(&mut current, "binary", number)?;
            rows.push(Row {
                check,
                file,
                entry,
                binary,
            });
            current.clear();
            in_gate = false;
        }
    }
    if in_gate {
        return refuse("the last gate block never closed with `consumer`".to_owned());
    }
    if rows.is_empty() {
        return refuse("the table names no gate".to_owned());
    }
    let mut checks = BTreeSet::new();
    for row in &rows {
        if !checks.insert(row.check.clone()) {
            return refuse(format!("two rows own {:?}", row.check));
        }
    }
    Ok(rows)
}

/// Takes `key` out of the row being read, refusing a row that never set it.
fn take(
    current: &mut BTreeMap<String, String>,
    key: &str,
    number: usize,
) -> Result<String, String> {
    match current.remove(key) {
        Some(value) => Ok(value),
        None => {
            lgwks_std::trace::debug!(key, number, "gate_ownership: a row is missing a key");
            Err(format!("line {number}: a gate row is missing {key:?}"))
        }
    }
}

/// Logs `reason` where the table refused it, then hands it back: a malformed
/// register is refused, never misread.
fn refuse(reason: String) -> Result<Vec<Row>, String> {
    lgwks_std::trace::debug!(reason = %reason, "gate_ownership: refusing a malformed table");
    Err(reason)
}

/// Every `.rs` file under `dir`, bounded: a tree is caller data and the walk
/// is bounded like every other input.
fn rust_files(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) -> TestResult {
    if depth == 0 {
        lgwks_std::trace::debug!(path = ?dir, "gate_ownership: the walk passed its bound");
        return Err(format!("{} is nested past the walk bound", dir.display()).into());
    }
    let mut entries: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        entries.push(entry?.path());
    }
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_files(&path, depth.saturating_sub(1), found)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
    Ok(())
}

/// Whether `text` defines `entry` as an unrestricted public function: the
/// mark the single-implementation rule looks for.
fn defines(text: &str, entry: &str) -> bool {
    text.contains(&format!("pub fn {entry}("))
}

#[test]
/// Every gate the table owns has exactly one implementation, and every binary
/// it names is a declared target.
fn sim_each_gate_has_exactly_one_implementation() -> TestResult {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let table = std::fs::read_to_string(root.join("contract/GATE_OWNERSHIP.toml"))?;
    let rows = read_table(&table).map_err(|reason| format!("GATE_OWNERSHIP.toml: {reason}"))?;
    let manifest = std::fs::read_to_string(root.join("crates/lgwks-deps/Cargo.toml"))?;
    let mut files = Vec::new();
    rust_files(&root.join("crates/lgwks-deps/src"), 8, &mut files)?;
    // Entry name to the files defining it, across the whole crate sources.
    let mut defined_in: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for path in &files {
        let text = std::fs::read_to_string(path)?;
        let relative = path
            .strip_prefix(&root)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .into_owned();
        for row in &rows {
            if defines(&text, &row.entry) {
                defined_in
                    .entry(row.entry.clone())
                    .or_default()
                    .insert(relative.clone());
            }
        }
    }
    for row in &rows {
        assert!(
            root.join(&row.file).is_file(),
            "{:?} names {}, which is not a file",
            row.check,
            row.file
        );
        let owners: BTreeSet<String> = rows
            .iter()
            .filter(|other| other.entry == row.entry)
            .map(|other| other.file.clone())
            .collect();
        let defining = match defined_in.get(&row.entry) {
            Some(files) => files.clone(),
            None => BTreeSet::new(),
        };
        assert_eq!(
            defining, owners,
            "{:?} (entry {:?}) is implemented outside its owning file(s) {owners:?}",
            row.check, row.entry
        );
        assert!(
            manifest.contains(&format!("name = \"{}\"", row.binary)),
            "{:?} names the binary {}, which is no [[bin]] target",
            row.check,
            row.binary
        );
    }
    Ok(())
}

#[test]
/// A malformed table is refused, not misread: the gate refuses a row it
/// cannot parse rather than owning a check it cannot name.
fn sim_a_malformed_table_is_refused() {
    assert!(
        read_table("[[gate]]\ncheck = \"x\"\n").is_err(),
        "an unclosed block is not a row"
    );
    assert!(
        read_table("[[gate]]\ncheck = \"x\"\nfile = \"f\"\nentry = \"e\"\nbinary = \"b\"\nconsumer = \"c\"\ncheck = \"y\"\n").is_err(),
        "a second row without its block is not parsed"
    );
    assert!(
        read_table("[[gate]]\ncheck = \"x\"\ncheck = \"y\"\nfile = \"f\"\nentry = \"e\"\nbinary = \"b\"\nconsumer = \"c\"\n").is_err(),
        "a duplicate key is not kept"
    );
    assert!(
        read_table("[[gate]]\ncheck = \"x\"\nfile = \"f\"\nentry = \"e\"\nbinary = \"b\"\nnope = \"c\"\nconsumer = \"c\"\n").is_err(),
        "an unknown key is not skipped"
    );
}
