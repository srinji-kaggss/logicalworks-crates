//! Does one approval admit one authored edge? Each axis of an approval (consumer,
//! requirement, source class, origin, kind, features, default-features,
//! optionality, target) is tested here on its own, so a refusal can name the
//! axis that drifted.

use crate::contract;
use crate::metadata::DirectEdge;

/// Whether `entry` names `consumer` among its allowed consumers.
///
/// Byte-exact against the Cargo-authored workspace package name. There is no
/// implicit `-`/`_` fold: a consumer is a package identity, and folding two
/// spellings together would let one workspace package stand in for another.
pub(crate) fn allows_consumer(entry: &contract::Entry, consumer: &str) -> bool {
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
pub(crate) fn origin_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
    match entry.origin.as_deref() {
        Some(approved) => same_origin(approved, edge.source.detail()),
        None => entry.source == "registry" && is_crates_io(edge.source.detail()),
    }
}

/// The approved origin to name in an [`Refusal::OriginDrift`].
pub(crate) fn approved_origin(entry: &contract::Entry, edge: &DirectEdge) -> String {
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
pub(crate) fn feature_policy_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
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
pub(crate) fn default_features_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
    entry
        .uses_default_features
        .is_none_or(|approved| approved == edge.uses_default_features)
}

/// Whether `entry`'s admitted optionality policy admits the edge.
pub(crate) fn optionality_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
    entry
        .optional
        .is_none_or(|approved| approved == edge.optional)
}

/// Whether `entry`'s admitted target scope admits the edge.
///
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

/// An unauthored policy admits any scope; an authored `""` requires an
/// unconditional declaration, and any other string must equal the edge's target
/// `cfg(…)` exactly.
pub(crate) fn target_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
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
/// ```
/// use lgwks_deps::declared_scope;
///
/// assert_eq!(declared_scope(None), "");
/// assert_eq!(declared_scope(Some("cfg(unix)")), "cfg(unix)");
/// ```
#[must_use]
pub fn declared_scope(scope: Option<&str>) -> &str {
    let Some(scope) = scope else {
        return "";
    };
    scope
}

/// The feature policy as it is named in a refusal.
pub(crate) fn approved_features(entry: &contract::Entry) -> String {
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
pub(crate) fn edge_matches(entry: &contract::Entry, edge: &DirectEdge) -> bool {
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
