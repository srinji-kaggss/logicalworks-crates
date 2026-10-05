//! Integration tests for `lgwks_deps`, linked as one binary (#272).
//!
//! One link and one test-binary start per crate instead of one per file.
//! Each former `tests/<name>.rs` is the module `<name>` here, so a filter on
//! `test(/^<name>::/)` selects what `--test <name>` used to, and the
//! `sim_*` module paths are what the simulation-evidence lane counts.
//! Tests that must own their process stay separate binaries beside this
//! directory; the crate's Cargo.toml or the PR names each and why.

// Fixtures more than one module uses, declared here once: a file loaded as
// a module twice is two copies of every type in it, and clippy refuses it.
// The modules that use them `use crate::<fixture>`.
#[path = "../support/deps_sim.rs"]
mod deps_sim;

#[path = "../support/sim.rs"]
mod sim;

mod check_cli;
mod contract_schema_compat;
mod feature_policy;
mod identity_binding;
mod metadata_dimensions;
mod metadata_public_api;
mod origin_binding;
mod prop_parsers;
mod scan_evidence;
mod sim_dependency_policy;
mod sim_license_policy;
mod sim_metadata_dimensions;
mod sim_origin;
mod sim_policy_properties;
mod workstream_c;
