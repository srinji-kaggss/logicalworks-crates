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

#[cfg(all(unix, feature = "process"))]
#[path = "../support/group_leader.rs"]
mod group_leader;

#[path = "../support/adversary.rs"]
mod adversary;

#[path = "../support/json_attacks.rs"]
mod json_attacks;

#[path = "../support/leb128_attacks.rs"]
mod leb128_attacks;

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
mod process_parity;
mod prop_codecs;
mod sim_adversary_base64;
mod sim_adversary_calendar;
mod sim_adversary_digest;
mod sim_adversary_fixture;
mod sim_adversary_hex;
mod sim_adversary_json_lexical;
mod sim_adversary_json_structure;
mod sim_adversary_leb128;
mod sim_adversary_percent;
mod sim_adversary_retry;
mod sim_adversary_sign;
mod sim_adversary_time;
mod sim_adversary_uuid;
#[cfg(feature = "build")]
mod sim_build;
// Builds its own downstream packages with its own json and ron features, so
// the run's features cannot change its result; once under `full`, not once per
// per-feature run (a nested build of 55-198 s each time).
#[cfg(feature = "full")]
mod serde_facade_consumers;
mod sim_codec;
mod sim_descendants;
mod sim_encoding;
mod sim_fs_capability;
mod sim_fs_walk;
mod sim_fs_walk_prune;
mod sim_glob_sweep;
mod sim_hash;
mod sim_hex;
mod sim_http;
mod sim_id;
mod sim_id_v7;
mod sim_join_all;
#[cfg(feature = "json")]
mod sim_json;
mod sim_kernel_owners;
mod sim_leb128;
mod sim_locale;
mod sim_measured_paths;
mod sim_orphan_reap;
mod sim_pattern;
mod sim_process_group;
mod sim_random_error;
mod sim_retry_arithmetic;
#[cfg(feature = "ron")]
mod sim_ron;
mod sim_seeded;
mod sim_shared_policy_tiers;
mod sim_sign;
mod sim_similarity_sweep;
mod sim_task_public;
mod sim_tenant_isolation;
mod sim_time_profile;
#[cfg(feature = "trace")]
mod sim_trace;
mod sim_wire;
mod similarity_evidence_contract;
mod similarity_public_api;
mod unsafe_boundary;
mod wire_consumer;
mod wire_feature_unification;
mod wire_fixture;
