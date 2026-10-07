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

#[path = "../support/declarable.rs"]
mod declarable;

#[path = "../support/effects.rs"]
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

#[cfg(all(feature = "inspect", feature = "script"))]
#[path = "../inspect_support/mod.rs"]
mod inspect_support;

#[path = "../support/journal.rs"]
mod journal_fixtures;

#[cfg(feature = "script")]
#[path = "../support/load.rs"]
mod load;

#[path = "../support/lock.rs"]
mod lock;

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
#[path = "../support/liveness.rs"]
mod liveness_fixtures;

#[path = "../support/scratch.rs"]
mod scratch;

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
fn probe_command(test: &str) -> Result<std::process::Command, Box<dyn std::error::Error>> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.args([test, "--exact", "--nocapture"]);
    Ok(command)
}

/// The libtest name of `test` declared in `module` (its `module_path!()`).
///
/// libtest names a test by its path below the crate root; the crate's own
/// segment is what `module_path!()` adds and an `--exact` filter must not carry.
fn probe_test(module: &str, test: &str) -> String {
    match module.split_once("::") {
        Some((_crate, path)) => format!("{path}::{test}"),
        None => test.to_owned(),
    }
}

mod ambiguous_commit;
mod authority;
mod cas;
mod credential;
mod durable_crash_group_commit;
mod durable_crash_observation;
mod durable_dispatch;
mod effect_identity;
mod effect_journal;
mod ephemeral;
mod flow_ron;
mod gh_binding;
mod inspect;
mod inspect_contract;
mod inspect_non_execution;
mod inspect_scale;
mod inspect_wiring;
mod journal_continuation;
mod journal_liveness;
mod journal_scale;
mod journal_writer_fence;
mod locator_eligibility;
mod no_default;
mod observe_refresh;
mod orphan_reap;
mod owner_epoch_takeover;
mod pr_review_journey;
mod process_escape;
mod process_ownership;
mod prop_each;
mod prop_journal;
mod proposal;
mod ready;
mod registry;
mod repair;
mod request_key;
mod resume_liveness;
mod retry_proxy;
mod rt_async_tier;
mod rt_process;
mod rt_process_files;
mod rt_runtime_stack;
mod script_flow;
mod script_refusals;
mod session;
mod sim_cancel_under_load;
mod sim_cas;
mod sim_change_ticks;
mod sim_clock;
mod sim_clock_kill;
mod sim_clock_wiring;
mod sim_continuation;
mod sim_continuation_seal;
mod sim_credential;
mod sim_dispatch;
mod sim_epoch_identity;
mod sim_group_commit;
mod sim_inspect;
mod sim_inspect_wiring;
mod sim_journal;
mod sim_journal_liveness;
mod sim_journal_tail;
mod sim_network;
mod sim_observe_refresh;
mod sim_process;
mod sim_process_env;
mod sim_process_orphans;
mod sim_process_output;
mod sim_proposal;
mod sim_ready;
mod sim_repair;
mod sim_replay_drift;
mod sim_request_key;
mod sim_retry_proxy;
mod sim_review_pr;
mod sim_run_boundaries;
mod sim_scale;
mod sim_script;
mod sim_skew;
mod sim_source_matrix;
mod sim_spec_materialize;
mod sim_stability;
mod sim_store_faults;
mod sim_store_scale;
mod sim_substrate;
mod sim_supervise_wait;
mod sim_t_rows;
mod sim_task;
mod sim_task_axes;
mod sim_task_resume;
mod sim_tenancy;
mod sim_tenancy_model;
mod sim_tenant_journal_kill;
mod skew;
mod spec_materialize;
mod stability;
mod store_read_failure;
mod sys_process_binding;
mod sys_process_portable;
mod t02_compile_surface;
mod t22_process_surface;
mod t_rows;
mod task_front_door;
mod task_million;
mod task_resume;
mod tenancy;
mod wrong_identity_evidence;
