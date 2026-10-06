//! Integration tests for `lgwks_std`, linked as one binary (#272).
//!
//! One link and one test-binary start per crate instead of one per file.
//! Each former `tests/<name>.rs` is the module `<name>` here, so a filter on
//! `test(/^<name>::/)` selects what `--test <name>` used to, and the
//! `sim_*` module paths are what the simulation-evidence lane counts.
//! Tests that must own their process stay separate binaries beside this
//! directory; the crate's Cargo.toml or the PR names each and why.

// Fixtures more than one module uses, declared here once: a file loaded as
// a module twice is two copies of every type in it, and clippy refuses it.
// Each is compiled when any module that uses it is, and those modules
// `use crate::<fixture>` under the name they already gave it.
#[path = "../support/consumer_probe.rs"]
mod consumer_probe;

#[path = "../support/rng.rs"]
mod rng;

#[path = "../support/seeded_bytes.rs"]
mod seeded_bytes;

#[path = "../support/seeded_sweep.rs"]
mod seeded_sweep;

#[path = "../support/wire_record.rs"]
mod wire_record;

mod glob_public;
mod pattern_external;
mod prop_codecs;
mod serde_facade_consumers;
mod sim_codec;
mod sim_encoding;
mod sim_fs_capability;
mod sim_fs_walk;
mod sim_glob_sweep;
mod sim_hash;
mod sim_hex;
mod sim_http;
mod sim_id;
mod sim_leb128;
mod sim_measured_paths;
mod sim_pattern;
mod sim_process_group;
mod sim_random_error;
mod sim_retry_arithmetic;
mod sim_shared_policy_tiers;
mod sim_similarity_sweep;
mod sim_task_public;
mod sim_tenant_isolation;
mod sim_time_profile;
mod sim_wire;
mod similarity_evidence_contract;
mod similarity_public_api;
mod wire_consumer;
mod wire_feature_unification;
mod wire_fixture;
