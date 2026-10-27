//! Every requirement a spec defines has an owner: a falsifier row that requires it.
//!
//! A requirement written as `**DX-07.**` in a `docs/*.spec.md` page is a claim
//! about the public interface. It is owned when some row of
//! `docs/acceptance/t-rows.toml` names it in `requires`, because that row's
//! tests are what the `Acceptance receipts` job records per revision. A
//! requirement no row requires is unowned: nothing would notice it drifting
//! (#420, where the declarative spec still pointed at its closed tracker #87).
//!
//! Only the families listed in `CHECKED` are refused today; a family joins
//! when each of its ids is owned or carries a reason in `EXEMPT`. A reason
//! names what holds the requirement instead of a row, so an exemption is a
//! statement a reviewer can check, never a bare allowlist entry.
//!
//! Usage: `check-spec-owners [ROOT]`. Exit status 1 on an unowned requirement.
//!
//! The one implementation of this check in the estate (#695): this file is the
//! rule, the `check-spec-owners` binary reads its root argument and runs
//! `execute`, and `logical_ci`'s `governance` gate calls `execute` as a
//! library, so the requirement that every checked spec id has a falsifier row
//! exists once with one owner.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The families whose every id must be owned. PR joins when PR-01 is owned: the
/// review path has no read-only profile yet, so a profile without the
/// publication right cannot be shown to stop at a draft (#421).
const CHECKED: [&str; 3] = ["DX", "AI", "LC"];

/// Ids a row does not require, and what holds each of them instead.
const EXEMPT: [(&str, &str); 7] = [
    (
        "DX-02",
        "a vocabulary (Task, Host, Report), not a behaviour: T01 drives exactly those three names",
    ),
    (
        "DX-12",
        "a release discipline (published API changes, compiled examples): held by the \
         readme-quickstart lane and the CHANGELOG, not by a falsifier row",
    ),
    (
        "AI-01",
        "the umbrella over AI-02..09: the executing half is owned by T26-T30, the writing half \
         by the bench/ai-authoring model matrix (#270), which is an evaluation, not a falsifier",
    ),
    (
        "AI-09",
        "an evaluation protocol (fixed models, matched budgets, variance): held by \
         bench/ai-authoring's README, not by a falsifier row",
    ),
    (
        "LC-14",
        "an implementation discipline: held by the workspace clippy contract (as_conversions, \
         arithmetic_side_effects forbidden) and the rust-guard, not by a behaviour a row can \
         falsify",
    ),
    (
        "LC-15",
        "a performance requirement: held by the measured async benchmarks of #269, not by a \
         pass/fail row",
    ),
    (
        "LC-16",
        "a structure rule (no new crate or dependency): held by `lgwks-deps check` and the size \
         law",
    ),
];

/// Why the check could not run.
#[derive(Debug)]
enum SpecError {
    /// A file or directory could not be read.
    Io {
        /// What was being read.
        path: PathBuf,
        /// What the operating system said.
        cause: io::Error,
    },
}

impl fmt::Display for SpecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Io {
                ref path,
                ref cause,
            } => {
                write!(formatter, "cannot read {}: {cause}", path.display())
            }
        }
    }
}

impl std::error::Error for SpecError {}

/// Logs `error` at the site that produced it, then hands it back as the `Err`.
fn fail<T>(error: SpecError, site: &'static str) -> Result<T, SpecError> {
    lgwks_std::trace::debug!(error = ?error, site, "returning an error to the caller");
    Err(error)
}

/// Reads a file whole, naming it when it cannot be read.
fn read(path: &Path) -> Result<String, SpecError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(cause) => fail(
            SpecError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "read",
        ),
    }
}

/// The family of an id: the text before its dash.
fn family_of(id: &str) -> &str {
    match id.split_once('-') {
        Some((family, _)) => family,
        None => id,
    }
}

/// A requirement id read from the front of `text`: `XX-NN` or `XXX-NN`, and the
/// length it took.
fn id_prefix(text: &str) -> Option<(String, usize)> {
    let letters = text
        .chars()
        .take_while(char::is_ascii_uppercase)
        .take(3)
        .count();
    if letters < 2 {
        return None;
    }
    let rest = text.get(letters..)?.strip_prefix('-')?;
    let digits = rest.get(..2)?;
    if !digits.chars().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    let family = text.get(..letters)?;
    Some((format!("{family}-{digits}"), letters.saturating_add(3)))
}

/// Every requirement id defined in `text` as `**XX-NN.**`.
fn definitions(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (start, _) in text.char_indices() {
        let Some(tail) = text.get(start..).and_then(|rest| rest.strip_prefix("**")) else {
            continue;
        };
        let Some((id, length)) = id_prefix(tail) else {
            continue;
        };
        if tail
            .get(length..)
            .is_some_and(|after| after.starts_with(".**"))
        {
            found.push(id);
        }
    }
    found
}

/// A token `XX-NN` or a bare `NN`, as its optional family and its number.
fn token(part: &str) -> Option<(Option<String>, String)> {
    if part.len() == 2 && part.chars().all(|digit| digit.is_ascii_digit()) {
        return Some((None, part.to_owned()));
    }
    let (id, length) = id_prefix(part)?;
    if length != part.len() {
        return None;
    }
    let (family, number) = id.split_once('-')?;
    Some((Some(family.to_owned()), number.to_owned()))
}

/// Every id a `requires` string names; `DX-01/03/04` names DX-01, DX-03, DX-04.
///
/// A bare number takes the family of the nearest id before it, so `DX-05/LC-08`
/// names DX-05 and LC-08, never DX-08. A part that is not an id breaks the run.
fn required_in(requires: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut family = String::new();
    for part in requires.split([',', ';', '/']) {
        let Some((named, number)) = token(part.trim()) else {
            family.clear();
            continue;
        };
        if let Some(named) = named {
            family = named;
        }
        if !family.is_empty() {
            ids.push(format!("{family}-{number}"));
        }
    }
    ids
}

/// The `requires = "..."` value of a table line, if the line is one.
fn requires_value(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("requires")?;
    let value = rest.trim_start().strip_prefix('=')?.trim();
    value.strip_prefix('"')?.strip_suffix('"')
}

/// The checked ids no row requires and no reason exempts, in order.
fn unowned(found: &Definitions, owned: &BTreeSet<String>) -> Vec<String> {
    found
        .keys()
        .filter(|key| {
            CHECKED.contains(&family_of(key))
                && !owned.contains(*key)
                && !EXEMPT.iter().any(|&(exempt, _)| exempt == key.as_str())
        })
        .cloned()
        .collect()
}

/// Every `docs/*.spec.md` page, sorted.
fn spec_pages(root: &Path) -> Result<Vec<PathBuf>, SpecError> {
    let directory = root.join("docs");
    let reader = match fs::read_dir(&directory) {
        Ok(reader) => reader,
        Err(cause) => {
            return fail(
                SpecError::Io {
                    path: directory,
                    cause,
                },
                "spec_pages",
            );
        }
    };
    let mut pages: Vec<PathBuf> = Vec::new();
    for entry in reader {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                let is_spec = path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(".spec.md"));
                if is_spec {
                    pages.push(path);
                }
            }
            Err(cause) => {
                return fail(
                    SpecError::Io {
                        path: directory,
                        cause,
                    },
                    "spec_pages",
                );
            }
        }
    }
    pages.sort();
    Ok(pages)
}

/// Requirement ids, each with the page that defines it.
type Definitions = BTreeMap<String, String>;

/// Every id defined in the `docs/*.spec.md` pages, with the page defining it.
fn defined_in_docs(root: &Path) -> Result<Definitions, SpecError> {
    let mut found = BTreeMap::new();
    for page in spec_pages(root)? {
        let Some(file) = page.file_name() else {
            continue;
        };
        let name = file.to_string_lossy().into_owned();
        for id in definitions(&read(&page)?) {
            found.entry(id).or_insert_with(|| name.clone());
        }
    }
    Ok(found)
}

/// Runs the check over `root`, writing its verdict to the two handles.
#[doc(hidden)]
pub fn execute(
    root: &Path,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<u8, Box<dyn std::error::Error>> {
    let found = defined_in_docs(root)?;
    let rows = read(&root.join("docs/acceptance/t-rows.toml"))?;
    let owned: BTreeSet<String> = rows
        .lines()
        .filter_map(requires_value)
        .flat_map(required_in)
        .collect();
    let missing = unowned(&found, &owned);
    if !missing.is_empty() {
        for key in &missing {
            let page = match found.get(key) {
                Some(page) => page.as_str(),
                None => "?",
            };
            writeln!(
                err,
                "spec owners: {key} ({page}) is required by no row of docs/acceptance/t-rows.toml"
            )?;
        }
        return Ok(1);
    }
    let checked = found
        .keys()
        .filter(|key| CHECKED.contains(&family_of(key)))
        .count();
    writeln!(
        out,
        "spec owners: {checked} {} requirement(s), each required by a falsifier row or exempt \
         with a reason ({} exempt)",
        CHECKED.join("/"),
        EXEMPT.len()
    )?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::{
        EXEMPT, defined_in_docs, definitions, execute, required_in, requires_value, unowned,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::error::Error;
    use std::path::Path;

    type TestResult = Result<(), Box<dyn Error>>;

    #[test]
    fn a_slash_list_names_every_id() {
        assert_eq!(
            required_in("DX-01/03/04"),
            vec!["DX-01", "DX-03", "DX-04"],
            "a slash list"
        );
        assert_eq!(
            required_in("DX-05/LC-08"),
            vec!["DX-05", "LC-08"],
            "a second family"
        );
        // A part that is not an id breaks the run: a number after it has no family.
        assert_eq!(
            required_in("DX-01/see below/05"),
            vec!["DX-01"],
            "a broken run"
        );
    }

    #[test]
    fn a_definition_is_a_bold_id_ending_in_a_dot() {
        assert_eq!(
            definitions("**DX-07.** text **AI-12.** and **X-01.** and **DX-7.** and **DX-08**"),
            vec!["DX-07", "AI-12"],
            "only the well-formed bold ids"
        );
    }

    #[test]
    fn a_requires_line_gives_its_quoted_value() {
        assert_eq!(
            requires_value("requires = \"DX-01/03\""),
            Some("DX-01/03"),
            "a row"
        );
        assert_eq!(requires_value("row = \"requires\""), None, "another key");
    }

    #[test]
    fn an_unrequired_id_is_reported_and_an_owned_one_is_not() {
        let found: BTreeMap<String, String> = [
            ("DX-98", "a.spec.md"),
            ("DX-01", "a.spec.md"),
            ("LC-99", "b.spec.md"),
            ("PR-99", "c.spec.md"),
        ]
        .into_iter()
        .map(|(key, page)| (key.to_owned(), page.to_owned()))
        .collect();
        let owned: BTreeSet<String> = ["DX-01".to_owned()].into_iter().collect();
        // LC is checked, so its unowned id is reported; PR is not checked yet.
        assert_eq!(
            unowned(&found, &owned),
            vec!["DX-98", "LC-99"],
            "unowned ids in order"
        );
    }

    #[test]
    fn an_exempt_id_is_not_reported() {
        let found: BTreeMap<String, String> = [("DX-02", "a.spec.md"), ("DX-12", "a.spec.md")]
            .into_iter()
            .map(|(key, page)| (key.to_owned(), page.to_owned()))
            .collect();
        assert!(
            unowned(&found, &BTreeSet::new()).is_empty(),
            "exempt ids are not reported"
        );
    }

    #[test]
    fn every_exemption_names_a_defined_id() -> TestResult {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let found = defined_in_docs(&root)?;
        let stale: Vec<&str> = EXEMPT
            .iter()
            .map(|&(id, _)| id)
            .filter(|id| !found.contains_key(*id))
            .collect();
        assert!(
            stale.is_empty(),
            "exemptions naming no defined id: {stale:?}"
        );
        Ok(())
    }

    #[test]
    fn this_repository_owns_every_checked_requirement() -> TestResult {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute(&root, &mut out, &mut err)?;
        assert_eq!(
            code,
            0,
            "unowned requirements: {}",
            String::from_utf8_lossy(&err)
        );
        Ok(())
    }
}
