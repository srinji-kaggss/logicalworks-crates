//! The audit: every authored direct edge judged against the register, plus the
//! register-level checks (frozen surfaces, licences, tiers, closed surfaces).

use crate::Refusal;
use crate::contract::{self, Contract};
use crate::metadata::DirectEdge;

/// Whether `entry` names `consumer` among its allowed consumers.
///
/// Byte-exact against the Cargo-authored workspace package name. There is no
/// implicit `-`/`_` fold: a consumer is a package identity, and folding two
/// spellings together would let one workspace package stand in for another.
fn allows_consumer(entry: &contract::Entry, consumer: &str) -> bool {
    entry
        .allowed_consumers
        .iter()
        .any(|allowed| allowed == consumer)
}

/// The one registry a legacy class-only approval admits.
///
/// The two Cargo spellings — the Git index and the sparse index — name this one
/// registry, so both compare equal to it and to each other. Any other registry
/// source is a different authority and needs its own authored `origin`.
pub(crate) const CRATES_IO_INDEX: &str = "registry+https://github.com/rust-lang/crates.io-index";
/// The sparse transport spelling of the crates.io registry.
pub(crate) const CRATES_IO_SPARSE: &str = "sparse+https://index.crates.io/";

/// Whether `detail` names the crates.io registry through either Cargo spelling.
pub(crate) fn is_crates_io(detail: &str) -> bool {
    detail == CRATES_IO_INDEX
        || detail == CRATES_IO_SPARSE
        || detail == "sparse+https://index.crates.io"
}

/// The repository portion of a Cargo Git source, before any revision policy.
pub(crate) fn git_repository(detail: &str) -> &str {
    match detail.find(['?', '#']) {
        Some(index) => &detail[..index],
        None => detail,
    }
}

/// The revision/reference policy of a Cargo Git source: the `?rev=…`/`?branch=…`
/// query and the `#…` resolved-revision fragment, or empty when none is pinned.
pub(crate) fn git_policy(detail: &str) -> &str {
    match detail.find(['?', '#']) {
        Some(index) => &detail[index..],
        None => "",
    }
}

/// Whether an authored origin admits an observed Cargo source exactly.
///
/// Registry identity is the whole source string, except that the two spellings
/// of crates.io name one registry. Git identity is the repository plus its
/// admitted revision/reference policy, so pinning a different revision inside
/// the approved repository is still a drift. Path identity is the whole
/// authority string.
pub(crate) fn same_origin(approved: &str, observed: &str) -> bool {
    if approved == observed {
        return true;
    }
    if approved.starts_with("registry+") || approved.starts_with("sparse+") {
        return is_crates_io(approved) && is_crates_io(observed);
    }
    if approved.starts_with("git+") && observed.starts_with("git+") {
        return git_repository(approved) == git_repository(observed)
            && git_policy(approved) == git_policy(observed);
    }
    false
}

/// Whether `entry`'s approval admits `edge`'s origin.
///
/// An authored `origin` is compared by [`same_origin`]. A legacy entry with no
/// `origin` is exact only for the one deterministic registry this estate uses;
/// a git or path edge needs an authored origin before it can be admitted, so a
/// class-only entry is insufficient for exact-origin assurance rather than
/// implicit approval of every origin in its class.
fn origin_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
    match entry.origin.as_deref() {
        Some(approved) => same_origin(approved, edge.source.detail()),
        None => entry.source == "registry" && is_crates_io(edge.source.detail()),
    }
}

/// The approved origin to name in an [`Refusal::OriginDrift`].
fn approved_origin(entry: &contract::Entry, edge: &DirectEdge) -> String {
    match entry.origin.as_deref() {
        Some(approved) => approved.to_owned(),
        None if entry.source == "registry" => CRATES_IO_INDEX.to_owned(),
        None => format!(
            "{} (class-only: an exact origin is required)",
            edge.source.class()
        ),
    }
}

/// Whether `entry`'s admitted feature policy admits `edge`'s enabled features.
///
/// An absent `features` list (or `required_features` list) leaves that half
/// grandfathered: the entry predates the dimension and does not refuse a feature
/// it never named. A present list is enforced exactly — every enabled feature
/// must be allowed, and every required feature must be enabled.
fn feature_policy_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
    if entry.features.as_ref().is_some_and(|allowed| {
        edge.features
            .iter()
            .any(|enabled| !allowed.contains(enabled))
    }) {
        return false;
    }
    if entry.required_features.as_ref().is_some_and(|required| {
        required
            .iter()
            .any(|needed| !edge.features.contains(needed))
    }) {
        return false;
    }
    true
}

/// Whether `entry`'s admitted `default-features` policy admits the edge.
fn default_features_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
    entry
        .uses_default_features
        .is_none_or(|approved| approved == edge.uses_default_features)
}

/// Whether `entry`'s admitted optionality policy admits the edge.
fn optionality_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
    entry
        .optional
        .is_none_or(|approved| approved == edge.optional)
}

/// How a scope is named in a refusal when there is none.
///
/// `<none>` is the label a refusal prints for an edge the manifest declares
/// unconditionally, which is a fact about the *text* a reader sees rather than a
/// value substituted for a missing scope: an authored `target = ""` says the
/// same thing as no `target` key, and both print `<none>`.
pub(crate) fn scope_label(scope: Option<&str>) -> &str {
    match scope {
        Some(scope) if !scope.is_empty() => scope,
        _ => "<none>",
    }
}

/// Whether `entry`'s admitted target scope admits the edge.
///
/// An unauthored policy admits any scope; an authored `""` requires an
/// unconditional declaration, and any other string must equal the edge's target
/// `cfg(…)` exactly.
fn target_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
    match entry.target.as_deref() {
        None => true,
        Some(approved) => approved == declared_scope(edge.target.as_deref()),
    }
}

/// The register's `target` spelling of a scope Cargo reported.
///
/// Cargo spells an unconditional dependency with no `target` key; the register
/// spells the same declaration `target = ""` (INV-DEP-13). This is the
/// conversion between those two vocabularies, named because the gate compares
/// them in more than one place and an entry that authors no `target` at all is a
/// *third* fact — an unconstrained dimension — which this function is not for
/// and which callers inside this crate read from an entry's `target` field directly.
///
/// An absent scope is spelled `""` and a scope is kept as Cargo wrote it
/// (`cfg(unix)`); the integration test `an_absent_target_is_spelled_empty_and_a_scope_is_kept`
/// asserts both.
#[must_use]
pub fn declared_scope(scope: Option<&str>) -> &str {
    let Some(scope) = scope else {
        return "";
    };
    scope
}

/// The feature policy as it is named in a refusal.
fn approved_features(entry: &contract::Entry) -> String {
    let allowed = entry.features.as_ref().map(|list| list.join(","));
    match (allowed, entry.required_features.as_ref()) {
        (None, _) => "no admitted feature set".to_owned(),
        (Some(allowed), Some(required)) if !required.is_empty() => {
            format!("{allowed} (required: {})", required.join(","))
        }
        (Some(allowed), _) => allowed,
    }
}

/// Whether an approval admits this exact edge.
///
/// Every axis must hold: the consumer is allowed, the requirement string is
/// identical, the source class and origin are identical, the dependency kind is
/// listed, and each admitted capability policy (features, default-features,
/// optionality, target) is satisfied. A partial match is not a weak admission:
/// it is a refusal with a named axis, which is why `audit_direct` re-tests each
/// axis to report *which* one drifted.
fn edge_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
    allows_consumer(entry, &edge.consumer)
        && entry.version == edge.requirement
        && entry.source == edge.source.class()
        && origin_matches(entry, edge)
        && entry
            .allowed_kinds
            .iter()
            .any(|kind| kind == edge.kind.as_str())
        && feature_policy_matches(entry, edge)
        && default_features_matches(entry, edge)
        && optionality_matches(entry, edge)
        && target_matches(entry, edge)
}

// ── Estate edges (#701) ─────────────────────────────────────────────────────

/// Whether `edge` reaches a published estate crate, which needs no approval.
///
/// Two facts decide it, and neither is a name alone: the package is one of the
/// five published estate crates ([`crate::estate::is_estate_crate`]), and the
/// edge resolves from the canonical registry. A path or Git package calling
/// itself `lgwks_std` keeps its name and loses its provenance, so it is audited
/// as the ordinary external edge it is — the same rule that keeps a copied
/// foreign crate from being read as a workspace member.
///
/// The rung is the ladder's: `lgwks_std` is rung 2, "preferred. The common
/// substrate", and a consumer that climbs the ladder to it was refused for
/// doing exactly what the ladder asks.
fn is_estate_edge(edge: &DirectEdge) -> bool {
    edge.source.class() == "registry"
        && is_crates_io(edge.source.detail())
        && crate::estate::is_estate_crate(&edge.package)
}

// ── Licence audit (#208) ────────────────────────────────────────────────────

/// What a manifest with no `license` key is reported as.
///
/// A distinct spelling, not an empty string, so the refusal says *absent* rather
/// than rendering a gap in the middle of a sentence that otherwise reads like a
/// permission.
const NO_LICENSE: &str = "<none>";

/// The licence identifiers in `expression` outside the register's `accepted`
/// set, in the order they appear.
///
/// The expression is scanned for SPDX *identifiers*, not split on whitespace.
/// The difference decides whether two real licences pass. `Apache-2.0 WITH
/// LLVM-exception` is one identifier with one exception attached; splitting it
/// yields a bare `LLVM-exception`, which is not a licence anyone can be under,
/// so `blake3` and `rustix` were refused for carrying exactly the terms the
/// register had approved for them. `WITH` binds to the identifier on its left
/// and is not a conjunction, so it never separates two alternatives.
///
/// `AND` and `OR` are operators and are consumed as part of the scan rather
/// than reported: refusing `OR` would refuse every compound expression ever
/// written. The register has already refused an expression that is not valid
/// SPDX, so what reaches here from Cargo metadata is reported whole rather than
/// parsed into a grammar this gate would then have to keep in step with the
/// specification.
///
/// An identifier that is an accepted one passes even when the expression pairs
/// it with an exception this repository does not name, because the accepted
/// table lists `Apache-2.0 WITH LLVM-exception` in full and matching it is what
/// "accepted" means; a bare `Apache-2.0 WITH GPL-exception` is refused on the
/// `GPL-exception` token, which is the case the exception arm exists to catch.
pub(crate) fn rejected_license_identifiers(expression: &str, accepted: &[String]) -> Vec<String> {
    let mut rejected = Vec::new();
    // Walk left to right, taking the longest accepted identifier that matches
    // here. Longest-first is what lets `Apache-2.0 WITH LLVM-exception` win over
    // the bare `Apache-2.0` that also matches at this position.
    let tokens = expression
        .split_ascii_whitespace()
        .map(|token| token.trim_matches(['(', ')']))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let mut index = 0;
    while index < tokens.len() {
        let rest = tokens[index..].join(" ");
        if let Some(accepted) = accepted
            .iter()
            .filter(|candidate| starts_with_term(&rest, candidate))
            .max_by_key(|candidate| candidate.len())
        {
            // Consume exactly the words this identifier occupies.
            let consumed = accepted.split_ascii_whitespace().count();
            index = index.saturating_add(consumed);
            continue;
        }
        let token = tokens[index];
        if !matches!(token, "AND" | "OR" | "WITH") {
            rejected.push(token.to_owned());
        }
        index = index.saturating_add(1);
    }
    rejected
}

/// Whether `rest` begins with the whole licence term `term`, ending at a word
/// boundary, so an accepted `MIT` does not also accept `MIT-0`.
fn starts_with_term(rest: &str, term: &str) -> bool {
    rest.strip_prefix(term)
        .is_some_and(|after| after.is_empty() || after.starts_with(' '))
}

/// Every approval whose recorded licence no longer says what the package
/// declares, or names a licence outside the register's `accepted_licenses`.
///
/// The accepted set is the register's own `[policy] accepted_licenses`. A
/// register that approves an observed edge and declares no accepted set is one
/// [`Refusal::LicensePolicyUndeclared`]: this crate is published and audits
/// other repositories, so it has no licence policy of its own to fall back on,
/// and admitting every licence because none was declared would be the gate
/// failing open.
///
/// One pass over the approvals, and each approval is asked about the *declared*
/// licence of its target rather than the licence the register recorded: the
/// question the audit exists to answer is what the code in the tree is actually
/// under, and a register that disagrees is the drift (#208). Reporting the
/// declared expression even when it equals the approved one is deliberate —
/// there is no second refusal to find, and one refusal that names the licence
/// is the whole report.
///
/// When two edges of the same package resolve to two different declared
/// expressions, every one of them is refused, because a single-version
/// dependency cannot honestly be approved under two licences and the gate has
/// no evidence to pick one.
pub(crate) fn license_refusals(edges: &[DirectEdge], register: &Contract) -> Vec<Refusal> {
    let Some(accepted) = register.policy.accepted_licenses.as_deref() else {
        let approvals = register
            .approvals()
            .filter(|entry| {
                edges
                    .iter()
                    .any(|edge| !edge.workspace && entry.admits(&edge.package))
            })
            .count();
        return if approvals == 0 {
            Vec::new()
        } else {
            vec![Refusal::LicensePolicyUndeclared { approvals }]
        };
    };
    register
        .approvals()
        .filter_map(|entry| {
            // Distinct observed licences for this package, `None` standing for a
            // target that declares none. Order follows the edge order, so the
            // refusal names the same expression on every run.
            let mut observed: Vec<Option<&str>> = Vec::new();
            for edge in edges
                .iter()
                .filter(|edge| !edge.workspace && entry.admits(&edge.package))
            {
                if !observed.contains(&edge.license()) {
                    observed.push(edge.license());
                }
            }
            let Some(first) = observed.first().copied() else {
                // An approval whose package Cargo does not report — an inactive
                // optional edge with no resolved package, for instance — carries
                // nothing to compare. `UnusedApproval` is what says so, and
                // refusing here would make a licence report restate it.
                return None;
            };
            let declared = declared_value(first).to_owned();
            let drift = observed.iter().copied().any(|candidate| {
                !declared_expression(candidate)
                    .trim()
                    .eq_ignore_ascii_case(entry.license().trim())
            });
            let rejected = observed
                .iter()
                .copied()
                .flat_map(|candidate| {
                    rejected_license_identifiers(&declared_expression(candidate), accepted)
                })
                .collect::<Vec<_>>();
            if !drift && rejected.is_empty() {
                return None;
            }
            Some(Refusal::LicenseNotAccepted {
                krate: entry.krate().to_owned(),
                approved: entry.license().to_owned(),
                declared,
                rejected,
            })
        })
        .collect()
}

/// The text a declared licence is compared and rendered as.
fn declared_value(observed: Option<&str>) -> &str {
    match observed {
        Some(expression) => expression,
        None => NO_LICENSE,
    }
}

/// The declared licence as the SPDX expression both licence checks compare.
///
/// Cargo documents `/` in a package's `license` field as the deprecated
/// spelling of `OR`, and crates.io still serves packages written that way
/// (`foundationdb` 0.11 declares `MIT/Apache-2.0`). Read literally, that is
/// one unknown identifier and a drift from an approval recorded as
/// `MIT OR Apache-2.0`; read as Cargo defines it, it is the same choice of
/// licences. Only the declared side is rewritten: a register must still be
/// written in SPDX, and its parser refuses the slash. The refusal keeps
/// rendering the declared text as the package wrote it.
pub(crate) fn declared_expression(observed: Option<&str>) -> std::borrow::Cow<'_, str> {
    let declared = declared_value(observed);
    if declared.contains('/') {
        std::borrow::Cow::Owned(
            declared
                .split('/')
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" OR "),
        )
    } else {
        std::borrow::Cow::Borrowed(declared)
    }
}

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
const VENDOR_SOURCE_CLASSES: [&str; 2] = ["registry", "git"];

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
fn vendor_tier_refusals(edges: &[DirectEdge], register: &Contract) -> Vec<Refusal> {
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
const VENDOR_TIER: contract::Tier = contract::Tier::Vendor;

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

/// Audits authored direct dependency edges against semantic ownership.
///
/// Five questions, each answered from a different evidence: is every frozen
/// surface's edge set closed (INV-DEP-1), is every edge owned, is every
/// approval still true (`UnusedApproval`), is every declared licence one this
/// repository accepts and the one the register approved (#208), and does every
/// approval's tier agree with the source its edge resolves from (#210).
///
/// An edge to a published estate crate is admitted before the ownership
/// question is asked: it is rung 2 of the ladder, and the register is not the
/// body that admits it (#701).
pub fn audit_direct(edges: &[DirectEdge], register: &Contract) -> Vec<Refusal> {
    let mut refusals = frozen_surface_tier_refusals(register);
    refusals.extend(vendor_tier_refusals(edges, register));
    refusals.extend(license_refusals(edges, register));
    if let Some(expected) = register.repository.as_ref() {
        for edge in edges.iter().filter(|edge| edge.workspace) {
            if let Some(declared) = edge.target_repository.as_ref()
                && declared != expected
            {
                refusals.push(Refusal::ForeignWorkspaceMember {
                    consumer: edge.consumer.clone(),
                    krate: edge.package.clone(),
                    declared_repository: declared.clone(),
                    expected_repository: expected.clone(),
                });
            }
        }
    }
    // Two things need no register entry, and neither is decided by a name
    // alone: workspace membership, from Cargo's own `workspace_members`, and an
    // estate edge, which is an estate-namespace package from the canonical
    // registry (`is_estate_edge`, #701). Every other external edge needs an
    // approval.
    let external: Vec<&DirectEdge> = edges.iter().filter(|edge| !edge.workspace).collect();
    for edge in &external {
        if is_estate_edge(edge) {
            continue;
        }
        let approvals: Vec<&contract::Entry> = register.approvals_for(&edge.package).collect();
        if approvals.is_empty() {
            refusals.push(Refusal::UnregisteredEdge {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                requirement: edge.requirement.clone(),
                source: format!("{}:{}", edge.source.class(), edge.source.detail()),
                kind: edge.kind.to_string(),
            });
            continue;
        }
        if approvals.iter().any(|entry| edge_matches(entry, edge)) {
            continue;
        }
        let consumer_approvals: Vec<&contract::Entry> = approvals
            .iter()
            .copied()
            .filter(|entry| allows_consumer(entry, &edge.consumer))
            .collect();
        // Only approvals that admit the edge's source class are candidates for
        // the class, origin, requirement or kind drift. Reporting a source
        // mismatch from an approval of a *different* class would name an
        // irrelevant candidate and misdirect the repair.
        let class_matching: Vec<&contract::Entry> = consumer_approvals
            .iter()
            .copied()
            .filter(|entry| entry.source == edge.source.class())
            .collect();
        if consumer_approvals.is_empty() {
            refusals.push(Refusal::ConsumerNotAllowed {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
            });
        } else if class_matching.is_empty() {
            if let Some(entry) = consumer_approvals.first() {
                refusals.push(Refusal::SourceDrift {
                    consumer: edge.consumer.clone(),
                    krate: edge.package.clone(),
                    approved: entry.source.clone(),
                    declared: edge.source.class().to_owned(),
                });
            }
        } else if let Some(entry) = class_matching
            .iter()
            .copied()
            .find(|entry| !origin_matches(entry, edge))
        {
            refusals.push(Refusal::OriginDrift {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                approved: approved_origin(entry, edge),
                declared: edge.source.detail().to_owned(),
            });
        } else if let Some(entry) = class_matching
            .iter()
            .copied()
            .find(|entry| entry.version != edge.requirement)
        {
            refusals.push(Refusal::RequirementDrift {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                approved: entry.version.clone(),
                declared: edge.requirement.clone(),
            });
        } else if let Some(entry) = class_matching
            .iter()
            .copied()
            .find(|entry| !feature_policy_matches(entry, edge))
        {
            refusals.push(Refusal::FeatureDrift {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                approved: approved_features(entry),
                declared: edge.features.join(","),
            });
        } else if let Some(entry) = class_matching
            .iter()
            .copied()
            .find(|entry| !default_features_matches(entry, edge))
        {
            refusals.push(Refusal::DefaultFeaturesDrift {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                approved: entry.uses_default_features,
                declared: edge.uses_default_features,
            });
        } else if let Some(entry) = class_matching
            .iter()
            .copied()
            .find(|entry| !optionality_matches(entry, edge))
        {
            refusals.push(Refusal::OptionalityDrift {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                approved: entry.optional,
                declared: edge.optional,
            });
        } else if let Some(entry) = class_matching
            .iter()
            .copied()
            .find(|entry| !target_matches(entry, edge))
        {
            refusals.push(Refusal::TargetDrift {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                approved: scope_label(entry.target.as_deref()).to_owned(),
                declared: scope_label(edge.target.as_deref()).to_owned(),
            });
        } else {
            refusals.push(Refusal::KindNotAllowed {
                consumer: edge.consumer.clone(),
                krate: edge.package.clone(),
                kind: edge.kind.to_string(),
            });
        }
    }
    for entry in &register.entries {
        let used = external.iter().any(|edge| {
            entry.admits(&edge.package) && edge.consumer == entry.owner && edge_matches(entry, edge)
        });
        if !used {
            refusals.push(Refusal::UnusedApproval {
                krate: entry.krate.clone(),
                owner: entry.owner.clone(),
                capability: entry.capability.clone(),
            });
        }
    }
    // Adoption mode is a posture, not an off switch. When the register says
    // `enforce = false` *and* the tree actually carries edge violations, one
    // refusal names the stand-down so the build fails for the same reason it
    // would under `enforce = true`. Without this the whole verdict reduced to
    // that one boolean: a reviewable one-token diff was enough to make a tree
    // the gate had just declared in violation report success. An empty audit
    // stays empty — adoption mode on a clean tree is a legitimate posture and
    // must not be refused.
    if !register.enforce && !refusals.is_empty() {
        refusals.push(Refusal::AdoptionModeRefusals {
            refusals: refusals.len(),
        });
    }
    refusals.sort_by_key(ToString::to_string);
    refusals
}
