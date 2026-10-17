//! The doc-example ceiling (#526): `script!` is the one supported interface, so the
//! number of documentation examples that reach a workspace crate by path may fall
//! and never rise.
//!
//! `contract/PUBLIC_SURFACE.toml` names, per documentation file, how many fenced
//! Rust blocks call a workspace crate directly. The judgement is the surface
//! ratchet's (`ratchet.rs`): exact, an unlisted file is held to zero, a ceiling
//! above the count is stale.
//!
//! Read: the `///` and `//!` lines of every library source file, and every
//! Markdown file under `crates/` and `docs/` that git holds. Not read: `#[doc = ..]`
//! attributes and block doc comments; the page an `include_str!` pulls into a
//! crate's docs is read as the Markdown file it is.
//!
//! A fence is the `CommonMark` one: three or more backticks or tildes, closed by the
//! same character at least as many times (an unclosed fence runs to the end). Its
//! block is Rust the way rustdoc decides: every token of its info string, a
//! parenthesised comment aside, is one rustdoc knows (`rust`, `ignore`,
//! `should_panic`, `no_run`, `compile_fail`, `test_harness`, `edition..`,
//! `standalone_crate`, an error code), so `text`, `toml` and `sh` blocks are not.
//!
//! Counted: a Rust block that names a workspace crate as a path, with no `script!`.
//! A block that holds `script!` is the supported shape, whatever it also imports.
//! The crate names are the workspace members the audit read, never a list here.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::files::markdown_files;
use super::lexer::{code_only, is_word, path_reaches};
use super::ratchet::{Ratchet, ceilings, judge};
use super::surface::is_library;
use super::{AuditError, posix_relative, read};

/// How the example ratchet reads its table and words its findings.
const EXAMPLES: Ratchet = Ratchet {
    label: "EXAMPLE",
    table: "examples",
    counted: "direct-use doc example(s)",
    none: "holds no direct-use doc example",
    refused: "doc examples go through `script!` (#526), so a new direct-use example is a refused one",
};

/// A fenced block being read.
struct Fence {
    /// The character that opened it.
    mark: char,
    /// How many of them the opening line wrote; a closing line needs as many.
    width: usize,
    /// Whether rustdoc would compile the block as Rust.
    rust: bool,
    /// The block's lines, kept only for a Rust block.
    body: String,
}

/// The fence a line opens: its character, width and info string.
fn opening(line: &str) -> Option<(char, usize, &str)> {
    let trimmed = line.trim_start();
    let mark = trimmed.chars().next()?;
    if mark != '`' && mark != '~' {
        return None;
    }
    let width = trimmed.chars().take_while(|ch| *ch == mark).count();
    if width < 3 {
        return None;
    }
    let info = trimmed.get(width..)?;
    if mark == '`' && info.contains('`') {
        return None;
    }
    Some((mark, width, info.trim()))
}

/// Whether `line` closes a fence of `width` copies of `mark`.
fn closes(line: &str, mark: char, width: usize) -> bool {
    let trimmed = line.trim();
    trimmed.chars().count() >= width && trimmed.chars().all(|ch| ch == mark)
}

/// Whether rustdoc knows an info-string token.
fn known(token: &str) -> bool {
    matches!(
        token,
        "rust"
            | "ignore"
            | "should_panic"
            | "no_run"
            | "compile_fail"
            | "test_harness"
            | "standalone_crate"
    ) || token.starts_with("ignore-")
        || token
            .strip_prefix("edition")
            .is_some_and(|year| !year.is_empty() && year.chars().all(|ch| ch.is_ascii_digit()))
        || token
            .strip_prefix('E')
            .is_some_and(|code| code.len() == 4 && code.chars().all(|ch| ch.is_ascii_digit()))
}

/// `info` without its parenthesised comments: `ignore (needs a network)` is `ignore`.
fn without_comments(info: &str) -> String {
    let mut bare = String::new();
    let mut depth = 0_usize;
    for ch in info.chars() {
        match ch {
            '(' => depth = depth.saturating_add(1),
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => bare.push(ch),
            _ => {}
        }
    }
    bare
}

/// Whether an info string makes its block Rust: an empty one does, and so does one
/// whose every token, comments aside, rustdoc knows.
fn is_rust(info: &str) -> bool {
    without_comments(info)
        .split([',', ' ', '\t'])
        .filter(|token| !token.is_empty())
        .all(known)
}

/// Whether `code` invokes `script!`.
fn holds_script(code: &str) -> bool {
    code.match_indices("script!").any(|(index, _)| {
        let before = code.get(..index).and_then(|head| head.chars().next_back());
        !before.is_some_and(is_word)
    })
}

/// Whether a Rust block names a workspace crate as a path and holds no `script!`.
fn direct(body: &str, crates: &BTreeSet<String>) -> bool {
    let code = code_only(body);
    !holds_script(&code)
        && crates
            .iter()
            .any(|name| !path_reaches(&code, name).is_empty())
}

/// How many direct-use Rust blocks `text` fences.
fn direct_fences(text: &str, crates: &BTreeSet<String>) -> usize {
    let mut count = 0_usize;
    let mut open: Option<Fence> = None;
    for line in text.lines() {
        let Some(mut fence) = open.take() else {
            open = opening(line).map(|(mark, width, info)| Fence {
                mark,
                width,
                rust: is_rust(info),
                body: String::new(),
            });
            continue;
        };
        if closes(line, fence.mark, fence.width) {
            count = count.saturating_add(usize::from(fence.rust && direct(&fence.body, crates)));
        } else {
            if fence.rust {
                fence.body.push_str(line);
                fence.body.push('\n');
            }
            open = Some(fence);
        }
    }
    if let Some(fence) = open {
        count = count.saturating_add(usize::from(fence.rust && direct(&fence.body, crates)));
    }
    count
}

/// The text of the `///` and `//!` doc lines of Rust `source`, one per line.
fn doc_lines(source: &str) -> String {
    let mut text = String::new();
    for line in source.lines() {
        let trimmed = line.trim_start();
        let outer = trimmed
            .strip_prefix("///")
            .filter(|rest| !rest.starts_with('/'));
        if let Some(rest) = outer.or_else(|| trimmed.strip_prefix("//!")) {
            text.push_str(rest);
            text.push('\n');
        }
    }
    text
}

/// Notes `file`'s direct-use blocks in `found` when it has any.
fn record(
    found: &mut BTreeMap<String, usize>,
    file: String,
    text: &str,
    crates: &BTreeSet<String>,
) {
    let total = direct_fences(text, crates);
    if total > 0 {
        found.insert(file, total);
    }
}

/// Documentation file -> its direct-use block count, for every file that has any.
fn counts(
    repo: &Path,
    source: &[PathBuf],
    crates: &BTreeSet<String>,
) -> Result<BTreeMap<String, usize>, AuditError> {
    let mut found = BTreeMap::new();
    for path in source {
        let relative = posix_relative(repo, path);
        if is_library(&relative) {
            record(&mut found, relative, &doc_lines(&read(path)?), crates);
        }
    }
    for path in markdown_files(repo)? {
        record(
            &mut found,
            posix_relative(repo, &path),
            &read(&path)?,
            crates,
        );
    }
    Ok(found)
}

/// Every example finding of the tree `source` names, against the contract.
pub(super) fn findings(
    repo: &Path,
    source: &[PathBuf],
    crates: &BTreeSet<String>,
) -> Result<Vec<String>, AuditError> {
    Ok(judge(
        &EXAMPLES,
        &counts(repo, source, crates)?,
        &ceilings(repo, &EXAMPLES)?,
    ))
}

/// Writes the example table the tree implies, to record a legitimate change.
pub(super) fn baseline(
    repo: &Path,
    source: &[PathBuf],
    crates: &BTreeSet<String>,
    out: &mut impl Write,
) -> Result<(), Box<dyn std::error::Error>> {
    writeln!(
        out,
        "\n# The doc-example ceiling (#526): per documentation file, how many fenced Rust\n\
         # blocks name a workspace crate by path and hold no `script!`. Exact, like the one above."
    )?;
    writeln!(out, "[{}]", EXAMPLES.table)?;
    for (file, count) in counts(repo, source, crates)? {
        writeln!(out, "\"{file}\" = {count}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{EXAMPLES, baseline, direct_fences, doc_lines, findings, is_rust, known, opening};
    use crate::files::{Scratch, put, says, source_files};
    use crate::ratchet::CONTRACT;
    use std::collections::BTreeSet;
    use std::error::Error;
    use std::path::Path;

    type TestResult = Result<(), Box<dyn Error>>;

    /// The workspace the fixtures live in.
    fn workspace() -> BTreeSet<String> {
        BTreeSet::from(["lgwks_std".to_owned(), "lgwks_bot".to_owned()])
    }

    /// Every example finding of the scratch repository as the disk holds it now.
    fn judged(root: &Path) -> Result<Vec<String>, Box<dyn Error>> {
        Ok(findings(root, &source_files(root)?, &workspace())?)
    }

    /// Requirement #526, from rustdoc's "Documentation tests" chapter: a block is
    /// Rust when every token of its info string is one rustdoc knows, so an empty
    /// string is Rust and `text`, `toml` and `sh` are not; a comment in parentheses
    /// (`ignore (needs a network)`) is not a token.
    #[test]
    fn an_info_string_is_rust_only_when_rustdoc_knows_every_token() {
        for info in [
            "",
            "rust",
            "no_run",
            "rust,ignore",
            "ignore-windows",
            "edition2021",
            "compile_fail,E0277",
            "should_panic standalone_crate",
            "ignore (needs extra dependency)",
            "rust,ignore (needs, a comma)",
        ] {
            assert!(is_rust(info), "`{info}` is a Rust block");
        }
        for info in [
            "text",
            "toml",
            "sh",
            "rust,toml",
            "edition",
            "E12",
            "ignored",
            "text (not rust)",
        ] {
            assert!(!is_rust(info), "`{info}` is not a Rust block");
        }
        assert!(known("test_harness"), "a harness token is known");
    }

    /// Requirement #526, from the `CommonMark` spec's fenced code blocks: three or
    /// more fence characters open a block, a closing fence needs as many, inline
    /// code is not a fence, and an unclosed block runs to the end of the text.
    #[test]
    fn a_fence_opens_with_three_marks_and_closes_with_as_many() {
        assert_eq!(
            opening("  ```rust,no_run"),
            Some(('`', 3, "rust,no_run")),
            "an indented backtick fence with its info string"
        );
        assert_eq!(
            opening("~~~~ toml"),
            Some(('~', 4, "toml")),
            "a tilde fence is a fence"
        );
        assert_eq!(opening("``` has `inline` code```"), None, "inline code");
        assert_eq!(opening("``"), None, "two marks are not a fence");
        let text = "````\n```\nlgwks_std::x();\n```\n````\nafter\n```\nlgwks_bot::y();\n";
        assert_eq!(
            direct_fences(text, &workspace()),
            2,
            "a short inner fence does not close the block, and an unclosed one still counts"
        );
    }

    /// Requirement #526: a Rust block that names a workspace crate by path is a
    /// direct use; one that goes through `script!`, a block of another language,
    /// a comment or string that merely names the crate, and an unrelated crate are not.
    #[test]
    fn only_a_rust_block_calling_a_workspace_crate_without_script_is_counted() {
        let text = "```\nuse lgwks_std::json::Value;\n```\n\
                    ```rust\nlet run = lgwks_bot::runner::new();\n```\n\
                    ```rust\nuse lgwks_bot::script;\nscript! { let one = 1; }\n```\n\
                    ```toml\nlgwks_std = \"1\"\n```\n\
                    ```rust\n// lgwks_std::x();\nlet note = \"lgwks_bot::y()\";\n```\n\
                    ```rust\nuse serde::Serialize;\n```\n";
        assert_eq!(
            direct_fences(text, &workspace()),
            2,
            "the std use and the bot call, and nothing else"
        );
    }

    /// Requirement #526: `///` and `//!` are doc lines, `////` and `//` are not.
    #[test]
    fn only_triple_slash_and_inner_doc_lines_are_documentation() {
        let source = "//! inner\n    /// outer\n//// four slashes\n// plain\nlet a = 1; /// tail\n";
        assert_eq!(
            doc_lines(source),
            " inner\n outer\n",
            "the two doc lines, text after the marker"
        );
    }

    /// Requirement #526 ("CI refuses a doc example that calls a crate item directly
    /// rather than through `script!`"), as a contributor meets it on a repository
    /// read from disk: a recorded tree holds; a direct-use example in a doc comment
    /// and in a page are each refused; a `script!` example passes; recording the table
    /// makes both legitimate; and removing an example leaves a stale line.
    #[test]
    fn a_contributor_adding_a_direct_use_example_is_refused_until_the_table_records_it()
    -> TestResult {
        let scratch = Scratch::new()?;
        let root = &scratch.root;
        put(
            root,
            "crates/demo/src/lib.rs",
            "//! ```\n//! use lgwks_std::json::Value;\n//! ```\npub fn one() {}\n",
        )?;
        put(
            root,
            CONTRACT,
            "[examples]\n\"crates/demo/src/lib.rs\" = 1\n",
        )?;
        assert!(judged(root)?.is_empty(), "the recorded tree holds");

        put(
            root,
            "crates/demo/src/lib.rs",
            "//! ```\n//! use lgwks_std::json::Value;\n//! ```\n/// ```\n/// lgwks_bot::run();\n/// ```\n\
             /// ```\n/// script! { let ok = 1; }\n/// ```\npub fn one() {}\n",
        )?;
        put(
            root,
            "docs/guide.md",
            "text\n\n```rust\nlgwks_std::task::spawn();\n```\n",
        )?;
        let refused = judged(root)?;
        assert_eq!(refused.len(), 2, "one rise and one new page: {refused:?}");
        assert!(
            says(
                &refused,
                "crates/demo/src/lib.rs",
                "2 direct-use doc example(s), ceiling 1"
            ),
            "the added doc comment example is over its file's ceiling: {refused:?}"
        );
        assert!(
            says(
                &refused,
                "docs/guide.md",
                "1 direct-use doc example(s), ceiling 0"
            ),
            "a new page starts at zero: {refused:?}"
        );

        let mut recorded = Vec::new();
        baseline(root, &source_files(root)?, &workspace(), &mut recorded)?;
        let table = String::from_utf8(recorded)?;
        put(root, CONTRACT, &table)?;
        assert!(
            judged(root)?.is_empty(),
            "the table the tree implies is the change made legitimate: {table}"
        );

        put(root, "docs/guide.md", "text only\n")?;
        let stale = judged(root)?;
        assert!(
            says(&stale, "docs/guide.md", "holds no direct-use doc example"),
            "a ceiling for a page that stopped having examples is stale: {stale:?}"
        );
        Ok(())
    }

    /// Requirement #526 (a gate that cannot read its contract must not pass): a
    /// ceiling that is not a whole number and an absent contract are refusals,
    /// never "no ceilings".
    #[test]
    fn an_unreadable_example_table_is_a_refusal_not_an_empty_one() -> TestResult {
        let scratch = Scratch::new()?;
        let root = &scratch.root;
        put(root, "docs/guide.md", "```\nlgwks_std::x();\n```\n")?;
        assert!(judged(root).is_err(), "an absent contract is refused");
        put(
            root,
            CONTRACT,
            &format!("[{}]\n\"docs/guide.md\" = \"many\"\n", EXAMPLES.table),
        )?;
        assert!(
            judged(root).is_err(),
            "a ceiling written \"many\" is refused"
        );
        Ok(())
    }
}
