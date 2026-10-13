//! Generates `llms.txt` from the repository's own documentation index.
//!
//! `llms.txt` is a curated index an agent can fetch once instead of crawling the
//! repository. Its whole value is being current: an index that points at
//! documents which moved is worse than no index, because the agent trusts it and
//! stops looking. So it is generated and never hand-edited, and CI re-runs this
//! binary and fails when the committed copy differs.
//!
//! The index has exactly one source per table: the crates table of `README.md`
//! and the Documentation table of `docs/README.md`. This binary reads those two
//! tables, checks every path they name exists, checks no document in `docs/` is
//! missing from the table, and writes `llms.txt`.
//!
//! Usage: `llms_txt [ROOT]` (default: the working directory). The exit status is
//! non-zero when the index is inconsistent, so the same binary works as a check.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// How deep the walk of `docs/` may go.
///
/// A tree is caller data; the walk is bounded like every other input.
const MAX_DEPTH: usize = 32;

/// Files the Documentation table does not already carry, with their reasons.
const OPTIONAL: [(&str, &str); 6] = [
    ("CHANGELOG.md", "Per-release changes across all four crates"),
    (
        "SECURITY.md",
        "Attack surface, reporting process, and advisories assessed",
    ),
    ("AGENTS.md", "How to add a dependency to this repository"),
    (
        "REQUIREMENTS.md",
        "Immutable product requirements, superseded never edited",
    ),
    ("INVARIANTS.md", "Short enforced rule list for this repository"),
    (
        "contract/APPROVED.toml",
        "Every approved external edge, and its owner",
    ),
];

/// Why the index could not be generated.
#[derive(Debug)]
enum IndexError {
    /// A file or directory could not be read.
    Io {
        /// What was being read.
        path: PathBuf,
        /// What the operating system said.
        cause: io::Error,
    },
    /// A document lacks a section the index is read from.
    NoSection {
        /// The document.
        source: &'static str,
        /// The heading it should hold.
        heading: &'static str,
    },
    /// `README.md` has no level-one heading.
    NoHeading,
    /// The crate manifest names no repository.
    NoRepository,
    /// A walk went deeper than [`MAX_DEPTH`].
    TooDeep {
        /// Where the walk stopped.
        path: PathBuf,
    },
    /// The tables and the tree disagree.
    Inconsistent(Vec<String>),
}

impl fmt::Display for IndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Io { ref path, ref cause } => {
                write!(formatter, "cannot read {}: {cause}", path.display())
            }
            Self::NoSection { source, heading } => {
                write!(formatter, "{source} has no '## {heading}' section")
            }
            Self::NoHeading => write!(formatter, "README.md has no H1"),
            Self::NoRepository => {
                write!(formatter, "crates/lgwks-std/Cargo.toml names no repository")
            }
            Self::TooDeep { ref path } => {
                write!(formatter, "{} is nested past {MAX_DEPTH} levels", path.display())
            }
            Self::Inconsistent(ref problems) => write!(formatter, "{}", problems.join("\nerror: ")),
        }
    }
}

impl std::error::Error for IndexError {}

/// Logs `error` at the site that produced it, then hands it back as the `Err`.
fn fail<T>(error: IndexError, site: &'static str) -> Result<T, IndexError> {
    lgwks_std::trace::debug!(error = ?error, site, "returning an error to the caller");
    Err(error)
}

/// The generated index and what it counted.
struct Generated {
    /// The file's whole text.
    text: String,
    /// How many crates the table listed.
    crates: usize,
    /// How many documents the table listed.
    documents: usize,
}

/// One table row: what it names, where it points, and what it says.
struct Row {
    /// The crate name or the document title.
    name: String,
    /// The docs.rs URL or the repository path.
    link: String,
    /// The row's last cell.
    description: String,
}

/// Reads a file whole, naming it when it cannot be read.
fn read(path: &Path) -> Result<String, IndexError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(cause) => fail(
            IndexError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "read",
        ),
    }
}

/// Every entry of `directory`, read whole.
fn entries_of(directory: &Path, site: &'static str) -> Result<Vec<fs::DirEntry>, IndexError> {
    let reader = match fs::read_dir(directory) {
        Ok(reader) => reader,
        Err(cause) => {
            return fail(
                IndexError::Io {
                    path: directory.to_path_buf(),
                    cause,
                },
                site,
            );
        }
    };
    let mut entries = Vec::new();
    for entry in reader {
        match entry {
            Ok(entry) => entries.push(entry),
            Err(cause) => {
                return fail(
                    IndexError::Io {
                        path: directory.to_path_buf(),
                        cause,
                    },
                    site,
                );
            }
        }
    }
    Ok(entries)
}

/// The last cell of a row, or an empty string for a row with none.
fn last_cell<'a>(row_cells: &[&'a str]) -> &'a str {
    match row_cells.last() {
        Some(cell) => cell,
        None => "",
    }
}

/// Splits a markdown table row into trimmed cells, dropping the outer pipes.
fn cells(row: &str) -> Vec<&str> {
    row.trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}

/// The lines of the `## <heading>` section, excluding the heading.
fn section<'a>(
    text: &'a str,
    heading: &'static str,
    source: &'static str,
) -> Result<Vec<&'a str>, IndexError> {
    let wanted = format!("## {heading}");
    let mut lines = text.lines();
    let mut found = false;
    for line in lines.by_ref() {
        if line.trim() == wanted {
            found = true;
            break;
        }
    }
    if !found {
        return fail(IndexError::NoSection { source, heading }, "section");
    }
    Ok(lines.take_while(|line| !line.starts_with("## ")).collect())
}

/// The document's own H1, skipping YAML front matter, else `fallback`.
fn title_of(text: &str, fallback: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut start = 0;
    if let Some(first) = lines.first()
        && first.trim() == "---"
    {
        for (index, line) in lines.iter().enumerate().skip(1) {
            if line.trim() == "---" {
                start = index.saturating_add(1);
                break;
            }
        }
    }
    for line in lines.iter().skip(start) {
        if let Some(title) = line.strip_prefix("# ") {
            return title.trim().to_owned();
        }
    }
    fallback.to_owned()
}

/// The paragraph under the H1, joined onto one line.
fn first_paragraph(text: &str) -> Result<String, IndexError> {
    let mut lines = text.lines();
    let mut found = false;
    for line in lines.by_ref() {
        if line.starts_with("# ") {
            found = true;
            break;
        }
    }
    if !found {
        return fail(IndexError::NoHeading, "first_paragraph");
    }
    let mut paragraph: Vec<&str> = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            if paragraph.is_empty() {
                continue;
            }
            break;
        }
        paragraph.push(line.trim());
    }
    Ok(paragraph.join(" "))
}

/// The crate manifest's `repository` field, which is the repository root.
fn repository_url(root: &Path) -> Result<String, IndexError> {
    let text = read(&root.join("crates/lgwks-std/Cargo.toml"))?;
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("repository") else {
            continue;
        };
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let quoted = value.trim();
        if let Some(inner) = quoted
            .strip_prefix('"')
            .and_then(|stripped| stripped.strip_suffix('"'))
        {
            return Ok(inner.trim_end_matches('/').to_owned());
        }
    }
    fail(IndexError::NoRepository, "repository_url")
}

/// A table row whose first cell carries a link: `` | [`text`](target) | ... ``.
fn link_row(row: &str) -> Option<&str> {
    let rest = row.strip_prefix('|')?.trim_start();
    let rest = rest.strip_prefix("[`")?;
    let (text, rest) = rest.split_once("`](")?;
    if text.is_empty() || text.contains('`') {
        return None;
    }
    let (target, _) = rest.split_once(')')?;
    if target.is_empty() {
        return None;
    }
    Some(target)
}

/// The docs.rs URL in a crates-table cell, if it holds one.
fn docs_rs_link(cell: &str) -> Option<&str> {
    let start = cell.find("(https://docs.rs/")?;
    let rest = cell.get(start.saturating_add(1)..)?;
    let (link, _) = rest.split_once(')')?;
    Some(link)
}

/// Every link target in `text`, as the reader's browser would resolve it.
///
/// A target is the run of characters after `](` that holds no `)`, `#` or
/// whitespace, closed by `)` or by a `#fragment` and then `)`.
fn link_targets(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    for piece in text.split("](").skip(1) {
        let end = match piece.find(|ch: char| ch == ')' || ch == '#' || ch.is_whitespace()) {
            Some(end) => end,
            None => piece.len(),
        };
        let Some(target) = piece.get(..end) else {
            continue;
        };
        if target.is_empty() {
            continue;
        }
        let Some(after) = piece.get(end..) else {
            continue;
        };
        let closes = match after.chars().next() {
            Some(')') => true,
            Some('#') => after.contains(')'),
            _ => false,
        };
        if closes {
            found.push(target);
        }
    }
    found
}

/// The canonical path of `candidate` when it is a file inside `root`.
fn file_under(root: &Path, candidate: &Path) -> Option<PathBuf> {
    let resolved = match fs::canonicalize(candidate) {
        Ok(path) => path,
        Err(error) => {
            lgwks_std::trace::debug!(%error, path = ?candidate, "a link target does not resolve");
            return None;
        }
    };
    if resolved.is_file() && resolved.starts_with(root) {
        Some(resolved)
    } else {
        None
    }
}

/// `path` relative to `root`, spelled with forward slashes.
fn relative(root: &Path, path: &Path) -> Option<String> {
    match path.strip_prefix(root) {
        Ok(rest) => Some(
            rest.components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/"),
        ),
        Err(error) => {
            lgwks_std::trace::debug!(%error, path = ?path, "a path is outside the repository");
            None
        }
    }
}

/// The directory a file sits in, or `fallback` for a path with no parent.
fn directory_of<'a>(path: &'a Path, fallback: &'a Path) -> &'a Path {
    match path.parent() {
        Some(parent) => parent,
        None => fallback,
    }
}

/// Every repository document a link in one of `documents` resolves to.
fn linked_from(root: &Path, documents: &[String]) -> Result<Vec<String>, IndexError> {
    let mut found: Vec<String> = Vec::new();
    for document in documents {
        let page = root.join(document);
        let text = read(&page)?;
        let directory = directory_of(&page, root);
        for target in link_targets(&text) {
            if target.contains("://") {
                continue;
            }
            if let Some(resolved) = file_under(root, &directory.join(target))
                && let Some(name) = relative(root, &resolved)
            {
                found.push(name);
            }
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

/// Every `.md` file under `directory`, sorted.
fn markdown_under(directory: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<(), IndexError> {
    if depth > MAX_DEPTH {
        return fail(
            IndexError::TooDeep {
                path: directory.to_path_buf(),
            },
            "markdown_under",
        );
    }
    for entry in entries_of(directory, "markdown_under")? {
        let path = entry.path();
        if path.is_dir() {
            markdown_under(&path, depth.saturating_add(1), out)?;
        } else if path.extension().is_some_and(|extension| extension == "md") {
            out.push(path);
        }
    }
    out.sort();
    Ok(())
}

/// The crate directories under `crates/`, sorted by name.
fn crate_directories(root: &Path) -> Result<Vec<String>, IndexError> {
    let mut names = Vec::new();
    for entry in entries_of(&root.join("crates"), "crate_directories")? {
        if entry.path().is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Reads the crates table into `(name, docs.rs link, description)` rows.
fn crates_table(readme: &str, problems: &mut Vec<String>) -> Result<Vec<Row>, IndexError> {
    let mut crates = Vec::new();
    for row in section(readme, "Crates", "README.md")? {
        if !row.starts_with("| `") {
            continue;
        }
        let row_cells = cells(row);
        let crate_name = match row_cells.first() {
            Some(cell) => cell.trim_matches('`'),
            None => "",
        };
        let link = match row_cells.get(2) {
            Some(cell) => docs_rs_link(cell),
            None => None,
        };
        let Some(link) = link else {
            problems.push(format!("crates table row for {crate_name} has no docs.rs link"));
            continue;
        };
        crates.push(Row {
            name: crate_name.to_owned(),
            link: link.to_owned(),
            description: last_cell(&row_cells).to_owned(),
        });
    }
    Ok(crates)
}

/// Reads the Documentation table into `(title, path, description)` rows.
fn documents_table(
    root: &Path,
    index: &str,
    index_path: &Path,
    problems: &mut Vec<String>,
) -> Result<Vec<Row>, IndexError> {
    let mut documents = Vec::new();
    for row in section(index, "Documentation", "docs/README.md")? {
        let Some(link) = link_row(row) else {
            continue;
        };
        let candidate = directory_of(index_path, root).join(link);
        let Some(path) = file_under(root, &candidate) else {
            problems.push(format!("the Documentation table names {link}, which does not exist"));
            continue;
        };
        let Some(name) = relative(root, &path) else {
            continue;
        };
        let text = read(&path)?;
        let fallback = match path.file_name() {
            Some(file) => file.to_string_lossy().into_owned(),
            None => String::new(),
        };
        documents.push(Row {
            name: title_of(&text, &fallback),
            link: name,
            description: last_cell(&cells(row)).to_owned(),
        });
    }
    Ok(documents)
}

/// Reads the two tables and the tree, and renders the index.
fn generate(root: &Path) -> Result<Generated, IndexError> {
    let root = match fs::canonicalize(root) {
        Ok(path) => path,
        Err(cause) => {
            return fail(
                IndexError::Io {
                    path: root.to_path_buf(),
                    cause,
                },
                "generate",
            );
        }
    };
    let readme = read(&root.join("README.md"))?;
    let index_path = root.join("docs").join("README.md");
    let index = read(&index_path)?;
    let repo = repository_url(&root)?;
    let mut problems: Vec<String> = Vec::new();
    let crates = crates_table(&readme, &mut problems)?;
    let documents = documents_table(&root, &index, &index_path, &mut problems)?;

    // A document nobody indexed is invisible to every reader that arrives through
    // this file. A guide split under the 120-line .md law keeps one entry page in
    // the table and links its parts from it, so a part is accepted only when an
    // indexed document really links to it.
    let indexed: Vec<String> = documents.iter().map(|row| row.link.clone()).collect();
    let linked = linked_from(&root, &indexed)?;
    let mut markdown = Vec::new();
    markdown_under(&root.join("docs"), 0, &mut markdown)?;
    for path in markdown {
        let Some(name) = relative(&root, &path) else {
            continue;
        };
        if indexed.contains(&name) || name == "docs/README.md" || linked.contains(&name) {
            continue;
        }
        problems.push(format!(
            "{name} exists but the Documentation table in docs/README.md omits it"
        ));
    }

    // A crate nobody indexed is invisible to every reader that arrives through
    // this file.
    for directory in crate_directories(&root)? {
        let crate_name = directory.replace('-', "_");
        if !crates.iter().any(|row| row.name == crate_name) {
            problems.push(format!(
                "crates/{directory} exists but the README crates table omits it"
            ));
        }
    }

    if !problems.is_empty() {
        return fail(IndexError::Inconsistent(problems), "generate");
    }
    render(&repo, &readme, &crates, &documents, &indexed)
}

/// Renders the index text from the tables.
fn render(
    repo: &str,
    readme: &str,
    crates: &[Row],
    documents: &[Row],
    indexed: &[String],
) -> Result<Generated, IndexError> {
    let blob = |path: &str| format!("{repo}/blob/HEAD/{path}");
    let mut out: Vec<String> = vec![
        "# logicalworks-crates".to_owned(),
        String::new(),
        format!("> {}", first_paragraph(readme)?),
        String::new(),
        format!("Source: {repo}"),
        String::new(),
        "Development index. Every link below resolves against `main` as it stood \
         when this file was generated, and `main` moves. The crates.io links name \
         the latest published version and the docs.rs links describe it; the \
         documentation links describe unreleased source. For a released version, \
         use that release's tag rather than anything below."
            .to_owned(),
        String::new(),
        "## Crates".to_owned(),
        String::new(),
    ];
    for row in crates {
        out.push(format!("- [{}]({}) — {}", row.name, row.link, row.description));
    }
    out.push(String::new());
    out.push("## Documentation".to_owned());
    out.push(String::new());
    for row in documents {
        out.push(format!(
            "- [{}]({}) — {}",
            row.name,
            blob(&row.link),
            row.description
        ));
    }
    let remaining: Vec<&(&str, &str)> = OPTIONAL
        .iter()
        .filter(|&&(target, _)| !indexed.iter().any(|name| name == target))
        .collect();
    if !remaining.is_empty() {
        out.push(String::new());
        out.push("## Optional".to_owned());
        out.push(String::new());
        for &(target, description) in remaining {
            out.push(format!("- [{target}]({}) — {description}", blob(target)));
        }
    }
    out.push(String::new());
    Ok(Generated {
        text: out.join("\n"),
        crates: crates.len(),
        documents: documents.len(),
    })
}

/// Generates the index and writes `llms.txt` at the root.
fn run(root: &Path, out: &mut impl Write) -> Result<(), Box<dyn std::error::Error>> {
    let generated = generate(root)?;
    fs::write(root.join("llms.txt"), &generated.text)?;
    writeln!(
        out,
        "wrote llms.txt — {} crates, {} documents",
        generated.crates, generated.documents
    )?;
    Ok(())
}

/// The root named on the command line, else the working directory.
fn root_argument() -> PathBuf {
    match std::env::args_os().nth(1) {
        Some(argument) => PathBuf::from(argument),
        None => PathBuf::from("."),
    }
}

fn main() -> ExitCode {
    let root = root_argument();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let stderr = io::stderr();
    let mut err = stderr.lock();
    match run(&root, &mut out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if writeln!(err, "error: {error}").is_err() {
                return ExitCode::from(2);
            }
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{cells, docs_rs_link, first_paragraph, generate, link_row, link_targets, title_of};
    use std::error::Error;
    use std::path::Path;

    type TestResult = Result<(), Box<dyn Error>>;

    #[test]
    fn a_row_is_split_into_trimmed_cells_without_its_outer_pipes() {
        assert_eq!(cells("| a | b  |c |"), vec!["a", "b", "c"]);
    }

    #[test]
    fn a_link_row_names_its_target_and_a_plain_row_names_none() {
        assert_eq!(link_row("| [`a.md`](guides/a.md) | text |"), Some("guides/a.md"));
        assert_eq!(link_row("| plain | text |"), None);
        assert_eq!(link_row("| [`a`]() | text |"), None);
    }

    #[test]
    fn a_docs_rs_link_is_read_out_of_its_cell() {
        assert_eq!(
            docs_rs_link("[docs](https://docs.rs/lgwks_std)"),
            Some("https://docs.rs/lgwks_std")
        );
        assert_eq!(docs_rs_link("no link here"), None);
    }

    #[test]
    fn link_targets_drop_fragments_and_refuse_a_title_after_a_space() {
        assert_eq!(
            link_targets("see [a](x.md#top) and [b](y.md) and [c](z.md \"t\")"),
            vec!["x.md", "y.md"]
        );
    }

    #[test]
    fn a_title_skips_front_matter_and_falls_back_to_the_name() {
        assert_eq!(title_of("---\nk: v\n---\n# Real\n", "f.md"), "Real");
        assert_eq!(title_of("no heading\n", "f.md"), "f.md");
    }

    #[test]
    fn the_first_paragraph_is_joined_onto_one_line() -> TestResult {
        let text = "# T\n\nfirst\nsecond\n\nthird\n";
        assert_eq!(first_paragraph(text)?, "first second");
        Ok(())
    }

    /// The committed `llms.txt` is exactly what this binary renders from the
    /// repository it sits in: the same check CI runs, as a test.
    #[test]
    fn the_committed_index_is_what_generate_renders() -> TestResult {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let generated = generate(&root)?;
        let committed = std::fs::read_to_string(root.join("llms.txt"))?;
        assert_eq!(generated.text, committed);
        Ok(())
    }
}
