//! The audited half of the `--contract` fixture pair.
//!
//! Its only interesting property is the manifest edge to `helper`; the source
//! exists so Cargo has a target to build.

/// Returns whether the fixture's helper is reachable from here.
pub fn has_helper() -> bool {
    true
}
