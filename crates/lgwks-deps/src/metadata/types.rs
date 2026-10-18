//! The dependency vocabulary Cargo's metadata is read into, and the refusals that reading can end in.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use super::capture::CleanupObligation;

/// One dependency kind Cargo exposes in package metadata.
///
/// Non-exhaustive so a Cargo version that adds a fourth kind extends this type
/// without breaking a consumer: `from_cargo` refuses an unrecognised spelling
/// rather than folding it into `Normal`, which would admit an edge under a kind
/// the register never approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum DependencyKind {
    /// A runtime or library dependency.
    Normal,
    /// A build-script dependency.
    Build,
    /// A test, example, or benchmark dependency.
    Dev,
}

impl DependencyKind {
    /// Maps Cargo's `kind` key onto a gate kind.
    ///
    /// Cargo spells the key `"normal"`, `"build"`, or `"dev"`, and omits it
    /// entirely for a normal dependency, so `None` is read as `Normal`. Any
    /// other value is a schema this gate version does not understand and is
    /// refused rather than defaulted: guessing the kind would admit an edge the
    /// register approved for a different one.
    pub(super) fn from_cargo(value: Option<&str>) -> Result<Self, MetadataError> {
        match value {
            None | Some("normal") => Ok(Self::Normal),
            Some("build") => Ok(Self::Build),
            Some("dev") => Ok(Self::Dev),
            Some(other) => Err(MetadataError::Schema(format!(
                "unknown Cargo dependency kind {other:?}"
            ))),
        }
    }

    /// Stable contract spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Build => "build",
            Self::Dev => "dev",
        }
    }
}

impl fmt::Display for DependencyKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Normalized origin of a direct dependency.
///
/// Non-exhaustive: [`class`](Self::class) collapses these to the four policy
/// classes the register speaks, and a new Cargo scheme lands in `Other` first.
/// Consumers must match with a wildcard arm so that addition is not breaking.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DependencySource {
    /// A registry dependency. The value is Cargo's complete registry source.
    Registry(String),
    /// A Git dependency. The value is Cargo's complete Git source.
    Git(String),
    /// A path dependency outside or inside the workspace.
    Path(String),
    /// A source scheme this gate version does not understand.
    Other(String),
}

impl DependencySource {
    /// Stable policy class used by the contract.
    ///
    /// The four classes are the vocabulary `contract/APPROVED.toml` approves,
    /// so this mapping is contractual: an edge whose class drifts from the
    /// approved one is reported as `SourceDrift` by the audit.
    #[must_use]
    pub const fn class(&self) -> &'static str {
        match *self {
            Self::Registry(_) => "registry",
            Self::Git(_) => "git",
            Self::Path(_) => "path",
            Self::Other(_) => "other",
        }
    }

    /// Exact Cargo source or path, retained for diagnostics.
    ///
    /// Byte-for-byte what Cargo reported, including any `git+…#rev` fragment,
    /// so a drift report names the origin the manifest actually declared rather
    /// than a normalized approximation of it.
    #[must_use]
    pub fn detail(&self) -> &str {
        match *self {
            Self::Registry(ref value)
            | Self::Git(ref value)
            | Self::Path(ref value)
            | Self::Other(ref value) => value,
        }
    }
}

/// One dependency declaration authored by a workspace package.
///
/// Non-exhaustive: an edge carries only what the register compares on, so a
/// future field is an additive change rather than a breaking one for any
/// consumer constructing these.
///
/// `workspace` and `source` are derived, not independent: `workspace` is true
/// only for an edge with no `source` key whose manifest-relative `path`
/// resolves into a workspace member's manifest directory, and `source`
/// separates a registry package from a path copy that happens to share its
/// name. A path edge Cargo cannot locate — no `path` key, or an escape past
/// the root — is external, never internal by name. A workspace member with no
/// manifest directory never gets this far: `parse` refuses the whole document
/// as a schema error before any edge is classified.
///
/// Read approval-relevant identity through accessors so callers cannot mutate
/// the graph after Cargo's metadata has been parsed: `consumer()`, `package()` and
/// `requirement()` are what the integration test
/// `downstream_can_read_metadata_edges_and_workspace_members` reads from a downstream crate.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DirectEdge {
    /// Workspace package declaring the dependency.
    pub(crate) consumer: String,
    /// Upstream package name, independent of a local rename.
    pub(crate) package: String,
    /// Manifest semver requirement exactly as Cargo reports it.
    pub(crate) requirement: String,
    /// Normal, build, or development edge.
    pub kind: DependencyKind,
    /// Registry, Git, or path origin.
    pub source: DependencySource,
    /// Whether the manifest marks the edge optional.
    pub optional: bool,
    /// True when the target package is another member of this workspace.
    pub workspace: bool,
    /// Repository declared by a workspace path target, when present.
    pub target_repository: Option<String>,
    /// Features Cargo reports as enabled on this authored declaration.
    ///
    /// Retained verbatim, including the ordering Cargo emitted it in, so an
    /// admitted-feature policy compares against what the manifest actually
    /// asked for rather than against a re-derived set.
    pub(crate) features: Vec<String>,
    /// Whether the declaration enables the dependency's default features.
    ///
    /// This is the authored `default-features = false` decision, preserved even
    /// though it is not part of the resolved dependency graph: a capability
    /// policy that admits an edge with defaults off and refuses one that turned
    /// them on has to see the authored bit.
    pub(crate) uses_default_features: bool,
    /// The target `cfg(…)` this declaration is scoped to, or `None` when it is
    /// unconditional. Cargo reports target-specific tables here.
    pub(crate) target: Option<String>,
    /// The manifest-local alias when `package =` renamed the upstream crate.
    ///
    /// Distinct from [`package`](Self::package), which is Cargo's package
    /// identity and the name the register approves. A rename is a local
    /// spelling and never a second identity.
    pub(crate) rename: Option<String>,
    /// SPDX expression the target's manifest declares, verbatim.
    ///
    /// `None` when Cargo reports no `license` key. The audit reads this to
    /// refuse an approval whose recorded licence no longer says what the
    /// manifest says (#208); a package that declares none is not read as
    /// permissive, because the value's absence is exactly the fact.
    pub license: Option<String>,
    /// Path the target's manifest names as its licence file, when it declares
    /// one instead of, or as well as, an SPDX expression.
    ///
    /// Retained beside [`Self::license`] so an audit refusal can name the file
    /// whose text governs when the expression alone does not say who the
    /// licensor is.
    pub license_file: Option<String>,
}

impl DirectEdge {
    /// Workspace package that authored this dependency declaration.
    #[must_use]
    pub fn consumer(&self) -> &str {
        &self.consumer
    }

    /// SPDX expression this package's own manifest declares, verbatim.
    ///
    /// `None` when the manifest declares none, which is a fact the licence
    /// audit refuses rather than a licence it resolves.
    #[must_use]
    pub fn license(&self) -> Option<&str> {
        self.license.as_deref()
    }

    /// Licence file this package's own manifest names, when it names one.
    #[must_use]
    pub fn license_file(&self) -> Option<&str> {
        self.license_file.as_deref()
    }

    /// Cargo package name, independent of a local dependency rename.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Manifest semver requirement exactly as Cargo reported it.
    #[must_use]
    pub fn requirement(&self) -> &str {
        &self.requirement
    }

    /// Features Cargo reports enabled on this declaration.
    #[must_use]
    pub fn features(&self) -> &[String] {
        &self.features
    }

    /// Whether the declaration enables the dependency's default features.
    #[must_use]
    pub const fn uses_default_features(&self) -> bool {
        self.uses_default_features
    }

    /// The target `cfg(…)` this declaration is scoped to, when scoped.
    #[must_use]
    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }

    /// The manifest-local alias when the dependency was renamed.
    #[must_use]
    pub fn rename(&self) -> Option<&str> {
        self.rename.as_deref()
    }
}

/// Failure to obtain or decode Cargo's authored dependency graph.
///
/// Every variant is a refusal, not an empty graph: a gate that returned "no
/// edges" when it could not run Cargo would pass the one repository it exists
/// to catch.
#[derive(Debug)]
#[non_exhaustive]
pub enum MetadataError {
    /// Cargo could not be started.
    Spawn(std::io::Error),
    /// The repository root could not be turned into an absolute path.
    ///
    /// Distinct from [`Self::Spawn`]: Cargo was never reached, because the
    /// manifest it would have been pointed at could not be named.
    Root {
        /// The manifest path as it was spelled before absolutization.
        path: PathBuf,
        /// Why the path could not be resolved.
        cause: std::io::Error,
    },
    /// Cargo returned a non-zero status.
    Cargo(String),
    /// Cargo returned JSON outside the supported format-1 subset.
    Json(lgwks_std::json::Error),
    /// Cargo returned an internally inconsistent field.
    Schema(String),
    /// Cargo did not finish before the collection deadline. Termination and
    /// capture cleanup are reported separately by `ProcessCleanup` if either
    /// cannot be confirmed.
    Timeout {
        /// The deadline that fired.
        after: Duration,
    },
    /// One of Cargo's sampled output files exceeded its retained-byte budget.
    /// Termination and capture cleanup are reported separately by
    /// `ProcessCleanup` if either cannot be confirmed.
    OutputTooLarge {
        /// Which stream overflowed: `stdout` or `stderr`.
        stream: &'static str,
        /// The per-stream budget in bytes.
        limit: usize,
    },
    /// A collection failure and its cleanup failures. The primary failure is
    /// retained independently from termination, reaping and capture removal.
    ProcessCleanup {
        /// The failure that caused collection to stop.
        cause: Option<Box<Self>>,
        /// Direct-child termination or reaping could not be confirmed.
        process: Option<std::io::Error>,
        /// Capture paths that could not be reclaimed.
        captures: Vec<std::io::Error>,
        /// A live owner for any unresolved child or capture cleanup.
        ///
        /// Boxed because it owns a `std::process::Child`, which is large on
        /// Windows: carried inline it made every `Result<_, MetadataError>`
        /// exceed clippy's `result_large_err` bound on that target alone.
        obligation: Box<CleanupObligation>,
    },
    /// OS entropy could not be read, so no capture file could be named.
    /// Cargo was never started: without a distinguisher the call refuses
    /// rather than risk a predictable capture path. Host-only: wasm has no
    /// entropy source (`lgwks_std::random` refuses the target), and the
    /// wasm distinguisher below cannot fail this way.
    #[cfg(not(target_family = "wasm"))]
    Entropy(lgwks_std::random::EntropyError),
}

impl fmt::Display for MetadataError {
    /// Names the stage that failed so a reader can tell an environment problem
    /// (`cargo` absent) from a contract problem (a source with the wrong
    /// schema) without re-running the gate.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Spawn(ref error) => write!(f, "cannot run cargo metadata: {error}"),
            Self::Root {
                ref path,
                ref cause,
            } => write!(
                f,
                "cannot resolve the manifest path {}: {cause}",
                path.display()
            ),
            Self::Cargo(ref error) => write!(f, "cargo metadata refused: {error}"),
            Self::Json(ref error) => write!(f, "cargo metadata JSON: {error}"),
            Self::Schema(ref error) => write!(f, "cargo metadata schema: {error}"),
            Self::Timeout { after } => {
                write!(
                    f,
                    "cargo metadata collection deadline expired after {after:?}"
                )
            }
            Self::OutputTooLarge { stream, limit } => {
                write!(f, "cargo metadata {stream} exceeded {limit} retained bytes")
            }
            Self::ProcessCleanup {
                ref cause,
                ref process,
                ref captures,
                ref obligation,
            } => write_cleanup_unconfirmed(
                f,
                cause.as_deref(),
                process.as_ref(),
                captures,
                obligation,
            ),
            #[cfg(not(target_family = "wasm"))]
            Self::Entropy(ref error) => write!(
                f,
                "cargo metadata capture file has no distinguisher: {error}"
            ),
        }
    }
}

/// Renders what could not be cleaned up after a refused capture.
///
/// A helper rather than the arm inline: as one block the arm carried six
/// propagation operators across the cause prefix, the process failure, the pid
/// arms and the capture paths, so an operator reading why cleanup was
/// unconfirmed had to hold six partial sentences at once -- and a writer that
/// failed part-way had already emitted the truncated prefix. Rendering into a
/// buffer and writing once means the sentence is either whole or absent.
fn write_cleanup_unconfirmed(
    formatter: &mut fmt::Formatter<'_>,
    cause: Option<&MetadataError>,
    process: Option<&std::io::Error>,
    captures: &[std::io::Error],
    obligation: &CleanupObligation,
) -> fmt::Result {
    let mut rendered = String::new();
    if let Some(cause) = cause {
        let prefix = format!("{cause}; ");
        rendered.push_str(&prefix);
    }
    rendered.push_str("cleanup unconfirmed");
    let report = &obligation.report;
    if let Some(error) = process {
        let step = match report.step {
            Some(step) => step.verb(),
            None => "stop",
        };
        let sentence = match report.pid {
            Some(pid) => format!("; could not {step} cargo (pid {pid}): {error}"),
            None => format!("; could not {step} cargo: {error}"),
        };
        rendered.push_str(&sentence);
    }
    let mut paths = report.capture_paths.iter();
    for error in captures {
        let sentence = match paths.next() {
            Some(path) => format!("; could not remove {}: {error}", path.display()),
            None => format!("; could not remove a capture file: {error}"),
        };
        rendered.push_str(&sentence);
    }
    formatter.write_str(&rendered)
}

impl std::error::Error for MetadataError {
    /// Chains the variants that wrap a cause; `Cargo` and `Schema` carry
    /// Cargo's own prose as a `String` and have nothing further to chain.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Spawn(ref error) => Some(error),
            Self::Root { ref cause, .. } => Some(cause),
            Self::Json(ref error) => Some(error),
            #[cfg(not(target_family = "wasm"))]
            Self::Entropy(ref error) => Some(error),
            Self::Cargo(_)
            | Self::Schema(_)
            | Self::Timeout { .. }
            | Self::OutputTooLarge { .. } => None,
            // The primary failure first: it is why collection stopped, and the
            // cleanup errors are what it left behind.
            Self::ProcessCleanup {
                cause: Some(ref cause),
                ..
            } => Some(&**cause),
            Self::ProcessCleanup {
                cause: None,
                ref process,
                ref captures,
                ..
            } => match process.as_ref().or_else(|| captures.first()) {
                Some(error) => Some(error),
                None => None,
            },
        }
    }
}
