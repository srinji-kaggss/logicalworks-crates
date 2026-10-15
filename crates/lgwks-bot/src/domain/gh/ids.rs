//! GitHub identifiers: the repository and the commit a review is about.

use super::GhError;

// ── GitHub identifiers ──────────────────────────────────────────────────────

/// The owner half of an `owner/repo` repository reference.
///
/// A newtype rather than a `String`, because a review's subject is three
/// things at once — repository, number, and commit — and the only way a
/// publication can be wrong is by being about the wrong one of them. Typing the
/// repository keeps a bare string from being passed where a commit is meant.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Repository {
    /// `owner/repo`, validated below.
    spec: String,
}

impl Repository {
    /// Validate and hold an `owner/repo` reference.
    ///
    /// # Errors
    ///
    /// [`GhError::Repository`] naming what is wrong with `spec`.
    pub fn new(spec: impl Into<String>) -> Result<Self, GhError> {
        let spec = spec.into();
        let invalid = |reason: &'static str| GhError::Repository {
            spec: spec.clone(),
            reason,
        };
        if spec.is_empty() {
            let refusal = Err(invalid("the repository reference is empty"));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        if spec.len() > MAX_REPOSITORY_BYTES {
            let refusal = Err(invalid("the repository reference is too long"));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        // `split_once` rather than a walk with a substituted empty string for
        // an absent segment: the owner half is present whenever a `/` is, so a
        // reference with no `/` at all is the one case where a half is
        // missing, and it is refused by the check below for the same reason an
        // empty half is — `owner` alone is not `owner/repo`.
        let Some((owner, rest)) = spec.split_once('/') else {
            let refusal = Err(invalid("both `owner` and `repo` must be non-empty"));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        };
        if rest.contains('/') {
            let refusal = Err(invalid(
                "a repository reference is `owner/repo`, with exactly one `/`",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        let name = rest;
        if owner.is_empty() || name.is_empty() {
            let refusal = Err(invalid("both `owner` and `repo` must be non-empty"));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        // `.` and `..` are path segments, not names: `../..` would turn
        // `repos/{owner}/{repo}/pulls` into a request for another endpoint.
        if [owner, name]
            .iter()
            .any(|segment| matches!(*segment, "." | ".."))
        {
            let refusal = Err(invalid(
                "`.` and `..` are path segments, not repository names",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        let allowed = |segment: &str| {
            segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        };
        if !allowed(owner) || !allowed(name) {
            let refusal = Err(invalid(
                "`owner` and `repo` hold ASCII letters, digits, '-', '_' and '.' only",
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        Ok(Self { spec })
    }

    /// The validated `owner/repo` reference.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.spec
    }
}

impl std::fmt::Display for Repository {
    /// The validated `owner/repo` reference.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.spec)
    }
}

/// A 40-character lowercase hexadecimal commit id.
///
/// Length and alphabet are checked, so a truncated or mistyped sha is refused
/// before it is sent as a subject rather than becoming a server-side 404 that
/// looks like a missing review.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommitId(String);

impl CommitId {
    /// Validate and hold a commit id.
    ///
    /// # Errors
    ///
    /// [`GhError::CommitId`] naming what is wrong with `hex`.
    pub fn new(hex: impl Into<String>) -> Result<Self, GhError> {
        let hex = hex.into();
        if hex.len() != SHA_HEX_LEN {
            let refusal = Err(GhError::CommitId {
                hex,
                reason: "a commit id is 40 hexadecimal characters",
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        if !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            let refusal = Err(GhError::CommitId {
                hex,
                reason: "a commit id is lowercase hexadecimal",
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "new: returning an error to the caller");
            return refusal;
        }
        Ok(Self(hex))
    }

    /// The validated hexadecimal commit id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CommitId {
    /// The validated hexadecimal commit id.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The length of a full commit id in hexadecimal characters.
pub const SHA_HEX_LEN: usize = 40;

/// The most bytes an `owner/repo` reference may hold.
pub const MAX_REPOSITORY_BYTES: usize = 256;
