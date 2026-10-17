//! The public-surface ceiling (#526): `script!` is the one supported interface, so
//! the number of `pub` declarations a library file writes may fall and never rise.
//!
//! `contract/PUBLIC_SURFACE.toml` names, per library source file, how many `pub`
//! items it declares. A file with no entry is held to zero, so a new `pub` item in
//! a new file, or one more in an old file, is a finding. The count is exact: a
//! ceiling above the count is stale and is a finding too, so lowering a surface
//! lowers its line in the same change and the table never drifts from the tree.
//!
//! Counted: `pub` followed by an item keyword, in code (comments and literals are
//! blanked). Not counted: `pub(crate)` and the other restricted forms, struct
//! fields, and any item under `#[cfg(test)]` or `#[doc(hidden)]`. A `#[doc(hidden)]`
//! on `mod x;` hides that line only, not the file `x` declares: that file keeps its
//! count until its own ceiling is lowered, which the exactness rule makes a stated
//! change.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::lexer::{code_only, is_word};
use super::toml_subset::{raw, read_tables};
use super::{AuditError, fail, posix_relative, read};

/// The contract file, relative to the repository root.
const CONTRACT: &str = "contract/PUBLIC_SURFACE.toml";

/// The table of the contract that holds the ceilings.
const TABLE: &str = "ceilings";

/// What `pub` may introduce, and the modifiers that may stand between it and `fn`.
const ITEMS: [&str; 14] = [
    "fn", "struct", "enum", "trait", "type", "const", "static", "mod", "use", "union", "macro",
    "async", "unsafe", "extern",
];

/// The index of the `]` that closes the `[` at `open`, else the end of `chars`.
fn bracket_end(chars: &[char], open: usize) -> usize {
    let mut depth = 0_usize;
    for (index, ch) in chars.iter().enumerate().skip(open) {
        match *ch {
            '[' => depth = depth.saturating_add(1),
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return index;
                }
            }
            _ => {}
        }
    }
    chars.len()
}

/// The index just past the `}` that closes the `{` at `open`, else the end.
fn brace_end(chars: &[char], open: usize) -> usize {
    let mut depth = 0_usize;
    for (index, ch) in chars.iter().enumerate().skip(open) {
        match *ch {
            '{' => depth = depth.saturating_add(1),
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return index.saturating_add(1);
                }
            }
            _ => {}
        }
    }
    chars.len()
}

/// The index just past the item that starts at `from`: its braces, or the `;`
/// outside any parentheses and brackets (`fn f(a: [u8; 4]);` ends at the last one).
fn item_end(chars: &[char], from: usize) -> usize {
    let mut depth = 0_usize;
    for (index, ch) in chars.iter().enumerate().skip(from) {
        match *ch {
            '(' | '[' => depth = depth.saturating_add(1),
            ')' | ']' => depth = depth.saturating_sub(1),
            ';' if depth == 0 => return index.saturating_add(1),
            '{' if depth == 0 => return brace_end(chars, index),
            _ => {}
        }
    }
    chars.len()
}

/// Whether an attribute's text takes its item out of the count: `doc(hidden)`, or
/// a `cfg` that holds only under `test`. `cfg(any(test, ..))` and `cfg(not(test))`
/// compile in a shipped build, so they stay counted.
fn is_hidden(attribute: &str) -> bool {
    let squeezed: String = attribute.chars().filter(|ch| !ch.is_whitespace()).collect();
    if squeezed == "doc(hidden)" {
        return true;
    }
    squeezed.starts_with("cfg(")
        && !squeezed.contains("not(")
        && !squeezed.contains("any(")
        && squeezed
            .split(|ch: char| !is_word(ch))
            .any(|word| word == "test")
}

/// `code` with every test-only and hidden item blanked, offsets and lines kept.
fn visible(code: &str) -> String {
    let mut chars: Vec<char> = code.chars().collect();
    let mut index = 0_usize;
    while index < chars.len() {
        let opens =
            chars.get(index) == Some(&'#') && chars.get(index.saturating_add(1)) == Some(&'[');
        if !opens {
            index = index.saturating_add(1);
            continue;
        }
        let open = index.saturating_add(1);
        let close = bracket_end(&chars, open);
        let Some(inside) = chars.get(open.saturating_add(1)..close) else {
            break;
        };
        let body: String = inside.iter().collect();
        if is_hidden(&body) {
            let end = item_end(&chars, close.saturating_add(1));
            for slot in chars.iter_mut().take(end).skip(index) {
                if *slot != '\n' {
                    *slot = ' ';
                }
            }
            index = end;
        } else {
            index = close.saturating_add(1);
        }
    }
    chars.into_iter().collect()
}

/// Whether the `pub` at `index` of `text` introduces an unrestricted item.
fn introduces_item(text: &str, index: usize) -> bool {
    let before = text.get(..index).and_then(|head| head.chars().next_back());
    if before.is_some_and(|ch| is_word(ch) || ch == ':' || ch == '.') {
        return false;
    }
    let Some(rest) = text.get(index.saturating_add(3)..) else {
        return false;
    };
    let spaced = rest.trim_start();
    if spaced.len() == rest.len() {
        return false;
    }
    let word: String = spaced
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect();
    ITEMS.contains(&word.as_str())
}

/// How many `pub` declarations `source` makes outside test-only and hidden items.
fn declarations(source: &str) -> usize {
    let text = visible(&code_only(source));
    text.match_indices("pub")
        .filter(|&(index, _)| introduces_item(&text, index))
        .count()
}

/// Whether `relative` is a library source file: under a crate's `src`, not a
/// binary's. A binary's items are not an interface anyone depends on.
fn is_library(relative: &str) -> bool {
    let parts: Vec<&str> = relative.split('/').collect();
    parts.first() == Some(&"crates")
        && parts.get(2) == Some(&"src")
        && parts.get(3) != Some(&"bin")
        && parts.get(3) != Some(&"main.rs")
        && parts.last() != Some(&"build.rs")
}

/// Library file -> its declaration count, for every file that declares any.
fn counts(repo: &Path, source: &[PathBuf]) -> Result<BTreeMap<String, usize>, AuditError> {
    let mut found = BTreeMap::new();
    for path in source {
        let relative = posix_relative(repo, path);
        if !is_library(&relative) {
            continue;
        }
        let total = declarations(&read(path)?);
        if total > 0 {
            found.insert(relative, total);
        }
    }
    Ok(found)
}

/// Library file -> its ceiling, as the contract writes it.
fn ceilings(repo: &Path) -> Result<BTreeMap<String, usize>, AuditError> {
    let path = repo.join(CONTRACT);
    let tables = read_tables(&read(&path)?);
    let mut found = BTreeMap::new();
    for table in tables.iter().filter(|table| table.path == TABLE) {
        for entry in &table.entries {
            let number = raw(table, &entry.key).and_then(|text| text.trim().parse::<usize>().ok());
            match number {
                Some(ceiling) => {
                    found.insert(entry.key.clone(), ceiling);
                }
                None => {
                    return fail(
                        AuditError::Manifest {
                            path,
                            missing: "a ceiling that is not a whole number",
                        },
                        "ceilings",
                    );
                }
            }
        }
    }
    Ok(found)
}

/// The finding for a file that declares `count` items against a `ceiling`.
fn over(file: &str, count: usize, ceiling: usize) -> String {
    format!(
        "{file} — SURFACE: {count} public declaration(s), ceiling {ceiling}; `script!` is the one \
         supported interface (#526), so a new `pub` item is a refused one"
    )
}

/// The finding for a ceiling that is above the `count` its file declares.
fn stale(file: &str, ceiling: usize, count: usize) -> String {
    format!(
        "{file} — SURFACE: ceiling {ceiling} is above the {count} declared; lower it in \
         {CONTRACT} so the table stays exact"
    )
}

/// The findings from holding what the tree declares to what the contract allows.
fn judge(declared: &BTreeMap<String, usize>, allowed: &BTreeMap<String, usize>) -> Vec<String> {
    let mut findings = Vec::new();
    for (file, count) in declared {
        match allowed.get(file) {
            Some(&ceiling) if *count > ceiling => findings.push(over(file, *count, ceiling)),
            Some(_) => {}
            None => findings.push(over(file, *count, 0)),
        }
    }
    for (file, ceiling) in allowed {
        match declared.get(file) {
            Some(&count) if *ceiling > count => findings.push(stale(file, *ceiling, count)),
            Some(_) => {}
            None => findings.push(format!(
                "{file} — SURFACE: ceiling {ceiling} for a file that declares no `pub` item; \
                 remove the line from {CONTRACT} so the table stays exact"
            )),
        }
    }
    findings
}

/// Every surface finding of the tree `source` names, against the contract.
pub(super) fn findings(repo: &Path, source: &[PathBuf]) -> Result<Vec<String>, AuditError> {
    Ok(judge(&counts(repo, source)?, &ceilings(repo)?))
}

/// Writes the contract the tree implies, to record a legitimate change.
pub(super) fn baseline(
    repo: &Path,
    source: &[PathBuf],
    out: &mut impl Write,
) -> Result<(), Box<dyn std::error::Error>> {
    writeln!(
        out,
        "# The public-surface ceiling (#526): per library file, how many `pub` items it\n\
         # declares. Exact, so a ceiling may fall and a rise is a visible change here.\n\
         # `check-std-first --surface-baseline` prints the table the tree implies."
    )?;
    writeln!(out, "[{TABLE}]")?;
    for (file, count) in counts(repo, source)? {
        writeln!(out, "\"{file}\" = {count}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CONTRACT, baseline, declarations, findings, is_library, judge};
    use crate::files::{Scratch, source_files};
    use std::collections::BTreeMap;
    use std::error::Error;
    use std::fs;
    use std::path::Path;

    type TestResult = Result<(), Box<dyn Error>>;

    /// Writes `text` at `relative` under `root`, making its directory.
    fn put(root: &Path, relative: &str, text: &str) -> TestResult {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, text)?;
        Ok(())
    }

    /// Every surface finding of the scratch repository as the disk holds it now.
    fn judged(root: &Path) -> Result<Vec<String>, Box<dyn Error>> {
        Ok(findings(root, &source_files(root)?)?)
    }

    /// A ceiling table from rows.
    fn table(rows: &[(&str, usize)]) -> BTreeMap<String, usize> {
        rows.iter()
            .map(|&(name, number)| (name.to_owned(), number))
            .collect()
    }

    /// Whether a finding about `file` says `text`.
    fn says(found: &[String], file: &str, text: &str) -> bool {
        found
            .iter()
            .any(|finding| finding.starts_with(file) && finding.contains(text))
    }

    /// Requirement #526, from the Rust Reference's visibility rules: `pub` alone
    /// is the unrestricted form, `pub(crate)` and its kin are not, a field is not
    /// an item, and prose that spells `pub fn` is not code.
    #[test]
    fn only_unrestricted_pub_items_in_code_are_declarations() {
        let source = "pub fn a() {}\npub(crate) fn b() {}\npub struct S { pub field: u8 }\n\
                      pub use x::y;\npub async fn c() {}\npub unsafe fn d() {}\n\
                      // pub fn in_a_comment() {}\n\
                      const T: &str = \"pub fn in_a_string() {}\";\n";
        assert_eq!(declarations(source), 5, "a, S, the use, c and d");
    }

    /// Requirement #526: test-only and hidden items are not the supported surface,
    /// and a `cfg` that compiles in a shipped build still is.
    #[test]
    fn test_only_and_hidden_items_are_not_counted_and_shipped_cfgs_are() {
        let source = "pub fn kept() {}\n#[cfg(test)]\nmod tests {\n    pub fn helper() {}\n    \
                      fn nested() { let _x = [0; 3]; }\n}\n#[doc(hidden)]\npub mod support;\n\
                      #[doc(hidden)]\npub struct Hidden { pub f: [u8; 4] }\n\
                      #[cfg(not(test))]\npub fn also_kept() {}\n\
                      #[cfg(any(test, unix))]\npub fn conditional() {}\n";
        assert_eq!(declarations(source), 3, "kept, also_kept and conditional");
    }

    /// Requirement #526: a rise is a finding, an unlisted file is held to zero, and
    /// a ceiling above the count (or for a file with none) is stale.
    #[test]
    fn a_rise_an_unlisted_file_and_a_stale_ceiling_are_each_a_finding() {
        let declared = table(&[("a.rs", 3), ("b.rs", 2), ("c.rs", 1), ("e.rs", 4)]);
        let allowed = table(&[("a.rs", 3), ("b.rs", 5), ("d.rs", 1), ("e.rs", 3)]);
        let found = judge(&declared, &allowed);
        assert_eq!(found.len(), 4, "{found:?}");
        assert!(
            says(&found, "c.rs", "ceiling 0"),
            "an unlisted file is held to zero: {found:?}"
        );
        assert!(
            says(&found, "b.rs", "above the 2"),
            "a ceiling above the count is stale: {found:?}"
        );
        assert!(
            says(&found, "d.rs", "declares no `pub` item"),
            "a ceiling for a file that declares none is stale: {found:?}"
        );
        assert!(
            says(&found, "e.rs", "ceiling 3"),
            "a rise past the ceiling is refused: {found:?}"
        );
        assert!(
            judge(&declared, &declared).is_empty(),
            "an exact table holds"
        );
    }

    /// Requirement #526: a library file is under a crate's `src`; a binary, a test
    /// and a build script are not an interface.
    #[test]
    fn only_a_crates_library_sources_are_held_to_a_ceiling() {
        assert!(
            is_library("crates/lgwks-bot/src/lib.rs"),
            "a library root is held"
        );
        assert!(
            is_library("crates/lgwks-bot/src/domain/mod.rs"),
            "a nested library module is held"
        );
        assert!(
            !is_library("crates/lgwks-deps/src/bin/check_std_first/main.rs"),
            "a binary's source is not an interface"
        );
        assert!(
            !is_library("crates/lgwks-bot/tests/it/a.rs"),
            "a test is not an interface"
        );
        assert!(
            !is_library("crates/x/src/main.rs"),
            "a binary root is not an interface"
        );
        assert!(
            !is_library("crates/x/build.rs"),
            "a build script is not an interface"
        );
    }

    /// Requirement #526 ("the new lane fails on a branch that adds a direct-use `pub`
    /// item"), as a contributor meets it on a repository read from disk the way the
    /// lane reads the real one: a recorded tree holds, one more item in an old file
    /// and a new file with one are each refused, recording the table makes both
    /// legitimate, and a file that stops declaring leaves a stale line to remove.
    #[test]
    fn a_contributor_adding_a_pub_item_is_refused_until_the_table_records_it() -> TestResult {
        let scratch = Scratch::new()?;
        let root = &scratch.root;
        put(
            root,
            "crates/demo/src/lib.rs",
            "pub fn one() {}\nfn private() {}\n",
        )?;
        put(
            root,
            CONTRACT,
            "[ceilings]\n\"crates/demo/src/lib.rs\" = 1\n",
        )?;
        assert!(judged(root)?.is_empty(), "the recorded tree holds");

        put(
            root,
            "crates/demo/src/lib.rs",
            "pub fn one() {}\npub fn two() {}\n",
        )?;
        put(root, "crates/demo/src/extra.rs", "pub struct Extra;\n")?;
        let refused = judged(root)?;
        assert_eq!(refused.len(), 2, "one rise and one new file: {refused:?}");
        assert!(
            says(
                &refused,
                "crates/demo/src/lib.rs",
                "2 public declaration(s), ceiling 1"
            ),
            "the added item is over its file's ceiling: {refused:?}"
        );
        assert!(
            says(
                &refused,
                "crates/demo/src/extra.rs",
                "1 public declaration(s), ceiling 0"
            ),
            "a new file starts at zero: {refused:?}"
        );

        let mut recorded = Vec::new();
        baseline(root, &source_files(root)?, &mut recorded)?;
        put(root, CONTRACT, &String::from_utf8(recorded)?)?;
        assert!(
            judged(root)?.is_empty(),
            "the table the tree implies is the change made legitimate"
        );

        put(root, "crates/demo/src/extra.rs", "fn hidden() {}\n")?;
        let stale = judged(root)?;
        assert!(
            says(&stale, "crates/demo/src/extra.rs", "declares no `pub` item"),
            "a ceiling for a file that stopped declaring is stale: {stale:?}"
        );
        Ok(())
    }

    /// Requirement #526 (a gate that cannot read its contract must not pass): a ceiling
    /// that is not a whole number and a contract that is absent are refusals, never
    /// "no ceilings" and never a ceiling of zero.
    #[test]
    fn an_unreadable_contract_is_a_refusal_not_an_empty_table() -> TestResult {
        let scratch = Scratch::new()?;
        let root = &scratch.root;
        put(root, "crates/demo/src/lib.rs", "pub fn one() {}\n")?;
        assert!(judged(root).is_err(), "an absent contract is refused");
        for spelling in ["\"many\"", "-1", "1.5"] {
            put(
                root,
                CONTRACT,
                &format!("[ceilings]\n\"crates/demo/src/lib.rs\" = {spelling}\n"),
            )?;
            assert!(
                judged(root).is_err(),
                "a ceiling written {spelling} is refused"
            );
        }
        Ok(())
    }
}
