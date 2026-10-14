//! Reading the documentation: finding `path:NNN` citations, resolving each
//! against its file, and comparing the std module inventory with its matrix.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use super::{CiteError, Collected, MAX_DEPTH, fail, read};

/// The extensions the guides cite.
const EXTENSIONS: [&str; 8] = ["rs", "py", "sh", "toml", "md", "json", "yml", "yaml"];

/// Documents that cite a different repository, and are therefore not checked.
const EXTERNAL_PAGES: [(&str, &str); 1] = [(
    "docs/bevy-admission.md",
    "cites the Bevy repository, not this one",
)];

/// The std library file whose public modules the closure matrix must list.
const STD_LIB: &str = "crates/lgwks-std/src/lib.rs";

/// The closure matrix page.
const STD_MATRIX: &str = "docs/std-ast-deps-closure-matrix-std.md";

/// One citation found in a page.
pub(super) struct Citation {
    /// The cited path.
    pub(super) path: String,
    /// The line spec, e.g. `1408-1411`.
    pub(super) spec: String,
    /// The whole citation as written, for messages.
    pub(super) whole: String,
}

/// Whether `ch` can be part of a cited path.
fn is_path_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '/' | '-')
}

/// The byte index after any whitespace starting at `from`.
fn skip_whitespace(text: &str, from: usize) -> usize {
    let Some(rest) = text.get(from..) else {
        return from;
    };
    let trimmed = rest.trim_start();
    from.saturating_add(rest.len().saturating_sub(trimmed.len()))
}

/// The byte index after a run of ASCII digits starting at `from`.
fn skip_digits(text: &str, from: usize) -> usize {
    let Some(rest) = text.get(from..) else {
        return from;
    };
    let count = rest.chars().take_while(char::is_ascii_digit).count();
    from.saturating_add(count)
}

/// The length of the line spec at the front of `text`:
/// `\d+(?:\s*[-,]\s*\d+)*`, or `None` when it does not start with a digit.
fn spec_length(text: &str) -> Option<usize> {
    let mut end = skip_digits(text, 0);
    if end == 0 {
        return None;
    }
    loop {
        let after_space = skip_whitespace(text, end);
        let Some(rest) = text.get(after_space..) else {
            break;
        };
        if !(rest.starts_with('-') || rest.starts_with(',')) {
            break;
        }
        let digits_from = skip_whitespace(text, after_space.saturating_add(1));
        let digits_to = skip_digits(text, digits_from);
        if digits_to == digits_from {
            break;
        }
        end = digits_to;
    }
    Some(end)
}

/// The run itself, if it ends in a cited extension after a dot.
fn cited_path(run: &str) -> Option<&str> {
    for extension in EXTENSIONS {
        if let Some(stem) = run.strip_suffix(extension)
            && stem.len() > 1
            && stem.ends_with('.')
        {
            return Some(run);
        }
    }
    None
}

/// Every `path.ext:lines` citation in `text`, in order, without overlap.
pub(super) fn citations(text: &str) -> Vec<Citation> {
    let mut found = Vec::new();
    let mut floor = 0_usize;
    for (colon, ch) in text.char_indices() {
        if ch != ':' || colon < floor {
            continue;
        }
        let after_colon = colon.saturating_add(1);
        let Some(after) = text.get(after_colon..) else {
            continue;
        };
        let Some(length) = spec_length(after) else {
            continue;
        };
        let Some(before) = text.get(floor..colon) else {
            continue;
        };
        let start = match before
            .char_indices()
            .rev()
            .find(|&(_, candidate)| !is_path_char(candidate))
        {
            Some((index, candidate)) => index.saturating_add(candidate.len_utf8()),
            None => 0,
        };
        let Some(run) = before.get(start..) else {
            continue;
        };
        let Some(path) = cited_path(run) else {
            continue;
        };
        let Some(spec) = after.get(..length) else {
            continue;
        };
        found.push(Citation {
            path: path.to_owned(),
            spec: spec.to_owned(),
            whole: format!("{path}:{spec}"),
        });
        floor = after_colon.saturating_add(length);
    }
    found
}

/// Every line number a citation spec names.
pub(super) fn line_numbers(spec: &str) -> Vec<u64> {
    let mut numbers = Vec::new();
    for part in spec.split(['-', ',']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.parse::<u64>() {
            Ok(number) => numbers.push(number),
            Err(error) => {
                lgwks_std::trace::debug!(%error, part, "a line number does not parse");
            }
        }
    }
    numbers
}

/// Why `line` cannot be the source of a citation, if it cannot.
pub(super) fn unsupportive_reason(line: &str) -> Option<&'static str> {
    let stripped = line.trim();
    if !stripped.is_empty()
        && stripped
            .chars()
            .all(|ch| matches!(ch, '}' | ']' | ')' | ';'))
    {
        return Some("it is only a closing delimiter");
    }
    match stripped {
        "///" => Some("it is an empty `///`"),
        "//!" => Some("it is an empty `//!`"),
        "//" => Some("it is an empty comment"),
        _ => None,
    }
}

/// The entries of `directory`, or none for a directory that is absent.
fn entries_of(directory: &Path) -> Result<Vec<PathBuf>, CiteError> {
    let reader = match fs::read_dir(directory) {
        Ok(reader) => reader,
        Err(cause) => {
            lgwks_std::trace::debug!(%cause, path = ?directory, "a documentation directory is absent");
            return Ok(Vec::new());
        }
    };
    let mut paths = Vec::new();
    for entry in reader {
        match entry {
            Ok(entry) => paths.push(entry.path()),
            Err(cause) => {
                return fail(
                    CiteError::Io {
                        path: directory.to_path_buf(),
                        cause,
                    },
                    "entries_of",
                );
            }
        }
    }
    Ok(paths)
}

/// Whether `path` is a markdown file.
fn is_markdown(path: &Path) -> bool {
    path.is_file() && path.extension().is_some_and(|extension| extension == "md")
}

/// Every `.md` file under `directory` (any depth), added to `out`.
fn markdown_under(
    directory: &Path,
    depth: usize,
    out: &mut BTreeSet<PathBuf>,
) -> Result<(), CiteError> {
    if depth > MAX_DEPTH {
        return fail(
            CiteError::TooDeep {
                path: directory.to_path_buf(),
            },
            "markdown_under",
        );
    }
    for path in entries_of(directory)? {
        if path.is_dir() {
            markdown_under(&path, depth.saturating_add(1), out)?;
        } else if is_markdown(&path) {
            out.insert(path);
        }
    }
    Ok(())
}

/// Every documentation page: `docs/**/*.md`, `*.md`, `crates/*/README.md`, `skills/**/*.md`.
fn document_paths(root: &Path) -> Result<Vec<PathBuf>, CiteError> {
    let mut found = BTreeSet::new();
    markdown_under(&root.join("docs"), 0, &mut found)?;
    markdown_under(&root.join("skills"), 0, &mut found)?;
    for path in entries_of(root)? {
        if is_markdown(&path) {
            found.insert(path);
        }
    }
    for member in entries_of(&root.join("crates"))? {
        let readme = member.join("README.md");
        if readme.is_file() {
            found.insert(readme);
        }
    }
    Ok(found.into_iter().collect())
}

/// `path` relative to `root`, spelled with forward slashes.
fn relative(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rest) => rest
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// The lines of a cited file, or `None` when it cannot be read.
fn cited_lines(root: &Path, path: &str) -> Option<Vec<String>> {
    match fs::read_to_string(root.join(path)) {
        Ok(body) => Some(body.lines().map(str::to_owned).collect()),
        Err(error) => {
            lgwks_std::trace::debug!(%error, path, "a cited file is unreadable");
            None
        }
    }
}

/// The line `number` (one-based) of `lines`, if it exists.
fn line_at(lines: &[String], number: u64) -> Option<&String> {
    match usize::try_from(number) {
        Ok(wide) => lines.get(wide.checked_sub(1)?),
        Err(_) => None,
    }
}

/// Checks one citation against its file, recording entries and failures.
fn check_citation(page: &str, citation: &Citation, lines: &[String], collected: &mut Collected) {
    for number in line_numbers(&citation.spec) {
        let Some(cited_line) = line_at(lines, number) else {
            collected.failures.push(format!(
                "{page}: {} — line {number} is outside the file (it has {})",
                citation.whole,
                lines.len()
            ));
            continue;
        };
        if cited_line.trim().is_empty() {
            collected.failures.push(format!(
                "{page}: {} — line {number} is blank",
                citation.whole
            ));
            continue;
        }
        if let Some(reason) = unsupportive_reason(cited_line) {
            let shown: String = cited_line.trim().chars().take(40).collect();
            collected.failures.push(format!(
                "{page}: {} — line {number} is `{shown}`, which supports no claim: {reason}",
                citation.whole
            ));
            continue;
        }
        let key = (citation.path.clone(), number);
        collected
            .entries
            .insert(key.clone(), cited_line.trim().to_owned());
        collected
            .sites
            .entry(key)
            .or_default()
            .push(page.to_owned());
    }
}

/// Walks the documentation and reads each cited line.
pub(super) fn collect(root: &Path) -> Result<Collected, CiteError> {
    let mut collected = Collected {
        entries: BTreeMap::new(),
        sites: BTreeMap::new(),
        skipped: Vec::new(),
        failures: Vec::new(),
    };
    let mut contents: HashMap<String, Option<Vec<String>>> = HashMap::new();
    for document in document_paths(root)? {
        let page = relative(root, &document);
        if EXTERNAL_PAGES.iter().any(|&(external, _)| external == page) {
            collected.skipped.push(page);
            continue;
        }
        let text = read(&document)?;
        for citation in citations(&text) {
            let lines = contents
                .entry(citation.path.clone())
                .or_insert_with(|| cited_lines(root, &citation.path));
            match lines.as_ref() {
                Some(lines) => check_citation(&page, &citation, lines, &mut collected),
                None => collected
                    .failures
                    .push(format!("{page}: {} — no such file", citation.whole)),
            }
        }
    }
    Ok(collected)
}

/// The `pub mod` names of the std library file.
pub(super) fn public_modules(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| line.strip_prefix("pub mod "))
        .filter_map(|rest| rest.strip_suffix(';'))
        .filter(|name| !name.is_empty() && name.chars().all(|ch| ch.is_alphanumeric() || ch == '_'))
        .map(str::to_owned)
        .collect()
}

/// The module names the closure matrix lists as `| N | `name` ...` rows.
pub(super) fn matrix_rows(text: &str) -> BTreeSet<String> {
    let mut rows = BTreeSet::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix('|') else {
            continue;
        };
        let rest = rest.trim_start();
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            continue;
        }
        let Some(rest) = rest.get(digits..) else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('|') else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('`') else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
            .collect();
        if !name.is_empty() {
            rows.insert(name);
        }
    }
    rows
}

/// Where the library's public modules and the matrix's module rows disagree.
pub(super) fn inventory_failures(root: &Path) -> Result<Vec<String>, CiteError> {
    let (lib, matrix) = (root.join(STD_LIB), root.join(STD_MATRIX));
    if !lib.is_file() || !matrix.is_file() {
        return Ok(vec![format!(
            "{STD_MATRIX}: cannot compare against {STD_LIB}, one of them is missing"
        )]);
    }
    let modules = public_modules(&read(&lib)?);
    let rows = matrix_rows(&read(&matrix)?);
    let mut failures: Vec<String> = modules
        .difference(&rows)
        .map(|name| format!("{STD_MATRIX}: no row for `pub mod {name}` in {STD_LIB}"))
        .collect();
    failures.extend(
        rows.difference(&modules)
            .map(|name| format!("{STD_MATRIX}: row `{name}` names no `pub mod` in {STD_LIB}")),
    );
    Ok(failures)
}

#[cfg(test)]
mod tests {
    use super::{citations, line_numbers, matrix_rows, public_modules, unsupportive_reason};

    #[test]
    fn a_citation_is_a_path_an_extension_a_colon_and_lines() {
        let found =
            citations("see `crates/a/src/x.rs:12` and docs/y.md:3-5, z.toml:7,9 and a.rs:x");
        let wholes: Vec<&str> = found
            .iter()
            .map(|citation| citation.whole.as_str())
            .collect();
        assert_eq!(
            wholes,
            vec!["crates/a/src/x.rs:12", "docs/y.md:3-5", "z.toml:7,9"],
            "three well-formed citations, none for a non-numeric line"
        );
    }

    #[test]
    fn a_range_and_a_scatter_name_every_line() {
        assert_eq!(
            line_numbers("1408-1411"),
            vec![1408, 1411],
            "a range names its ends"
        );
        assert_eq!(
            line_numbers("7, 9"),
            vec![7, 9],
            "a scatter names each line"
        );
    }

    #[test]
    fn a_delimiter_or_empty_comment_supports_no_claim() {
        assert!(
            unsupportive_reason("   });  ").is_some(),
            "a closing delimiter"
        );
        assert_eq!(
            unsupportive_reason("///"),
            Some("it is an empty `///`"),
            "empty doc line"
        );
        assert_eq!(unsupportive_reason("fn f() {}"), None, "real code");
    }

    #[test]
    fn modules_and_matrix_rows_are_read_by_their_markers() {
        let modules = public_modules("pub mod json;\n  pub mod nested;\npub mod hash;\n");
        assert_eq!(
            modules.into_iter().collect::<Vec<_>>(),
            vec!["hash", "json"],
            "only column-zero pub mod lines"
        );
        let rows = matrix_rows("| 1 | `json` | x |\n|  22 |`hash`| y |\n| a | `no` |\n");
        assert_eq!(
            rows.into_iter().collect::<Vec<_>>(),
            vec!["hash", "json"],
            "numbered rows"
        );
    }
}
