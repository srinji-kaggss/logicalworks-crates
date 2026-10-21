//! The estate's own crates, and how the ladder recognises one (#701).
//!
//! Rung 2 of the admission ladder is the estate's published substrate. An edge
//! to one of them from crates.io is admitted by provenance: no `[[approved]]`
//! block, no `approved_by`, no per-consumer signature. Before this module, a
//! consumer that did exactly what the ladder asks was refused for it, and the
//! only form on offer was the third-party `boundary` block.
//!
//! The recognition is a closed list rather than a prefix. A prefix rule would
//! admit a name nobody published or reviewed — `lgwks_stdx` would be rung 2 by
//! spelling alone — so a new estate crate is an edit here, reviewed like any
//! other widening of the surface.

/// The crates this repository publishes on crates.io, in both spellings Cargo
/// folds when it resolves a registry package.
///
/// These are the five workspace members and nothing else. Every other byte must
/// match, which is the byte-exact identity INV-DEP-14 requires of an approval:
/// `LGWKS_STD`, `lgwksstd` and the bare prefix `lgwks_` are not estate crates.
const ESTATE_CRATES: [&str; 10] = [
    "lgwks_std",
    "lgwks_bot",
    "lgwks_ast",
    "lgwks_deps",
    "lgwks_macros",
    "lgwks-std",
    "lgwks-bot",
    "lgwks-ast",
    "lgwks-deps",
    "lgwks-macros",
];

/// Whether `krate` is one of the five published estate crates.
///
/// A name places a crate on a rung; it admits nothing by itself. The audit
/// requires the edge to resolve from the canonical registry as well, so a path
/// or Git package calling itself `lgwks_std` keeps the name and loses the
/// provenance.
#[must_use]
pub(crate) fn is_estate_crate(krate: &str) -> bool {
    ESTATE_CRATES.contains(&krate)
}

#[cfg(test)]
mod tests {
    use super::is_estate_crate;

    /// Every name this repository publishes, in both spellings Cargo folds.
    #[test]
    fn every_published_crate_is_an_estate_crate() {
        for krate in [
            "lgwks_std",
            "lgwks_bot",
            "lgwks_ast",
            "lgwks_deps",
            "lgwks_macros",
            "lgwks-std",
            "lgwks-bot",
            "lgwks-ast",
            "lgwks-deps",
            "lgwks-macros",
        ] {
            assert!(
                is_estate_crate(krate),
                "{krate} is published by this repository, so rung 2 is where the ladder \
                 places it"
            );
        }
    }

    /// The list is closed: no prefix, case fold or lookalike may borrow it, so
    /// admitting a new name takes a review rather than a spelling.
    #[test]
    fn no_other_name_is_an_estate_crate() {
        for krate in [
            "rusqlite",
            "serde",
            "logos",
            "std",
            "lgwks",
            "lgwks_",
            "lgwks_stdx",
            "lgwks_std_extra",
            "mylgwks_std",
            "LGWKS_STD",
            "lgwksstd",
        ] {
            assert!(
                !is_estate_crate(krate),
                "{krate} is not a published estate crate, so admitting it still needs an \
                 approval"
            );
        }
    }
}
