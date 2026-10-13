//! The lock: what each cited line said when a person last confirmed it.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use super::{CiteError, Key, fail, read};

/// The lock's header comment, written verbatim ahead of its rows.
const LOCK_HEADER: &str = "\
# The line each documentation citation points at, as it read when a person last
# checked that it supports the sentence citing it.
#
# One entry per cited line: `<source path>:<line>` TAB `<the line, stripped>`.
# Generated and checked by the `check-doc-citations` binary of `lgwks_deps`; regenerate with
# `--update`, and read the diff. A line whose text changes here is a line under
# a citation that moved, and re-anchoring is confirming each one still supports
# its claim rather than rewriting the entry to match.
";

/// How the entries differ from the lock.
pub(super) struct Drift {
    /// Cited lines whose text differs from the lock.
    pub(super) moved: Vec<Key>,
    /// Cited lines the lock lacks.
    pub(super) unpinned: Vec<Key>,
    /// Lock rows nothing cites.
    pub(super) stale: Vec<Key>,
}

/// Reads the lock as `key -> cited line text`; absent is an empty lock.
pub(super) fn read_lock(path: &Path) -> Result<BTreeMap<Key, String>, CiteError> {
    let mut entries = BTreeMap::new();
    if !path.exists() {
        return Ok(entries);
    }
    for line in read(path)?.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, text)) = line.split_once('\t') else {
            continue;
        };
        let key = key.trim();
        let Some((file, number)) = key.rsplit_once(':') else {
            return fail(
                CiteError::BadLockKey {
                    key: key.to_owned(),
                },
                "read_lock",
            );
        };
        let number = match number.parse::<u64>() {
            Ok(number) => number,
            Err(_) => {
                return fail(
                    CiteError::BadLockKey {
                        key: key.to_owned(),
                    },
                    "read_lock",
                );
            }
        };
        entries.insert((file.to_owned(), number), text.to_owned());
    }
    Ok(entries)
}

/// Writes the lock from the current entries.
pub(super) fn write_lock(path: &Path, entries: &BTreeMap<Key, String>) -> Result<(), CiteError> {
    let mut rows = vec![LOCK_HEADER.to_owned()];
    for (key, text) in entries {
        rows.push(format!("{}:{}\t{text}\n", key.0, key.1));
    }
    match fs::write(path, rows.concat()) {
        Ok(()) => Ok(()),
        Err(cause) => fail(
            CiteError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "write_lock",
        ),
    }
}

/// A key spelled `<path>:<line>`.
pub(super) fn spell(key: &Key) -> String {
    format!("{}:{}", key.0, key.1)
}

/// The first `width` characters of `text`.
pub(super) fn clip(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
}

/// The text recorded for `key` in `entries`, or empty when absent.
pub(super) fn text_of(entries: &BTreeMap<Key, String>, key: &Key) -> String {
    match entries.get(key) {
        Some(text) => text.clone(),
        None => String::new(),
    }
}

/// Compares the collected entries with the lock.
pub(super) fn drift(entries: &BTreeMap<Key, String>, locked: &BTreeMap<Key, String>) -> Drift {
    let mut found = Drift {
        moved: Vec::new(),
        unpinned: Vec::new(),
        stale: Vec::new(),
    };
    for (key, text) in entries {
        match locked.get(key) {
            Some(old) if old != text => found.moved.push(key.clone()),
            Some(_) => {}
            None => found.unpinned.push(key.clone()),
        }
    }
    for key in locked.keys() {
        if !entries.contains_key(key) {
            found.stale.push(key.clone());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{clip, drift, spell, text_of};
    use std::collections::BTreeMap;

    #[test]
    fn drift_separates_moved_unpinned_and_stale_rows() {
        let entries: BTreeMap<(String, u64), String> = [
            (("a.rs".to_owned(), 1), "same".to_owned()),
            (("a.rs".to_owned(), 2), "changed".to_owned()),
            (("a.rs".to_owned(), 3), "new".to_owned()),
        ]
        .into_iter()
        .collect();
        let locked: BTreeMap<(String, u64), String> = [
            (("a.rs".to_owned(), 1), "same".to_owned()),
            (("a.rs".to_owned(), 2), "was".to_owned()),
            (("a.rs".to_owned(), 9), "gone".to_owned()),
        ]
        .into_iter()
        .collect();
        let found = drift(&entries, &locked);
        assert_eq!(found.moved, vec![("a.rs".to_owned(), 2)], "the changed row");
        assert_eq!(found.unpinned, vec![("a.rs".to_owned(), 3)], "the new row");
        assert_eq!(found.stale, vec![("a.rs".to_owned(), 9)], "the uncited row");
    }

    #[test]
    fn keys_and_text_are_spelled_and_clipped() {
        let key = ("docs/x.md".to_owned(), 12);
        assert_eq!(spell(&key), "docs/x.md:12", "a key reads path:line");
        assert_eq!(clip("abcdef", 3), "abc", "the first characters");
        assert_eq!(
            text_of(&BTreeMap::new(), &key),
            "",
            "an absent key reads empty"
        );
    }
}
