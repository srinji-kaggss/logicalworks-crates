//! Simulated plans: random selections over the committed benchmark are planned
//! and the grid is held to an independent count, its declined pairs, its trial
//! ids and the way a resume splits it.
//!
//! The plan is what a run promises to do, so a wrong count is a wrong
//! experiment: the first matrix of this benchmark ran cells its sheets could not
//! express, and a trial directory that two jobs share is a trial that never ran.

use std::collections::{BTreeMap, BTreeSet};

use super::bench::Bench;
use super::plan::{
    DRY_MODEL, MUTANT_MODEL, Mode, Selection, package_for, plan, split_done, trial_id,
};
use super::replay::{Failure, replay_sweep};
use super::seed::Rng;
use super::sim_kit::{MODELS, committed, ensure, one_of, record};

/// Every mode a run can be.
const MODES: [Mode; 3] = [Mode::Dry, Mode::Mutants, Mode::Models];

/// A non-empty subset of `names`, each kept by a coin, in their order.
fn subset(rng: &mut Rng, names: &[String]) -> Vec<String> {
    let mut chosen: Vec<String> = Vec::new();
    for name in names {
        if rng.coin() {
            chosen.push(name.clone());
        }
    }
    if chosen.is_empty()
        && let Some(first) = names.first()
    {
        chosen.push(first.clone());
    }
    chosen
}

/// A selection over `bench`, drawn from the sequence.
fn selection(rng: &mut Rng, bench: &Bench) -> Selection {
    let models: Vec<String> = MODELS.iter().map(|model| (*model).to_owned()).collect();
    let arms: Vec<String> = bench.arms.iter().map(|arm| arm.name.clone()).collect();
    let tasks: Vec<String> = bench.tasks.iter().map(|task| task.name.clone()).collect();
    let profiles: Vec<String> = bench
        .profiles
        .iter()
        .map(|profile| profile.name.clone())
        .collect();
    Selection {
        models: subset(rng, &models),
        arms: subset(rng, &arms),
        tasks: subset(rng, &tasks),
        profiles: subset(rng, &profiles),
        trials: rng.between(1, 3),
    }
}

/// How many trials `mode` asks for under `chosen`, counted from the declared
/// exclusions and nothing the planner computed.
fn expected_jobs(bench: &Bench, mode: Mode, chosen: &Selection) -> Result<usize, Failure> {
    let allowed = chosen
        .arms
        .iter()
        .flat_map(|arm| chosen.tasks.iter().map(move |task| (arm, task)))
        .filter(|pair| !bench.excludes(pair.0, pair.1))
        .count();
    let trials = usize::try_from(chosen.trials)?;
    Ok(match mode {
        Mode::Mutants => chosen.tasks.len(),
        Mode::Dry => allowed.saturating_mul(trials),
        Mode::Models => allowed
            .saturating_mul(chosen.models.len())
            .saturating_mul(chosen.profiles.len())
            .saturating_mul(trials),
    })
}

/// Requirement: the grid is every (model, arm, task, profile, trial) the selection
/// names less the pairs the benchmark declines, and each declined pair is
/// reported with its reason (the first matrix asked models for capabilities a
/// sheet did not offer; Director, #270).
#[test]
fn the_grid_is_every_selected_cell_less_the_pairs_the_benchmark_declines() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("grid", 300, |rng, trace| {
        let chosen = selection(rng, &bench);
        let mode = *one_of(rng, "modes", &MODES)?;
        let planned = plan(&bench, mode, &chosen)?;
        let want = expected_jobs(&bench, mode, &chosen)?;
        ensure(planned.jobs.len() == want, || {
            format!(
                "{} trials planned for {:?}, expected {want}",
                planned.jobs.len(),
                mode
            )
        })?;
        ensure(
            planned
                .jobs
                .iter()
                .all(|found| !bench.excludes(&found.arm, &found.task)),
            || "a declined pair was planned".to_owned(),
        )?;
        let declined: Vec<(String, String)> = bench
            .exclusions
            .iter()
            .filter(|found| chosen.tasks.contains(&found.task))
            .map(|found| (found.arm.clone(), found.task.clone()))
            .collect();
        let skipped: Vec<(String, String)> = planned
            .skipped
            .iter()
            .map(|found| (found.arm.clone(), found.task.clone()))
            .collect();
        ensure(skipped == declined, || {
            format!("skipped {skipped:?} but the benchmark declines {declined:?}")
        })?;
        ensure(
            planned.skipped.iter().all(|found| !found.why.is_empty()),
            || "a declined pair carries no reason".to_owned(),
        )?;
        trace.record_number("jobs", planned.jobs.len());
        for found in &planned.jobs {
            trace.record(&trial_id(found));
        }
        Ok(())
    })
}

/// Requirement: a dry run is one canned solution per cell under the first
/// profile, since a canned solution never reads a persona and five copies would
/// be one trial under five labels; a mutant run is one trial per task, filed
/// under the mutant arm and no profile.
#[test]
fn a_dry_run_holds_one_profile_and_a_mutant_run_one_trial_per_task() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("shapes", 150, |rng, trace| {
        let chosen = selection(rng, &bench);
        let first = chosen
            .profiles
            .first()
            .ok_or("a selection with no profile")?;
        let dry = plan(&bench, Mode::Dry, &chosen)?;
        ensure(
            dry.jobs.iter().all(|found| {
                found.model == DRY_MODEL && &found.profile == first && found.trial < chosen.trials
            }),
            || "a dry trial names another model or profile, or a trial past the count".to_owned(),
        )?;
        let mutants = plan(&bench, Mode::Mutants, &chosen)?;
        let tasks: Vec<&String> = mutants.jobs.iter().map(|found| &found.task).collect();
        ensure(tasks == chosen.tasks.iter().collect::<Vec<_>>(), || {
            "a mutant run is not one trial per selected task, in order".to_owned()
        })?;
        ensure(
            mutants.jobs.iter().all(|found| {
                found.model == MUTANT_MODEL
                    && found.arm == bench.mutant_arm
                    && found.profile.is_empty()
                    && found.trial == 0
            }),
            || "a mutant trial is not filed under the mutant arm and no profile".to_owned(),
        )?;
        trace.record_number("dry", dry.jobs.len());
        trace.record_number("mutants", mutants.jobs.len());
        Ok(())
    })
}

/// Requirement: every trial has a directory and a package of its own, so no
/// two trials share a cargo `-C metadata` and one trial's oracle binary can never
/// stand in for another's. Names stay inside `[A-Za-z0-9_.-]` for directories,
/// `[A-Za-z0-9_]` for packages (the Cargo Book, "The name field"), and under
/// 121 characters.
#[test]
fn every_trial_has_a_directory_and_a_package_name_of_its_own() -> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("names", 3, |rng, trace| {
        let chosen = Selection {
            models: MODELS.iter().map(|model| (*model).to_owned()).collect(),
            arms: Vec::new(),
            tasks: Vec::new(),
            profiles: Vec::new(),
            trials: rng.between(1, 3),
        };
        let planned = plan(&bench, Mode::Models, &chosen)?;
        let mut directories = BTreeSet::new();
        let mut packages = BTreeSet::new();
        for found in &planned.jobs {
            let id = trial_id(found);
            let package = package_for(&bench.package_prefix, &id);
            ensure(
                id.chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-')),
                || format!("{id} is not a safe directory name"),
            )?;
            ensure(
                package.starts_with(&bench.package_prefix)
                    && package
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                    && package.len() < 121,
                || format!("{package} is not a cargo package name"),
            )?;
            ensure(
                directories.insert(id.clone()) && packages.insert(package),
                || format!("{id} shares a directory or package with another trial"),
            )?;
        }
        ensure(
            directories.len() == planned.jobs.len() && !planned.jobs.is_empty(),
            || "the grid is empty or lost a trial".to_owned(),
        )?;
        trace.record_number("trials", planned.jobs.len());
        Ok(())
    })
}

/// Requirement: two trials that would share a directory are refused before any
/// runs. Model ids that differ only in characters a directory name cannot hold
/// (`a/b` and `a_b`), or a model named twice, would otherwise run one of them
/// into the other's evidence.
#[test]
fn model_ids_that_would_share_a_directory_are_refused_before_anything_runs() -> Result<(), Failure>
{
    let bench = committed()?;
    replay_sweep("collisions", 40, |rng, trace| {
        let word = format!("m{}", rng.below(1000));
        for models in [
            vec![format!("{word}/x"), format!("{word}_x")],
            vec![word.clone(), word.clone()],
        ] {
            let chosen = Selection {
                models,
                arms: vec!["new".to_owned()],
                tasks: vec!["aggregate".to_owned()],
                profiles: vec!["agent".to_owned()],
                trials: 1,
            };
            let said = match plan(&bench, Mode::Models, &chosen) {
                Ok(_) => {
                    return Err("two trials shared a directory and the plan was allowed".into());
                }
                Err(error) => error.to_string(),
            };
            ensure(said.contains("share the directory"), || {
                format!("the refusal said `{said}`")
            })?;
        }
        trace.record(&word);
        Ok(())
    })
}

/// Requirement: a name the manifest does not declare is a usage error that lists
/// the names it does, never a quietly empty run.
#[test]
fn a_name_the_manifest_does_not_declare_is_refused_listing_the_ones_it_does() -> Result<(), Failure>
{
    let bench = committed()?;
    replay_sweep("unknown names", 40, |rng, trace| {
        let word = format!("nonesuch{}", rng.below(1000));
        let base = selection(rng, &bench);
        for kind in ["arm", "task", "profile"] {
            let mut chosen = Selection {
                models: base.models.clone(),
                arms: base.arms.clone(),
                tasks: base.tasks.clone(),
                profiles: base.profiles.clone(),
                trials: base.trials,
            };
            let declared: Vec<&str> = match kind {
                "arm" => {
                    chosen.arms = vec![word.clone()];
                    bench.arms.iter().map(|found| found.name.as_str()).collect()
                }
                "task" => {
                    chosen.tasks = vec![word.clone()];
                    bench
                        .tasks
                        .iter()
                        .map(|found| found.name.as_str())
                        .collect()
                }
                _ => {
                    chosen.profiles = vec![word.clone()];
                    bench
                        .profiles
                        .iter()
                        .map(|found| found.name.as_str())
                        .collect()
                }
            };
            let said = match plan(&bench, Mode::Dry, &chosen) {
                Ok(_) => return Err(format!("an undeclared {kind} was planned").into()),
                Err(error) => error.to_string(),
            };
            ensure(
                said.contains(&format!("unknown {kind} {word:?}"))
                    && declared.iter().all(|name| said.contains(name)),
                || format!("the refusal for an unknown {kind} said `{said}`"),
            )?;
        }
        trace.record(&word);
        Ok(())
    })
}

/// Requirement: a resume partitions the plan. The trials with a finished record
/// are kept, in plan order, with the records they have; every other trial runs;
/// none is both and none is lost.
#[test]
fn a_resume_splits_the_plan_into_the_trials_it_kept_and_the_trials_left_to_run()
-> Result<(), Failure> {
    let bench = committed()?;
    replay_sweep("split", 200, |rng, trace| {
        let chosen = selection(rng, &bench);
        let planned = plan(&bench, Mode::Models, &chosen)?;
        let mut done = BTreeMap::new();
        let mut kept = Vec::new();
        let mut left = Vec::new();
        for found in &planned.jobs {
            if rng.coin() {
                let line = record(rng, &bench, found)?;
                done.insert(line.trial_id.clone(), line.clone());
                kept.push(line);
            } else {
                left.push(found.clone());
            }
        }
        let total = planned.jobs.len();
        let (finished, remaining) = split_done(planned.jobs, &done);
        ensure(finished == kept && remaining == left, || {
            "the split is not the finished records and the unfinished trials, in plan order"
                .to_owned()
        })?;
        ensure(
            finished.len().saturating_add(remaining.len()) == total,
            || "the split lost or repeated a trial".to_owned(),
        )?;
        trace.record_number("total", total);
        trace.record_number("kept", finished.len());
        Ok(())
    })
}
