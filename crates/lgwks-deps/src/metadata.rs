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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DependencyKind {
    /// A runtime or library dependency.
    Normal,
    /// A build-script dependency.
    Build,
    /// A test, example, or benchmark dependency.
    Dev,
}

impl DependencyKind {
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub const fn class(&self) -> &'static str {
        match self {
            Self::Registry(_) => "registry",
            Self::Git(_) => "git",
            Self::Path(_) => "path",
            Self::Other(_) => "other",
        }
    }

    /// Exact Cargo source or path, retained for diagnostics.
    pub fn detail(&self) -> &str {
        match self {
            Self::Registry(value) | Self::Git(value) | Self::Path(value) | Self::Other(value) => {
                value
            }
        }
    }
}

/// One dependency declaration authored by a workspace package.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug)]
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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "cannot run cargo metadata: {error}"),
            Self::Cargo(error) => write!(f, "cargo metadata refused: {error}"),
            Self::Json(error) => write!(f, "cargo metadata JSON: {error}"),
            Self::Schema(error) => write!(f, "cargo metadata schema: {error}"),
        }
    }
}

impl std::error::Error for MetadataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Cargo(_) | Self::Schema(_) => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: Vec<String>,
}

#[derive(Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct CargoPackage {
    id: String,
    name: String,
    repository: Option<String>,
    dependencies: Vec<CargoDependency>,
}

#[derive(Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct CargoDependency {
    name: String,
    source: Option<String>,
    req: String,
    kind: Option<String>,
    optional: bool,
    path: Option<String>,
}

fn source(dependency: &CargoDependency) -> Result<DependencySource, MetadataError> {
    match (&dependency.source, &dependency.path) {
        (Some(value), _) if value.starts_with("registry+") => {
            Ok(DependencySource::Registry(value.clone()))
        }
        (Some(value), _) if value.starts_with("git+") => Ok(DependencySource::Git(value.clone())),
        (Some(value), _) => Ok(DependencySource::Other(value.clone())),
        (None, Some(path)) => Ok(DependencySource::Path(path.clone())),
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
                    .map(ToString::to_string),
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
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    parse(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_inactive_optional_and_dev_edges() {
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
        let edges = parse(input).unwrap();
        assert_eq!(edges.len(), 2);
        let serde = edges.iter().find(|edge| edge.package == "serde").unwrap();
        let proptest = edges
            .iter()
            .find(|edge| edge.package == "proptest")
            .unwrap();
        assert!(serde.optional);
        assert!(!serde.workspace);
        assert_eq!(proptest.kind, DependencyKind::Dev);
    }

    #[test]
    fn ignores_non_workspace_transitives() {
        let input = r#"{
          "packages": [
            {"id":"path+file:///repo#app@0.1.0","name":"app","repository":null,"dependencies":[]},
            {"id":"registry+x#serde@1.0.0","name":"serde","repository":null,"dependencies":[]}
          ],
          "workspace_members": ["path+file:///repo#app@0.1.0"]
        }"#;
        assert!(parse(input).unwrap().is_empty());
    }
}
