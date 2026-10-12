//! The licence audit (#208): what each package declares against what the
//! register approved and what the register accepts.

use crate::Refusal;
use crate::contract::Contract;
use crate::metadata::DirectEdge;

// ── Licence audit (#208) ────────────────────────────────────────────────────

/// What a manifest with no `license` key is reported as.
///
/// A distinct spelling, not an empty string, so the refusal says *absent* rather
/// than rendering a gap in the middle of a sentence that otherwise reads like a
/// permission.
pub(crate) const NO_LICENSE: &str = "<none>";

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
pub(crate) fn starts_with_term(rest: &str, term: &str) -> bool {
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
pub(crate) fn declared_value(observed: Option<&str>) -> &str {
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
