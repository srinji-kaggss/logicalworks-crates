//! What the gate refuses ([`Refusal`]) and why it could not reach a verdict
//! ([`GateError`]). Both are re-exported at the crate root.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use crate::{CONTRACT_PATH, contract, invariants, lock, metadata};

/// One dependency the register does not admit.
///
/// `#[non_exhaustive]`: the set of drift classes grows as the gate learns to
/// name them, so a consumer must carry a catch-all arm instead of pinning the
/// list. Matching a variant you know, and constructing one, are unaffected.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// A path target claims a repository outside this workspace authority.
    ForeignWorkspaceMember {
        /// Workspace package declaring the path edge.
        consumer: String,
        /// Copied or embedded package name.
        krate: String,
        /// Repository declared by the target package.
        declared_repository: String,
        /// Repository admitted by this contract.
        expected_repository: String,
    },
    /// A workspace package authored an external edge with no semantic owner.
    UnregisteredEdge {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Cargo manifest requirement.
        requirement: String,
        /// Registry, Git, path, or other.
        source: String,
        /// Normal, build, or dev.
        kind: String,
    },
    /// The package is registered, but not for this workspace consumer.
    ConsumerNotAllowed {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
    },
    /// The package is registered, but not at this manifest requirement.
    RequirementDrift {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Approved manifest requirement.
        approved: String,
        /// Authored manifest requirement.
        declared: String,
    },
    /// The package is registered, but the edge changed origin class.
    SourceDrift {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Approved source class.
        approved: String,
        /// Authored source class.
        declared: String,
    },
    /// The package's source class is approved, but the admitted origin differs:
    /// another registry, another Git repository or revision policy, or another
    /// external path. A class approval is not an origin approval.
    OriginDrift {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Approved origin identity.
        approved: String,
        /// Authored origin identity.
        declared: String,
    },
    /// The package is registered, but not for this dependency kind.
    KindNotAllowed {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Authored dependency kind.
        kind: String,
    },
    /// The package's feature set is outside the admitted policy: a feature the
    /// edge enables is not allowed, or a required feature is absent.
    FeatureDrift {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Admitted feature policy, as authored.
        approved: String,
        /// Features the edge actually enables.
        declared: String,
    },
    /// The package's authored `default-features` bit differs from the policy.
    DefaultFeaturesDrift {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Admitted value of `default-features`, which the entry authored: this
        /// refusal only exists for an authored dimension.
        approved: Option<bool>,
        /// Authored value of `default-features`.
        declared: bool,
    },
    /// The package's authored optionality differs from the policy.
    OptionalityDrift {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Admitted optionality, which the entry authored: this refusal only
        /// exists for an authored dimension.
        approved: Option<bool>,
        /// Authored optionality.
        declared: bool,
    },
    /// The package's target scope differs from the admitted policy.
    TargetDrift {
        /// Package declaring the edge.
        consumer: String,
        /// External package name.
        krate: String,
        /// Admitted target scope, or `<none>` for an unconditional edge.
        approved: String,
        /// Authored target scope, or `<none>`.
        declared: String,
    },
    /// A semantic approval has no authored edge and is stale authority.
    UnusedApproval {
        /// Approved external package.
        krate: String,
        /// Crate responsible for the capability.
        owner: String,
        /// Capability the approval claims to supply.
        capability: String,
    },
    /// A frozen surface's external edge was re-tiered out of the tier the
    /// register freezes it at, which is how this repository refuses `lgwks_ast`
    /// the growth INV-DEP-1 forbids.
    ///
    /// The register names both halves: `[policy] frozen_surfaces` (here,
    /// `lgwks_ast`) and `frozen_tier` (here, `boundary`). A growth attempt that
    /// promotes the new edge out of that tier — or demotes it out of the
    /// register entirely, which this crate separately refuses as
    /// [`Refusal::UnregisteredEdge`] — is how the freeze is expressed. No name
    /// is compiled in: the freeze covers exactly the surfaces a register
    /// declares and the edges it says they own.
    FrozenSurfaceTier {
        /// Frozen surface package that owns the edge.
        consumer: String,
        /// External package name whose approval was re-tiered.
        krate: String,
        /// Tier the approval now claims.
        tier: String,
    },
    /// The register approves external edges the tree really has but declares
    /// no `[policy] accepted_licenses`, so there is no licence policy to judge
    /// them by.
    ///
    /// The gate is a published crate that audits other repositories; a set
    /// compiled into it would be one repository's choice imposed on every
    /// other. One refusal for the whole register, because the fix is one line
    /// in it.
    LicensePolicyUndeclared {
        /// Approvals with an observed edge that went unjudged.
        approvals: usize,
    },
    /// The licence the package's own manifest declares is not the licence the
    /// register approved, or is not one the register accepts.
    ///
    /// Two refusals share one variant because they are the same question asked
    /// in two directions — *is the approved licence still true, and is it one we
    /// accept?* — and a reader who got one answer deserves the same report
    /// shape for the other. `approved` is what the register recorded and
    /// `declared` is what `cargo metadata` reports: when they differ, upstream
    /// changed the licence without a re-approval; when they agree and still
    /// name something outside the register's `accepted_licenses`, the approval
    /// itself is out of policy. `"<none>"` stands for a manifest that declares no `license`
    /// key at all, so an absence reads as an absence and not as a permission.
    LicenseNotAccepted {
        /// Package whose declared licence was refused.
        krate: String,
        /// Licence expression the register approved.
        approved: String,
        /// Licence expression the package's manifest declares.
        declared: String,
        /// Licence identifiers outside the register's accepted set, in the
        /// order they appear in `declared`.
        rejected: Vec<String>,
    },
    /// An approval claims the `vendor` tier while its edge still resolves to
    /// the source class a registry edge has.
    ///
    /// `vendor` is rung 7 of the ladder — "audited upstream source checked into
    /// the workspace, not a registry edge" (`docs/dependency-doctrine.md` §1).
    /// This is the reader that tier never had. It does not ask whether the tier
    /// is the right *label* for a dependency, which is a review decision this
    /// gate has no evidence to make; it asks the one question the tier can
    /// decide on its own from data the gate already has, which is whether the
    /// tier contradicts the source class the edge actually resolves from.
    VendorTierConflict {
        /// Package whose approval claims the vendor tier.
        krate: String,
        /// Source class the authored edge actually resolves from.
        source: String,
    },
    /// A workspace member or an approval owner names a surface outside the
    /// closed set the register's `[policy] surfaces` declares (INV-DEP-1 for
    /// this repository).
    ///
    /// The gate is membership-driven, so a sixth crate would otherwise be
    /// audited like any other and an `owner = "lgwks_rogue"` approval would
    /// pass an identifier check. This is the refusal that makes the surface
    /// list a closed vocabulary rather than a sentence.
    UnknownSurface {
        /// The member package or owner name that is not a recognised surface.
        krate: String,
        /// Where it was found: `workspace member` or `approval owner`.
        role: String,
    },
    /// `[policy] enforce = false` was used to turn a tree that *actually
    /// carries refusals* into a passing build.
    ///
    /// Adoption mode is a reviewable posture, not an off switch: it may report
    /// refusals as guidance, but it cannot make them non-fatal. This refusal is
    /// added on top of the edge refusals it counted, so the verdict is the same
    /// under `enforce = true` and `enforce = false` and the boolean cannot be
    /// inverted without the build changing. The count is carried so the report
    /// says how many violations the posture tried to stand down.
    AdoptionModeRefusals {
        /// Number of dependency-edge refusals the posture stood down.
        refusals: usize,
    },
    /// A crate the deferral catalog still defers is approved: the request is
    /// closed with the trigger and the owner, not with "not now".
    DeferredAdmitted {
        /// External package the catalog defers.
        krate: String,
        /// Estate owner that must be fixed first.
        owner: String,
        /// Measured trigger that would admit the crate.
        trigger: String,
    },
    /// An approved edge with no lifecycle mortgage: the register names the
    /// edge, but no row says what replaces it, what retires it, or when it
    /// was last reviewed.
    MissingLifecycle {
        /// External package with no mortgage row.
        krate: String,
    },
    /// An upstream pin or sealed exception passed its expiry date: renew the
    /// row with a review or remove the pin, but do not ship the excuse.
    PatchExpired {
        /// Crate the expired row is about.
        krate: String,
        /// ISO date the row stopped being honored.
        expiry: String,
        /// Upstream PR for a pin, or the exception record for a provider.
        upstream: String,
    },
    /// A blocked TLS provider is in the resolved tree with no live sealed
    /// exception: `ring` and `webpki-roots` carry licence terms no register
    /// accepts, so the chain that pulled one in must be recorded or removed.
    ProviderBlocked {
        /// Blocked provider package in the resolved lockfile.
        krate: String,
        /// Resolved version.
        version: String,
        /// Dependency chain from the provider up to the undepended package.
        via: String,
    },
    /// A supply-chain ledger the gate was told to read cannot be read or
    /// parsed. The catalog and the ledger are opt-in, but a file that exists
    /// and says nothing readable is a refusal, never a silent skip.
    LedgerUnreadable {
        /// Ledger file that could not be read or parsed.
        file: String,
        /// What was wrong, with the source line when parsing failed.
        reason: String,
    },
    /// The root manifest or the cargo config overrides where dependency bytes
    /// come from: a `[patch]` or `[replace]` table, or a source replacement
    /// other than crates.io read from `vendored-sources` (#266).
    ///
    /// `cargo metadata` still names the approved package under an override,
    /// so the edge audit alone would pass a build that compiles other bytes.
    DependencyOverride {
        /// The file declaring it, relative to the repository root.
        file: String,
        /// The table or key, as written: `[patch.crates-io]`, or
        /// `source.crates-io.replace-with = "mirror"`.
        table: String,
    },
}

/// How a refusal prints a bit the entry authored.
///
/// This refusal exists only for an authored dimension, so the printed bit is
/// always one the register carries; the absent arm names that rather than
/// printing a `true` or `false` the register never wrote.
pub(crate) fn bit_label(bit: Option<bool>) -> &'static str {
    match bit {
        Some(true) => "true",
        Some(false) => "false",
        None => "no admitted bit",
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::ForeignWorkspaceMember {
                ref consumer,
                ref krate,
                ref declared_repository,
                ref expected_repository,
            } => write!(
                formatter,
                "{consumer} embeds workspace package {krate} from {declared_repository}; consume its published crate instead (workspace authority is {expected_repository})"
            ),
            Self::UnregisteredEdge {
                ref consumer,
                ref krate,
                ref requirement,
                ref source,
                ref kind,
            } => write!(
                formatter,
                "{consumer} declares unowned {kind} edge {krate} {requirement} from {source}"
            ),
            Self::ConsumerNotAllowed {
                ref consumer,
                ref krate,
            } => write!(
                formatter,
                "{consumer} declares {krate}, but no approval allows that consumer"
            ),
            Self::RequirementDrift {
                ref consumer,
                ref krate,
                ref approved,
                ref declared,
            } => write!(
                formatter,
                "{consumer} declares {krate} requirement {declared}, contract approves {approved}"
            ),
            Self::SourceDrift {
                ref consumer,
                ref krate,
                ref approved,
                ref declared,
            } => write!(
                formatter,
                "{consumer} declares {krate} from {declared}, contract approves {approved}"
            ),
            Self::OriginDrift {
                ref consumer,
                ref krate,
                ref approved,
                ref declared,
            } => write!(
                formatter,
                "{consumer} declares {krate} from {declared}, contract approves origin {approved}"
            ),
            Self::KindNotAllowed {
                ref consumer,
                ref krate,
                ref kind,
            } => write!(
                formatter,
                "{consumer} declares {krate} as {kind}, but that edge kind is not approved"
            ),
            Self::FeatureDrift {
                ref consumer,
                ref krate,
                ref approved,
                ref declared,
            } => write!(
                formatter,
                "{consumer} declares {krate} with features [{declared}], contract admits [{approved}]"
            ),
            Self::DefaultFeaturesDrift {
                ref consumer,
                ref krate,
                approved,
                declared,
            } => write!(
                formatter,
                "{consumer} declares {krate} with default-features = {declared}, contract admits {}",
                bit_label(approved)
            ),
            Self::OptionalityDrift {
                ref consumer,
                ref krate,
                approved,
                declared,
            } => write!(
                formatter,
                "{consumer} declares {krate} optional = {declared}, contract admits {}",
                bit_label(approved)
            ),
            Self::TargetDrift {
                ref consumer,
                ref krate,
                ref approved,
                ref declared,
            } => write!(
                formatter,
                "{consumer} declares {krate} for target {declared}, contract admits {approved}"
            ),
            Self::UnusedApproval {
                ref krate,
                ref owner,
                ref capability,
            } => write!(
                formatter,
                "unused approval for {krate} capability {capability} owned by {owner}"
            ),
            Self::FrozenSurfaceTier {
                ref consumer,
                ref krate,
                ref tier,
            } => write!(
                formatter,
                "{consumer} is a frozen surface and its {krate} edge is tiered {tier}; \
                 a frozen surface's edges stay boundary, and the surface's approved set \
                 is closed"
            ),
            Self::LicenseNotAccepted {
                ref krate,
                ref approved,
                ref declared,
                ref rejected,
            } => {
                let refused = rejected.join(", ");
                write!(
                    formatter,
                    "{krate} declares licence {declared}, which the register's \
                     approved {approved} does not admit ({refused}); re-approve the \
                     licence or remove the edge"
                )
            }
            Self::VendorTierConflict {
                ref krate,
                ref source,
            } => write!(
                formatter,
                "{krate} is approved at the vendor tier but its edge resolves from \
                 {source}; vendor means audited source checked into this workspace, \
                 not a registry edge"
            ),
            Self::UnknownSurface {
                ref krate,
                ref role,
            } => write!(
                formatter,
                "{krate} is a {role} but not one of the surfaces the register's \
                 [policy] surfaces declares; a new surface is a decision to record \
                 there, not a crate to add"
            ),
            Self::LicensePolicyUndeclared { approvals } => write!(
                formatter,
                "the register approves {approvals} external edge(s) but declares no \
                 [policy] accepted_licenses; name the SPDX identifiers this repository \
                 accepts (for example `accepted_licenses = \"MIT, Apache-2.0\"`)"
            ),
            Self::AdoptionModeRefusals { refusals } => write!(
                formatter,
                "[policy] enforce = false stood down {refusals} dependency-edge \
                 violations; adoption mode reports refusals, it does not make them \
                 pass — register the edges or re-enable enforcement"
            ),
            Self::DependencyOverride {
                ref file,
                ref table,
            } => write!(
                formatter,
                "{file} declares {table}, which replaces dependency bytes the register \
                 approved; remove it (the one admitted override is \
                 `[source.crates-io] replace-with = \"vendored-sources\"`)"
            ),
            Self::DeferredAdmitted {
                ref krate,
                ref owner,
                ref trigger,
            } => write!(
                formatter,
                "{krate} is approved but the deferral catalog still defers it: fix the \
                 estate owner {owner} first, and admit it only when {trigger}"
            ),
            Self::MissingLifecycle { ref krate } => write!(
                formatter,
                "{krate} is approved but the lifecycle ledger names no mortgage for it: \
                 record its replacement, removal trigger and last review in \
                 contract/LIFECYCLE.toml"
            ),
            Self::PatchExpired {
                ref krate,
                ref expiry,
                ref upstream,
            } => write!(
                formatter,
                "the ledger row for {krate} expired on {expiry}: renew it with a review \
                 or remove the pin; upstream is {upstream}"
            ),
            Self::ProviderBlocked {
                ref krate,
                ref version,
                ref via,
            } => write!(
                formatter,
                "{krate} {version} is a blocked TLS provider in the resolved tree with \
                 no live sealed exception; record the owner, date, blast radius and \
                 removal path, or remove the chain that pulled it in ({via})"
            ),
            Self::LedgerUnreadable { ref reason, .. } => write!(formatter, "{reason}"),
        }
    }
}

impl Refusal {
    /// The crate this refusal is about.
    ///
    /// An adoption-mode refusal is not about any one crate: it is about the
    /// register's own `[policy]` block standing down a count of edge
    /// violations, so it carries no crate name and answers `"<policy>"`; so
    /// does a register with no licence policy, which is about the block too. An
    /// override and an unreadable ledger are about a file's table, so both
    /// answer `"override"`. A caller that wants to distinguish the shapes
    /// matches on the variant rather than on this label.
    #[must_use]
    pub fn krate(&self) -> &str {
        match *self {
            Self::ForeignWorkspaceMember { ref krate, .. }
            | Self::UnregisteredEdge { ref krate, .. }
            | Self::ConsumerNotAllowed { ref krate, .. }
            | Self::RequirementDrift { ref krate, .. }
            | Self::SourceDrift { ref krate, .. }
            | Self::OriginDrift { ref krate, .. }
            | Self::KindNotAllowed { ref krate, .. }
            | Self::FeatureDrift { ref krate, .. }
            | Self::DefaultFeaturesDrift { ref krate, .. }
            | Self::OptionalityDrift { ref krate, .. }
            | Self::TargetDrift { ref krate, .. }
            | Self::UnusedApproval { ref krate, .. }
            | Self::FrozenSurfaceTier { ref krate, .. }
            | Self::LicenseNotAccepted { ref krate, .. }
            | Self::VendorTierConflict { ref krate, .. }
            | Self::UnknownSurface { ref krate, .. }
            | Self::DeferredAdmitted { ref krate, .. }
            | Self::MissingLifecycle { ref krate, .. }
            | Self::PatchExpired { ref krate, .. }
            | Self::ProviderBlocked { ref krate, .. } => krate,
            Self::AdoptionModeRefusals { .. } | Self::LicensePolicyUndeclared { .. } => "<policy>",
            // An override is about a file's table, not one crate: a `[patch]`
            // can carry many packages, and a source replacement carries them all.
            // A ledger that cannot be read is about the file, for the same reason.
            Self::DependencyOverride { .. } | Self::LedgerUnreadable { .. } => "<override>",
        }
    }

    /// Whether this refusal names the register's enforcement policy rather
    /// than a dependency edge.
    ///
    /// Kept beside [`Refusal::krate`] because the two must not drift: every
    /// variant that returns the `"<policy>"` label has to answer `true` here,
    /// or a consumer filtering by scope would find a crate-shaped refusal
    /// carrying a policy-shaped name.
    #[must_use]
    pub const fn is_policy(&self) -> bool {
        matches!(
            *self,
            Self::AdoptionModeRefusals { .. } | Self::LicensePolicyUndeclared { .. }
        )
    }
}

/// Why the gate could not reach a verdict. Every variant is a refusal, not a
/// pass; see the fail-closed note on this module.
///
/// `#[non_exhaustive]`: a new failure mode is a refusal that must be added, and
/// adding it must not be a breaking change for embedders matching on this type.
#[derive(Debug)]
#[non_exhaustive]
pub enum GateError {
    /// No `Cargo.lock` was found at or above the starting directory.
    LockNotFound {
        /// Directory the search started from.
        from: PathBuf,
    },
    /// The repository has no register.
    ContractNotFound {
        /// Path the register was expected at.
        path: PathBuf,
    },
    /// A file could not be read.
    Unreadable {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying I/O cause.
        cause: std::io::Error,
    },
    /// The register is not a valid contract.
    Contract(contract::ContractError),
    /// The optional invariant register could not be read or parsed.
    Invariant(invariants::InvariantError),
    /// `Cargo.lock` does not parse.
    ///
    /// Its own variant rather than `Unreadable`: the file read fine, and
    /// "unreadable" would send an operator looking at permissions or encoding
    /// when the actual defect is a `[[package]]` block naming no package. The
    /// gate audits a dependency graph, so a lock whose contents cannot be read
    /// is a graph whose contents cannot be audited.
    Lock(lock::LockError),
    /// Cargo's authored direct dependency graph could not be obtained.
    Metadata(metadata::MetadataError),
}

impl fmt::Display for GateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::LockNotFound { ref from } => {
                write!(formatter, "no Cargo.lock at or above {}", from.display())
            }
            Self::ContractNotFound { ref path } => write!(
                formatter,
                "no dependency register at {} — every repo the gate guards must carry one; \
                 run `lgwks-deps init` to write a fail-closed starting register",
                path.display()
            ),
            Self::Unreadable {
                ref path,
                ref cause,
            } => {
                write!(formatter, "cannot read {}: {cause}", path.display())
            }
            Self::Contract(ref error) => write!(formatter, "{CONTRACT_PATH}: {error}"),
            Self::Invariant(ref error) => {
                write!(formatter, "{}: {error}", invariants::INVARIANTS_PATH)
            }
            Self::Lock(ref error) => write!(formatter, "Cargo.lock: {error}"),
            Self::Metadata(ref error) => write!(formatter, "Cargo metadata: {error}"),
        }
    }
}

impl Error for GateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match *self {
            Self::Unreadable { ref cause, .. } => Some(cause),
            Self::Contract(ref error) => Some(error),
            Self::Invariant(ref error) => Some(error),
            Self::Lock(ref error) => Some(error),
            Self::Metadata(ref error) => Some(error),
            _ => None,
        }
    }
}
