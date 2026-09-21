//! Cargo metadata ingestion for direct dependency-edge admission.
//!
//! The lock file answers which bytes happened to resolve. It does not answer
//! which workspace package authored an edge. `cargo metadata --no-deps` does,
//! including inactive optional dependencies, so this module is the source of
//! truth for INV-DEP-EDGE-OWNED.

use std::fmt;
use std::path::Path;
use std::process::Command;

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
/// only for an edge with no `source` key whose target name is a workspace
/// member, and `source` separates a registry package from a path copy that
/// happens to share its name.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DirectEdge {
    /// Workspace package declaring the dependency.
    pub consumer: String,
    /// Upstream package name, independent of a local rename.
    pub package: String,
    /// Manifest semver requirement exactly as Cargo reports it.
    pub requirement: String,
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
    /// Cargo returned a non-zero status.
    Cargo(String),
    /// Cargo returned JSON outside the supported format-1 subset.
    Json(lgwks_std::json::Error),
    /// Cargo returned an internally inconsistent field.
    Schema(String),
}

impl fmt::Display for MetadataError {
    /// Names the stage that failed so a reader can tell an environment problem
    /// (`cargo` absent) from a contract problem (a source with the wrong
    /// schema) without re-running the gate.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Spawn(ref error) => write!(f, "cannot run cargo metadata: {error}"),
            Self::Cargo(ref error) => write!(f, "cargo metadata refused: {error}"),
            Self::Json(ref error) => write!(f, "cargo metadata JSON: {error}"),
            Self::Schema(ref error) => write!(f, "cargo metadata schema: {error}"),
        }
    }
}

impl std::error::Error for MetadataError {
    /// Chains the two variants that wrap a cause; `Cargo` and `Schema` carry
    /// Cargo's own prose as a `String` and have nothing further to chain.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Spawn(ref error) => Some(error),
            Self::Json(ref error) => Some(error),
            Self::Cargo(_) | Self::Schema(_) => None,
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
    /// Manifest path for a filesystem dependency, relative to the declaring
    /// package. Present only when `source` is absent.
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
    let members: std::collections::BTreeSet<&str> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();
    let member_names: std::collections::BTreeSet<&str> = metadata
        .packages
        .iter()
        .filter(|package| members.contains(package.id.as_str()))
        .map(|package| package.name.as_str())
        .collect();
    let member_repositories: std::collections::BTreeMap<&str, Option<&str>> = metadata
        .packages
        .iter()
        .filter(|package| members.contains(package.id.as_str()))
        .map(|package| (package.name.as_str(), package.repository.as_deref()))
        .collect();
    let mut edges = Vec::new();
    for package in metadata
        .packages
        .iter()
        .filter(|package| members.contains(package.id.as_str()))
    {
        for dependency in &package.dependencies {
            edges.push(DirectEdge {
                consumer: package.name.clone(),
                package: dependency.name.clone(),
                requirement: dependency.req.clone(),
                kind: DependencyKind::from_cargo(dependency.kind.as_deref())?,
                source: source(dependency)?,
                optional: dependency.optional,
                workspace: dependency.source.is_none()
                    && member_names.contains(dependency.name.as_str()),
                target_repository: member_repositories
                    .get(dependency.name.as_str())
                    .and_then(|repository| *repository)
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

/// Runs locked Cargo metadata and returns every direct workspace edge.
pub fn read(root: &Path) -> Result<Vec<DirectEdge>, MetadataError> {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(root.join("Cargo.toml"))
        .current_dir(root)
        .output()
        .map_err(MetadataError::Spawn)?;
    if !output.status.success() {
        return Err(MetadataError::Cargo(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    parse(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test bodies propagate with `?` rather than panicking: `unwrap` is
    /// forbidden workspace-wide, and a failing edge extraction should surface
    /// as the error it is, not as a panic with no variant attached.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn includes_inactive_optional_and_dev_edges() -> TestResult {
        let input = r#"{
          "packages": [{
            "id": "path+file:///repo#app@0.1.0",
            "name": "app",
            "repository": "https://example.invalid/app",
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
            {"id":"path+file:///repo#app@0.1.0","name":"app","repository":null,"dependencies":[]},
            {"id":"registry+x#serde@1.0.0","name":"serde","repository":null,"dependencies":[]}
          ],
          "workspace_members": ["path+file:///repo#app@0.1.0"]
        }"#;
        assert!(parse(input)?.is_empty());
        Ok(())
    }
}
