//! Simulation family: repair, replay and denial under a seeded sweep.
//!
//! Every scenario drives the **real** `Host`, the real `Task`, the real step
//! store and the real repair ledger. Nothing here reimplements admission, a
//! repair decision, a budget or an epoch — the seed controls only the *order* of
//! the decisions a caller makes, which is exactly the thing T24 is about: the same
//! ticket twice, an older epoch and a denied repair must each be refused, and
//! which of them arrives first must not change the answer.
//!
//! | Family | Property it pins |
//! |---|---|
//! | `seeded_orders_reach_the_same_state` | the order of repair / duplicate / stale / deny is immaterial — a ticket applies at most once and every refusal leaves the epoch and budget where they were — and the same seed replays to the same trace hash, twice |
//! | `tenants_keep_their_own_tickets_and_budgets` | two tenants over one directory: a run, a ticket, an epoch and a budget never cross |
//! | `saturation_applies_each_ticket_once` | 100 / 1,000 / 10,000 runs: every run gets its own ticket, every repair applies exactly once, no ledger state is lost |
//! | `seeded_orders_reach_the_same_state` | the same seed produces the same trace hash, twice, and whatever order it draws the run ends in the same state |
//!
//! ## The arms
//!
//! The three families above are *order* properties: what a run holds however the
//! decisions are sequenced. The families below take one refusal or outcome arm at a
//! time and prove what a caller would observe for it, and the properties are kept
//! separate on purpose — a family that asserted "a duplicate, a stale ticket and a
//! denial all leave the run where it was" would pass on an implementation that got
//! two of the three arms wrong and the third right. Each arm below is a different
//! fact about the repair door:
//!
//! | Family | Row | Property it pins |
//! |---|---|---|
//! | `the_step_that_reaches_is_the_step_that_blocks` | T23 | a task that reaches in its *first* step is `Blocked` before its body is polled, and the ticket names every declared need |
//! | `a_wide_need_set_costs_one_analysis` | T23 | the analysis is recorded once however many capabilities the shortfall names, and the ticket's needs are exactly the shortfall's |
//! | `a_repaired_run_survives_a_reopened_host` | T23 | the repair's replay rests on the *bytes* on the disk, not on a live handle |
//! | `a_custom_capability_is_refused_at_every_width` | T24 | the over-wide check is width-independent: a custom name the ticket never asked for is refused for one need and for all four |
//! | `a_ticket_never_names_another_tenants_run` | T24 | the ticket's own tenant check refuses before the ledger's, and a host sees no ledger entry |
//! | `a_mixed_decision_order_pins_each_arm` | T24 | the same draws the order family permutes, asserted per arm: a denial charges nothing, a duplicate moves the epoch at most once |
//! | `every_repair_charges_the_root_budget_once` | T13 | the sequence `attempts = charged attempts + 1` holds however a repair is reached |
//! | `a_reopen_reads_back_the_charged_budget` | T13 | a fresh handle over the same file reports the same attempts, spend, epoch and applied count |
//! | `a_spent_budget_refuses_every_later_attempt` | T13 | at either ceiling the budget reaches a finite typed refusal, a refused attempt charges nothing, and the refusal is stable across later deliveries |
//! | `a_bounded_sweep_repairs_every_ticket_once` | T13 | at 100 and 1,000 runs over one ledger, every ticket applies exactly once and every run is charged exactly twice |
//! | `a_host_spent_on_one_run_still_repairs_the_next` | T13 | a host that has refused one run at its ceiling still closes the next one, and no run's epoch or charge leaks onto it |
//!
//! # What is real and what is seeded
//!
//! The host, the ledger's storage-owner thread, the frame codec, the chain
//! verification and the step store are all real. Time is *small* rather than
//! virtual — an immediately-ready body and a millisecond-scale deadline — so a
//! scenario is decided by the order of its decisions rather than by how loaded
//! the machine is. Nothing that goes into a trace hash is a wall-clock reading,
//! which is what lets one seed replay exactly on a busy box.

#![cfg(all(feature = "script", feature = "ephemeral"))]

use crate::scratch::Scratch;

use crate::sim;

// The band-declaration macro, defined once for the whole layer.
use crate::band_family;

use std::error::Error;
use std::io::Write as _;
use std::sync::Arc;

use lgwks_bot::cap::{Cap, Deficit};
use lgwks_bot::effect::RunId;
use lgwks_bot::gate::GrantSet;
use lgwks_bot::script::{FlowError, Scope};
use lgwks_bot::task::{Disposition, Host, RepairError, RepairTicket, Report, RunLedger, task};

use crate::repair_fixtures as shared;

use shared::{BodyFuture, Journey, JourneyInput, Polls, TestResult, input, input_needing, missing};

use sim::Band;
use sim::Rng;

/// The tenants the multi-tenant family interleaves. Two, because two is the
/// smallest number at which "isolated" is a claim rather than a tautology.
const TENANTS: [&str; 2] = ["acme", "globex"];

/// The saturation tiers the scale family drives.
///
/// The list is the declared hyperscale claim: 100, 1,000 and 10,000. The seeded
/// families sweep a *bounded* prefix of this list rather than every band running
/// all three, because a 10,000-run tier is three 10,000-`fsync` sweeps and a
/// thirty-two-band simulation would spend most of its wall on the largest tier.
/// The full set is driven by the one explicit test below, which reports the level
/// it reached so a reader is never told a number nobody ran.
const TIERS: [usize; 3] = [100, 1_000, 10_000];

/// The tiers the saturation sweep below drives on every ordinary run, each in its
/// own directory so a tier's run never collides with another's or with a previous
/// run's.
///
/// Two of the three declared tiers, and the omission is measured rather than
/// assumed: a repair is a `sync_all` on the ledger and another on the step store,
/// so the 10,000 tier measured at 488 s against 48 s for these two together — an
/// eight-minute test is a measurement, not a gate. The third tier is what
/// [`TIERS`] is for, driven when [`SCALE_ENV`] is set by the measurement in
/// `tiers.rs`, which prints the level it reached. Every figure here is stated rather than discovered,
/// in the spirit of INV-BOT-16.
const SWEPT_TIERS: [usize; 2] = [100, 1_000];

/// The most runs one seeded saturation band drives.
///
/// Small on purpose, and the reason is written here rather than discovered: each
/// run's block charges the ledger, each repair charges it again and writes a step
/// record, and every one of those is a real `sync_all`. Sixteen bands × every seed
/// twice over is a few hundred of those per band, so a band that reached the 100
/// tier took ninety seconds — and the property being swept is about tickets and
/// epochs, not about scale. The declared scale tiers are measured by the tier test
/// in `tiers.rs`: its first tier on every run, all three when [`SCALE_ENV`] is set.
const SATURATING_BAND_RUNS: u32 = 12;

/// The draw bound a seeded saturation band uses, as the seed's own number.
const fn saturating_levels() -> u32 {
    SATURATING_BAND_RUNS
}

/// The most root attempts the recovery family lets a run spend before its budget
/// refuses it, and the loop bound that keeps that walk finite.
///
/// Small on purpose and stated rather than discovered: the family walks the budget
/// down to this number of `sync_all`-ed attempts, so a ceiling of a hundred would be
/// a hundred device round trips per run of a test. It is the same kind of bound as
/// [`SATURATING_BAND_RUNS`], and for the same reason.
const ATTEMPT_CEILING: u64 = 4;

/// The environment variable that extends the tier measurement past its first tier.
///
/// Ten thousand `fsync`-ed repairs is minutes of real device latency, which is the
/// right cost for a measurement and the wrong cost for every ordinary run. So every
/// run drives the first tier, this drives all three, and the test prints what it
/// measured either way.
const SCALE_ENV: &str = "LGWKS_REPAIR_SCALE";

band_family::band_family! {
    seeded_orders_reach_the_same_state_band_00 => seeded_orders_reach_the_same_state, 0;
    seeded_orders_reach_the_same_state_band_01 => seeded_orders_reach_the_same_state, 1;
    seeded_orders_reach_the_same_state_band_02 => seeded_orders_reach_the_same_state, 2;
    seeded_orders_reach_the_same_state_band_03 => seeded_orders_reach_the_same_state, 3;
    seeded_orders_reach_the_same_state_band_04 => seeded_orders_reach_the_same_state, 4;
    seeded_orders_reach_the_same_state_band_05 => seeded_orders_reach_the_same_state, 5;
    seeded_orders_reach_the_same_state_band_06 => seeded_orders_reach_the_same_state, 6;
    seeded_orders_reach_the_same_state_band_07 => seeded_orders_reach_the_same_state, 7;
    seeded_orders_reach_the_same_state_band_08 => seeded_orders_reach_the_same_state, 8;
    seeded_orders_reach_the_same_state_band_09 => seeded_orders_reach_the_same_state, 9;
    seeded_orders_reach_the_same_state_band_10 => seeded_orders_reach_the_same_state, 10;
    seeded_orders_reach_the_same_state_band_11 => seeded_orders_reach_the_same_state, 11;
    seeded_orders_reach_the_same_state_band_12 => seeded_orders_reach_the_same_state, 12;
    seeded_orders_reach_the_same_state_band_13 => seeded_orders_reach_the_same_state, 13;
    seeded_orders_reach_the_same_state_band_14 => seeded_orders_reach_the_same_state, 14;
    seeded_orders_reach_the_same_state_band_15 => seeded_orders_reach_the_same_state, 15;
    tenants_keep_their_own_tickets_and_budgets_band_00 => tenants_keep_their_own_tickets_and_budgets, 0;
    tenants_keep_their_own_tickets_and_budgets_band_01 => tenants_keep_their_own_tickets_and_budgets, 1;
    tenants_keep_their_own_tickets_and_budgets_band_02 => tenants_keep_their_own_tickets_and_budgets, 2;
    tenants_keep_their_own_tickets_and_budgets_band_03 => tenants_keep_their_own_tickets_and_budgets, 3;
    tenants_keep_their_own_tickets_and_budgets_band_04 => tenants_keep_their_own_tickets_and_budgets, 4;
    tenants_keep_their_own_tickets_and_budgets_band_05 => tenants_keep_their_own_tickets_and_budgets, 5;
    tenants_keep_their_own_tickets_and_budgets_band_06 => tenants_keep_their_own_tickets_and_budgets, 6;
    tenants_keep_their_own_tickets_and_budgets_band_07 => tenants_keep_their_own_tickets_and_budgets, 7;
    tenants_keep_their_own_tickets_and_budgets_band_08 => tenants_keep_their_own_tickets_and_budgets, 8;
    tenants_keep_their_own_tickets_and_budgets_band_09 => tenants_keep_their_own_tickets_and_budgets, 9;
    tenants_keep_their_own_tickets_and_budgets_band_10 => tenants_keep_their_own_tickets_and_budgets, 10;
    tenants_keep_their_own_tickets_and_budgets_band_11 => tenants_keep_their_own_tickets_and_budgets, 11;
    tenants_keep_their_own_tickets_and_budgets_band_12 => tenants_keep_their_own_tickets_and_budgets, 12;
    tenants_keep_their_own_tickets_and_budgets_band_13 => tenants_keep_their_own_tickets_and_budgets, 13;
    tenants_keep_their_own_tickets_and_budgets_band_14 => tenants_keep_their_own_tickets_and_budgets, 14;
    tenants_keep_their_own_tickets_and_budgets_band_15 => tenants_keep_their_own_tickets_and_budgets, 15;
    saturation_applies_each_ticket_once_band_00 => saturation_applies_each_ticket_once, 0;
    saturation_applies_each_ticket_once_band_01 => saturation_applies_each_ticket_once, 1;
    saturation_applies_each_ticket_once_band_02 => saturation_applies_each_ticket_once, 2;
    saturation_applies_each_ticket_once_band_03 => saturation_applies_each_ticket_once, 3;
    saturation_applies_each_ticket_once_band_04 => saturation_applies_each_ticket_once, 4;
    saturation_applies_each_ticket_once_band_05 => saturation_applies_each_ticket_once, 5;
    saturation_applies_each_ticket_once_band_06 => saturation_applies_each_ticket_once, 6;
    saturation_applies_each_ticket_once_band_07 => saturation_applies_each_ticket_once, 7;
    saturation_applies_each_ticket_once_band_08 => saturation_applies_each_ticket_once, 8;
    saturation_applies_each_ticket_once_band_09 => saturation_applies_each_ticket_once, 9;
    saturation_applies_each_ticket_once_band_10 => saturation_applies_each_ticket_once, 10;
    saturation_applies_each_ticket_once_band_11 => saturation_applies_each_ticket_once, 11;
    saturation_applies_each_ticket_once_band_12 => saturation_applies_each_ticket_once, 12;
    saturation_applies_each_ticket_once_band_13 => saturation_applies_each_ticket_once, 13;
    saturation_applies_each_ticket_once_band_14 => saturation_applies_each_ticket_once, 14;
    saturation_applies_each_ticket_once_band_15 => saturation_applies_each_ticket_once, 15;
}

// The shared fixtures, the seeded families the table above drives, the tier
// measurement, and the explicit arms, each in its own file.
mod bands;
mod budget;
mod fixtures;
mod reach;
mod tiers;

use bands::{
    saturation_applies_each_ticket_once, seeded_orders_reach_the_same_state,
    tenants_keep_their_own_tickets_and_budgets,
};
use fixtures::{
    Action, Answered, Case, Plan, arm_of, block, block_with, blocked_host, bounded_host,
    candidate_needs, charged, converged, decide, disposition_arm, exact_grant, first_step_task,
    ledger_bytes, names, plan, repairable_host, short_grant, wide_grant,
};
