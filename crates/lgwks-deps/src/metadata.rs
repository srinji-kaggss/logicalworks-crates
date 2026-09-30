//! Cargo metadata ingestion for direct dependency-edge admission.
//!
//! The lock file answers which bytes happened to resolve. It does not answer
//! which workspace package authored an edge. `cargo metadata --no-deps` does,
//! including inactive optional dependencies, so this module is the source of
//! truth for INV-DEP-EDGE-OWNED.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lgwks_std::json::Deserialize;

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
    fn from_cargo(value: Option<&str>) -> Result<Self, MetadataError> {
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
/// the graph after Cargo's metadata has been parsed:
///
/// ```rust
/// use lgwks_deps::metadata::DirectEdge;
///
/// fn approval_identity(edge: &DirectEdge) -> (&str, &str, &str) {
///     (edge.consumer(), edge.package(), edge.requirement())
/// }
/// ```
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
}

impl DirectEdge {
    /// Workspace package that authored this dependency declaration.
    #[must_use]
    pub fn consumer(&self) -> &str {
        &self.consumer
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
            } => {
                if let Some(cause) = cause.as_deref() {
                    write!(f, "{cause}; ")?;
                }
                write!(f, "cleanup unconfirmed")?;
                let report = &obligation.report;
                if let Some(ref error) = *process {
                    let step = report.step.map_or("stop", CleanupStep::verb);
                    match report.pid {
                        Some(pid) => write!(f, "; could not {step} cargo (pid {pid}): {error}")?,
                        None => write!(f, "; could not {step} cargo: {error}")?,
                    }
                }
                let mut paths = report.capture_paths.iter();
                for error in captures.iter() {
                    match paths.next() {
                        Some(path) => {
                            write!(f, "; could not remove {}: {error}", path.display())?;
                        }
                        None => write!(f, "; could not remove a capture file: {error}")?,
                    }
                }
                Ok(())
            }
            #[cfg(not(target_family = "wasm"))]
            Self::Entropy(ref error) => write!(
                f,
                "cargo metadata capture file has no distinguisher: {error}"
            ),
        }
    }
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

/// The subset of a `cargo metadata --format-version 1` response this gate reads.
///
/// Only the keys the edge extraction needs are declared; the response carries
/// far more (targets, features, resolve) and those are not modelled here.
#[derive(Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct CargoMetadata {
    /// Every package in the response, including the transitive dependencies of
    /// non-workspace packages, which `parse` filters out by membership.
    packages: Vec<CargoPackage>,
    /// Package ids of the workspace members, in `path+file:///…#name@version`
    /// form. Membership is decided by this list rather than by path prefix, so
    /// a path dependency living inside the repository is not mistaken for a
    /// member and does not get its edges counted twice.
    workspace_members: Vec<String>,
}

/// One package entry in a metadata response.
#[derive(Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct CargoPackage {
    /// Opaque package id; the only key matched against `workspace_members`.
    id: String,
    /// Package name as published. This is the name the register approves.
    name: String,
    /// Declared repository URL, when the manifest has one. Read only for
    /// workspace members, where it proves the member is ours.
    repository: Option<String>,
    /// Absolute path to the package's `Cargo.toml`. Read for workspace members
    /// so a scope can be resolved to a real module file rather than to a
    /// directory guessed from the package name.
    manifest_path: Option<String>,
    /// The dependency declarations this package authored, including optional
    /// edges that are inactive in the current feature selection, which is the
    /// reason metadata rather than the lockfile is the source of truth here.
    dependencies: Vec<CargoDependency>,
}

/// One authored dependency declaration, exactly as Cargo reports it.
#[derive(Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct CargoDependency {
    /// Upstream package name, after `package =` renaming is applied. A local
    /// rename in the manifest does not change this.
    name: String,
    /// Cargo's source string (`registry+…`, `git+…#rev`). Absent for a
    /// filesystem dependency, which is Cargo's encoding of path/workspace.
    source: Option<String>,
    /// Manifest semver requirement verbatim; compared byte-for-byte against
    /// the register, so `^1` and `1` are different approvals.
    req: String,
    /// `"normal"`, `"build"`, or `"dev"`; absent in some schema revisions and
    /// then read as normal.
    kind: Option<String>,
    /// Whether the manifest marks the edge optional. An optional edge is still
    /// audited: it is authored, even when the feature is off.
    optional: bool,
    /// Manifest path for a filesystem dependency, as Cargo reports it:
    /// absolute in current Cargo, resolved against the declaring package's
    /// directory when relative. Present only when `source` is absent.
    path: Option<String>,
}

/// Classifies one authored edge by the origin Cargo recorded.
///
/// A `source` key takes precedence over `path`, matching Cargo's own reading:
/// a registry package reached through a path override reports both, and the
/// registry spelling is what the register approves. `registry+` and `git+` are
/// the two schemes the gate names; any other scheme is carried verbatim as
/// [`DependencySource::Other`] so an unknown origin is reported rather than
/// dropped. An edge with neither key cannot be placed at all and is refused.
fn source(dependency: &CargoDependency) -> Result<DependencySource, MetadataError> {
    match (dependency.source.as_deref(), dependency.path.as_deref()) {
        (Some(value), _) if value.starts_with("registry+") => {
            Ok(DependencySource::Registry(value.to_owned()))
        }
        (Some(value), _) if value.starts_with("git+") => {
            Ok(DependencySource::Git(value.to_owned()))
        }
        (Some(value), _) => Ok(DependencySource::Other(value.to_owned())),
        (None, Some(path)) => Ok(DependencySource::Path(path.to_owned())),
        (None, None) => Err(MetadataError::Schema(format!(
            "dependency {:?} has neither source nor path",
            dependency.name
        ))),
    }
}

/// Decodes format-version 1 metadata into direct workspace edges.
pub fn parse(text: &str) -> Result<Vec<DirectEdge>, MetadataError> {
    let metadata: CargoMetadata = lgwks_std::json::from_str(text).map_err(MetadataError::Json)?;
    direct_edges(metadata)
}

/// Joins a manifest-relative dependency path to its declaring directory.
///
/// Purely lexical: `..` pops one component and `.` vanishes, so the answer
/// cannot change between classification and audit the way a filesystem probe
/// could. Cargo currently reports path keys absolute; an absolute key is
/// normalized on its own rather than joined. Returns `None` when the path
/// escapes the filesystem root it is resolved against — an unlocatable
/// target fails closed to external rather than guessing a membership.
fn lexical_join(base: &Path, relative: &str) -> Option<PathBuf> {
    use std::path::Component;
    let key = Path::new(relative);
    // Cargo path keys use `/` on every platform, and `Path::is_absolute`
    // answers for the host: a leading slash is absolute even where the host
    // would call it drive-relative (Windows), so the test is syntactic.
    let standalone = key.is_absolute() || relative.starts_with('/');
    let mut joined = if standalone {
        PathBuf::new()
    } else {
        base.to_path_buf()
    };
    for component in key.components() {
        match component {
            Component::Prefix(_) | Component::RootDir if standalone => {
                joined.push(component.as_os_str());
            }
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                if !joined.pop() {
                    return None;
                }
            }
            Component::Normal(part) => joined.push(part),
        }
    }
    Some(joined)
}

/// Extracts direct edges from one decoded Cargo metadata response.
///
/// Workspace membership is bound to the target's resolved directory, not its
/// name: an outside path package that shares a member's name is external
/// (issue #143 R13). The dependency declaration carries only the name and the
/// manifest-relative path, so the path is joined to the declaring package's
/// manifest directory and normalized lexically — no filesystem access, so a
/// hostile tree cannot change the answer between classification and audit.
/// Anything unresolvable fails closed to external.
fn direct_edges(metadata: CargoMetadata) -> Result<Vec<DirectEdge>, MetadataError> {
    let workspace = validated_workspace(&metadata)?;
    let member_packages = workspace.packages;
    let member_dirs = workspace.member_dirs;
    let declaring_dirs = workspace.declaring_dirs;
    let mut edges = Vec::new();
    for package in member_packages {
        let declaring = declaring_dirs.get(package.id.as_str());
        for dependency in &package.dependencies {
            let member = match (
                dependency.source.as_deref(),
                dependency.path.as_deref(),
                declaring,
            ) {
                (None, Some(path), Some(dir)) => {
                    lexical_join(dir, path).and_then(|target| member_dirs.get(&target).copied())
                }
                _ => None,
            };
            if let Some((member_name, _)) = member
                && dependency.name != member_name
            {
                return Err(MetadataError::Schema(format!(
                    "dependency {:?} resolves to workspace package {:?}",
                    dependency.name, member_name
                )));
            }
            edges.push(DirectEdge {
                consumer: package.name.clone(),
                package: dependency.name.clone(),
                requirement: dependency.req.clone(),
                kind: DependencyKind::from_cargo(dependency.kind.as_deref())?,
                source: source(dependency)?,
                optional: dependency.optional,
                workspace: member.is_some(),
                target_repository: member
                    .and_then(|(_, repository)| repository)
                    .map(str::to_owned),
            });
        }
    }
    edges.sort_by(|left, right| {
        (&left.consumer, &left.package, left.kind).cmp(&(
            &right.consumer,
            &right.package,
            right.kind,
        ))
    });
    Ok(edges)
}

/// Validated view shared by edge extraction and workspace inventory.
struct ValidatedWorkspace<'a> {
    /// Workspace packages resolved uniquely from the member-id list.
    packages: Vec<&'a CargoPackage>,
    /// Unique manifest directories mapped to package identity and repository.
    member_dirs: std::collections::BTreeMap<PathBuf, (&'a str, Option<&'a str>)>,
    /// Manifest directories keyed by each validated Cargo package id.
    declaring_dirs: std::collections::BTreeMap<&'a str, PathBuf>,
}

/// Validates identity completeness before classifying any package or edge.
fn validated_workspace(metadata: &CargoMetadata) -> Result<ValidatedWorkspace<'_>, MetadataError> {
    let mut package_ids = std::collections::BTreeSet::new();
    for package in &metadata.packages {
        if package.id.trim().is_empty() || package.name.trim().is_empty() {
            return Err(MetadataError::Schema(
                "Cargo package identity contains a blank id or name".to_owned(),
            ));
        }
        if !package_ids.insert(package.id.as_str()) {
            return Err(MetadataError::Schema(format!(
                "duplicate Cargo package id {:?}",
                package.id
            )));
        }
    }
    let mut member_ids = std::collections::BTreeSet::new();
    for id in &metadata.workspace_members {
        if !member_ids.insert(id.as_str()) {
            return Err(MetadataError::Schema(format!(
                "duplicate Cargo workspace member id {id:?}"
            )));
        }
        if !package_ids.contains(id.as_str()) {
            return Err(MetadataError::Schema(format!(
                "workspace member id {id:?} has no package record"
            )));
        }
    }
    let packages: Vec<&CargoPackage> = metadata
        .packages
        .iter()
        .filter(|package| member_ids.contains(package.id.as_str()))
        .collect();
    let mut member_dirs = std::collections::BTreeMap::new();
    let mut declaring_dirs = std::collections::BTreeMap::new();
    for package in &packages {
        let manifest = package.manifest_path.as_deref().ok_or_else(|| {
            MetadataError::Schema(format!(
                "workspace member {:?} has no manifest_path",
                package.id
            ))
        })?;
        let dir = Path::new(manifest)
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| {
                MetadataError::Schema(format!(
                    "workspace member {:?} has invalid manifest_path {manifest:?}",
                    package.id
                ))
            })?
            .to_path_buf();
        if member_dirs
            .insert(
                dir.clone(),
                (package.name.as_str(), package.repository.as_deref()),
            )
            .is_some()
        {
            return Err(MetadataError::Schema(format!(
                "multiple workspace packages claim manifest directory {:?}",
                dir
            )));
        }
        declaring_dirs.insert(package.id.as_str(), dir);
    }
    Ok(ValidatedWorkspace {
        packages,
        member_dirs,
        declaring_dirs,
    })
}

/// Elapsed-deadline budget for the Cargo subprocess: `cargo metadata` on
/// this workspace answers in about a second, so a healthy run never notices
/// the 120s collection ceiling. The ceiling bounds collection polling; OS
/// termination and reap failures are reported separately (issue #159).
const METADATA_TIMEOUT: Duration = Duration::from_secs(120);
/// Per-stream retained-byte budget for the Cargo subprocess. The 5ms polling
/// interval makes disk growth between samples possible, so this is not a hard
/// on-disk quota (issue #159 M3).
const METADATA_STREAM_CAP: usize = 8 * 1024 * 1024;
/// Exclusive-create attempts per capture file: each attempt draws a fresh
/// distinguisher, so this bound is never reached by chance, only by a
/// broken clock or a hostile temp dir, both of which refuse (issue #143 R14).
const CAPTURE_ATTEMPTS: usize = 16;

/// A reaped child's exit status with its bounded output.
struct BoundedOutput {
    /// What the child exited with; a non-zero status is Cargo's refusal.
    status: std::process::ExitStatus,
    /// At most the per-stream budget of stdout bytes.
    stdout: Vec<u8>,
    /// At most the per-stream budget of stderr bytes.
    stderr: Vec<u8>,
    /// A capture-removal failure after complete output, reported beside it.
    unresolved: Option<Unresolved>,
}

/// Capture removals that failed after a complete collection, and the owner
/// that can retry them.
#[derive(Debug)]
struct Unresolved {
    /// Every removal failure, in the order of the obligation's report.
    captures: Vec<std::io::Error>,
    /// The owner of the paths that could not be removed.
    obligation: Box<CleanupObligation>,
}

impl Unresolved {
    /// The cleanup failure as a refusal, with `cause` as its primary failure
    /// when collection was refused afterwards.
    fn into_error(self, cause: Option<MetadataError>) -> MetadataError {
        MetadataError::ProcessCleanup {
            cause: cause.map(Box::new),
            process: None,
            captures: self.captures,
            obligation: self.obligation,
        }
    }
}

/// Owns capture pathnames from the instant a capture file is created through
/// every later fallible collection step.
#[derive(Debug)]
struct CaptureFiles {
    /// Paths whose names this call created exclusively.
    paths: Vec<PathBuf>,
}

/// Calls the operating system unless one test has armed a single-use fault.
fn remove_capture(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if let Some(error) = tests::take_fault(tests::FaultPoint::Unlink) {
        return Err(error);
    }
    std::fs::remove_file(path)
}

/// Observes the direct child without treating an observation error as absence.
fn observe_child(
    child: &mut std::process::Child,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    #[cfg(test)]
    if let Some(error) = tests::take_fault(tests::FaultPoint::WaitObservation) {
        return Err(error);
    }
    child.try_wait()
}

/// Requests termination and preserves an OS refusal for the cleanup owner.
fn terminate_child(child: &mut std::process::Child) -> std::io::Result<()> {
    #[cfg(test)]
    if let Some(error) = tests::take_fault(tests::FaultPoint::Kill) {
        return Err(error);
    }
    child.kill()
}

/// Waits for a direct child; an error leaves it in the retryable owner.
fn reap_child(child: &mut std::process::Child) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(test)]
    if let Some(error) = tests::take_fault(tests::FaultPoint::Reap) {
        return Err(error);
    }
    child.wait()
}

impl CaptureFiles {
    /// Starts an empty cleanup owner before acquiring the first file.
    fn new() -> Self {
        Self { paths: Vec::new() }
    }

    /// Records a newly acquired path before another operation can fail.
    fn own(&mut self, path: PathBuf) {
        #[cfg(test)]
        tests::record_capture(path.clone());
        self.paths.push(path);
    }

    /// Removes every owned path and returns every failure except confirmed
    /// absence. Failed removals remain visible to the caller.
    fn cleanup(&mut self) -> Vec<std::io::Error> {
        let mut failures = Vec::new();
        let mut pending = Vec::new();
        for path in self.paths.drain(..) {
            match remove_capture(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    failures.push(error);
                    pending.push(path);
                }
            }
        }
        self.paths = pending;
        failures
    }
}

impl Drop for CaptureFiles {
    /// Retries cleanup on early return; this fallback never reports success.
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

/// Retains ownership of a direct child or capture whose cleanup was not
/// confirmed. Keep this value and retry until it reports `true`.
#[must_use = "dropping an unresolved cleanup obligation abandons further observation"]
#[derive(Debug)]
pub struct CleanupObligation {
    /// The child whose termination or reap remains unresolved.
    child: Option<std::process::Child>,
    /// Capture paths whose removal remains unresolved.
    captures: CaptureFiles,
    /// What failed when the obligation was created, fixed so the refusal's
    /// text does not change as retries make progress.
    report: CleanupReport,
}

/// The facts a cleanup refusal names: which step failed, on which process, and
/// which capture paths could not be removed.
#[derive(Debug)]
struct CleanupReport {
    /// The process step that failed, if one did.
    step: Option<CleanupStep>,
    /// The direct child's process id, while the obligation still owns it.
    pid: Option<u32>,
    /// Capture paths whose removal failed, in the order of the retained errors.
    capture_paths: Vec<PathBuf>,
}

/// A process-cleanup step that can fail and leave the child owned.
#[derive(Debug, Clone, Copy)]
enum CleanupStep {
    /// Sending the kill request.
    Kill,
    /// Waiting for the exited child to be reaped.
    Reap,
}

impl CleanupStep {
    /// The verb a refusal uses for this step.
    fn verb(self) -> &'static str {
        match self {
            Self::Kill => "kill",
            Self::Reap => "reap",
        }
    }
}

impl CleanupObligation {
    /// Takes ownership of an unresolved child and capture set, recording what
    /// failed at this moment for the refusal's text.
    fn new(
        step: Option<CleanupStep>,
        child: Option<std::process::Child>,
        captures: &mut CaptureFiles,
    ) -> Self {
        let paths = std::mem::take(&mut captures.paths);
        Self {
            report: CleanupReport {
                step,
                pid: child.as_ref().map(std::process::Child::id),
                capture_paths: paths.clone(),
            },
            child,
            captures: CaptureFiles { paths },
        }
    }
}

impl CleanupObligation {
    /// Attempts direct-child termination/reap and capture removal once.
    /// Returns `true` only when no owned child or capture path remains.
    ///
    /// ```rust
    /// use lgwks_deps::metadata::MetadataError;
    ///
    /// fn retry_cleanup(error: MetadataError) -> Result<bool, std::io::Error> {
    ///     match error {
    ///         MetadataError::ProcessCleanup { mut obligation, .. } => {
    ///             obligation.retry_cleanup()
    ///         }
    ///         _ => Ok(true),
    ///     }
    /// }
    /// ```
    pub fn retry_cleanup(&mut self) -> Result<bool, std::io::Error> {
        if let Some(child) = self.child.as_mut() {
            match terminate_child(child) {
                Ok(()) => {}
                Err(kill_error) => match child.try_wait() {
                    Ok(Some(_)) => self.child = None,
                    Ok(None) => return Err(kill_error),
                    Err(observation_error) => return Err(observation_error),
                },
            }
            if let Some(child) = self.child.as_mut() {
                match observe_child(child)? {
                    Some(_) => self.child = None,
                    None => return Ok(false),
                }
            }
        }
        if let Some(error) = self.captures.cleanup().into_iter().next() {
            return Err(error);
        }
        Ok(self.captures.paths.is_empty())
    }
}

/// How many poll quanta a dropped obligation waits for its child to exit.
///
/// Twenty 5 ms quanta: long enough for a killed child to be reaped on a loaded
/// host, short enough that dropping an error never stalls the caller for more
/// than a tenth of a second.
const DROP_CLEANUP_QUANTA: usize = 20;

/// A dropped obligation makes a last, bounded attempt to kill and reap.
///
/// The shipped gate reports a `ProcessCleanup` refusal as text and drops the
/// error, so without this the Cargo child it names was never killed or reaped
/// by anyone. A killed child is not reaped the instant the signal is sent, so
/// one `try_wait` would usually leave a zombie; this repeats the same `kill`
/// plus `try_wait` a retry makes for at most twenty poll quanta and then gives
/// up, because a destructor must not block without bound. Capture files have
/// their own `Drop`.
impl Drop for CleanupObligation {
    fn drop(&mut self) {
        for _ in 0..DROP_CLEANUP_QUANTA {
            if matches!(self.retry_cleanup(), Ok(true)) {
                return;
            }
            poll_quantum();
        }
    }
}

/// One deadline-poll quantum.
///
/// The suppression is the narrow exception for the `deny` API ban: no
/// replacement exists for parking a synchronous gate-library thread between
/// child polls, the 5ms quantum bounds deadline overshoot, and the poll loop
/// is the only waiter so it always stops.
#[expect(
    clippy::disallowed_methods,
    reason = "no async runtime exists in this sync library; the bounded poll quantum is the deadline mechanism, not reactor blocking"
)]
fn poll_quantum() {
    std::thread::sleep(Duration::from_millis(5));
}

/// Runs one subprocess to completion under a deadline and output budgets.
///
/// The child writes to two capture files, never to pipes: a flood cannot
/// block the child on a full pipe, and no reader thread can be held open by
/// a grandchild that inherited a descriptor. Each poll quantum stats both
/// files, and the first one past budget requests direct-child termination
/// before returning [`MetadataError::OutputTooLarge`]; the deadline does the
/// same with [`MetadataError::Timeout`]. Termination, reap and capture-removal
/// failures are retained in [`MetadataError::ProcessCleanup`]. A refusal
/// carries no partial graph: the gate reports the failure rather than
/// decoding a prefix of it.
///
/// The kill targets the direct child only. A descendant may keep an inherited
/// capture descriptor and continue writing after the direct child's exit; the
/// sampled retained-byte cap does not bound that writer's disk use or reclaim
/// its open file immediately. The supported profile is trusted Cargo and its
/// direct child, not hostile descendant containment.
fn run_bounded(
    program: &str,
    args: &[OsString],
    dir: &Path,
    timeout: Duration,
    stream_cap: usize,
) -> Result<BoundedOutput, MetadataError> {
    use std::fs::File;
    use std::io::Read as _;
    use std::process::Stdio;

    /// One capture file: created empty and exclusive, unlinked on every
    /// return path. Creation uses `create_new`, so even a distinguisher
    /// collision is a retried name, never another call's file truncated.
    fn capture(name: &str) -> Result<(PathBuf, File), MetadataError> {
        use std::io::ErrorKind;
        #[cfg(test)]
        {
            let fault = if name == "stdout" {
                tests::FaultPoint::CaptureStdout
            } else {
                tests::FaultPoint::CaptureStderr
            };
            if let Some(error) = tests::take_fault(fault) {
                return Err(MetadataError::Spawn(error));
            }
        }
        // Bounded: each attempt draws a fresh distinguisher, so exhaustion
        // is a refusal shape, not a spin.
        for _ in 0..CAPTURE_ATTEMPTS {
            let path = std::env::temp_dir().join(format!(
                "lgwks-deps-{name}-{distinguisher}.capture",
                distinguisher = distinguisher()?,
            ));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => return Ok((path, file)),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(MetadataError::Spawn(error)),
            }
        }
        Err(MetadataError::Spawn(std::io::Error::from(
            ErrorKind::AlreadyExists,
        )))
    }

    /// Names one capture file: 128 bits of OS entropy (`lgwks_std::random`,
    /// the one source INV-RANDOM-ONE-SOURCE allows). A process id or a clock
    /// would be reused by the OS and could name another call's file. An
    /// entropy refusal names no file and starts no child; the call ends here.
    #[cfg(not(target_family = "wasm"))]
    fn distinguisher() -> Result<String, MetadataError> {
        lgwks_std::random::bytes::<16>()
            .map(lgwks_std::hex::encode)
            .map_err(MetadataError::Entropy)
    }

    /// wasm has no entropy source: `lgwks_std::random` refuses the target
    /// with a `compile_error!`, and the WASI boundary job builds this crate
    /// for `wasm32-wasip1`. Nanos plus a monotone sequence name the file,
    /// and `create_new` above is what makes that safe: a repeated name is
    /// retried, never opened. Spawning cannot succeed on this target anyway,
    /// so these files only ever live until the `Spawn` refusal unlinks them.
    #[cfg(target_family = "wasm")]
    fn distinguisher() -> Result<String, MetadataError> {
        static CAPTURE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or(0);
        let seq = CAPTURE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(format!("{nanos}-{seq}"))
    }

    /// Read at most one byte past budget: anything longer is an overflow,
    /// and the retained bytes are dropped with the Vec, never decoded.
    fn read_capped(
        path: &Path,
        stream_cap: usize,
        _stream: &'static str,
    ) -> Result<(Vec<u8>, bool), MetadataError> {
        #[cfg(test)]
        {
            let fault = if _stream == "stdout" {
                tests::FaultPoint::ReadStdout
            } else {
                tests::FaultPoint::ReadStderr
            };
            if let Some(error) = tests::take_fault(fault) {
                return Err(MetadataError::Spawn(error));
            }
        }
        let limit = u64::try_from(stream_cap).unwrap_or(u64::MAX);
        let file = File::open(path).map_err(MetadataError::Spawn)?;
        let mut kept = Vec::new();
        file.take(limit.saturating_add(1))
            .read_to_end(&mut kept)
            .map_err(MetadataError::Spawn)?;
        let overflow = kept.len() > stream_cap;
        Ok((kept, overflow))
    }

    /// Keeps the original failure distinct from any cleanup failure.
    fn cleanup_error(
        cause: Option<MetadataError>,
        process: Option<(CleanupStep, std::io::Error)>,
        child: Option<std::process::Child>,
        captures: &mut CaptureFiles,
    ) -> MetadataError {
        let capture_errors = captures.cleanup();
        if process.is_none()
            && capture_errors.is_empty()
            && let Some(cause) = cause
        {
            return cause;
        }
        let (step, process) = match process {
            Some((step, error)) => (Some(step), Some(error)),
            None => (None, None),
        };
        MetadataError::ProcessCleanup {
            cause: cause.map(Box::new),
            process,
            captures: capture_errors,
            obligation: Box::new(CleanupObligation::new(step, child, captures)),
        }
    }

    let mut captures = CaptureFiles::new();
    let (stdout_path, stdout_file) = match capture("stdout") {
        Ok(capture) => capture,
        Err(error) => return Err(cleanup_error(Some(error), None, None, &mut captures)),
    };
    captures.own(stdout_path.clone());
    let (stderr_path, stderr_file) = match capture("stderr") {
        Ok(capture) => capture,
        Err(error) => return Err(cleanup_error(Some(error), None, None, &mut captures)),
    };
    captures.own(stderr_path.clone());
    #[cfg(test)]
    let spawn_fault = tests::take_fault(tests::FaultPoint::Spawn);
    #[cfg(not(test))]
    let spawn_fault: Option<std::io::Error> = None;
    let spawned = match spawn_fault {
        Some(error) => Err(error),
        None => Command::new(program)
            .args(args)
            .current_dir(dir)
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file))
            .spawn(),
    };
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            return Err(cleanup_error(
                Some(MetadataError::Spawn(error)),
                None,
                None,
                &mut captures,
            ));
        }
    };
    let deadline = std::time::Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(std::time::Instant::now);
    // The only waiter, and it always stops: each quantum either observes
    // the exit, fires the deadline, or finds a capture file past budget.
    // File sizes only grow while the child lives, so a stat past budget
    // stands against every later one.
    let outcome = loop {
        match observe_child(&mut child) {
            Err(error) => break Err(MetadataError::Spawn(error)),
            Ok(Some(status)) => break Ok(status),
            Ok(None) if std::time::Instant::now() >= deadline => {
                break Err(MetadataError::Timeout { after: timeout });
            }
            Ok(None) => {
                let limit = u64::try_from(stream_cap).unwrap_or(u64::MAX);
                #[cfg(test)]
                let stdout_stat_fault = tests::take_fault(tests::FaultPoint::StatStdout);
                #[cfg(not(test))]
                let stdout_stat_fault: Option<std::io::Error> = None;
                if let Some(error) = stdout_stat_fault {
                    break Err(MetadataError::Spawn(error));
                }
                let stdout_size = match std::fs::metadata(&stdout_path) {
                    Ok(size) => size,
                    Err(error) => break Err(MetadataError::Spawn(error)),
                };
                if stdout_size.len() > limit {
                    break Err(MetadataError::OutputTooLarge {
                        stream: "stdout",
                        limit: stream_cap,
                    });
                }
                #[cfg(test)]
                let stderr_stat_fault = tests::take_fault(tests::FaultPoint::StatStderr);
                #[cfg(not(test))]
                let stderr_stat_fault: Option<std::io::Error> = None;
                if let Some(error) = stderr_stat_fault {
                    break Err(MetadataError::Spawn(error));
                }
                let stderr_size = match std::fs::metadata(&stderr_path) {
                    Ok(size) => size,
                    Err(error) => break Err(MetadataError::Spawn(error)),
                };
                if stderr_size.len() > limit {
                    break Err(MetadataError::OutputTooLarge {
                        stream: "stderr",
                        limit: stream_cap,
                    });
                }
                poll_quantum();
            }
        }
    };
    let status = match outcome {
        Err(cause) => {
            let process_error = match terminate_child(&mut child) {
                Ok(()) => reap_child(&mut child)
                    .err()
                    .map(|error| (CleanupStep::Reap, error)),
                Err(kill_error) => match child.try_wait() {
                    Ok(Some(_)) => None,
                    Ok(None) | Err(_) => Some((CleanupStep::Kill, kill_error)),
                },
            };
            return match process_error {
                Some(error) => Err(cleanup_error(
                    Some(cause),
                    Some(error),
                    Some(child),
                    &mut captures,
                )),
                None => Err(cleanup_error(Some(cause), None, None, &mut captures)),
            };
        }
        Ok(status) => match reap_child(&mut child) {
            Ok(_) => status,
            Err(error) => {
                return Err(cleanup_error(
                    None,
                    Some((CleanupStep::Reap, error)),
                    Some(child),
                    &mut captures,
                ));
            }
        },
    };
    let (stdout, stdout_overflow) = match read_capped(&stdout_path, stream_cap, "stdout") {
        Ok(output) => output,
        Err(error) => return Err(cleanup_error(Some(error), None, None, &mut captures)),
    };
    let (stderr, stderr_overflow) = match read_capped(&stderr_path, stream_cap, "stderr") {
        Ok(output) => output,
        Err(error) => return Err(cleanup_error(Some(error), None, None, &mut captures)),
    };
    let overflow = if stdout_overflow {
        Some(MetadataError::OutputTooLarge {
            stream: "stdout",
            limit: stream_cap,
        })
    } else if stderr_overflow {
        Some(MetadataError::OutputTooLarge {
            stream: "stderr",
            limit: stream_cap,
        })
    } else {
        None
    };
    if let Some(cause) = overflow {
        return Err(cleanup_error(Some(cause), None, None, &mut captures));
    }
    // Both streams are read in full and the child is reaped, so the output is
    // complete. A capture that cannot be removed now is reported beside it,
    // not in place of it: the graph is valid, and the leftover path is owned
    // by the obligation for a retry.
    let cleanup_errors = captures.cleanup();
    let unresolved = (!cleanup_errors.is_empty()).then(|| Unresolved {
        captures: cleanup_errors,
        obligation: Box::new(CleanupObligation::new(None, None, &mut captures)),
    });
    Ok(BoundedOutput {
        status,
        stdout,
        stderr,
        unresolved,
    })
}

/// Runs locked Cargo metadata and decodes the supported response shape.
///
/// The manifest path is made absolute before Cargo is started. `--manifest-path`
/// is resolved by Cargo against *its* working directory, which this call sets to
/// `root`, so a relative `root` would otherwise be joined to itself — a gate
/// invoked as `check crates/thing` would report a manifest that plainly exists
/// as missing, and every fixture test built on a relative path would pass on
/// that refusal rather than on the rule it meant to exercise.
fn read_metadata(root: &Path) -> Result<Collected<CargoMetadata>, MetadataError> {
    let manifest = manifest_path(root)?;
    let output = run_bounded(
        "cargo",
        &[
            OsString::from("metadata"),
            OsString::from("--locked"),
            OsString::from("--no-deps"),
            OsString::from("--format-version"),
            OsString::from("1"),
            OsString::from("--manifest-path"),
            manifest.into_os_string(),
        ],
        root,
        METADATA_TIMEOUT,
        METADATA_STREAM_CAP,
    )?;
    let collected = Collected {
        value: output.stdout,
        unresolved: output.unresolved,
    };
    if !output.status.success() {
        let refusal =
            MetadataError::Cargo(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        return Err(collected.refuse(refusal));
    }
    collected.try_map(|stdout| lgwks_std::json::from_slice(&stdout).map_err(MetadataError::Json))
}

/// A complete collection, and any capture cleanup that could not be confirmed
/// after it.
///
/// Cargo's output was read in full and its process reaped, so [`value`] is the
/// whole answer. A capture file that could not then be removed is reported
/// beside it as [`MetadataError::ProcessCleanup`], whose obligation still owns
/// the path for a retry, rather than turning a valid graph into a refusal.
///
/// [`value`]: Collected::value
///
/// ```rust
/// use lgwks_deps::metadata::{self, Collected, DirectEdge};
///
/// fn edges(root: &std::path::Path) -> Result<Vec<DirectEdge>, metadata::MetadataError> {
///     let (edges, unresolved) = metadata::read(root)?.into_parts();
///     if let Some(cleanup) = unresolved {
///         eprintln!("warning: {cleanup}");
///     }
///     Ok(edges)
/// }
/// ```
#[derive(Debug)]
#[must_use = "a collection may carry a cleanup failure that must be reported"]
pub struct Collected<T> {
    /// The complete result.
    value: T,
    /// Capture removals that failed after the result was complete.
    unresolved: Option<Unresolved>,
}

impl<T> Collected<T> {
    /// The result, and the cleanup that could not be confirmed after it as a
    /// [`MetadataError::ProcessCleanup`] with no primary cause.
    pub fn into_parts(self) -> (T, Option<MetadataError>) {
        (
            self.value,
            self.unresolved
                .map(|unresolved| unresolved.into_error(None)),
        )
    }

    /// Transforms the result and keeps the cleanup beside it.
    pub fn map<U>(self, transform: impl FnOnce(T) -> U) -> Collected<U> {
        Collected {
            value: transform(self.value),
            unresolved: self.unresolved,
        }
    }

    /// Transforms the result; a refusal keeps the cleanup failure attached.
    fn try_map<U>(
        self,
        transform: impl FnOnce(T) -> Result<U, MetadataError>,
    ) -> Result<Collected<U>, MetadataError> {
        match transform(self.value) {
            Ok(value) => Ok(Collected {
                value,
                unresolved: self.unresolved,
            }),
            Err(refusal) => Err(Self::attach(refusal, self.unresolved)),
        }
    }

    /// A refusal decided after collection, with any cleanup failure attached so
    /// neither is lost.
    fn refuse(self, refusal: MetadataError) -> MetadataError {
        Self::attach(refusal, self.unresolved)
    }

    /// `refusal` as the primary cause of an unresolved cleanup, if there is one.
    fn attach(refusal: MetadataError, unresolved: Option<Unresolved>) -> MetadataError {
        match unresolved {
            Some(unresolved) => unresolved.into_error(Some(refusal)),
            None => refusal,
        }
    }
}

/// The manifest Cargo must read, resolved before the child can reinterpret it.
///
/// Cargo resolves `--manifest-path` against *its own* working directory, which
/// the call above sets to `root`. A path built from a relative `root` therefore
/// has its components applied twice — once here, once in the child — and names
/// a manifest that does not exist: a repository the operator named is reported
/// as missing, and any fallback that guessed instead would audit a tree nobody
/// named. Resolving the path once, here, against this process's working
/// directory leaves the child nothing to reinterpret, and keeps the refusal
/// message naming the manifest Cargo was actually given.
fn manifest_path(root: &Path) -> Result<std::path::PathBuf, MetadataError> {
    let manifest = root.join("Cargo.toml");
    std::path::absolute(&manifest).map_err(|cause| MetadataError::Root {
        path: manifest,
        cause,
    })
}

/// Runs locked Cargo metadata and returns every direct workspace edge, with
/// any capture cleanup that could not be confirmed after a complete read.
pub fn read(root: &Path) -> Result<Collected<Vec<DirectEdge>>, MetadataError> {
    read_metadata(root)?.try_map(direct_edges)
}

/// One workspace member, with the directory holding its manifest.
///
/// Scope resolution needs the directory: a scope of `lgwks_bot::verb` is only
/// real if `verb` is a module of *that* package, and the package name alone
/// does not say where its sources are.
///
/// ```rust
/// use lgwks_deps::metadata::Member;
///
/// fn package_name(member: &Member) -> &str {
///     member.name()
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Member {
    /// Package name as the manifest declares it.
    pub(crate) name: String,
    /// Directory holding the member's `Cargo.toml`, as Cargo reported it.
    pub manifest_dir: PathBuf,
}

impl Member {
    /// Cargo package name for this workspace member.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Runs locked Cargo metadata and returns its workspace members.
///
/// Scope validation uses this list rather than path prefixes, so an invariant
/// cannot claim authority over a directory merely because it lives under the
/// repository. Members are sorted by name for deterministic diagnostics.
///
/// A member whose `manifest_path` Cargo omitted is `Schema`: the directory is
/// the whole point of this call, and a member the audit cannot locate must be
/// a refusal rather than a member it quietly cannot resolve scopes against.
///
/// A capture cleanup that could not be confirmed after a complete read is
/// carried beside the members; see [`Collected`].
pub fn workspace_members(root: &Path) -> Result<Collected<Vec<Member>>, MetadataError> {
    read_metadata(root)?.try_map(|metadata| located_members(&metadata))
}

/// Every workspace member with its validated manifest directory, by name.
fn located_members(metadata: &CargoMetadata) -> Result<Vec<Member>, MetadataError> {
    let workspace = validated_workspace(metadata)?;
    let mut located = Vec::new();
    for package in workspace.packages {
        let manifest_dir = workspace
            .declaring_dirs
            .get(package.id.as_str())
            .cloned()
            .ok_or_else(|| {
                MetadataError::Schema(format!(
                    "workspace member {:?} has no validated manifest directory",
                    package.id
                ))
            })?;
        located.push(Member {
            name: package.name.clone(),
            manifest_dir,
        });
    }
    located.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(located)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum FaultPoint {
        CaptureStdout,
        CaptureStderr,
        Spawn,
        WaitObservation,
        StatStdout,
        StatStderr,
        ReadStdout,
        ReadStderr,
        Kill,
        Reap,
        Unlink,
    }

    thread_local! {
        static FAULTS: RefCell<VecDeque<FaultPoint>> = const { RefCell::new(VecDeque::new()) };
        static OWNED_CAPTURES: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn take_fault(point: FaultPoint) -> Option<std::io::Error> {
        FAULTS.with(|faults| {
            if faults.borrow().front() == Some(&point) {
                faults.borrow_mut().pop_front();
                Some(std::io::Error::other(format!(
                    "injected metadata fault at {point:?}"
                )))
            } else {
                None
            }
        })
    }

    fn arm_faults(points: &[FaultPoint]) {
        FAULTS.with(|faults| *faults.borrow_mut() = points.iter().copied().collect());
        OWNED_CAPTURES.with(|paths| paths.borrow_mut().clear());
    }

    pub(super) fn record_capture(path: PathBuf) {
        OWNED_CAPTURES.with(|paths| paths.borrow_mut().push(path));
    }

    fn captured_paths() -> Vec<PathBuf> {
        OWNED_CAPTURES.with(|paths| std::mem::take(&mut *paths.borrow_mut()))
    }

    /// Retries cleanup for the same bounded quanta `Drop` allows. A killed
    /// child is not reaped the instant the signal lands, so one
    /// `retry_cleanup` legitimately reports `false` on a loaded host.
    fn retry_until_resolved(obligation: &mut CleanupObligation) -> std::io::Result<bool> {
        for _ in 0..DROP_CLEANUP_QUANTA {
            if obligation.retry_cleanup()? {
                return Ok(true);
            }
            poll_quantum();
        }
        obligation.retry_cleanup()
    }

    fn assert_paths_removed(paths: &[PathBuf]) -> TestResult {
        for path in paths {
            assert_eq!(
                std::fs::metadata(path).err().map(|error| error.kind()),
                Some(std::io::ErrorKind::NotFound),
                "capture path should be gone: {}",
                path.display()
            );
        }
        Ok(())
    }

    /// Test bodies propagate with `?` rather than panicking: `unwrap` is
    /// forbidden workspace-wide, and a failing edge extraction should surface
    /// as the error it is, not as a panic with no variant attached.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn missing_workspace_package_record_is_a_schema_refusal() {
        let input = r#"{"packages":[],"workspace_members":["path+file:///repo#app@0.1.0"]}"#;
        assert!(
            matches!(
                parse(input),
                Err(MetadataError::Schema(message)) if message.contains("has no package record")
            ),
            "a missing workspace member record must be a schema refusal"
        );
    }

    #[test]
    fn a_valid_empty_workspace_remains_valid() -> TestResult {
        assert!(
            parse(r#"{"packages":[],"workspace_members":[]}"#)?.is_empty(),
            "an empty virtual workspace is a valid metadata subject"
        );
        Ok(())
    }

    #[test]
    fn duplicate_package_ids_and_manifest_directories_are_schema_refusals() {
        let duplicate_id = r#"{"packages":[
          {"id":"same","name":"app","manifest_path":"/repo/Cargo.toml","dependencies":[]},
          {"id":"same","name":"other","manifest_path":"/repo/other/Cargo.toml","dependencies":[]}
        ],"workspace_members":["same"]}"#;
        assert!(
            matches!(
                parse(duplicate_id),
                Err(MetadataError::Schema(message)) if message.contains("duplicate Cargo package id")
            ),
            "duplicate package ids must be refused before graph extraction"
        );

        let duplicate_dir = r#"{"packages":[
          {"id":"first","name":"first","manifest_path":"/repo/Cargo.toml","dependencies":[]},
          {"id":"second","name":"second","manifest_path":"/repo/Cargo.toml","dependencies":[]}
        ],"workspace_members":["first","second"]}"#;
        assert!(
            matches!(
                parse(duplicate_dir),
                Err(MetadataError::Schema(message)) if message.contains("multiple workspace packages")
            ),
            "two members cannot claim the same manifest directory"
        );
    }

    #[test]
    fn duplicate_workspace_ids_and_missing_member_manifests_are_refused() {
        let duplicate_member = r#"{"packages":[
          {"id":"app","name":"app","manifest_path":"/repo/Cargo.toml","dependencies":[]}
        ],"workspace_members":["app","app"]}"#;
        assert!(
            matches!(
                parse(duplicate_member),
                Err(MetadataError::Schema(message)) if message.contains("duplicate Cargo workspace member id")
            ),
            "a repeated workspace member id must be refused"
        );

        let missing_manifest = r#"{"packages":[
          {"id":"app","name":"app","dependencies":[]}
        ],"workspace_members":["app"]}"#;
        assert!(
            matches!(
                parse(missing_manifest),
                Err(MetadataError::Schema(message)) if message.contains("has no manifest_path")
            ),
            "a workspace member without a manifest identity must be refused"
        );
    }

    #[test]
    fn a_member_path_with_a_different_package_identity_is_refused() {
        let input = r#"{
          "packages": [
            {"id":"app","name":"app","manifest_path":"/repo/Cargo.toml","dependencies":[
              {"name":"renamed","source":null,"req":"*","kind":null,"optional":false,"path":"helper"}
            ]},
            {"id":"helper","name":"helper","manifest_path":"/repo/helper/Cargo.toml","dependencies":[]}
          ],"workspace_members":["app","helper"]
        }"#;
        assert!(
            matches!(
                parse(input),
                Err(MetadataError::Schema(message)) if message.contains("resolves to workspace package")
            ),
            "a path target's package identity must agree with its member record"
        );
    }

    /// The manifest path must be absolute before it reaches the child.
    ///
    /// `read_metadata` spawns Cargo with its working directory set to the
    /// repository root, and Cargo resolves `--manifest-path` against *that*
    /// directory. A relative manifest path is therefore applied twice — once
    /// against this process's working directory and once against the child's —
    /// and names a manifest that does not exist, so the gate reports a
    /// repository the operator named as missing. The check is lexical: it holds
    /// whatever the working directory happens to be, and for the same reason a
    /// root that is already absolute must reach Cargo unchanged.
    #[test]
    fn a_manifest_path_is_resolved_before_cargo_can_reinterpret_it() -> TestResult {
        let relative = manifest_path(Path::new("crates/lgwks-deps"))?;
        assert!(
            relative.is_absolute(),
            "a relative root must be resolved here: {}",
            relative.display()
        );
        assert!(
            relative.ends_with("crates/lgwks-deps/Cargo.toml"),
            "the resolved path is the root's own manifest: {}",
            relative.display()
        );
        let root = std::env::current_dir()?.join("crates/lgwks-deps");
        assert_eq!(
            manifest_path(&root)?,
            root.join("Cargo.toml"),
            "an absolute root must reach Cargo as the same manifest"
        );
        Ok(())
    }

    #[test]
    fn includes_inactive_optional_and_dev_edges() -> TestResult {
        let input = r#"{
          "packages": [{
            "id": "path+file:///repo#app@0.1.0",
            "name": "app",
            "repository": "https://example.invalid/app",
            "manifest_path": "/repo/Cargo.toml",
            "dependencies": [
              {"name":"serde","source":"registry+https://github.com/rust-lang/crates.io-index","req":"^1","kind":null,"optional":true,"path":null},
              {"name":"proptest","source":"registry+https://github.com/rust-lang/crates.io-index","req":"^1.6","kind":"dev","optional":false,"path":null}
            ]
          }],
          "workspace_members": ["path+file:///repo#app@0.1.0"]
        }"#;
        let edges = parse(input)?;
        assert_eq!(edges.len(), 2);
        let serde = edges
            .iter()
            .find(|edge| edge.package == "serde")
            .ok_or("the serde edge should have been extracted")?;
        let proptest = edges
            .iter()
            .find(|edge| edge.package == "proptest")
            .ok_or("the proptest edge should have been extracted")?;
        assert!(serde.optional);
        assert!(!serde.workspace);
        assert_eq!(proptest.kind, DependencyKind::Dev);
        Ok(())
    }

    #[test]
    fn ignores_non_workspace_transitives() -> TestResult {
        let input = r#"{
          "packages": [
            {"id":"path+file:///repo#app@0.1.0","name":"app","repository":null,
             "manifest_path":"/repo/Cargo.toml","dependencies":[]},
            {"id":"registry+x#serde@1.0.0","name":"serde","repository":null,"dependencies":[]}
          ],
          "workspace_members": ["path+file:///repo#app@0.1.0"]
        }"#;
        assert!(parse(input)?.is_empty());
        Ok(())
    }

    /// An outside path package sharing a member's name is not that member
    /// (issue #143 R13).
    ///
    /// `app` depends on a `helper` that lives outside the workspace while the
    /// workspace has its own member also called `helper`. Name membership
    /// alone would mark the outside edge internal and let `audit_direct`
    /// skip external admission for it — a gate bypass. The edge must be
    /// external, carry its path source, and inherit no member repository.
    #[test]
    fn an_outside_path_package_with_a_member_name_is_not_a_member() -> TestResult {
        let input = r#"{
          "packages": [
            {"id":"path+file:///repo#app@0.1.0","name":"app","repository":null,
             "manifest_path":"/repo/Cargo.toml",
             "dependencies":[
               {"name":"helper","source":null,"req":"*","kind":null,"optional":false,"path":"../outside/helper"}
             ]},
            {"id":"path+file:///repo/helper#helper@0.1.0","name":"helper",
             "repository":"https://example.invalid/helper",
             "manifest_path":"/repo/helper/Cargo.toml","dependencies":[]},
            {"id":"path+file:///outside/helper#helper@0.2.0","name":"helper",
             "repository":null,
             "manifest_path":"/outside/helper/Cargo.toml","dependencies":[]}
          ],
          "workspace_members": ["path+file:///repo#app@0.1.0","path+file:///repo/helper#helper@0.1.0"]
        }"#;
        let edges = parse(input)?;
        assert_eq!(edges.len(), 1);
        let edge = &edges[0];
        assert_eq!(edge.consumer, "app");
        assert_eq!(edge.package, "helper");
        assert!(
            !edge.workspace,
            "an outside path target must not inherit membership from its name"
        );
        assert_eq!(
            edge.source,
            DependencySource::Path("../outside/helper".to_owned())
        );
        assert_eq!(
            edge.target_repository, None,
            "an outside target must not inherit the member's repository"
        );
        Ok(())
    }

    /// The positive control: a path edge that resolves into a member's
    /// directory stays internal and keeps that member's repository.
    #[test]
    fn a_path_edge_resolving_into_a_member_directory_is_a_member() -> TestResult {
        let input = r#"{
          "packages": [
            {"id":"path+file:///repo#app@0.1.0","name":"app","repository":null,
             "manifest_path":"/repo/Cargo.toml",
             "dependencies":[
               {"name":"helper","source":null,"req":"*","kind":null,"optional":false,"path":"helper"}
             ]},
            {"id":"path+file:///repo/helper#helper@0.1.0","name":"helper",
             "repository":"https://example.invalid/helper",
             "manifest_path":"/repo/helper/Cargo.toml","dependencies":[]}
          ],
          "workspace_members": ["path+file:///repo#app@0.1.0","path+file:///repo/helper#helper@0.1.0"]
        }"#;
        let edges = parse(input)?;
        assert_eq!(edges.len(), 1);
        let edge = &edges[0];
        assert!(
            edge.workspace,
            "a path target inside a member directory is that member"
        );
        assert_eq!(
            edge.target_repository,
            Some("https://example.invalid/helper".to_owned())
        );
        Ok(())
    }

    /// A relative path that escapes the root it resolves against has no
    /// location to compare: the join refuses rather than naming a directory
    /// outside the filesystem.
    #[test]
    fn a_path_escaping_its_root_has_no_join() -> TestResult {
        assert_eq!(
            lexical_join(Path::new("/repo"), "sub/../helper"),
            Some(PathBuf::from("/repo/helper"))
        );
        assert_eq!(lexical_join(Path::new("/repo"), "../../escape"), None);
        // Cargo currently reports path keys absolute: the key stands alone.
        assert_eq!(
            lexical_join(Path::new("/repo/app"), "/repo/helper"),
            Some(PathBuf::from("/repo/helper"))
        );
        Ok(())
    }

    /// A controllable stand-in for the `cargo` subprocess: sleep, flood, or
    /// emit, with no network, no lockfile, and no toolchain beyond the
    /// platform shell. `cfg`, not runtime detection: the command must exist
    /// where the test compiles.
    #[cfg(unix)]
    fn sleeper() -> (&'static str, Vec<OsString>) {
        // Background-plus-wait, not a bare sleep: the shell forks a
        // grandchild that inherits the capture files, which is the shape
        // that held pipes open and defeated a pipe-based design on hosted
        // CI. Killing the direct child must still end the call at the
        // deadline even with the grandchild's descriptors open.
        (
            "sh",
            vec![OsString::from("-c"), OsString::from("sleep 10 & wait")],
        )
    }

    #[cfg(not(unix))]
    fn sleeper() -> (&'static str, Vec<OsString>) {
        (
            "powershell",
            vec![
                OsString::from("-NoProfile"),
                OsString::from("-Command"),
                OsString::from("Start-Sleep -Seconds 10"),
            ],
        )
    }

    #[cfg(unix)]
    fn stdout_flood() -> (&'static str, Vec<OsString>) {
        (
            "sh",
            vec![
                OsString::from("-c"),
                OsString::from("head -c 2000000 /dev/zero"),
            ],
        )
    }

    #[cfg(not(unix))]
    fn stdout_flood() -> (&'static str, Vec<OsString>) {
        (
            "powershell",
            vec![
                OsString::from("-NoProfile"),
                OsString::from("-Command"),
                OsString::from("Write-Output ('x' * 2000000)"),
            ],
        )
    }

    #[cfg(unix)]
    fn stderr_flood() -> (&'static str, Vec<OsString>) {
        (
            "sh",
            vec![
                OsString::from("-c"),
                OsString::from("head -c 2000000 /dev/zero >&2"),
            ],
        )
    }

    #[cfg(not(unix))]
    fn stderr_flood() -> (&'static str, Vec<OsString>) {
        (
            "powershell",
            vec![
                OsString::from("-NoProfile"),
                OsString::from("-Command"),
                OsString::from("[Console]::Error.Write(('x' * 2000000) -join '')"),
            ],
        )
    }

    #[cfg(unix)]
    fn small_answer() -> (&'static str, Vec<OsString>) {
        (
            "sh",
            vec![
                OsString::from("-c"),
                OsString::from("printf '{\"ok\":true}'"),
            ],
        )
    }

    #[cfg(not(unix))]
    fn small_answer() -> (&'static str, Vec<OsString>) {
        (
            "powershell",
            vec![
                OsString::from("-NoProfile"),
                OsString::from("-Command"),
                OsString::from("Write-Output '{\"ok\":true}'"),
            ],
        )
    }

    /// A hung Cargo must be killed by the deadline, not waited out, and the
    /// refusal must name the deadline rather than decoding a partial graph
    /// (issue #143 R14).
    #[test]
    fn a_hung_subprocess_is_killed_by_the_deadline() -> TestResult {
        let (program, args) = sleeper();
        let timeout = Duration::from_millis(500);
        let started = std::time::Instant::now();
        let refused = run_bounded(program, &args, Path::new("."), timeout, 1024 * 1024);
        let elapsed = started.elapsed();
        match refused {
            Err(MetadataError::Timeout { after }) => assert_eq!(after, timeout),
            Err(other) => return Err(format!("expected a timeout refusal, got {other}").into()),
            Ok(_) => return Err("a hung child must not report success".into()),
        }
        // Overshoot is observed, not assumed: the deadline is checked every
        // 5 ms poll quantum and the kill and reap follow at once, so anything
        // near a second past the deadline is a defect, not scheduling noise.
        let overshoot = elapsed.saturating_sub(timeout);
        assert!(
            overshoot < Duration::from_millis(1500),
            "the deadline must kill the 10s sleeper promptly: {overshoot:?} past a {timeout:?} deadline"
        );
        Ok(())
    }

    /// INV-DEP-8's stated scope, observed with a real descendant: the direct
    /// child exits at once after starting a grandchild that inherits the
    /// capture descriptors. Collection must return without waiting for the
    /// grandchild, and every capture name must be gone, while the grandchild
    /// is still alive holding its descriptor. Disk space for an unlinked file
    /// is reclaimed only when that last descriptor closes, which is exactly
    /// the limit the invariant declares. The grandchild's PID comes from its
    /// own output, so the only process signalled is the one this test made.
    #[cfg(unix)]
    #[test]
    fn a_descendant_holding_a_capture_neither_blocks_collection_nor_keeps_its_name() -> TestResult {
        arm_faults(&[]);
        let started = std::time::Instant::now();
        let output = run_bounded(
            "sh",
            &[OsString::from("-c"), OsString::from("sleep 30 & echo $!")],
            Path::new("."),
            Duration::from_secs(10),
            32 * 1024,
        )?;
        let elapsed = started.elapsed();
        let pid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        assert!(
            !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()),
            "the direct child must report its grandchild's pid: {pid:?}"
        );
        let alive = |pid: &str| {
            Command::new("kill")
                .args(["-0", pid])
                .status()
                .is_ok_and(|status| status.success())
        };
        let grandchild_was_alive = alive(&pid);
        let _signalled = Command::new("kill").args(["-9", &pid]).status();
        assert!(
            elapsed < Duration::from_secs(5),
            "collection must not wait for a descendant holding the capture: {elapsed:?}"
        );
        assert!(
            grandchild_was_alive,
            "the grandchild must still hold its descriptor when collection returns"
        );
        assert_paths_removed(&captured_paths())
    }

    /// A flooding stdout is refused past its budget instead of retained.
    #[test]
    fn a_flooding_stdout_is_refused_past_its_budget() -> TestResult {
        let (program, args) = stdout_flood();
        let refused = run_bounded(
            program,
            &args,
            Path::new("."),
            Duration::from_secs(30),
            32 * 1024,
        );
        match refused {
            Err(MetadataError::OutputTooLarge { stream, limit }) => {
                assert_eq!(stream, "stdout");
                assert_eq!(limit, 32 * 1024);
            }
            Err(other) => {
                return Err(format!("expected an output-budget refusal, got {other}").into());
            }
            Ok(_) => return Err("a 2MB flood must not report success".into()),
        }
        Ok(())
    }

    /// A flooding stderr is refused past its budget instead of retained.
    #[test]
    fn a_flooding_stderr_is_refused_past_its_budget() -> TestResult {
        let (program, args) = stderr_flood();
        let refused = run_bounded(
            program,
            &args,
            Path::new("."),
            Duration::from_secs(30),
            32 * 1024,
        );
        match refused {
            Err(MetadataError::OutputTooLarge { stream, limit }) => {
                assert_eq!(stream, "stderr");
                assert_eq!(limit, 32 * 1024);
            }
            Err(other) => {
                return Err(format!("expected an output-budget refusal, got {other}").into());
            }
            Ok(_) => return Err("a 2MB flood must not report success".into()),
        }
        Ok(())
    }

    /// The control: a small healthy answer passes through unchanged.
    #[test]
    fn a_small_answer_passes_through_unchanged() -> TestResult {
        let (program, args) = small_answer();
        let output = run_bounded(
            program,
            &args,
            Path::new("."),
            Duration::from_secs(30),
            32 * 1024,
        )?;
        assert!(
            output.status.success(),
            "the probe command must succeed: {:?}",
            output.status
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("{\"ok\":true}"),
            "the probe bytes must survive: {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
        Ok(())
    }

    /// A capture name stays owned across a subsequent early return and its
    /// actual temporary file is reclaimed by the RAII fallback.
    #[test]
    fn an_owned_capture_is_removed_on_early_return() -> TestResult {
        use std::fs::OpenOptions;

        let token = lgwks_std::random::bytes::<16>()?;
        let path = std::env::temp_dir().join(format!(
            "lgwks-deps-test-{}.capture",
            lgwks_std::hex::encode(token)
        ));
        let _file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let mut captures = CaptureFiles::new();
        captures.own(path.clone());
        drop(_file);
        drop(captures);
        assert_eq!(
            std::fs::metadata(&path).err().map(|error| error.kind()),
            Some(std::io::ErrorKind::NotFound),
            "the acquired capture pathname must be reclaimed on scope exit"
        );
        Ok(())
    }

    /// Dropping an unresolved obligation kills and reaps its child, so a
    /// refusal the caller only prints does not leave Cargo running.
    #[cfg(unix)]
    #[test]
    fn a_dropped_obligation_kills_and_reaps_its_child() -> TestResult {
        let child = std::process::Command::new("sleep").arg("30").spawn()?;
        let pid = child.id().to_string();
        drop(CleanupObligation::new(
            Some(CleanupStep::Kill),
            Some(child),
            &mut CaptureFiles::new(),
        ));
        // `kill -0` succeeds for any process that still has a table entry,
        // zombies included, so a failure here means killed *and* reaped.
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()?;
        assert!(
            !alive.success(),
            "the dropped obligation's child {pid} is still in the process table"
        );
        Ok(())
    }

    /// An already-exited real child is cleared only after the OS reports its
    /// status; a failed kill is not itself treated as proof of absence.
    #[test]
    fn cleanup_obligation_confirms_an_already_exited_child() -> TestResult {
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .arg("--list")
            .spawn()?;
        let _status = child.wait()?;
        let mut obligation = CleanupObligation::new(
            Some(CleanupStep::Kill),
            Some(child),
            &mut CaptureFiles::new(),
        );
        assert!(
            obligation.retry_cleanup()?,
            "observed process absence and empty capture ownership complete cleanup"
        );
        Ok(())
    }

    /// Every fallible collection stage retains and resolves the resources it
    /// already acquired. The injected errors are single-use and thread-local;
    /// the process and capture files are real operating-system resources.
    #[test]
    fn injected_collection_fault_matrix_keeps_cleanup_owned_and_truthful() -> TestResult {
        let (small_program, small_args) = small_answer();
        let (sleep_program, sleep_args) = sleeper();
        let stages = [
            (FaultPoint::CaptureStdout, false),
            (FaultPoint::CaptureStderr, false),
            (FaultPoint::Spawn, false),
            (FaultPoint::WaitObservation, true),
            (FaultPoint::StatStdout, true),
            (FaultPoint::StatStderr, true),
            (FaultPoint::ReadStdout, false),
            (FaultPoint::ReadStderr, false),
        ];
        for (fault, running_child) in stages {
            arm_faults(&[fault]);
            let (program, args) = if running_child {
                (sleep_program, sleep_args.as_slice())
            } else {
                (small_program, small_args.as_slice())
            };
            let result = run_bounded(
                program,
                args,
                Path::new("."),
                Duration::from_secs(3),
                1024 * 1024,
            );
            let error = match result {
                Err(error) => error,
                Ok(_) => return Err(format!("fault {fault:?} unexpectedly passed").into()),
            };
            assert!(
                matches!(error, MetadataError::Spawn(_)),
                "fault {fault:?} should remain the primary collection error: {error}"
            );
            let paths = captured_paths();
            assert_paths_removed(&paths)?;
            assert!(
                FAULTS.with(|faults| faults.borrow().is_empty()),
                "fault {fault:?} was not reached"
            );
        }

        for failure in [FaultPoint::Kill, FaultPoint::Reap] {
            arm_faults(&[FaultPoint::WaitObservation, failure]);
            let error = match run_bounded(
                sleep_program,
                &sleep_args,
                Path::new("."),
                Duration::from_secs(3),
                1024 * 1024,
            ) {
                Err(error) => error,
                Ok(_) => return Err(format!("fault {failure:?} unexpectedly passed").into()),
            };
            let text = error.to_string();
            let MetadataError::ProcessCleanup {
                process,
                mut obligation,
                ..
            } = error
            else {
                return Err(format!("fault {failure:?} did not expose an obligation").into());
            };
            assert!(process.is_some(), "fault {failure:?} must remain visible");
            let verb = if failure == FaultPoint::Kill {
                "kill"
            } else {
                "reap"
            };
            let pid = obligation
                .report
                .pid
                .ok_or("an owned child keeps its pid in the report")?;
            assert!(
                text.contains(&format!("could not {verb} cargo (pid {pid}): ")),
                "the refusal names the step, the pid and the OS error: {text}"
            );
            assert!(
                retry_until_resolved(&mut obligation)?,
                "retry should finish owned cleanup"
            );
            assert_paths_removed(&captured_paths())?;
            assert!(FAULTS.with(|faults| faults.borrow().is_empty()));
        }

        // Output read in full and child reaped, then one unlink fails: the
        // output is returned, and the failure is reported beside it (#193).
        arm_faults(&[FaultPoint::Unlink]);
        let output = run_bounded(
            small_program,
            &small_args,
            Path::new("."),
            Duration::from_secs(3),
            1024 * 1024,
        )
        .map_err(|error| format!("a complete collection was refused over cleanup: {error}"))?;
        assert!(output.status.success(), "the child's own verdict is kept");
        assert!(!output.stdout.is_empty(), "the complete output is kept");
        let unresolved = output
            .unresolved
            .ok_or("the unlink failure must be reported beside the output")?;
        let error = unresolved.into_error(None);
        let text = error.to_string();
        let MetadataError::ProcessCleanup {
            cause,
            captures,
            mut obligation,
            ..
        } = error
        else {
            return Err("unlink fault did not expose an unresolved cleanup owner".into());
        };
        assert!(cause.is_none(), "nothing was refused before the cleanup");
        assert_eq!(captures.len(), 1, "the unlink failure is retained");
        let retained = obligation.captures.paths.clone();
        assert_eq!(retained.len(), 1, "the unresolved path stays owned");
        assert!(
            text.contains(&format!("could not remove {}: ", retained[0].display())),
            "the report names the path it could not remove: {text}"
        );
        assert!(std::fs::metadata(&retained[0]).is_ok());
        assert!(retry_until_resolved(&mut obligation)?);
        assert_paths_removed(&retained)?;
        assert!(FAULTS.with(|faults| faults.borrow().is_empty()));
        Ok(())
    }

    /// A refusal decided after a complete read (Cargo's non-zero status, a
    /// JSON or schema error) keeps an unresolved capture cleanup attached, so
    /// neither the refusal nor the leftover path is lost.
    #[test]
    fn a_refusal_after_collection_keeps_the_unresolved_cleanup_attached() -> TestResult {
        let path = std::env::temp_dir().join(format!(
            "lgwks-deps-attach-{}.capture",
            lgwks_std::hex::encode(lgwks_std::random::bytes::<8>()?)
        ));
        std::fs::write(&path, b"{}")?;
        let mut captures = CaptureFiles::new();
        captures.own(path.clone());
        let collected = Collected {
            value: (),
            unresolved: Some(Unresolved {
                captures: vec![std::io::Error::other("injected unlink failure")],
                obligation: Box::new(CleanupObligation::new(None, None, &mut captures)),
            }),
        };
        let refusal = collected.refuse(MetadataError::Cargo("refused".to_owned()));
        let text = refusal.to_string();
        let MetadataError::ProcessCleanup {
            cause,
            mut obligation,
            ..
        } = refusal
        else {
            return Err("the cleanup failure was dropped by the refusal".into());
        };
        assert!(
            cause.as_deref().is_some_and(
                |error| matches!(*error, MetadataError::Cargo(ref message) if message == "refused")
            ),
            "the refusal is the primary cause"
        );
        assert!(
            text.starts_with(
                "cargo metadata refused: refused; cleanup unconfirmed; could not remove "
            ),
            "both are reported, refusal first: {text}"
        );
        assert!(
            obligation.retry_cleanup()?,
            "the retained owner still removes the path"
        );
        assert_paths_removed(&[path])?;
        Ok(())
    }

    /// A declaring member with no manifest directory has nothing to resolve a
    /// path edge against. Before #158 the edge was classified external; now
    /// the member itself is an identity defect, so the whole document is
    /// refused rather than any edge being classified from it.
    #[test]
    fn a_path_edge_whose_declarer_has_no_manifest_is_refused() {
        let input = r#"{
          "packages": [
            {"id":"path+file:///repo#app@0.1.0","name":"app","repository":null,
             "manifest_path":null,
             "dependencies":[
               {"name":"helper","source":null,"req":"*","kind":null,"optional":false,"path":"helper"}
             ]},
            {"id":"path+file:///repo/helper#helper@0.1.0","name":"helper",
             "repository":"https://example.invalid/helper",
             "manifest_path":"/repo/helper/Cargo.toml","dependencies":[]}
          ],
          "workspace_members": ["path+file:///repo#app@0.1.0","path+file:///repo/helper#helper@0.1.0"]
        }"#;
        assert!(
            matches!(
                parse(input),
                Err(MetadataError::Schema(message)) if message.contains("has no manifest_path")
            ),
            "a member without a manifest must be refused, not classified"
        );
    }
}
