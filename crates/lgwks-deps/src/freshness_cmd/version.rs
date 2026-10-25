//! Version precedence as SemVer 2.0.0 section 11 defines it, which is the order
//! Cargo resolves by and the only order in which "newest" means anything.
//!
//! The command used to call a package stale when the registry's newest version
//! differed from the locked one. Different is not behind: a lock that is ahead of
//! the newest release still on offer differs and is not stale. This module
//! answers the ordering question for any two version strings and knows nothing
//! of the registry or of the lock.

use std::cmp::Ordering;
use std::fmt;

/// One version, parsed for comparison.
///
/// Build metadata (`+...`) is checked and then dropped: section 10 makes it
/// irrelevant to precedence, so two versions that differ only in it are equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Version {
    /// MAJOR, MINOR and PATCH, most significant first.
    core: [u64; 3],
    /// The dot-separated pre-release identifiers; empty for a release.
    pre: Vec<Identifier>,
}

/// One pre-release identifier.
///
/// The variant order is the rule. Derived ordering puts every numeric identifier
/// below every alphanumeric one (11.4.3), compares numerics as numbers (11.4.1)
/// and alphanumerics by ASCII byte order (11.4.2).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Identifier {
    /// Digits only, without a leading zero.
    Numeric(u64),
    /// Letters, digits and hyphens, with at least one character that is not a
    /// digit.
    Alphanumeric(String),
}

/// Why a version string was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum VersionError {
    /// The part before any `-` or `+` is not `MAJOR.MINOR.PATCH`.
    Core {
        /// The text that was read as the core.
        text: String,
    },
    /// A number is empty, has a sign or a leading zero, holds a non-digit, or
    /// does not fit in 64 bits.
    Number {
        /// The part that was read as a number.
        part: String,
    },
    /// A pre-release or build identifier is empty or holds a character outside
    /// `[0-9A-Za-z-]`.
    Identifier {
        /// The identifier as written.
        part: String,
    },
}

impl fmt::Display for VersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Core { ref text } => {
                write!(formatter, "{text:?} is not MAJOR.MINOR.PATCH")
            }
            Self::Number { ref part } => write!(
                formatter,
                "{part:?} is not a whole number without a leading zero that fits in 64 bits"
            ),
            Self::Identifier { ref part } => write!(
                formatter,
                "{part:?} is not a non-empty identifier of letters, digits and hyphens"
            ),
        }
    }
}

impl std::error::Error for VersionError {}

impl Version {
    /// Reads a version in the grammar of semver.org section 2.
    ///
    /// Anything the grammar does not admit is refused rather than read as the
    /// nearest version it resembles: a version that cannot be ordered must not
    /// be ordered.
    pub(super) fn parse(text: &str) -> Result<Self, VersionError> {
        let (head, build) = split_off(text, '+');
        if let Some(build) = build {
            check_identifiers(build)?;
        }
        let (core, pre) = split_off(head, '-');
        let core = parse_core(core)?;
        let pre = match pre {
            Some(pre) => parse_pre(pre)?,
            None => Vec::new(),
        };
        Ok(Self { core, pre })
    }

    /// Whether this is a pre-release, which sorts below its own release.
    pub(super) fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core
            .cmp(&other.core)
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                // 11.3: a pre-release is lower than the release it precedes.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                // 11.4.4: with equal leading identifiers, the longer set is higher,
                // which is how a vector compares.
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// `text` cut at the first `separator`: what comes before, and what follows.
fn split_off(text: &str, separator: char) -> (&str, Option<&str>) {
    match text.split_once(separator) {
        Some((head, tail)) => (head, Some(tail)),
        None => (text, None),
    }
}

/// MAJOR, MINOR and PATCH: exactly three numbers.
fn parse_core(core: &str) -> Result<[u64; 3], VersionError> {
    let mut parts = core.split('.');
    let found = (parts.next(), parts.next(), parts.next(), parts.next());
    match found {
        (Some(major), Some(minor), Some(patch), None) => {
            Ok([number(major)?, number(minor)?, number(patch)?])
        }
        _ => Err(VersionError::Core {
            text: core.to_owned(),
        }),
    }
}

/// A numeric part: digits only, no leading zero unless it is `0`, and it fits.
fn number(part: &str) -> Result<u64, VersionError> {
    let canonical = all_digits(part) && (part == "0" || !part.starts_with('0'));
    match part.parse::<u64>() {
        Ok(value) if canonical => Ok(value),
        _ => Err(VersionError::Number {
            part: part.to_owned(),
        }),
    }
}

/// The pre-release identifiers after the first `-`.
fn parse_pre(text: &str) -> Result<Vec<Identifier>, VersionError> {
    text.split('.').map(identifier).collect()
}

/// One pre-release identifier, numeric when it is all digits.
fn identifier(part: &str) -> Result<Identifier, VersionError> {
    match (is_identifier(part), all_digits(part)) {
        (false, _) => Err(VersionError::Identifier {
            part: part.to_owned(),
        }),
        (true, true) => number(part).map(Identifier::Numeric),
        (true, false) => Ok(Identifier::Alphanumeric(part.to_owned())),
    }
}

/// Build identifiers: the same alphabet, and leading zeros are allowed.
fn check_identifiers(text: &str) -> Result<(), VersionError> {
    match text.split('.').find(|part| !is_identifier(part)) {
        Some(part) => Err(VersionError::Identifier {
            part: part.to_owned(),
        }),
        None => Ok(()),
    }
}

/// A non-empty run of `[0-9A-Za-z-]`.
fn is_identifier(part: &str) -> bool {
    !part.is_empty()
        && part
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// A non-empty run of ASCII digits.
fn all_digits(part: &str) -> bool {
    !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod sim_version;
