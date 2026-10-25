//! The sparse registry index, read for one question: which version could this
//! lock move to.
//!
//! The registry publishes one file per crate, one JSON object per line and one
//! line per published version, each with a `yanked` flag (Cargo Book, "Registry
//! Index"). Cargo itself reads these files in parallel when it resolves, and so
//! can this command. Nothing here knows about `Cargo.lock` or about how a file
//! is fetched: a name goes in and a path comes out, a body goes in and entries
//! come out, and entries and a resolved version go in and a verdict comes out.

use std::fmt;

use lgwks_std::json::{self, Value};

use super::version::Version;

/// Most characters crates.io allows in a crate name.
const MAX_NAME: usize = 64;

/// One published version, as one line of an index file records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Entry {
    /// The version as the index spelled it.
    pub(super) text: String,
    /// Its place in the order.
    pub(super) version: Version,
    /// Whether the registry withdrew it. A yanked version is never offered.
    pub(super) yanked: bool,
}

/// What the index says about a resolved version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Newest {
    /// The newest version this lock could move to, as the index spelled it.
    pub(super) text: String,
    /// Whether it is ahead of the resolved version in precedence.
    pub(super) stale: bool,
}

/// Why a name or a file of the index was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum IndexError {
    /// A crate name that cannot be a path in the index.
    Name {
        /// The name as the lock wrote it.
        name: String,
    },
    /// A line that is not a version record.
    Line {
        /// The 1-based line of the file.
        line: usize,
        /// What is wrong with it.
        reason: String,
    },
}

impl fmt::Display for IndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Name { ref name } => {
                write!(formatter, "{name:?} is not a crate name the index can hold")
            }
            Self::Line { line, ref reason } => {
                write!(formatter, "index line {line}: {reason}")
            }
        }
    }
}

impl std::error::Error for IndexError {}

/// Where `name`'s file lives, relative to the index root.
///
/// The layout is the registry's: one- and two-letter names under `1/` and `2/`,
/// three-letter names under `3/<first letter>/`, and every longer name under
/// `<first two>/<next two>/`, all in lower case. A name outside
/// `[A-Za-z0-9_-]` is refused, because it is about to become part of a URL.
pub(super) fn path(name: &str) -> Result<String, IndexError> {
    let lower = name.to_ascii_lowercase();
    let admissible = !lower.is_empty()
        && lower.len() <= MAX_NAME
        && lower
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    let folder = if admissible { directory(&lower) } else { None };
    folder
        .map(|folder| format!("{folder}/{lower}"))
        .ok_or_else(|| IndexError::Name {
            name: name.to_owned(),
        })
}

/// The directory a lower-case name's file sits in.
fn directory(lower: &str) -> Option<String> {
    match lower.len() {
        1 => Some("1".to_owned()),
        2 => Some("2".to_owned()),
        3 => lower.get(..1).map(|first| format!("3/{first}")),
        _ => lower
            .get(..2)
            .zip(lower.get(2..4))
            .map(|(head, next)| format!("{head}/{next}")),
    }
}

/// Reads an index file: one entry per non-blank line.
///
/// One unreadable line refuses the whole file. The newest version might be on
/// that line, and a verdict computed without it would be a guess presented as
/// an answer.
pub(super) fn parse(body: &str) -> Result<Vec<Entry>, IndexError> {
    let mut entries = Vec::new();
    for (text, line) in body.lines().zip(1_usize..) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let record = entry(trimmed).map_err(|fault| IndexError::Line {
            line,
            reason: fault.to_string(),
        })?;
        entries.push(record);
    }
    Ok(entries)
}

/// Why one line is not a version record.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LineFault {
    /// The line is not JSON.
    Json(String),
    /// A required key is absent or has the wrong type.
    Missing(&'static str),
    /// `vers` is not a version.
    Version(String),
}

impl fmt::Display for LineFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Json(ref reason) => write!(formatter, "not JSON: {reason}"),
            Self::Missing(key) => write!(formatter, "no usable {key:?} key"),
            Self::Version(ref reason) => write!(formatter, "vers: {reason}"),
        }
    }
}

/// One line as an entry.
fn entry(text: &str) -> Result<Entry, LineFault> {
    let record: Value = json::from_str(text).map_err(|error| LineFault::Json(error.to_string()))?;
    let spelled = record
        .get("vers")
        .and_then(Value::as_str)
        .ok_or(LineFault::Missing("vers"))?;
    let yanked = record
        .get("yanked")
        .and_then(Value::as_bool)
        .ok_or(LineFault::Missing("yanked"))?;
    let version = Version::parse(spelled).map_err(|error| LineFault::Version(error.to_string()))?;
    Ok(Entry {
        text: spelled.to_owned(),
        version,
        yanked,
    })
}

/// The newest version `resolved` could move to, or `None` when there is none.
///
/// A yanked version is never a candidate. A pre-release is a candidate only when
/// `resolved` is itself a pre-release: Cargo never selects a pre-release for a
/// requirement that did not ask for one, so reporting one as the newest would
/// name a version `cargo update` cannot reach. A lock already on a pre-release
/// has asked, and is compared against the pre-releases too.
pub(super) fn newest(entries: &[Entry], resolved: &Version) -> Option<Newest> {
    let admit_prerelease = resolved.is_prerelease();
    let best = entries
        .iter()
        .filter(|entry| !entry.yanked && (admit_prerelease || !entry.version.is_prerelease()))
        .max_by(|left, right| left.version.cmp(&right.version))?;
    Some(Newest {
        text: best.text.clone(),
        stale: best.version > *resolved,
    })
}

#[cfg(test)]
mod sim_index;
