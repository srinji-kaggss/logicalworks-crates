//! Integration tests for `lgwks_bot`, linked as one binary (#272).
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
#[path = "../sim/bands.rs"]
mod band_family;

#[cfg(any(
    all(
        unix,
        feature = "rt",
        feature = "time",
        feature = "sync",
        feature = "process"
    ),
    feature = "script"
))]
#[path = "../support/compile.rs"]
mod compile;

#[cfg(all(feature = "ecs", feature = "wire"))]
#[path = "../support/declarable.rs"]
mod declarable;

#[path = "../support/effects.rs"]
#[cfg(all(feature = "ecs", feature = "wire"))]
mod effects;

#[cfg(all(
    unix,
    feature = "script",
    feature = "process",
    feature = "time",
    feature = "sync",
    feature = "ephemeral"
))]
#[path = "../support/fake_gh.rs"]
mod fake_gh;

#[cfg(all(unix, feature = "script", feature = "process", feature = "ephemeral"))]
#[path = "../support/fake_gh_issues.rs"]
mod fake_gh_issues;

#[cfg(all(feature = "inspect", feature = "script"))]
#[path = "../inspect_support/mod.rs"]
mod inspect_support;

#[path = "../support/journal.rs"]
#[cfg(feature = "wire")]
mod journal_fixtures;

#[cfg(feature = "script")]
#[path = "../support/load.rs"]
mod load;

#[path = "../support/lock.rs"]
mod lock;

#[cfg(all(feature = "ecs", feature = "wire"))]
#[path = "../support/poll.rs"]
mod poll;

#[cfg(all(
    unix,
    feature = "rt",
    feature = "time",
    feature = "sync",
    feature = "process"
))]
#[path = "../support/process.rs"]
mod process_probe;

#[cfg(any(feature = "script", all(feature = "ecs", feature = "wire")))]
#[path = "../../../lgwks-std/tests/support/prop.rs"]
mod prop;

#[cfg(feature = "script")]
#[path = "../support/proposal.rs"]
mod proposal_fixtures;

#[cfg(all(feature = "script", feature = "ephemeral"))]
#[path = "../support/repair.rs"]
mod repair_fixtures;

#[cfg(all(feature = "script", feature = "ephemeral"))]
#[path = "../support/request.rs"]
mod request_fixtures;

#[cfg(all(feature = "script", feature = "ephemeral"))]
#[path = "../support/resume.rs"]
mod resume_fixtures;

#[cfg(feature = "rt")]
#[cfg(feature = "wire")]
#[path = "../support/liveness.rs"]
mod liveness_fixtures;

#[cfg(feature = "script")]
#[path = "../support/run_world.rs"]
mod run_world;

#[path = "../support/scratch.rs"]
mod scratch;

#[cfg(all(feature = "rt", feature = "time", feature = "sync", feature = "script"))]
#[path = "../support/tenancy_harness.rs"]
mod tenancy_harness;

#[path = "../sim/mod.rs"]
mod sim;

#[path = "../support/sweep.rs"]
mod sweep_fixtures;

/// This test binary re-invoked to run the one test named `test` as a probe child.
///
/// One builder for every kill and crash probe, so the executable and the
/// argument shape cannot drift between them. `test` is the libtest name: the
/// test's path below the crate root, which is what a test's own thread is named
/// and what [`probe_test`] builds from a `module_path!()`.
#[cfg(feature = "wire")]
fn probe_command(test: &str) -> Result<std::process::Command, Box<dyn std::error::Error>> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.args([test, "--exact", "--nocapture"]);
    Ok(command)
}

/// The libtest name of `test` declared in `module` (its `module_path!()`).
///
/// libtest names a test by its path below the crate root; the crate's own
/// segment is what `module_path!()` adds and an `--exact` filter must not carry.
#[cfg(feature = "wire")]
fn probe_test(module: &str, test: &str) -> String {
    match module.split_once("::") {
        Some((_crate, path)) => format!("{path}::{test}"),
        None => test.to_owned(),
    }
}

#[cfg(all(feature = "ecs", feature = "wire"))]
mod ambiguous_commit;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod authority;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod bot_front_door;
mod cas;
mod clock_restart;
mod core_boundary;
mod credential;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod dist_lease;
#[cfg(feature = "net")]
#[cfg(feature = "wire")]
mod dist_lease_tcp;
#[cfg(feature = "net")]
mod domain_values;
mod durable_crash_group_commit;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod durable_crash_observation;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod durable_dispatch;
#[cfg(all(feature = "edge", feature = "ephemeral"))]
mod edge;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod effect_identity;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod effect_journal;
mod ephemeral;
#[cfg(feature = "script")]
mod estate;
#[cfg(all(feature = "ecs", feature = "wire", feature = "ron"))]
mod flow_ron;
mod gh_binding;
mod gh_issue;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod grant_replace;
mod inspect;
mod inspect_contract;
mod inspect_non_execution;
mod inspect_scale;
mod inspect_wiring;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod journal_continuation;
#[cfg(feature = "wire")]
mod journal_liveness;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod journal_scale;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod journal_writer_fence;
mod locator_eligibility;
mod net_split;
mod no_default;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod observe_refresh;
#[cfg(feature = "wire")]
mod orphan_reap;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod owner_epoch_takeover;
mod pr_review_journey;
mod process_escape;
mod process_fire;
mod process_ownership;
mod prop_each;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod prop_journal;
mod proposal;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod readme_front_door;
mod ready;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod registry;
mod repair;
mod request_key;
mod resume_liveness;
#[cfg(feature = "net")]
mod retry_proxy;
mod rt_async_tier;
#[cfg(all(feature = "rt", feature = "time", feature = "sync"))]
mod rt_front_door;
mod rt_joinset;
mod rt_process;
mod rt_process_files;
mod rt_process_stdin;
mod rt_runtime_stack;
#[cfg(feature = "script")]
mod run_events;
mod script_domains;
mod script_flow;
#[cfg(feature = "script")]
mod script_front_door;
mod script_map_parity;
mod script_refusals;
mod script_trail;
mod session;
mod session_front_door;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_append_verdict;
mod sim_cancel_under_load;
mod sim_cas;
mod sim_cas_corrupt;
mod sim_cas_disk;
mod sim_cas_execute;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_change_ticks;
mod sim_clock;
#[cfg(feature = "wire")]
mod sim_clock_kill;
mod sim_clock_wiring;
mod sim_confinement;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_continuation;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_continuation_seal;
mod sim_credential;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_dispatch;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_dist_lease;
#[cfg(feature = "net")]
mod sim_dist_lease_tcp;
#[cfg(feature = "ecs")]
mod sim_ecs_backoff;
mod sim_epoch_identity;
#[cfg(feature = "script")]
mod sim_estate;
mod sim_fan_out;
mod sim_gh_issue;
mod sim_group_commit;
mod sim_inspect;
mod sim_inspect_wiring;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_journal;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_journal_liveness;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_journal_tail;
mod sim_keyed;
mod sim_landlock;
mod sim_language;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_network;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_observe_refresh;
mod sim_permit_release;
mod sim_process;
mod sim_process_env;
mod sim_process_orphans;
mod sim_process_output;
mod sim_progress_overflow;
mod sim_proposal;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_readme_front_door;
mod sim_ready;
mod sim_repair;
mod sim_repeat_ceiling;
mod sim_replay_drift;
mod sim_request_key;
#[cfg(feature = "net")]
mod sim_retry_proxy;
mod sim_review_pr;
#[cfg(all(feature = "rt", feature = "time", feature = "sync"))]
mod sim_rt_front_door;
mod sim_run_boundaries;
#[cfg(feature = "script")]
mod sim_run_events;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_scale;
mod sim_script;
mod sim_script_domains;
mod sim_script_fanout;
mod sim_script_feeds;
mod sim_script_steps;
mod sim_script_vocab;
mod sim_script_words;
mod sim_session_front_door;
mod sim_skew;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_source_matrix;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_spec_materialize;
mod sim_stability;
mod sim_step_key_runs;
mod sim_store_faults;
mod sim_store_scale;
mod sim_substrate;
mod sim_supervise_reports;
mod sim_supervise_wait;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_t_rows;
mod sim_t_rows_default;
mod sim_task;
mod sim_task_axes;
mod sim_task_resume;
mod sim_tenancy;
mod sim_tenancy_flood;
mod sim_tenancy_model;
mod sim_tenancy_scope;
mod sim_tenant_delivery;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_tenant_journal_kill;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod sim_tenant_store;
mod skew;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod spec_front_door;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod spec_materialize;
mod stability;
mod store_read_failure;
mod sys_process_binding;
mod sys_process_portable;
mod t02_compile_surface;
mod t22_process_surface;
mod t_rows;
mod t_rows_default;
mod task_front_door;
mod task_million;
mod task_resume;
mod tenancy;
mod tenancy_scale;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod tenant_store;
#[cfg(all(feature = "ecs", feature = "wire"))]
mod wrong_identity_evidence;
