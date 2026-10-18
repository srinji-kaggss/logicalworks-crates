//! Collecting the graph: the locked `cargo metadata` reads, the `Collected` result and the workspace members.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::capture::{METADATA_STREAM_CAP, METADATA_TIMEOUT, Unresolved, run_bounded};
use super::edges::{
    CargoMetadata, CargoPackage, direct_edges, license_targets, validated_workspace,
};
use super::{DirectEdge, MetadataError};

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
        &metadata_args(manifest.into_os_string(), Resolve::DeclaredOnly),
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
        let refusal = Err(collected.refuse(refusal));
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "read_metadata: returning an error to the caller");
        return refusal;
    }
    collected.try_map(|stdout| lgwks_std::json::from_slice(&stdout).map_err(MetadataError::Json))
}

/// A complete collection, and any capture cleanup that could not be confirmed
/// after it.
///
/// Cargo's output was read in full and its process reaped, so the value is the
/// whole answer. A capture file that could not then be removed is reported
/// beside it as [`MetadataError::ProcessCleanup`], whose obligation still owns
/// the path for a retry, rather than turning a valid graph into a refusal.
/// [`Collected::into_parts`] is the only way to the value, so a caller takes
/// the cleanup in the same move and cannot borrow past it.
///
/// The integration test `downstream_can_read_metadata_edges_and_workspace_members` takes the
/// value and the cleanup together through `into_parts`, the way a downstream consumer does.
#[derive(Debug)]
#[must_use = "a collection may carry a cleanup failure that must be reported"]
pub struct Collected<T> {
    /// The complete result.
    pub(super) value: T,
    /// Capture removals that failed after the result was complete.
    pub(super) unresolved: Option<Unresolved>,
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
    pub(super) fn refuse(self, refusal: MetadataError) -> MetadataError {
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
pub(super) fn manifest_path(root: &Path) -> Result<std::path::PathBuf, MetadataError> {
    let manifest = root.join("Cargo.toml");
    std::path::absolute(&manifest).map_err(|cause| MetadataError::Root {
        path: manifest,
        cause,
    })
}

/// Runs locked Cargo metadata and returns every direct workspace edge, with
/// any capture cleanup that could not be confirmed after a complete read.
pub fn read(root: &Path) -> Result<Collected<Vec<DirectEdge>>, MetadataError> {
    Ok(read_with_members(root)?.map(|(edges, _)| edges))
}

/// Direct edges and the package name of every workspace member, from one run.
pub type EdgesAndMembers = (Vec<DirectEdge>, Vec<String>);

/// [`read`], and the package name of every workspace member from the same
/// Cargo run, so the member list and the edges cannot describe two states.
pub fn read_with_members(root: &Path) -> Result<Collected<EdgesAndMembers>, MetadataError> {
    read_metadata(root)?.try_map(|metadata| {
        let members = located_members(&metadata)?
            .into_iter()
            .map(|member| member.name)
            .collect();
        let mut edges = direct_edges(metadata)?;
        // A second read, this time with dependencies, because the first cannot
        // see them. `--no-deps` is what makes an inactive optional edge
        // visible, and it is also what hides every third-party package: the
        // licence audit needs exactly the packages the edge audit does not
        // care about. A failed second read leaves the licences absent, and the
        // audit reports that as a refusal naming the crate -- an edge whose
        // terms are unknown must not read as an edge whose terms are absent.
        if let Some(resolved) = read_resolved_packages(root) {
            let declared = license_targets(&resolved);
            for edge in &mut edges {
                if let Some(licence) = declared.get(edge.package.as_str()).copied() {
                    edge.license = licence.license.map(str::to_owned);
                    edge.license_file = licence.license_file.map(str::to_owned);
                }
            }
        }
        Ok((edges, members))
    })
}

/// Packages Cargo resolved, dependencies included, or `None` when that read
/// could not be completed.
///
/// Failing here is not itself a refusal. The edge audit has already decided
/// what it was going to decide from the first read, and the licence audit says
/// in its own words which crate it could not price. Swallowing the cause keeps
/// one unreadable query from denying the graph that was read successfully --
/// and the licences stay absent, which the audit reports rather than hides.
fn read_resolved_packages(root: &Path) -> Option<Vec<CargoPackage>> {
    let manifest = manifest_path(root).ok()?;
    // Every feature: an inactive optional edge is absent from the default
    // resolve, and an absent package has no licence to audit.
    let output = run_bounded(
        "cargo",
        &metadata_args(manifest.into_os_string(), Resolve::EveryFeature),
        root,
        METADATA_TIMEOUT,
        METADATA_STREAM_CAP,
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let metadata: CargoMetadata = lgwks_std::json::from_slice(&output.stdout).ok()?;
    Some(metadata.packages)
}

/// Which edges a `cargo metadata` read resolves.
#[derive(Clone, Copy)]
enum Resolve {
    /// Only the edges a workspace member declares.
    DeclaredOnly,
    /// Every edge, including the optional ones no default build activates.
    EveryFeature,
}

/// The `cargo metadata` argument list this gate reads.
///
/// Both reads are one command against one manifest; only the feature selection
/// differs. Written once because a third read would otherwise be a third copy,
/// and a copy is where a flag goes missing.
fn metadata_args(manifest: OsString, resolve: Resolve) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("metadata"),
        OsString::from("--locked"),
        OsString::from("--format-version"),
        OsString::from("1"),
        OsString::from("--manifest-path"),
        manifest,
    ];
    args.push(match resolve {
        Resolve::DeclaredOnly => OsString::from("--no-deps"),
        Resolve::EveryFeature => OsString::from("--all-features"),
    });
    args
}

/// One workspace member, with the directory holding its manifest.
///
/// Scope resolution needs the directory: a scope of `lgwks_bot::verb` is only
/// real if `verb` is a module of *that* package, and the package name alone
/// does not say where its sources are.
///
/// `name()` reads the package name; the same integration test reads it from a downstream crate.
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
