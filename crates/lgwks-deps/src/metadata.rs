//! Cargo metadata ingestion for direct dependency-edge admission.
//!
//! The lock file answers which bytes happened to resolve. It does not answer
//! which workspace package authored an edge. `cargo metadata --no-deps` does,
//! including inactive optional dependencies, so this module is the source of
//! truth for INV-DEP-EDGE-OWNED.

/// The bounded `cargo` subprocess: capture files, the deadline, the byte budgets and the cleanup obligation.
mod capture;
/// Collecting the graph: the locked `cargo metadata` reads, the `Collected` result and the workspace members.
mod collect;
/// Decoding Cargo's metadata JSON into direct workspace edges: classification, membership and licences.
mod edges;
/// The dependency vocabulary Cargo's metadata is read into, and the refusals that reading can end in.
mod types;

/// The bounded subprocess's tests, with the faults they inject.
#[cfg(test)]
mod capture_tests;
/// The edge extraction's tests.
#[cfg(test)]
mod edges_tests;
/// The single-use faults the collection tests inject, and the checks they share.
#[cfg(test)]
mod faults;
/// Seeded sweeps over `lexical_join` against a component-stack model.
#[cfg(test)]
mod sim_edges_join;
/// Seeded sweeps over `parse` against documents drawn from the seed.
#[cfg(test)]
mod sim_edges_parse;

#[doc(hidden)]
pub use self::capture::CleanupObligation;
#[doc(hidden)]
pub use self::collect::{
    Collected, EdgesAndMembers, Member, read, read_with_members, workspace_members,
};
#[doc(hidden)]
pub use self::edges::parse;
#[doc(hidden)]
pub use self::types::{DependencyKind, DependencySource, DirectEdge, MetadataError};
