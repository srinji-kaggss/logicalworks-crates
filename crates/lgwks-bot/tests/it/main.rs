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

#[cfg(any(all(feature = "script", feature = "ephemeral"), feature = "rt"))]
#[path = "../support/resume.rs"]
mod resume_fixtures;

#[path = "../sim/mod.rs"]
mod sim;

mod ambiguous_commit;
mod authority;
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
mod journal_liveness;
mod journal_scale;
mod locator_eligibility;
mod no_default;
mod observe_refresh;
mod pr_review_journey;
mod process_ownership;
mod prop_each;
mod prop_journal;
mod proposal;
mod ready;
mod registry;
mod repair;
mod request_key;
mod resume_liveness;
mod rt_async_tier;
mod rt_process;
mod rt_process_files;
mod rt_runtime_stack;
mod script_flow;
mod script_refusals;
mod session;
mod sim_clock;
mod sim_clock_wiring;
mod sim_dispatch;
mod sim_epoch_identity;
mod sim_group_commit;
mod sim_inspect;
mod sim_inspect_wiring;
mod sim_journal;
mod sim_journal_liveness;
mod sim_network;
mod sim_observe_refresh;
mod sim_process;
mod sim_process_output;
mod sim_proposal;
mod sim_ready;
mod sim_repair;
mod sim_replay_drift;
mod sim_request_key;
mod sim_review_pr;
mod sim_run_boundaries;
mod sim_scale;
mod sim_script;
mod sim_source_matrix;
mod sim_spec_materialize;
mod sim_store_faults;
mod sim_store_scale;
mod sim_task;
mod sim_task_axes;
mod sim_task_resume;
mod spec_materialize;
mod store_read_failure;
mod sys_process_binding;
mod sys_process_portable;
mod t02_compile_surface;
mod t22_process_surface;
mod task_front_door;
mod task_million;
mod wrong_identity_evidence;
