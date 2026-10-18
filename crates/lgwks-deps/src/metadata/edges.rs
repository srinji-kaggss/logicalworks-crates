//! Decoding Cargo's metadata JSON into direct workspace edges: classification, membership and licences.

use std::path::{Path, PathBuf};

use lgwks_std::json::Deserialize;

use super::{DependencyKind, DependencySource, DirectEdge, MetadataError};

/// The subset of a `cargo metadata --format-version 1` response this gate reads.
///
/// Only the keys the edge extraction needs are declared; the response carries
/// far more (targets, features, resolve) and those are not modelled here.
#[derive(Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
pub(super) struct CargoMetadata {
    /// Every package in the response, including the transitive dependencies of
    /// non-workspace packages, which `parse` filters out by membership.
    pub(super) packages: Vec<CargoPackage>,
    /// Package ids of the workspace members, in `path+file:///…#name@version`
    /// form. Membership is decided by this list rather than by path prefix, so
    /// a path dependency living inside the repository is not mistaken for a
    /// member and does not get its edges counted twice.
    workspace_members: Vec<String>,
}

/// One package entry in a metadata response.
#[derive(Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
pub(super) struct CargoPackage {
    /// Opaque package id; the only key matched against `workspace_members`.
    pub(super) id: String,
    /// Package name as published. This is the name the register approves.
    pub(super) name: String,
    /// Declared repository URL, when the manifest has one. Read only for
    /// workspace members, where it proves the member is ours.
    repository: Option<String>,
    /// Absolute path to the package's `Cargo.toml`. Read for workspace members
    /// so a scope can be resolved to a real module file rather than to a
    /// directory guessed from the package name.
    manifest_path: Option<String>,
    /// SPDX expression the manifest's `[package] license` declares.
    ///
    /// The register records one of these per approval (#208). Cargo reports the
    /// string verbatim, including a compound `OR` expression and its ordering,
    /// so the audit compares expressions rather than trying to resolve them: a
    /// licence this gate cannot parse is refused, not guessed at.
    license: Option<String>,
    /// Path the manifest's `[package] license-file` names, when it names one.
    license_file: Option<String>,
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
    /// Features Cargo reports enabled on this declaration. Absent in
    /// hand-written fixtures and older schema revisions, so it defaults empty;
    /// real Cargo always emits it.
    #[serde(default)]
    features: Vec<String>,
    /// Whether the declaration enables default features. Cargo always emits
    /// this; a fixture that omits it is read as the Cargo default, `true`.
    #[serde(default = "default_true")]
    uses_default_features: bool,
    /// Target `cfg(…)` for a target-specific declaration.
    #[serde(default)]
    target: Option<String>,
    /// Local alias when the manifest renamed the dependency with `package =`.
    #[serde(default)]
    rename: Option<String>,
}

/// The Cargo default for `uses_default_features` when a fixture omits the key.
const fn default_true() -> bool {
    true
}

/// Classifies one authored edge by the origin Cargo recorded.
///
/// A `source` key takes precedence over `path`, matching Cargo's own reading:
/// a registry package reached through a path override reports both, and the
/// registry spelling is what the register approves. `registry+`, `sparse+` and
/// `git+` are the schemes the gate names — the two registry spellings differ
/// only in transport and name one registry — while any other scheme is carried
/// verbatim as
/// [`DependencySource::Other`] so an unknown origin is reported rather than
/// dropped. An edge with neither key cannot be placed at all and is refused.
fn source(dependency: &CargoDependency) -> Result<DependencySource, MetadataError> {
    match (dependency.source.as_deref(), dependency.path.as_deref()) {
        (Some(value), _) if value.starts_with("registry+") => {
            Ok(DependencySource::Registry(value.to_owned()))
        }
        (Some(value), _) if value.starts_with("sparse+") => {
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
pub(super) fn lexical_join(base: &Path, relative: &str) -> Option<PathBuf> {
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
///
/// The declared licence travels with the edge (#208). It is read from the
/// package record rather than the dependency declaration because Cargo
/// publishes a package's `license` on the *resolved* package: a path
/// dependency that lives outside the resolved graph, a feature-gated edge, and
/// an ordinary registry edge all resolve to the same record. A name two
/// packages share therefore cannot borrow another package's licence by
/// renaming, and an approval is compared against the licence of the package
/// Cargo actually resolved.
pub(super) fn direct_edges(metadata: CargoMetadata) -> Result<Vec<DirectEdge>, MetadataError> {
    let workspace = validated_workspace(&metadata)?;
    let member_packages = workspace.packages;
    let member_dirs = workspace.member_dirs;
    let declaring_dirs = workspace.declaring_dirs;
    let licence_targets = license_targets(&metadata.packages);
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
                let refusal = Err(MetadataError::Schema(format!(
                    "dependency {:?} resolves to workspace package {:?}",
                    dependency.name, member_name
                )));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "direct_edges: returning an error to the caller");
                return refusal;
            }
            // A dependency this `--no-deps` read did not resolve has no declared
            // licence to read. The fields stay `None` rather than receiving an
            // empty record, because "Cargo declares no licence" and "this read
            // never saw the package" are different facts and only the licence
            // audit can tell them apart — by refusing an edge whose terms are
            // unknown. Writing the record unconditionally made the two read as
            // one.
            let declared = licence_targets.get(dependency.name.as_str()).copied();
            let (license, license_file) = match declared {
                Some(declared) => (declared.license, declared.license_file),
                None => (None, None),
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
                features: dependency.features.clone(),
                uses_default_features: dependency.uses_default_features,
                target: dependency.target.clone(),
                rename: dependency.rename.clone(),
                license: license.map(str::to_owned),
                license_file: license_file.map(str::to_owned),
            });
        }
    }
    // A package may declare the same upstream crate several times (once per
    // target, once renamed, once optional); every dimension the register can
    // distinguish joins the sort so the emitted order is a total order and two
    // runs over one document agree.
    edges.sort_by(|left, right| {
        (
            &left.consumer,
            &left.package,
            left.kind,
            &left.target,
            &left.rename,
            &left.features,
        )
            .cmp(&(
                &right.consumer,
                &right.package,
                right.kind,
                &right.target,
                &right.rename,
                &right.features,
            ))
    });
    Ok(edges)
}

/// The licence a package's own manifest declares.
#[derive(Clone, Copy)]
pub(super) struct DeclaredLicense<'a> {
    /// SPDX expression Cargo reported, when the manifest has a `license` key.
    pub(super) license: Option<&'a str>,
    /// Path Cargo reported for `license-file`, when the manifest has one.
    pub(super) license_file: Option<&'a str>,
}

/// Indexes declared licences by package name.
///
/// A name is bound to one licence, or to none: Cargo's own resolution can
/// produce two versions of the same package name, and taking whichever record
/// sorted last would make an edge's licence depend on enumeration order. The
/// registry is keyed by name because that is the identity the register
/// approves, and a name two *different* packages share is already refused as a
/// schema error by [`direct_edges`]'s membership check for workspace targets.
/// A name that no resolved package carries is absent here, and
/// [`DirectEdge::license`] answers `None` — an absence, not a licence.
pub(super) fn license_targets(
    packages: &[CargoPackage],
) -> std::collections::BTreeMap<&str, DeclaredLicense<'_>> {
    let mut targets = std::collections::BTreeMap::new();
    for package in packages {
        targets
            .entry(package.name.as_str())
            .or_insert_with(|| DeclaredLicense {
                license: package.license.as_deref(),
                license_file: package.license_file.as_deref(),
            });
    }
    targets
}

/// Validated view shared by edge extraction and workspace inventory.
pub(super) struct ValidatedWorkspace<'a> {
    /// Workspace packages resolved uniquely from the member-id list.
    pub(super) packages: Vec<&'a CargoPackage>,
    /// Unique manifest directories mapped to package identity and repository.
    member_dirs: std::collections::BTreeMap<PathBuf, (&'a str, Option<&'a str>)>,
    /// Manifest directories keyed by each validated Cargo package id.
    pub(super) declaring_dirs: std::collections::BTreeMap<&'a str, PathBuf>,
}

/// Validates identity completeness before classifying any package or edge.
pub(super) fn validated_workspace(
    metadata: &CargoMetadata,
) -> Result<ValidatedWorkspace<'_>, MetadataError> {
    let mut package_ids = std::collections::BTreeSet::new();
    for package in &metadata.packages {
        if package.id.trim().is_empty() || package.name.trim().is_empty() {
            let refusal = Err(MetadataError::Schema(
                "Cargo package identity contains a blank id or name".to_owned(),
            ));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validated_workspace: returning an error to the caller");
            return refusal;
        }
        if !package_ids.insert(package.id.as_str()) {
            let refusal = Err(MetadataError::Schema(format!(
                "duplicate Cargo package id {:?}",
                package.id
            )));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validated_workspace: returning an error to the caller");
            return refusal;
        }
    }
    let mut member_ids = std::collections::BTreeSet::new();
    for id in &metadata.workspace_members {
        if !member_ids.insert(id.as_str()) {
            let refusal = Err(MetadataError::Schema(format!(
                "duplicate Cargo workspace member id {id:?}"
            )));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validated_workspace: returning an error to the caller");
            return refusal;
        }
        if !package_ids.contains(id.as_str()) {
            let refusal = Err(MetadataError::Schema(format!(
                "workspace member id {id:?} has no package record"
            )));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validated_workspace: returning an error to the caller");
            return refusal;
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
            let refusal = Err(MetadataError::Schema(format!(
                "multiple workspace packages claim manifest directory {:?}",
                dir
            )));
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "validated_workspace: returning an error to the caller");
            return refusal;
        }
        declaring_dirs.insert(package.id.as_str(), dir);
    }
    Ok(ValidatedWorkspace {
        packages,
        member_dirs,
        declaring_dirs,
    })
}
