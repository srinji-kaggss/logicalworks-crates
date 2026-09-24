//! Cargo metadata ingestion for direct dependency-edge admission.
//!
//! The lock file answers which bytes happened to resolve. It does not answer
//! which workspace package authored an edge. `cargo metadata --no-deps` does,
//! including inactive optional dependencies, so this module is the source of
//! truth for INV-DEP-EDGE-OWNED.

use std::fmt;
use std::path::{Path, PathBuf};
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
/// only for an edge with no `source` key whose manifest-relative `path`
/// resolves into a workspace member's manifest directory, and `source`
/// separates a registry package from a path copy that happens to share its
/// name. A path edge Cargo cannot locate — no `path` key, no declaring
/// manifest directory, or an escape past the root — is external, never
/// internal by name.
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
    let mut joined = if key.is_absolute() {
        PathBuf::new()
    } else {
        base.to_path_buf()
    };
    for component in key.components() {
        match component {
            Component::Prefix(_) | Component::RootDir if key.is_absolute() => {
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
    let members: std::collections::BTreeSet<&str> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();
    let member_packages: Vec<&CargoPackage> = metadata
        .packages
        .iter()
        .filter(|package| members.contains(package.id.as_str()))
        .collect();
    let member_dirs: std::collections::BTreeMap<PathBuf, (&str, Option<&str>)> = member_packages
        .iter()
        .filter_map(|package| {
            let dir = Path::new(package.manifest_path.as_deref()?)
                .parent()?
                .to_path_buf();
            Some((dir, (package.name.as_str(), package.repository.as_deref())))
        })
        .collect();
    let declaring_dirs: std::collections::BTreeMap<&str, PathBuf> = member_packages
        .iter()
        .filter_map(|package| {
            let dir = Path::new(package.manifest_path.as_deref()?)
                .parent()?
                .to_path_buf();
            Some((package.id.as_str(), dir))
        })
        .collect();
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

/// Runs locked Cargo metadata and decodes the supported response shape.
///
/// The manifest path is made absolute before Cargo is started. `--manifest-path`
/// is resolved by Cargo against *its* working directory, which this call sets to
/// `root`, so a relative `root` would otherwise be joined to itself — a gate
/// invoked as `check crates/thing` would report a manifest that plainly exists
/// as missing, and every fixture test built on a relative path would pass on
/// that refusal rather than on the rule it meant to exercise.
fn read_metadata(root: &Path) -> Result<CargoMetadata, MetadataError> {
    let manifest = manifest_path(root)?;
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(manifest)
        .current_dir(root)
        .output()
        .map_err(MetadataError::Spawn)?;
    if !output.status.success() {
        return Err(MetadataError::Cargo(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    lgwks_std::json::from_slice(&output.stdout).map_err(MetadataError::Json)
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

/// Runs locked Cargo metadata and returns every direct workspace edge.
pub fn read(root: &Path) -> Result<Vec<DirectEdge>, MetadataError> {
    direct_edges(read_metadata(root)?)
}

/// One workspace member, with the directory holding its manifest.
///
/// Scope resolution needs the directory: a scope of `lgwks_bot::verb` is only
/// real if `verb` is a module of *that* package, and the package name alone
/// does not say where its sources are.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Member {
    /// Package name as the manifest declares it.
    pub name: String,
    /// Directory holding the member's `Cargo.toml`, as Cargo reported it.
    pub manifest_dir: PathBuf,
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
pub fn workspace_members(root: &Path) -> Result<Vec<Member>, MetadataError> {
    let metadata = read_metadata(root)?;
    let members: std::collections::BTreeSet<&str> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();
    let mut located = Vec::new();
    for package in metadata
        .packages
        .iter()
        .filter(|package| members.contains(package.id.as_str()))
    {
        let manifest_path = package.manifest_path.as_deref().ok_or_else(|| {
            MetadataError::Schema(format!(
                "workspace member {:?} has no manifest_path",
                package.name
            ))
        })?;
        let manifest_dir = Path::new(manifest_path)
            .parent()
            .ok_or_else(|| {
                MetadataError::Schema(format!(
                    "workspace member {:?} manifest_path {manifest_path:?} has no directory",
                    package.name
                ))
            })?
            .to_path_buf();
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

    /// Test bodies propagate with `?` rather than panicking: `unwrap` is
    /// forbidden workspace-wide, and a failing edge extraction should surface
    /// as the error it is, not as a panic with no variant attached.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

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

    /// Fail closed when the declaring package has no manifest directory to
    /// resolve against: a path edge Cargo cannot locate is external, never
    /// internal by name.
    #[test]
    fn a_path_edge_without_a_locatable_declarer_is_external() -> TestResult {
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
        let edges = parse(input)?;
        assert_eq!(edges.len(), 1);
        assert!(
            !edges[0].workspace,
            "an unresolvable path target must fail closed to external"
        );
        assert_eq!(edges[0].target_repository, None);
        Ok(())
    }
}
