//! Integration tests for `lgwks_ast`, linked as one binary (#272).
//!
//! One link and one test-binary start per crate instead of one per file.
//! Each former `tests/<name>.rs` is the module `<name>` here, so a filter on
//! `test(/^<name>::/)` selects what `--test <name>` used to, and the
//! `sim_*` module paths are what the simulation-evidence lane counts.
//! Tests that must own their process stay separate binaries beside this
//! directory; the crate's Cargo.toml or the PR names each and why.

/// Running the shipped `lgwks-ast` binary, the way its two test modules do.
#[cfg(feature = "tool")]
mod cli;
mod content_detection;
mod front_door;
mod hostile;
mod parse_deadline;
mod scan_bounds;
mod scan_cases;
mod scan_evidence;
mod scan_verdicts;
mod script_read;
mod script_tool;
mod sim_diagnostics;
mod sim_parse_bounds;
mod sim_parse_deadline;
mod sim_scan_detectors;
mod sim_scan_manifest;
mod sim_script_find;

/// A scratch directory that dies with the test that made it, the one every
/// suite in the estate names and removes the same way (INV-BOT-116).
#[cfg(feature = "tool")]
#[path = "../../../lgwks-bot/tests/support/scratch.rs"]
mod scratch;
/// The seed substrate every `sim_*` module in this binary drives.
///
/// One copy per binary rather than one per module: `#[path]` includes resolve to
/// the same file, and loading it twice makes the generator a second definition
/// of `Rng` in one crate -- two types with one name, so a seed recorded against
/// one module would not mean the same draw sequence in the other. Declared here
/// so there is exactly one. It is `std` only, and compiled under `lang-rust`
/// because every module that draws from it parses Rust: a build with no grammar
/// has no family to drive it.
#[cfg(feature = "lang-rust")]
#[path = "../../../lgwks-bot/tests/sim/seed.rs"]
mod seed;
/// The weighted coin and the trace's emptiness query, over the same core.
#[cfg(feature = "lang-rust")]
#[path = "../../../lgwks-bot/tests/sim/seed_helpers.rs"]
mod seed_helpers;

/// The estate's draws over that substrate (`pick_named`, `coin`) and the replay
/// receipt, which `lgwks_deps`' simulations share: one copy, so a seed means one
/// stream in every crate. The scan simulation is its only user here.
#[cfg(feature = "scan")]
#[path = "../../../lgwks-deps/tests/support/sim.rs"]
mod sim;
