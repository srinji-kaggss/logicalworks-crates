//! Which trials a run holds: the grid of models, arms, tasks, profiles and
//! trials a mode asks for, less the pairs the benchmark declines, and the name
//! each trial's directory and package go by.

use std::collections::{BTreeMap, BTreeSet};

use super::BenchError;
use super::bench::Bench;
use super::fail;
use super::record::Record;

/// What kind of run this is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mode {
    /// Reference solutions stand in for a model, to prove the plumbing.
    Dry,
    /// Each task's deliberate mutant, to prove the oracle catches it.
    Mutants,
    /// Fixed models author the solutions.
    Models,
}

impl Mode {
    /// The word a run's directory name and manifest carry.
    pub(crate) fn run_label(self) -> &'static str {
        match self {
            Self::Dry => "dry",
            Self::Mutants => "mutants",
            Self::Models => "models",
        }
    }

    /// The word a trial's record carries.
    pub(crate) fn record_label(self) -> &'static str {
        match self {
            Self::Dry => "dry",
            Self::Mutants => "mutant",
            Self::Models => "model",
        }
    }
}

/// What the command line chose to run.
pub(crate) struct Selection {
    /// The models, for a models run.
    pub(crate) models: Vec<String>,
    /// The arms; empty means every arm.
    pub(crate) arms: Vec<String>,
    /// The tasks; empty means every task.
    pub(crate) tasks: Vec<String>,
    /// The profiles; empty means every profile.
    pub(crate) profiles: Vec<String>,
    /// How many trials each cell holds.
    pub(crate) trials: u32,
}

/// One trial to run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Job {
    /// What kind of run it belongs to.
    pub(crate) mode: Mode,
    /// The model, or a stand-in label for a dry or mutant run.
    pub(crate) model: String,
    /// The arm.
    pub(crate) arm: String,
    /// The task.
    pub(crate) task: String,
    /// The profile; empty for a mutant.
    pub(crate) profile: String,
    /// Which trial of its cell.
    pub(crate) trial: u32,
}

/// A cell the benchmark does not run, and why.
pub(crate) struct Skipped {
    /// The arm.
    pub(crate) arm: String,
    /// The task.
    pub(crate) task: String,
    /// Why the arm cannot express the task.
    pub(crate) why: String,
}

/// The trials of a run and the cells left out.
pub(crate) struct Plan {
    /// The trials, in grid order.
    pub(crate) jobs: Vec<Job>,
    /// The cells left out.
    pub(crate) skipped: Vec<Skipped>,
}

/// The label a dry run files its trials under.
pub(crate) const DRY_MODEL: &str = "__dry-run__";

/// The label a mutant run files its trials under.
pub(crate) const MUTANT_MODEL: &str = "__mutant__";

/// `names` if it names only known items, else a refusal; every item when empty.
fn chosen(
    kind: &str,
    names: &[String],
    every: Vec<String>,
) -> Result<Vec<String>, BenchError> {
    if names.is_empty() {
        return Ok(every);
    }
    if let Some(unknown) = names.iter().find(|name| !every.contains(name)) {
        return fail(
            BenchError::Usage(format!(
                "ai-authoring: unknown {kind} {unknown:?}; the manifest declares {}",
                every.join(", ")
            )),
            "chosen",
        );
    }
    Ok(names.to_vec())
}

/// The trials `mode` asks for under `selection`.
pub(crate) fn plan(bench: &Bench, mode: Mode, selection: &Selection) -> Result<Plan, BenchError> {
    let arms = chosen(
        "arm",
        &selection.arms,
        bench.arms.iter().map(|arm| arm.name.clone()).collect(),
    )?;
    let tasks = chosen(
        "task",
        &selection.tasks,
        bench.tasks.iter().map(|task| task.name.clone()).collect(),
    )?;
    let profiles = chosen(
        "profile",
        &selection.profiles,
        bench
            .profiles
            .iter()
            .map(|profile| profile.name.clone())
            .collect(),
    )?;
    let mut jobs = Vec::new();
    match mode {
        Mode::Mutants => {
            for task in &tasks {
                jobs.push(Job {
                    mode,
                    model: MUTANT_MODEL.to_owned(),
                    arm: bench.mutant_arm.clone(),
                    task: task.clone(),
                    profile: String::new(),
                    trial: 0,
                });
            }
        }
        Mode::Dry => {
            // A canned solution never reads a persona, so the first profile
            // stands for all of them: five copies would be one trial under five
            // labels.
            for arm in &arms {
                for task in &tasks {
                    for profile in profiles.iter().take(1) {
                        for trial in 0..selection.trials {
                            jobs.push(Job {
                                mode,
                                model: DRY_MODEL.to_owned(),
                                arm: arm.clone(),
                                task: task.clone(),
                                profile: profile.clone(),
                                trial,
                            });
                        }
                    }
                }
            }
        }
        Mode::Models => {
            for model in &selection.models {
                for arm in &arms {
                    for task in &tasks {
                        for profile in &profiles {
                            for trial in 0..selection.trials {
                                jobs.push(Job {
                                    mode,
                                    model: model.clone(),
                                    arm: arm.clone(),
                                    task: task.clone(),
                                    profile: profile.clone(),
                                    trial,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    jobs.retain(|job| !bench.excludes(&job.arm, &job.task));
    let mut seen = BTreeSet::new();
    for job in &jobs {
        let id = trial_id(job);
        if !seen.insert(id.clone()) {
            return fail(
                BenchError::Usage(format!(
                    "ai-authoring: two trials would share the directory {id}; model ids that differ only in characters a directory name cannot hold collide"
                )),
                "plan",
            );
        }
    }
    let skipped = bench
        .exclusions
        .iter()
        .filter(|exclusion| tasks.contains(&exclusion.task))
        .map(|exclusion| Skipped {
            arm: exclusion.arm.clone(),
            task: exclusion.task.clone(),
            why: exclusion.why.clone(),
        })
        .collect();
    Ok(Plan { jobs, skipped })
}

/// Splits a plan's jobs into the records a resume kept and the jobs still to run.
///
/// A job is kept when its trial id has a finished record; every other job runs.
/// The two halves partition the plan: no job is both, and none is lost.
pub(crate) fn split_done(
    jobs: Vec<Job>,
    done: &BTreeMap<String, Record>,
) -> (Vec<Record>, Vec<Job>) {
    let mut kept = Vec::new();
    let mut remaining = Vec::new();
    for job in jobs {
        match done.get(&trial_id(&job)) {
            Some(record) => kept.push(record.clone()),
            None => remaining.push(job),
        }
    }
    (kept, remaining)
}

/// `text` with everything outside `[A-Za-z0-9_.-]` replaced by `_`.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// The trial's directory name and package seed.
///
/// The profile is in the id, so two profiles of one cell never share a package
/// name and so never share cargo's `-C metadata`: a shared one would let one
/// trial's oracle binary stand in for another's.
pub(crate) fn trial_id(job: &Job) -> String {
    let profile = if job.profile.is_empty() {
        "none"
    } else {
        job.profile.as_str()
    };
    format!(
        "{}__{}__{}__{profile}__t{}",
        sanitize(&job.model),
        job.arm,
        job.task,
        job.trial
    )
}

/// The longest package name a trial crate is given.
const PACKAGE_LIMIT: usize = 120;

/// A cargo-valid, per-trial package name that starts with `prefix`.
pub(crate) fn package_for(prefix: &str, id: &str) -> String {
    let suffix: String = id
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || ch == '_' { ch } else { '_' })
        .collect();
    format!("{prefix}_{suffix}")
        .chars()
        .take(PACKAGE_LIMIT)
        .collect()
}
