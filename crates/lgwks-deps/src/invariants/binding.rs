//! Binding a recorded run to the history it is claimed about.
//!
//! A revision that is merely well formed certifies nothing: any hex string of the
//! right length would have turned a `Resolved` invariant into an `Attested` one,
//! at whatever commit happened to be under review. The question evidence has to
//! answer is not "is this the current commit" (a committed register can never
//! name the commit that contains it) but "is what the run observed still what is
//! here". That has three parts, each decided by Git rather than by the shape of a
//! string:
//!
//! 1. the revision names a commit in this repository, and does so unambiguously;
//! 2. that commit is in the history of `HEAD`, so the run was of this line of
//!    work and not of a fabricated or foreign revision;
//! 3. nothing the run depended on has changed since: the enforcer file for a
//!    path, or every manifest and `clippy.toml` for a lint, compared with the
//!    working tree so an uncommitted edit counts.
//!
//! The alternative considered is comparing the recorded revision with `HEAD` for
//! equality, which is how attestations in toto and SLSA bind a claim to one
//! artifact digest. It is the right shape for a build artifact and the wrong one
//! here, because the register is itself a file in the tree and so is always
//! committed after the run it records.

use std::fmt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use super::EnforcedBy;

/// Environment Git reads that would redirect it away from `root`. A hook runs
/// with some of these set, and a gate that answered about the wrong repository
/// would be worse than one that refused.
const REDIRECTING_ENV: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// Pathspecs a lint declaration can live in.
const LINT_DECLARATIONS: [&str; 3] = [
    ":(glob)**/Cargo.toml",
    ":(literal)clippy.toml",
    ":(literal).cargo/config.toml",
];

/// Why recorded evidence could not be bound to the tree being reviewed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EvidenceGap {
    /// The revision names no commit in this repository, or names more than one.
    UnknownRevision,
    /// The revision is a commit here, but not one `HEAD` descends from.
    NotInHistory,
    /// The enforcer changed after the run was observed, so the run says nothing
    /// about it as it stands.
    EnforcerChanged {
        /// The selector the evidence is for.
        selector: String,
    },
    /// Git could not answer, for example because `root` is not a repository.
    /// Unprovable is refused, never assumed bound.
    Unverifiable {
        /// What Git or the operating system said.
        cause: String,
    },
}

impl fmt::Display for EvidenceGap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::UnknownRevision => {
                formatter.write_str("the revision names no single commit in this repository")
            }
            Self::NotInHistory => formatter.write_str("the revision is not in the history of HEAD"),
            Self::EnforcerChanged { ref selector } => {
                write!(formatter, "{selector} changed after the run was observed")
            }
            Self::Unverifiable { ref cause } => {
                write!(formatter, "the revision could not be checked: {cause}")
            }
        }
    }
}

/// Run `git -C root args…` with no stdin and no redirecting environment.
fn git(root: &Path, args: &[&str]) -> Result<Output, EvidenceGap> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .env("GIT_OPTIONAL_LOCKS", "0");
    for name in REDIRECTING_ENV {
        command.env_remove(name);
    }
    command.output().map_err(|error| EvidenceGap::Unverifiable {
        cause: format!("git could not be run: {error}"),
    })
}

/// Refuse with `gap`, leaving a trace of why: a caller that sees only the value
/// cannot tell which of the audit's checks produced it.
fn refuse(gap: EvidenceGap) -> Result<(), EvidenceGap> {
    let refusal = Err(gap);
    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "bind: returning an error to the caller");
    refusal
}

/// A Git exit code that is neither success nor the command's one defined "no".
fn unverifiable(output: &Output) -> EvidenceGap {
    EvidenceGap::Unverifiable {
        cause: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }
}

/// Whether the run recorded at `revision` is still about the tree under `root`.
///
/// `enforcer` is the entry's own resolved reference. Three to four short Git
/// processes per attested entry, and none for an entry with no evidence.
pub(super) fn bind(
    root: &Path,
    revision: &str,
    enforcer: Option<&EnforcedBy>,
) -> Result<(), EvidenceGap> {
    // The revision is validated as hex by the parser, so it cannot be read as an
    // option; the `^{commit}` suffix makes a tag or tree object refuse here.
    let named = format!("{revision}^{{commit}}");
    let resolved = git(root, &["rev-parse", "--verify", "--quiet", &named])?;
    let commit = match resolved.status.code() {
        Some(0) => String::from_utf8_lossy(&resolved.stdout).trim().to_owned(),
        Some(1) => return refuse(EvidenceGap::UnknownRevision),
        _ => return refuse(unverifiable(&resolved)),
    };

    let ancestry = git(root, &["merge-base", "--is-ancestor", &commit, "HEAD"])?;
    match ancestry.status.code() {
        Some(0) => {}
        Some(1) => return refuse(EvidenceGap::NotInHistory),
        _ => return refuse(unverifiable(&ancestry)),
    }

    let Some(enforcer) = enforcer else {
        return Ok(());
    };
    let changed = || EvidenceGap::EnforcerChanged {
        selector: enforcer.selector(),
    };
    let pathspecs: Vec<String> = match *enforcer {
        EnforcedBy::File(ref path) => {
            let spec = format!(":(literal){path}");
            // A file that did not exist when the run was observed has no
            // unchanged state to compare with, and an untracked file is
            // invisible to `git diff`.
            let existed = git(root, &["cat-file", "-e", &format!("{commit}:{path}")])?;
            if !existed.status.success() {
                return refuse(changed());
            }
            vec![spec]
        }
        _ => LINT_DECLARATIONS
            .iter()
            .map(|spec| (*spec).to_owned())
            .collect(),
    };
    let mut arguments = vec!["diff", "--quiet", commit.as_str(), "--"];
    arguments.extend(pathspecs.iter().map(String::as_str));
    let difference = git(root, &arguments)?;
    match difference.status.code() {
        Some(0) => Ok(()),
        Some(1) => Err(changed()),
        _ => Err(unverifiable(&difference)),
    }
}
