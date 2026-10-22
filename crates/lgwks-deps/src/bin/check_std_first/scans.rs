//! The judgements the audit makes over source: capabilities `std` or `lgwks_std`
//! already provide written out by hand, the storefront edges nothing consumes,
//! and the register's `policy-only:` marker.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::AuditError;
use super::files::cleaned_text;
use super::lexer::{code_only, is_word, path_reaches};
use super::manifest::Record;

/// The register `reason` prefix of an admitted-not-implemented edge (#418).
pub(super) const POLICY_ONLY: &str = "policy-only:";

/// Why a hand-formatted hex encoder is a finding. Every refusal names its
/// catalog entry (#732): a refusal that points at an owner nobody can look up is
/// the dead end the catalog exists to close.
const WHY_HEX: &str = "hand-formatted hex; `lgwks_std::hex::encode` is the estate's encoder, see the `hex` row of docs/catalog.md";

/// Why a written-out base64 alphabet is a finding.
const WHY_BASE64: &str = "a base64 alphabet written out; `lgwks_std::encoding` owns base64, see the `encoding` row of docs/catalog.md";

/// Why raw FFI is a finding.
const WHY_FFI: &str = "raw FFI; `lgwks_std` is where an audited syscall is allowed to live, see the `process` and `fs` rows of docs/catalog.md";

/// Why an OS thread or process id used as an identity is a finding.
const WHY_ID: &str = "a thread or process id used as an identity; both are reused by the OS, so \
                      `lgwks_std::random` is the distinguishable source, see the `id` row of docs/catalog.md";

/// Why an untracked thread is a finding.
const WHY_SPAWN: &str = "an untracked thread; `lgwks_std::task` and the bot's `JoinSet` are where a \
                         spawn is owned, see the `task` row of docs/catalog.md";

/// Why entropy from outside `lgwks_std::random` is a finding.
const WHY_ENTROPY: &str = "entropy from outside `lgwks_std::random`; INV-RANDOM-ONE-SOURCE admits one \
                           CSPRNG backend and `lgwks_std` owns it, see the `random` row of docs/catalog.md";

/// The base64 alphabet, as it would be written out.
/// Spelled in two pieces so this file does not contain the pattern it looks for.
const BASE64_ALPHABET: &str = concat!(
    "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
    "abcdefghijklmnopqrstuvwxyz0123456789+/"
);

/// The spawn path, spelled in two pieces for the same reason.
const SPAWN_PATH: &str = concat!("std::thr", "ead::spawn");

/// The urandom device path, spelled in two pieces for the same reason.
const URANDOM_PATH: &str = concat!("/dev/ura", "ndom");

/// The text after any whitespace at the front of `text`.
fn after_space(text: &str) -> &str {
    text.trim_start()
}

/// The text from byte `from`, or empty when `from` is past the end.
fn tail(text: &str, from: usize) -> &str {
    match text.get(from..) {
        Some(rest) if !rest.is_empty() => rest,
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

/// Whether the character before `index` continues a word.
fn word_before(text: &str, index: usize) -> bool {
    text.get(..index)
        .and_then(|head| head.chars().next_back())
        .is_some_and(is_word)
}

/// The offsets of `{ :02x }`-style format arguments: `\{\s*:0?2[xX]\s*\}`.
fn hex_offsets(text: &str) -> Vec<usize> {
    let mut found = Vec::new();
    for (index, _) in text.match_indices('{') {
        let rest = tail(text, index.saturating_add(1));
        let Some(rest) = after_space(rest).strip_prefix(':') else {
            continue;
        };
        let Some(rest) = skip_char(rest, '0').strip_prefix('2') else {
            continue;
        };
        let Some(rest) = rest.strip_prefix(['x', 'X']) else {
            continue;
        };
        if after_space(rest).starts_with('}') {
            found.push(index);
        }
    }
    found
}

/// The offsets of `extern "C"` followed by a word character: `\bextern\s+"C"\b`.
fn ffi_offsets(text: &str) -> Vec<usize> {
    let mut found = Vec::new();
    for (index, _) in text.match_indices("extern") {
        if word_before(text, index) {
            continue;
        }
        let rest = tail(text, index.saturating_add("extern".len()));
        let spaced = after_space(rest);
        if spaced.len() == rest.len() {
            continue;
        }
        let Some(after) = spaced.strip_prefix("\"C\"") else {
            continue;
        };
        if after.chars().next().is_some_and(is_word) {
            found.push(index);
        }
    }
    found
}

/// The offsets of a process or thread id read as an identity.
fn identity_offsets(text: &str) -> Vec<usize> {
    let mut found = Vec::new();
    for (index, _) in text.match_indices("std::process::id") {
        let rest = tail(text, index.saturating_add("std::process::id".len()));
        if after_space(rest).starts_with('(') {
            found.push(index);
        }
    }
    for (index, _) in text.match_indices("std::thread::current") {
        let rest = tail(text, index.saturating_add("std::thread::current".len()));
        let Some(rest) = after_space(rest).strip_prefix('(') else {
            continue;
        };
        let Some(rest) = after_space(rest).strip_prefix(')') else {
            continue;
        };
        let Some(rest) = after_space(rest).strip_prefix(".id") else {
            continue;
        };
        if after_space(rest).starts_with('(') {
            found.push(index);
        }
    }
    found.sort_unstable();
    found
}

/// The offsets of `std::thread::spawn` as a whole path: `\bstd::thread::spawn\b`.
fn spawn_offsets(text: &str) -> Vec<usize> {
    text.match_indices(SPAWN_PATH)
        .filter(|&(index, _)| {
            let after = tail(text, index.saturating_add(SPAWN_PATH.len()))
                .chars()
                .next();
            !word_before(text, index) && !after.is_some_and(is_word)
        })
        .map(|(index, _)| index)
        .collect()
}

/// The offsets of entropy read from outside `lgwks_std::random`.
fn entropy_offsets(text: &str) -> Vec<usize> {
    let mut found = Vec::new();
    for word in ["rand", "getrandom"] {
        for (index, _) in text.match_indices(word) {
            if word_before(text, index) {
                continue;
            }
            let rest = tail(text, index.saturating_add(word.len()));
            if after_space(rest).starts_with("::") {
                found.push(index);
            }
        }
    }
    found.extend(text.match_indices(URANDOM_PATH).map(|(index, _)| index));
    found.sort_unstable();
    found
}

/// A capability `std` or `lgwks_std` already provides, written by hand: each
/// pattern's offsets in `text`, with the replacement it names.
///
/// The patterns are a fixed, named list rather than a heuristic, because a
/// checker that guesses produces false positives and a checker with false
/// positives gets switched off.
pub(super) fn std_first_hits(text: &str) -> Vec<(usize, &'static str)> {
    let mut hits = Vec::new();
    hits.extend(
        hex_offsets(text)
            .into_iter()
            .map(|offset| (offset, WHY_HEX)),
    );
    hits.extend(
        text.match_indices(BASE64_ALPHABET)
            .map(|(offset, _)| (offset, WHY_BASE64)),
    );
    hits.extend(
        ffi_offsets(text)
            .into_iter()
            .map(|offset| (offset, WHY_FFI)),
    );
    hits.extend(
        identity_offsets(text)
            .into_iter()
            .map(|offset| (offset, WHY_ID)),
    );
    hits.extend(
        spawn_offsets(text)
            .into_iter()
            .map(|offset| (offset, WHY_SPAWN)),
    );
    hits.extend(
        entropy_offsets(text)
            .into_iter()
            .map(|offset| (offset, WHY_ENTROPY)),
    );
    hits
}

/// The identifiers in `text`, in order.
fn identifiers(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch);
        } else if !current.is_empty() {
            found.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        found.push(current);
    }
    found
        .into_iter()
        .filter(|word| !word.starts_with(|ch: char| ch.is_ascii_digit()))
        .collect()
}

/// The names `lgwks_deps::<…>` paths in `code` reach: a single segment or a braced group.
fn storefront_paths(code: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (index, _) in code.match_indices("lgwks_deps::") {
        if word_before(code, index) {
            continue;
        }
        let rest = tail(code, index.saturating_add("lgwks_deps::".len()));
        if let Some(group) = rest.strip_prefix('{') {
            if let Some((inside, _)) = group.split_once('}') {
                names.extend(identifiers(inside));
            }
            continue;
        }
        let first: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .collect();
        if first
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        {
            names.push(first);
        }
    }
    names
}

/// Each `lgwks_deps`-only edge -> the files whose code consumes it.
///
/// Consumption is a `lgwks_deps::<edge>` path in another workspace crate, or a
/// `<edge>::` path in the storefront's own source past `lib.rs`, whose re-export
/// lines are the admission, not a use of it. Tests, examples and fixtures of the
/// storefront itself are not consumers: a fixture that names `gpui` proves the
/// gate sees the edge, not that anything ships with it.
pub(super) fn storefront_consumers(
    repo: &Path,
    files: &[PathBuf],
    owned: &BTreeSet<String>,
) -> Result<BTreeMap<String, BTreeSet<String>>, AuditError> {
    let own_src = "crates/lgwks-deps/src/";
    let mut consumers: BTreeMap<String, BTreeSet<String>> = owned
        .iter()
        .map(|name| (name.clone(), BTreeSet::new()))
        .collect();
    for path in files {
        let relative = super::posix_relative(repo, path);
        let code = code_only(&cleaned_text(path)?);
        if relative.starts_with(own_src) {
            if relative == format!("{own_src}lib.rs") {
                continue;
            }
            for name in owned {
                if !path_reaches(&code, name).is_empty()
                    && let Some(users) = consumers.get_mut(name)
                {
                    users.insert(relative.clone());
                }
            }
            continue;
        }
        if relative.starts_with("crates/lgwks-deps/") {
            continue;
        }
        for name in storefront_paths(&code) {
            if let Some(users) = consumers.get_mut(&name) {
                users.insert(relative.clone());
            }
        }
    }
    Ok(consumers)
}

/// The edges the storefront owns and admits no other crate to.
pub(super) fn storefront_only(approved: &BTreeMap<String, Record>) -> BTreeSet<String> {
    approved
        .iter()
        .filter(|&(_, record)| {
            record.owner.as_deref() == Some("lgwks_deps")
                && record.consumers.len() == 1
                && record.consumers.contains("lgwks_deps")
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// The register lines whose `policy-only:` marker disagrees with the source.
///
/// Both directions fail: an unconsumed edge left unmarked reads as a delivered
/// capability, and a consumed edge still marked hides that it shipped.
pub(super) fn policy_only_findings(
    approved: &BTreeMap<String, Record>,
    consumers: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<String> {
    let mut findings = Vec::new();
    for (name, users) in consumers {
        let marked = approved
            .get(name)
            .and_then(|record| record.reason.as_deref())
            .is_some_and(|reason| reason.starts_with(POLICY_ONLY));
        match (users.iter().next(), marked) {
            (None, false) => findings.push(format!(
                "contract/APPROVED.toml `{name}` — POLICY-ONLY: no workspace code consumes it, so \
                 its reason must open with `{POLICY_ONLY}` (#418)"
            )),
            (Some(first), true) => findings.push(format!(
                "contract/APPROVED.toml `{name}` — POLICY-ONLY: marked `{POLICY_ONLY}` but consumed \
                 by {first}; drop the marker"
            )),
            (None, true) | (Some(_), false) => {}
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::{
        POLICY_ONLY, policy_only_findings, std_first_hits, storefront_consumers, storefront_only,
    };
    use crate::files::Scratch;
    use crate::manifest::Record;
    use std::collections::{BTreeMap, BTreeSet};
    use std::error::Error;
    use std::fs;
    use std::path::PathBuf;

    type TestResult = Result<(), Box<dyn Error>>;

    /// A register record with the given owner, consumers and reason.
    fn record(owner: &str, consumers: &[&str], reason: &str) -> Record {
        Record {
            owner: Some(owner.to_owned()),
            capability: None,
            tier: None,
            reason: Some(reason.to_owned()),
            consumers: consumers.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    /// The consumers of `owned` across `sources` written into a scratch repository.
    fn consumers_of(
        sources: &[(&str, &str)],
        owned: &[&str],
    ) -> Result<BTreeMap<String, BTreeSet<String>>, Box<dyn Error>> {
        let scratch = Scratch::new()?;
        let mut files: Vec<PathBuf> = Vec::new();
        for &(relative, text) in sources {
            let path = scratch.root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, text)?;
            files.push(path);
        }
        let owned: BTreeSet<String> = owned.iter().map(|name| (*name).to_owned()).collect();
        Ok(storefront_consumers(&scratch.root, &files, &owned)?)
    }

    #[test]
    fn a_facade_path_in_another_crate_consumes_and_a_reexport_does_not() -> TestResult {
        let found = consumers_of(
            &[
                (
                    "crates/lgwks-deps/src/lib.rs",
                    "pub use gpui::{self, App as GpuiApp};\npub use bevy_ecs;\n",
                ),
                (
                    "crates/lgwks-deps/tests/fixtures/app/src/main.rs",
                    "use lgwks_deps::gpui::App;\n",
                ),
                (
                    "crates/lgwks-bot/src/ecs.rs",
                    "use lgwks_deps::{bevy_ecs::{self, prelude::World}};\n",
                ),
                (
                    "crates/lgwks-bot/src/note.rs",
                    "// lgwks_deps::gpui\nlet s = \"lgwks_deps::gpui\";\n",
                ),
            ],
            &["gpui", "bevy_ecs"],
        )?;
        assert_eq!(
            found.get("gpui").map(BTreeSet::len),
            Some(0),
            "nothing consumes gpui"
        );
        assert_eq!(
            found
                .get("bevy_ecs")
                .map(|users| users.iter().cloned().collect::<Vec<_>>()),
            Some(vec!["crates/lgwks-bot/src/ecs.rs".to_owned()]),
            "the bot consumes bevy_ecs through the facade"
        );
        Ok(())
    }

    #[test]
    fn the_storefronts_own_source_past_lib_rs_consumes() -> TestResult {
        let found = consumers_of(
            &[
                (
                    "crates/lgwks-deps/src/scan.rs",
                    "let f = syn::parse_file(t);\n",
                ),
                (
                    "crates/lgwks-deps/src/report.rs",
                    "let g = not_gpui::App::new();\n",
                ),
            ],
            &["syn", "gpui"],
        )?;
        assert_eq!(
            found
                .get("syn")
                .map(|users| users.iter().cloned().collect::<Vec<_>>()),
            Some(vec!["crates/lgwks-deps/src/scan.rs".to_owned()]),
            "scan.rs uses syn"
        );
        assert_eq!(
            found.get("gpui").map(BTreeSet::len),
            Some(0),
            "a longer name is not gpui"
        );
        Ok(())
    }

    #[test]
    fn the_marker_must_match_consumption_in_both_directions() {
        let approved: BTreeMap<String, Record> = [
            ("gpui", record("lgwks_deps", &["lgwks_deps"], "A GPU UI.")),
            (
                "candle_core",
                record(
                    "lgwks_deps",
                    &["lgwks_deps"],
                    &format!("{POLICY_ONLY} tensors."),
                ),
            ),
            (
                "tokio",
                record(
                    "lgwks_deps",
                    &["lgwks_deps"],
                    &format!("{POLICY_ONLY} runtime."),
                ),
            ),
            ("syn", record("lgwks_deps", &["lgwks_deps"], "Parsing.")),
        ]
        .into_iter()
        .map(|(name, found)| (name.to_owned(), found))
        .collect();
        let consumers: BTreeMap<String, BTreeSet<String>> = [
            ("gpui", vec![]),
            ("candle_core", vec![]),
            ("tokio", vec!["crates/lgwks-bot/src/lib.rs"]),
            ("syn", vec!["x.rs"]),
        ]
        .into_iter()
        .map(|(name, users)| {
            (
                name.to_owned(),
                users.into_iter().map(str::to_owned).collect(),
            )
        })
        .collect();
        let found = policy_only_findings(&approved, &consumers);
        assert_eq!(
            found.len(),
            2,
            "one unmarked-unused, one marked-used: {found:?}"
        );
        assert!(
            found
                .iter()
                .any(|line| line.contains("`gpui` — POLICY-ONLY: no workspace code consumes")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|line| line.contains("`tokio` — POLICY-ONLY: marked")
                    && line.contains("crates/lgwks-bot/src/lib.rs")),
            "{found:?}"
        );
    }

    #[test]
    fn only_edges_admitting_the_storefront_alone_are_judged() {
        let approved: BTreeMap<String, Record> = [
            ("gpui", record("lgwks_deps", &["lgwks_deps"], "")),
            (
                "proptest",
                record("lgwks_deps", &["lgwks_std", "lgwks_deps"], ""),
            ),
            ("regex", record("lgwks_std", &["lgwks_std"], "")),
        ]
        .into_iter()
        .map(|(name, found)| (name.to_owned(), found))
        .collect();
        assert_eq!(
            storefront_only(&approved).into_iter().collect::<Vec<_>>(),
            vec!["gpui"],
            "only the storefront-alone edge"
        );
    }

    #[test]
    fn each_std_first_pattern_names_its_replacement() {
        // Each pattern is split across a `concat!` so this test file does not
        // itself contain what the audit looks for.
        let text = concat!(
            "format!(\"{:0",
            "2x}\", b); ",
            "ext",
            "ern \"C\"fn; ",
            "std::proc",
            "ess::id(); ",
            "std::thr",
            "ead::spawn(f); ",
            "ra",
            "nd::random(); ",
            "let s = std::thr",
            "ead::spawnx();"
        );
        let hits = std_first_hits(text);
        let reasons: Vec<&str> = hits.iter().map(|&(_, why)| why).collect();
        assert!(
            reasons.iter().any(|why| why.contains("hand-formatted hex")),
            "{reasons:?}"
        );
        assert!(
            reasons.iter().any(|why| why.contains("raw FFI")),
            "{reasons:?}"
        );
        assert!(
            reasons
                .iter()
                .any(|why| why.contains("used as an identity")),
            "{reasons:?}"
        );
        assert_eq!(
            reasons
                .iter()
                .filter(|why| why.contains("untracked thread"))
                .count(),
            1,
            "spawn is found and spawnx is not"
        );
        assert!(
            reasons.iter().any(|why| why.contains("entropy")),
            "{reasons:?}"
        );
    }
}
