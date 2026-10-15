//! Dependency overrides the register cannot see (#266, from #419).
//!
//! `[patch]` and `[replace]` swap the bytes behind an approved edge for other
//! bytes, and a `[source.*] replace-with` reroutes every registry fetch. Each
//! one leaves `cargo metadata` naming the approved package while the build
//! compiles something else, so the edge audit would pass a tree it never saw.
//! No override is admitted: #266 removed the vendored tree (option B), so the
//! one replacement this reader once allowed, crates.io read from the checked-in
//! `vendored-sources` directory, names a directory that no longer exists.
//!
//! Cargo honours `[patch]` and `[replace]` only in the workspace root manifest
//! (a member's are ignored with a warning) and in `.cargo/config.toml`, and
//! source replacement only in the config, so those are the files read. A table
//! header inside a multi-line string is read as a header: the scan refuses
//! rather than parse TOML strings, which fails closed.

use std::path::Path;

use crate::{GateError, Refusal};

/// Every override in the repository rooted at `root`.
///
/// # Errors
///
/// [`GateError::Unreadable`] when the root manifest, or a cargo config that
/// exists, cannot be read. An absent config is no override.
pub(crate) fn refusals(root: &Path) -> Result<Vec<Refusal>, GateError> {
    let mut refusals = manifest_refusals("Cargo.toml", &crate::read(&root.join("Cargo.toml"))?);
    for name in [".cargo/config.toml", ".cargo/config"] {
        let path = root.join(name);
        if path.is_file() {
            refusals.extend(config_refusals(name, &crate::read(&path)?));
        }
    }
    Ok(refusals)
}

/// The overrides a root manifest declares: every `[patch…]` and `[replace…]`.
pub(crate) fn manifest_refusals(file: &str, text: &str) -> Vec<Refusal> {
    keys(text)
        .into_iter()
        .filter(|key| is_patch_or_replace(&key.path))
        .map(|key| refusal(file, &key.render()))
        .collect()
}

/// The overrides a cargo config declares: every `[patch…]` and every
/// `source.*.replace-with`, whatever it names.
pub(crate) fn config_refusals(file: &str, text: &str) -> Vec<Refusal> {
    keys(text)
        .into_iter()
        .filter(|key| is_patch_or_replace(&key.path) || is_source_replacement(&key.path))
        .map(|key| refusal(file, &key.render()))
        .collect()
}

/// The refusal naming one override.
fn refusal(file: &str, table: &str) -> Refusal {
    Refusal::DependencyOverride {
        file: file.to_owned(),
        table: table.to_owned(),
    }
}

/// Whether a key path sits under `patch` or `replace`.
fn is_patch_or_replace(path: &[String]) -> bool {
    matches!(path.first().map(String::as_str), Some("patch" | "replace"))
}

/// Whether a key path is `source.<name>.replace-with`, the key that reroutes
/// every fetch of `<name>`.
fn is_source_replacement(path: &[String]) -> bool {
    matches!(path, [first, _, last] if first == "source" && last == "replace-with")
}

/// One table header or key, as a dotted path with its value when it has one.
#[derive(Debug, PartialEq, Eq)]
struct Key {
    /// Normalised path segments: whitespace and quotes removed.
    path: Vec<String>,
    /// Whether this is a `[table]` header rather than a key line.
    header: bool,
    /// The string value of a `key = "value"` line; `None` for a header or a
    /// non-string value such as an inline table.
    value: Option<String>,
}

impl Key {
    /// How the key is named in a refusal.
    fn render(&self) -> String {
        let path = self.path.join(".");
        match (self.header, self.value.as_deref()) {
            (true, _) => format!("[{path}]"),
            (false, Some(value)) => format!("{path} = \"{value}\""),
            (false, None) => path,
        }
    }
}

/// Every table header and every key line under its table, in order.
///
/// A header yields its own path; a key yields the enclosing table's path with
/// the key's segments appended, so `[source.crates-io]` then
/// `replace-with = "x"` and a top-level `source.crates-io.replace-with = "x"`
/// read the same.
fn keys(text: &str) -> Vec<Key> {
    let mut out = Vec::new();
    let mut table: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(inner) = header_inner(line) {
            table = segments(inner);
            out.push(Key {
                path: table.clone(),
                header: true,
                value: None,
            });
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let mut path = table.clone();
        path.extend(segments(name));
        out.push(Key {
            path,
            header: false,
            value: string_value(value),
        });
    }
    out
}

/// The text between a header's brackets, for `[a.b]` and `[[a.b]]`.
fn header_inner(line: &str) -> Option<&str> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    match inner.strip_prefix('[') {
        Some(array) => array.strip_suffix(']'),
        None => Some(inner),
    }
}

/// A dotted key split into segments, each trimmed and unquoted.
fn segments(dotted: &str) -> Vec<String> {
    dotted
        .split('.')
        .map(|segment| segment.trim().trim_matches(['"', '\'']).to_owned())
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// The contents of a quoted string value, or `None` for anything else.
fn string_value(value: &str) -> Option<String> {
    let value = value.trim();
    let quote = value
        .chars()
        .next()
        .filter(|first| matches!(first, '"' | '\''))?;
    let inner = value.strip_prefix(quote)?;
    let end = inner.find(quote)?;
    inner.get(..end).map(str::to_owned)
}

/// The line up to a `#` that is outside a quoted string.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    for (index, character) in line.char_indices() {
        match (quote, character) {
            // `index` comes from `char_indices`, so it is a boundary and the
            // slice exists; the `None` arm keeps the whole line rather than
            // inventing a shorter one.
            (None, '#') => {
                return match line.get(..index) {
                    Some(head) => head,
                    None => line,
                };
            }
            (None, '"' | '\'') => quote = Some(character),
            (Some(open), close) if open == close => quote = None,
            _ => {}
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tables each refusal names, for compact assertions.
    fn tables(refusals: &[Refusal]) -> Vec<String> {
        refusals
            .iter()
            .filter_map(|refusal| match *refusal {
                Refusal::DependencyOverride { ref table, .. } => Some(table.clone()),
                _ => None,
            })
            .collect()
    }

    /// This repository's own manifest and config shapes: no override.
    #[test]
    fn a_clean_manifest_and_config_pass() {
        let manifest = "[workspace]\nmembers = [\"crates/*\"]\n\n[profile.release]\nlto = true\n";
        let config = "[build]\nrustflags = [\"-C\", \"target-cpu=native\"]\n";
        assert!(manifest_refusals("Cargo.toml", manifest).is_empty());
        assert!(config_refusals(".cargo/config.toml", config).is_empty());
    }

    /// Cargo reference, "Overriding dependencies": `[patch.crates-io]` in the
    /// root manifest swaps a registry package's source.
    #[test]
    fn a_patch_table_in_the_manifest_is_refused() {
        let manifest = "[workspace]\n\n[patch.crates-io]\nserde = { path = \"../serde\" }\n";
        assert_eq!(
            tables(&manifest_refusals("Cargo.toml", manifest)),
            vec!["[patch.crates-io]", "patch.crates-io.serde"]
        );
    }

    /// The deprecated `[replace]` table is an override too.
    #[test]
    fn a_replace_table_in_the_manifest_is_refused() {
        let manifest = "[replace]\n\"serde:1.0.0\" = { git = \"https://example.invalid/serde\" }\n";
        let refused = manifest_refusals("Cargo.toml", manifest);
        assert_eq!(refused.len(), 2);
        assert!(refused[0].to_string().contains("[replace]"));
    }

    /// A dotted top-level key reaches the same table as a header does.
    #[test]
    fn a_dotted_patch_key_is_refused_like_a_header() {
        let manifest = "patch.crates-io.serde = { path = \"../serde\" }\n";
        assert_eq!(
            tables(&manifest_refusals("Cargo.toml", manifest)),
            vec!["patch.crates-io.serde"]
        );
    }

    /// Cargo reference, "Source Replacement": the replacement #266 once admitted,
    /// crates.io read from `vendored-sources`, is refused now that the vendored
    /// tree is gone (#519); the refusal names the key that reroutes the fetch.
    #[test]
    fn the_vendored_source_replacement_is_refused_now_the_tree_is_gone() {
        let config = "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n\
                      [source.vendored-sources]\ndirectory = \"vendor\"\n";
        assert_eq!(
            tables(&config_refusals(".cargo/config.toml", config)),
            vec!["source.crates-io.replace-with = \"vendored-sources\""]
        );
    }

    /// Any replacement target, and any source replaced, is refused.
    #[test]
    fn a_foreign_source_replacement_is_refused() {
        let mirror = "[source.crates-io]\nreplace-with = \"my-mirror\"\n";
        assert_eq!(
            tables(&config_refusals(".cargo/config.toml", mirror)),
            vec!["source.crates-io.replace-with = \"my-mirror\""]
        );
        let other = "[source.internal]\nreplace-with = 'vendored-sources'\n";
        assert_eq!(config_refusals(".cargo/config.toml", other).len(), 1);
        let dotted = "source.crates-io.replace-with = \"elsewhere\" # comment\n";
        assert_eq!(config_refusals(".cargo/config.toml", dotted).len(), 1);
    }

    /// Cargo 1.56 reads `[patch]` from the config as well as the manifest.
    #[test]
    fn a_patch_table_in_the_config_is_refused() {
        let config = "[patch.crates-io]\nserde = { path = \"../serde\" }\n";
        assert_eq!(config_refusals(".cargo/config.toml", config).len(), 2);
    }

    /// A `#` inside a string is not a comment, and a commented-out override is
    /// not an override.
    #[test]
    fn comments_are_read_outside_strings_only() {
        let manifest = "# [patch.crates-io]\nname = \"a#b\"\n";
        assert!(manifest_refusals("Cargo.toml", manifest).is_empty());
    }

    /// One seeded draw of a manifest: a clean prefix, then an override written
    /// in one of the spellings Cargo accepts, or none.
    fn drawn(seed: u64) -> Result<(String, usize), lgwks_std::seeded::DrawError> {
        const SPELLINGS: [&str; 6] = [
            "[patch.crates-io]\nserde = { path = \"x\" }\n",
            "[ patch . \"crates-io\" ]\nserde = { path = \"x\" }\n",
            "[replace]\n\"serde:1.0.0\" = { path = \"x\" }\n",
            "patch.crates-io.serde = { path = \"x\" }\n",
            "[patch.'https://example.invalid/index']\nserde = { path = \"x\" }\n",
            "[[patch.unknown]]\n",
        ];
        const EXPECTED: [usize; 6] = [2, 2, 2, 1, 2, 1];
        // The override goes first: a dotted key is top-level only before the
        // first header (after `[workspace]` it would be `workspace.patch…`,
        // which is not an override), and a header after it resets the table.
        const WORKSPACE: &str = "[workspace]\nresolver = \"3\"\n";
        let mut draw = lgwks_std::seeded::Seeded::from_seed(seed);
        let mut text = String::new();
        let comments = draw.below(4)?;
        for _ in 0..comments {
            text.push_str("# [patch.crates-io] only a comment\n");
        }
        let which = usize::try_from(draw.below(7)?).ok();
        let picked = which.and_then(|index| SPELLINGS.get(index).zip(EXPECTED.get(index)));
        let Some((spelling, &expected)) = picked else {
            text.push_str(WORKSPACE);
            return Ok((text, 0));
        };
        text.push_str(spelling);
        text.push_str(WORKSPACE);
        Ok((text, expected))
    }

    /// Every seeded manifest is refused exactly as many times as the model
    /// says, and the same seeds replay to the same trace.
    #[test]
    fn sim_every_seeded_override_spelling_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let mut trace = Vec::new();
        for seed in 0..512_u64 {
            let (text, expected) = drawn(seed)?;
            let refused = manifest_refusals("Cargo.toml", &text);
            assert_eq!(refused.len(), expected, "seed {seed}:\n{text}");
            trace.push(refused.len());
        }
        let mut again = Vec::new();
        for seed in 0..512_u64 {
            again.push(manifest_refusals("Cargo.toml", &drawn(seed)?.0).len());
        }
        assert_eq!(trace, again);
        assert!(trace.contains(&0) && trace.contains(&1) && trace.contains(&2));
        Ok(())
    }
}
