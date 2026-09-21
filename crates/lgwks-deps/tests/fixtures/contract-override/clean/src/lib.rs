//! The clean half of the `--contract` fixture pair.
//!
//! It is deliberately empty of dependency edges: the point of this repository
//! is that a gate pointed at it by mistake returns success.

/// Returns the value it was handed, so the fixture has one compiled item.
pub fn identity(value: u32) -> u32 {
    value
}
