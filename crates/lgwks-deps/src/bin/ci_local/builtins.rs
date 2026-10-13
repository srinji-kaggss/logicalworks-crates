//! The builtin lanes that read the tree: checks the gate runs in-process rather
//! than as a shell command, keyed by the `builtin` name in the lane table.
//!
//! The lanes that run other programs are in `runners`, and the invariants
//! resolver is its own module.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::receipt::git;
use super::toml_subset::{read_tables, text};
use super::{Context, GateError, Verdict, fail, read};

/// Build output and caches that must never be tracked or staged.
const FORBIDDEN_PREFIXES: [&str; 5] = [
    "target/",
    "graphify-out/",
    ".lgwks/",
    "node_modules/",
    ".codegraph/",
];

/// How many lines after a suppression its `reason` may sit within.
const REASON_WINDOW: usize = 7;

/// A call that must not appear in shipped code, spelled in two pieces so this
/// file does not contain the text its own scan looks for.
const FORBIDDEN_CALL: &str = concat!(".unw", "rap()");

/// The text of `text` from byte `from`, or empty when that is not a boundary.
pub(super) fn tail(text: &str, from: usize) -> &str {
    match text.get(from..) {
        Some(rest) if !rest.is_empty() => rest,
        Some(_) | None => "",
    }
}

/// The text of `text` up to byte `to`, or empty when that is not a boundary.
pub(super) fn head(text: &str, to: usize) -> &str {
    match text.get(..to) {
        Some(front) if !front.is_empty() => front,
        Some(_) | None => "",
    }
}

/// `text` without one leading `prefix`, if it has one.
fn skip_char(text: &str, prefix: char) -> &str {
    match text.strip_prefix(prefix) {
        Some(rest) => rest,
        None => text,
    }
}

/// Every `.rs` file under `directory`, sorted, skipping any path with a component in `skip`.
pub(super) fn rust_files_under(directory: &Path, skip: &[&str]) -> Result<Vec<PathBuf>, GateError> {
    let mut found = Vec::new();
    collect_rust(directory, skip, &mut found, 0)?;
    found.sort();
    Ok(found)
}

/// Adds the `.rs` files under `directory` to `found`.
fn collect_rust(
    directory: &Path,
    skip: &[&str],
    found: &mut Vec<PathBuf>,
    depth: usize,
) -> Result<(), GateError> {
    if depth > 64 {
        return fail(
            GateError::Manifest(format!(
                "{}: nested deeper than 64 directories",
                directory.display()
            )),
            "collect_rust",
        );
    }
    let reader = match fs::read_dir(directory) {
        Ok(reader) => reader,
        Err(error) => {
            lgwks_std::trace::debug!(%error, path = ?directory, "a directory could not be read");
            return Ok(());
        }
    };
    for entry in reader.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if skip.contains(&name.as_str()) {
            continue;
        }
        if path.is_dir() {
            collect_rust(&path, skip, found, depth.saturating_add(1))?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
    Ok(())
}

/// The directories directly under `crates/`, sorted.
pub(super) fn crate_directories(root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = match fs::read_dir(root.join("crates")) {
        Ok(reader) => reader
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect(),
        Err(error) => {
            lgwks_std::trace::debug!(%error, "there is no crates directory");
            Vec::new()
        }
    };
    found.sort();
    found
}

/// `path` relative to `root`, with `/` separators.
pub(super) fn relative(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(inner) => inner
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.display().to_string(),
    }
}

/// A passing verdict.
pub(super) fn pass(detail: &str) -> Verdict {
    Verdict {
        code: 0,
        detail: detail.to_owned(),
    }
}

/// A failing verdict.
pub(super) fn refuse(detail: String) -> Verdict {
    Verdict { code: 1, detail }
}

/// Ban the forbidden call in `crates/*/src` outside each file's `mod tests`.
pub(super) fn unwrap_scan(context: &Context<'_>) -> Result<Verdict, GateError> {
    let mut hits = Vec::new();
    for directory in crate_directories(context.root) {
        for path in rust_files_under(&directory.join("src"), &[])? {
            let source = read(&path)?;
            let lines: Vec<&str> = source.lines().collect();
            let tests_at = lines
                .iter()
                .position(|line| line.contains("mod tests"))
                .map(|index| index.saturating_add(1));
            for (index, line) in lines.iter().enumerate() {
                let number = index.saturating_add(1);
                if line.contains(FORBIDDEN_CALL) && !tests_at.is_some_and(|start| number >= start) {
                    hits.push(format!(
                        "unwrap outside tests: {}:{number}",
                        relative(context.root, &path)
                    ));
                }
            }
        }
    }
    if hits.is_empty() {
        Ok(pass("no unwrap outside tests"))
    } else {
        Ok(refuse(hits.join("\n")))
    }
}

/// Whether a line opens an `allow` or `expect` attribute.
fn is_suppression(line: &str) -> bool {
    let Some(rest) = line.trim_start_matches([' ', '\t']).strip_prefix('#') else {
        return false;
    };
    match skip_char(rest, '!').strip_prefix('[') {
        Some(inside) => inside.starts_with("allow(") || inside.starts_with("expect("),
        None => false,
    }
}

/// Every `allow` / `expect` attribute carries a `reason =` in the next lines.
pub(super) fn suppressions(context: &Context<'_>) -> Result<Verdict, GateError> {
    let mut hits = Vec::new();
    for directory in crate_directories(context.root) {
        for path in rust_files_under(&directory, &["target"])? {
            let source = read(&path)?;
            let lines: Vec<&str> = source.lines().collect();
            for (index, line) in lines.iter().enumerate() {
                if !is_suppression(line) {
                    continue;
                }
                let end = index.saturating_add(REASON_WINDOW).min(lines.len());
                let window = match lines.get(index..end) {
                    Some(slice) => slice.join("\n"),
                    None => String::new(),
                };
                if !window.contains("reason") {
                    hits.push(format!(
                        "reasonless suppression: {}:{}",
                        relative(context.root, &path),
                        index.saturating_add(1)
                    ));
                }
            }
        }
    }
    if hits.is_empty() {
        Ok(pass("every suppression names a reason"))
    } else {
        Ok(refuse(hits.join("\n")))
    }
}

/// The forbidden names in a NUL-separated git listing, labelled.
fn forbidden_in(label: &str, listing: &str, offenders: &mut Vec<String>) {
    for name in listing.split('\0').filter(|name| !name.is_empty()) {
        let normalized = name.replace('\\', "/");
        if FORBIDDEN_PREFIXES
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
        {
            offenders.push(format!("{label}: {normalized}"));
        }
    }
}

/// Whether `directory` exists and holds anything.
fn is_populated(directory: &Path) -> bool {
    match fs::read_dir(directory) {
        Ok(mut entries) => entries.next().is_some(),
        Err(_) => false,
    }
}

/// Derived output must not be tracked or staged (issue #127, defect 3).
pub(super) fn artifacts(context: &Context<'_>) -> Result<Verdict, GateError> {
    let mut offenders = Vec::new();
    match git(context.root, &["ls-files", "-z"]) {
        Some(tracked) => forbidden_in("tracked", &tracked, &mut offenders),
        None => {
            for prefix in FORBIDDEN_PREFIXES {
                if is_populated(&context.root.join(prefix)) {
                    offenders.push(format!("present (not a git work tree): {prefix}"));
                }
            }
        }
    }
    if let Some(staged) = git(context.root, &["diff", "--cached", "--name-only", "-z"]) {
        forbidden_in("staged", &staged, &mut offenders);
    }
    if offenders.is_empty() {
        Ok(pass("no derived build output tracked or staged"))
    } else {
        let lines: Vec<String> = offenders
            .iter()
            .map(|offender| format!("derived build output committed or staged: {offender}"))
            .collect();
        Ok(refuse(lines.join("\n")))
    }
}

/// The position after the whitespace at `at`.
fn skip_space(chars: &[char], at: usize) -> usize {
    let mut cursor = at;
    while chars.get(cursor).is_some_and(|ch| ch.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    cursor
}

/// The position after `word` if it is at `at`.
fn expect(chars: &[char], at: usize, word: &str) -> Option<usize> {
    let mut cursor = at;
    for expected in word.chars() {
        if chars.get(cursor) != Some(&expected) {
            return None;
        }
        cursor = cursor.saturating_add(1);
    }
    Some(cursor)
}

/// A run of ASCII digits at `at`, and the position after it.
fn digits(chars: &[char], at: usize) -> Option<(String, usize)> {
    let run: String = chars
        .iter()
        .skip(at)
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    if run.is_empty() {
        None
    } else {
        let end = at.saturating_add(run.len());
        Some((run, end))
    }
}

/// `major.minor` and an optional `.patch` at `at`: the two leading numbers and the end.
fn read_version(chars: &[char], at: usize) -> Option<(String, String, usize)> {
    let (major, after_major) = digits(chars, at)?;
    let after_dot = expect(chars, after_major, ".")?;
    let (minor, after_minor) = digits(chars, after_dot)?;
    let end = match expect(chars, after_minor, ".").and_then(|start| digits(chars, start)) {
        Some((_, after_patch)) => after_patch,
        None => after_minor,
    };
    Some((major, minor, end))
}

/// A `name = "x.y"` or `name = { version = "x.y" }` pin that starts at `at`.
fn assignment_pin(chars: &[char], at: usize, name: &str) -> Option<(String, String, usize)> {
    let after_name = expect(chars, at, name)?;
    let after_equals = skip_space(chars, expect(chars, skip_space(chars, after_name), "=")?);
    let at_value = if chars.get(after_equals) == Some(&'{') {
        let after_key = expect(
            chars,
            skip_space(chars, after_equals.saturating_add(1)),
            "version",
        )?;
        skip_space(chars, expect(chars, skip_space(chars, after_key), "=")?)
    } else {
        after_equals
    };
    let (major, minor, end) = read_version(chars, expect(chars, at_value, "\"")?)?;
    let closed = expect(chars, end, "\"")?;
    Some((major, minor, closed))
}

/// A table row naming `name` with a version that starts at `at`.
fn table_pin(chars: &[char], at: usize, name: &str) -> Option<(String, String, usize)> {
    let after_bar = skip_space(chars, expect(chars, at, "|")?);
    let after_tick = expect(chars, after_bar, "`")?;
    let after_name = expect(chars, after_tick, name)?;
    let after_close = expect(chars, after_name, "`")?;
    let after_second = skip_space(chars, expect(chars, skip_space(chars, after_close), "|")?);
    let (major, minor, end) = read_version(chars, after_second)?;
    let closed = expect(chars, skip_space(chars, end), "|")?;
    Some((major, minor, closed))
}

/// The next dot-separated part, or empty.
fn next_part<'text>(parts: &mut impl Iterator<Item = &'text str>) -> &'text str {
    match parts.next() {
        Some(part) if !part.is_empty() => part,
        Some(_) | None => "",
    }
}

/// The pins in `body` for `name` whose major.minor differs from `version`'s.
fn stale_pins(body: &str, name: &str, version: &str) -> Vec<String> {
    let chars: Vec<char> = body.chars().collect();
    let mut parts = version.split('.');
    let major = next_part(&mut parts);
    let minor = next_part(&mut parts);
    let mut stale = Vec::new();
    for finder in [assignment_pin, table_pin] {
        for at in 0..chars.len() {
            if let Some((found_major, found_minor, end)) = finder(&chars, at, name)
                && (found_major != major || found_minor != minor)
            {
                let matched: String = chars.iter().skip(at).take(end.saturating_sub(at)).collect();
                stale.push(format!("{matched} (manifest is {version})"));
            }
        }
    }
    stale
}

/// A field of the `[package]` table of the manifest at `relative_path`.
fn package_field(
    context: &Context<'_>,
    relative_path: &str,
    key: &str,
) -> Result<String, GateError> {
    let tables = read_tables(&read(&context.root.join(relative_path))?);
    match tables
        .iter()
        .find(|table| table.path == "package")
        .and_then(|table| text(table, key))
    {
        Some(found) => Ok(found.to_owned()),
        None => fail(
            GateError::Manifest(format!("{relative_path}: no [package] {key}")),
            "package_field",
        ),
    }
}

/// The names the README section marks with a bolded list item.
fn bolded_names(section: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (index, _) in section.match_indices("- **") {
        let rest = tail(section, index.saturating_add(4));
        let name: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
            .collect();
        if !name.is_empty() && tail(rest, name.len()).starts_with("**") {
            names.push(name);
        }
    }
    names
}

/// The dependency names the standard-library crate's manifest declares.
fn declared_in_manifest(context: &Context<'_>) -> Result<BTreeSet<String>, GateError> {
    let tables = read_tables(&read(&context.root.join("crates/lgwks-std/Cargo.toml"))?);
    Ok(tables
        .iter()
        .filter(|table| table.path == "dependencies")
        .flat_map(|table| table.entries.iter())
        .filter_map(|entry| entry.key.split('.').next().map(str::to_owned))
        .collect())
}

/// README dependency philosophy and version pins track the manifests.
pub(super) fn contract_drift(context: &Context<'_>) -> Result<Verdict, GateError> {
    let manifest_deps = declared_in_manifest(context)?;
    let readme = read(&context.root.join("crates/lgwks-std/README.md"))?;
    let Some(start) = readme.find("## Dependency philosophy") else {
        return Ok(refuse(
            "README dependency philosophy section missing".to_owned(),
        ));
    };
    let section: String = tail(&readme, start).chars().take(1800).collect();
    let readme_deps: BTreeSet<String> = bolded_names(&section).into_iter().collect();
    let missing_from_readme: Vec<&String> = manifest_deps.difference(&readme_deps).collect();
    let missing_from_manifest: Vec<&String> = readme_deps.difference(&manifest_deps).collect();
    if !missing_from_readme.is_empty() || !missing_from_manifest.is_empty() {
        return Ok(refuse(format!(
            "Dependency contract drift detected. missing_from_readme={missing_from_readme:?}, \
             missing_from_manifest={missing_from_manifest:?}"
        )));
    }
    let repository = package_field(context, "crates/lgwks-std/Cargo.toml", "repository")?;
    if repository != "https://github.com/srinji-kaggss/logicalworks-crates" {
        return Ok(refuse(format!(
            "Unexpected package repository URL: {repository}"
        )));
    }
    let mut pins = Vec::new();
    for (name, directory) in [
        ("lgwks_std", "lgwks-std"),
        ("lgwks_bot", "lgwks-bot"),
        ("lgwks_ast", "lgwks-ast"),
        ("lgwks_deps", "lgwks-deps"),
    ] {
        pins.push((
            name,
            package_field(
                context,
                &format!("crates/{directory}/Cargo.toml"),
                "version",
            )?,
        ));
    }
    let mut documents = vec![context.root.join("README.md")];
    documents.extend(
        crate_directories(context.root)
            .into_iter()
            .map(|directory| directory.join("README.md"))
            .filter(|path| path.is_file()),
    );
    let mut bad = Vec::new();
    for document in documents {
        let body = read(&document)?;
        for pin in &pins {
            for stale in stale_pins(&body, pin.0, &pin.1) {
                bad.push(format!("{}: {stale}", relative(context.root, &document)));
            }
        }
    }
    if bad.is_empty() {
        Ok(pass(
            "README dependency philosophy and version pins track the manifests",
        ))
    } else {
        Ok(refuse(format!(
            "Stale README version pins:\n{}",
            bad.join("\n")
        )))
    }
}

/// The fenced rust blocks of `section`.
fn rust_blocks(section: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut rest = section;
    while let Some(open) = rest.find("```rust\n") {
        let after = tail(rest, open.saturating_add(8));
        let Some(close) = after.find("```") else {
            break;
        };
        blocks.push(head(after, close).to_owned());
        rest = tail(after, close.saturating_add(3));
    }
    blocks
}

/// The root README quickstart is byte-identical to the compiled example.
pub(super) fn readme_quickstart(context: &Context<'_>) -> Result<Verdict, GateError> {
    let readme = read(&context.root.join("README.md"))?;
    let Some((_, after)) = readme.split_once("## Quickstart") else {
        return Ok(refuse("README.md has no ## Quickstart section".to_owned()));
    };
    let section = match after.split_once("\n## ") {
        Some((front, _)) => front,
        None => after,
    };
    let blocks = rust_blocks(section);
    let Some(only) = blocks.first().filter(|_| blocks.len() == 1) else {
        return Ok(refuse(format!(
            "expected one rust block under Quickstart, found {}",
            blocks.len()
        )));
    };
    let readme_block = only.trim();
    let example = read(&context.root.join("crates/lgwks-bot/examples/quickstart.rs"))?;
    let kept: Vec<&str> = example
        .split('\n')
        .filter(|line| !line.starts_with("//!"))
        .collect();
    let body = kept.join("\n");
    let body = body.trim();
    if readme_block == body {
        Ok(pass("README quickstart matches the compiled example"))
    } else {
        Ok(refuse(format!(
            "README quickstart drifted from crates/lgwks-bot/examples/quickstart.rs:\n--- README.md ---\n{readme_block}\n--- quickstart.rs ---\n{body}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::{bolded_names, is_suppression, rust_blocks, stale_pins};

    #[test]
    fn an_attribute_line_is_a_suppression_only_for_allow_and_expect() {
        assert!(is_suppression("    #[allow(dead_code)]"), "allow");
        assert!(is_suppression("#![expect(unused)]"), "inner expect");
        assert!(!is_suppression("#[derive(Debug)]"), "derive");
        assert!(!is_suppression("// #[allow(x)]"), "a comment");
    }

    #[test]
    fn stale_pins_compare_major_and_minor_in_both_shapes() {
        let body = "lgwks_std = \"0.4\"\nlgwks_std = { version = \"0.5.2\", features = [] }\n| `lgwks_std` | 0.4.1 |\n| `lgwks_std` | 0.5.0 |\nlgwks_bot = \"9.9\"\n";
        let stale = stale_pins(body, "lgwks_std", "0.5.3");
        assert_eq!(
            stale.len(),
            2,
            "one assignment and one table row are stale: {stale:?}"
        );
        assert!(
            stale
                .iter()
                .all(|entry| entry.ends_with("(manifest is 0.5.3)")),
            "{stale:?}"
        );
        assert!(
            stale
                .iter()
                .any(|entry| entry.starts_with("lgwks_std = \"0.4\"")),
            "{stale:?}"
        );
        assert!(
            stale
                .iter()
                .any(|entry| entry.starts_with("| `lgwks_std` | 0.4.1 |")),
            "{stale:?}"
        );
    }

    #[test]
    fn bold_names_need_their_closing_stars() {
        assert_eq!(
            bolded_names("- **serde** ok\n- **json_v2**\n- **open\n"),
            vec!["serde", "json_v2"],
            "closed names only"
        );
    }

    #[test]
    fn rust_blocks_are_the_fenced_rust_only() {
        let blocks = rust_blocks(
            "a\n```rust\nfn main() {}\n```\nb\n```toml\nx = 1\n```\n```rust\nlet y = 2;\n```\n",
        );
        assert_eq!(
            blocks,
            vec!["fn main() {}\n", "let y = 2;\n"],
            "only rust fences"
        );
    }
}
