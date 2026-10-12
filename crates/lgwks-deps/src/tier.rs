//! The tier and surface audits (#210, INV-DEP-1): which approvals contradict the
//! source their edge resolves from, which re-tier a frozen surface, and which
//! workspace members or owners sit outside the register's closed set.

use crate::Refusal;
use crate::contract::{self, Contract};
use crate::metadata::DirectEdge;

// ── Tier audit (#210) ───────────────────────────────────────────────────────

/// The source class an approval at the [`contract::Tier::Vendor`] tier may
/// resolve from.
///
/// `vendor` means audited source checked into the workspace rather than a
/// registry edge. This repository checks third-party source into `vendor/` for
/// *offline builds* while still resolving those crates from a registry, so
/// "there is a vendored directory" is not evidence about an edge's source class
/// — and `vendor.rs`'s [`crate::vendor::Report`] binds on `Cargo.lock` hashes
/// precisely so the tree cannot stand in for a source class. What can be read
/// from an edge is where it came from, so `registry` and `git` — the two classes
/// Cargo resolves at build time from somewhere else — contradict the tier. See
/// [`vendor_tier_refusals`] for why `path` is deliberately not refused.
pub(crate) const VENDOR_SOURCE_CLASSES: [&str; 2] = ["registry", "git"];

/// Every approval that claims the vendor tier while its edge still resolves from
/// a source the tier rules out.
///
/// The one claim `vendor` makes that is checkable from the edge itself. It is
/// deliberately *not* the claim a reader might want it to make: nothing in this
/// repository says whether a given crate's code was audited before it was
/// checked in — `vendor.rs` records coverage and hash equality, not a review —
/// so requiring proof of review would mean inventing an approval artefact that
/// does not exist, and refusing everything at the tier would make the tier
/// unusable and thereby leave it unexercised rather than meaningful.
///
/// Nor does this refuse `tier = "vendor"` on a `path` edge. A vendored crate
/// normally *is* a path dependency, and a path edge is exactly what "checked
/// into the workspace" looks like to Cargo; the check that belongs on that
/// shape is INV-VENDOR-SINGLE-TREE, which `vendor.rs` already enforces against
/// the lockfile. This rule is added to cover the one hole the existing gate left
/// open: `Tier::parse` accepted `vendor` at load and nothing downstream
/// distinguished it, so a boundary dependency could be re-declared as audited
/// vendored source and no refusal followed.
pub(crate) fn vendor_tier_refusals(edges: &[DirectEdge], register: &Contract) -> Vec<Refusal> {
    register
        .approvals()
        .filter(|entry| entry.tier() == VENDOR_TIER)
        .filter_map(|entry| {
            let source = edges
                .iter()
                .filter(|edge| !edge.workspace && entry.admits(&edge.package))
                .map(|edge| edge.source.class())
                .find(|class| VENDOR_SOURCE_CLASSES.contains(class))?;
            Some(Refusal::VendorTierConflict {
                krate: entry.krate().to_owned(),
                source: (*source).to_owned(),
            })
        })
        .collect()
}

/// The tier a vendored approval must be judged against.
///
/// Named rather than written inline so the refusal and this reader cannot
/// disagree about which tier is being policed.
pub(crate) const VENDOR_TIER: contract::Tier = contract::Tier::Vendor;

/// Every approval that re-tiers a frozen surface's own edge away from the
/// register's `frozen_tier`, as one refusal each.
///
/// Which surfaces are frozen, and at what tier, is the register's
/// `[policy] frozen_surfaces` and `frozen_tier`; a register that declares
/// neither freezes nothing.
///
/// The second, structural half of INV-DEP-1's *never grow `lgwks_ast`*: adding
/// the edge at all is already refused as [`Refusal::UnregisteredEdge`], and
/// re-approving it as `vendor` rather than `boundary` is what a growth attempt
/// would reach for next. Both refusals name the surface, so a refusal says which
/// surface is in violation rather than only which crate is unowned.
pub(crate) fn frozen_surface_tier_refusals(register: &Contract) -> Vec<Refusal> {
    let Some(frozen) = register.policy.frozen.as_ref() else {
        return Vec::new();
    };
    register
        .approvals()
        .filter(|entry| entry.tier() != frozen.tier)
        .filter(|entry| frozen.surfaces.iter().any(|name| name == entry.owner()))
        .map(|entry| Refusal::FrozenSurfaceTier {
            consumer: entry.owner().to_owned(),
            krate: entry.krate().to_owned(),
            tier: entry.tier().to_string(),
        })
        .collect()
}

/// Every workspace member and every approval owner outside the register's
/// `[policy] surfaces`, as one refusal each.
///
/// The closed set is the register's: INV-DEP-1's five surfaces are what this
/// repository's own register declares, and another repository declares its own
/// or none. A register that declares none keeps no closed set, and no member or
/// owner is refused for its name.
pub(crate) fn surface_refusals(members: &[String], register: &Contract) -> Vec<Refusal> {
    let Some(surfaces) = register.policy.surfaces.as_deref() else {
        return Vec::new();
    };
    let unknown = |name: &str| !surfaces.iter().any(|surface| surface == name);
    let members = members
        .iter()
        .filter(|name| unknown(name))
        .map(|name| ("workspace member", name.as_str()));
    let mut owners: Vec<&str> = register.approvals().map(|entry| entry.owner()).collect();
    owners.sort_unstable();
    owners.dedup();
    let owners = owners
        .into_iter()
        .filter(|name| unknown(name))
        .map(|name| ("approval owner", name));
    members
        .chain(owners)
        .map(|(role, name)| Refusal::UnknownSurface {
            krate: name.to_owned(),
            role: role.to_owned(),
        })
        .collect()
}
