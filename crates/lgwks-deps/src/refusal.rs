//! How a refusal reads and what it is about: the `Display` text and the
//! `krate` and `is_policy` accessors of [`Refusal`].

use std::fmt;

use crate::Refusal;
use crate::edge::bit_label;

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
        }
    }
}

impl Refusal {
    /// The crate this refusal is about.
    ///
    /// An adoption-mode refusal is not about any one crate: it is about the
    /// register's own `[policy]` block standing down a count of edge
    /// violations, so it carries no crate name and answers `"<policy>"`; so
    /// does a register with no licence policy, which is about the block too. A
    /// caller that wants to distinguish the two shapes matches on the variant
    /// rather than on this label.
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
            | Self::UnknownSurface { ref krate, .. } => krate,
            Self::AdoptionModeRefusals { .. } | Self::LicensePolicyUndeclared { .. } => "<policy>",
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
