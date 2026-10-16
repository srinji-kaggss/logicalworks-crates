//! A run: plan the trials, run them under a bounded pool of workers, keep every
//! record durably the moment its trial ends, and write the results.
//!
//! Every worker is owned by the scope that starts it, so none outlives the run,
//! and every child process a worker starts is owned by the run's `Children`, so
//! none outlives the run either.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;

use lgwks_std::json::Value;

use super::author::{CliAuthor, ensure_sandbox, full_prompt};
use super::bench::Bench;
use super::cargo::Cargo;
use super::children::{Children, Launch, guard, inherited_env};
use super::cli::Options;
use super::grade::Verdict;
use super::plan::{Job, Mode, Selection, plan, split_done, trial_id};
use super::record::Record;
use super::scaffold::{
    CargoJudge, mutant_text, oracle_for, place_reference, prepare, prompt_texts, reference_text,
    write_outputs, write_scratch,
};
use super::stats::wide;
use super::store;
use super::summary::{protocol, render, summarize};
use super::trial::{Inputs, Start, run_trial};
use super::{BenchError, Console, GENERATED_BY, fail};

/// A UTC stamp without its separators, as a directory name: `20261016T120000Z`.
fn compact(stamp: &str) -> String {
    stamp
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect()
}

/// `chosen`, or `default` when the command line did not choose.
fn or_default<T>(chosen: Option<T>, default: T) -> T {
    match chosen {
        Some(found) => found,
        None => default,
    }
}

/// The commit the benchmark's checkout is at.
///
/// A record names the commit it ran at, because trials graded against two
/// different crate trees are not one measurement.
fn head_commit(bench: &Bench, children: &Children) -> Result<String, BenchError> {
    let root = bench.root.display().to_string();
    let argv: Vec<String> = ["git", "rev-parse", "HEAD"]
        .iter()
        .map(|part| (*part).to_owned())
        .collect();
    let done = children.capture(&Launch {
        argv: &argv,
        root: &bench.root,
        env: &inherited_env(),
        stdin: None,
        bound: bench.limits.git,
    });
    let commit = done.stdout.trim().to_owned();
    if done.code == 0 && !commit.is_empty() {
        return Ok(commit);
    }
    fail(
        BenchError::Refused(format!(
            "{root} is not in a git checkout, so a run cannot name the commit it ran at"
        )),
        "head_commit",
    )
}

/// What a run needs that the command line may have chosen.
struct Choices {
    /// How many repairs a failing compile gets.
    max_repairs: u32,
    /// The model command.
    command: String,
    /// How many trials run at once.
    parallel: usize,
    /// What the planner is asked for.
    selection: Selection,
}

/// Resolves the command line against the manifest's defaults.
fn choose(bench: &Bench, options: &Options) -> Choices {
    let models = if options.models.is_empty() {
        bench.defaults.models.clone()
    } else {
        options.models.clone()
    };
    Choices {
        max_repairs: or_default(options.max_repairs, bench.defaults.max_repairs),
        command: match options.command {
            Some(ref given) => given.clone(),
            None => bench.model.command.clone(),
        },
        parallel: or_default(options.parallel, bench.defaults.parallel),
        selection: Selection {
            models,
            arms: options.arms.clone(),
            tasks: options.tasks.clone(),
            profiles: options.profiles.clone(),
            trials: or_default(options.trials, bench.defaults.trials),
        },
    }
}

/// Where a run keeps its results and its trials.
struct Places {
    /// The results directory.
    dir: PathBuf,
    /// The trial work root.
    work: PathBuf,
    /// The trials a resume found finished.
    done: BTreeMap<String, Record>,
    /// Lines of the log a resume dropped.
    torn: usize,
}

/// `path` made absolute against the working directory, naming it when it cannot be.
fn absolute(path: &Path) -> Result<PathBuf, BenchError> {
    match std::path::absolute(path) {
        Ok(found) => Ok(found),
        Err(cause) => fail(
            BenchError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "absolute",
        ),
    }
}

/// Opens a fresh run, or reopens the interrupted one `--resume` names.
fn places(
    bench: &Bench,
    options: &Options,
    mode: Mode,
    stamp: &str,
    commit: &str,
) -> Result<Places, BenchError> {
    if let Some(ref interrupted) = options.resume {
        let dir = match std::fs::canonicalize(interrupted) {
            Ok(found) => found,
            Err(cause) => {
                return fail(
                    BenchError::Io {
                        path: interrupted.clone(),
                        cause,
                    },
                    "places",
                );
            }
        };
        let resumed = store::resume(&dir, mode, commit)?;
        return Ok(Places {
            dir,
            work: resumed.work,
            done: resumed.done,
            torn: resumed.torn,
        });
    }
    let label = format!("{}-{}", compact(stamp), mode.run_label());
    let work = match options.work {
        Some(ref given) => absolute(given)?,
        None => std::env::temp_dir().join(format!("lgwks-ai-authoring-{label}")),
    };
    let dir = match options.out {
        Some(ref given) => absolute(given)?,
        None => absolute(&bench.path(&bench.layout.runs).join(&label))?,
    };
    store::begin(&dir, &work, mode, commit)?;
    Ok(Places {
        dir,
        work,
        done: BTreeMap::new(),
        torn: 0,
    })
}

/// The pieces a run's workers share.
struct Shared<'run> {
    /// The benchmark.
    bench: &'run Bench,
    /// The mode.
    mode: Mode,
    /// The results directory.
    dir: &'run Path,
    /// The trial work root.
    work: &'run Path,
    /// Cargo, serialized.
    cargo: &'run Cargo<'run>,
    /// The children the run owns.
    children: &'run Children,
    /// Where lines are printed.
    console: &'run Console,
    /// The commit the run is at.
    commit: &'run str,
    /// The model command.
    command: &'run str,
    /// Repairs a failing compile gets.
    max_repairs: u32,
    /// The trials still to run.
    queue: Mutex<VecDeque<Job>>,
    /// Every trial this run has finished.
    records: Mutex<Vec<Record>>,
    /// Why a worker stopped early.
    errors: Mutex<Vec<BenchError>>,
}

/// Runs one trial to its record.
fn execute(shared: &Shared<'_>, job: &Job) -> Result<Record, BenchError> {
    let bench = shared.bench;
    let (Some(task), Some(arm)) = (bench.task(&job.task), bench.arm(&job.arm)) else {
        return fail(
            BenchError::Usage(format!("{}/{} is not declared", job.arm, job.task)),
            "execute",
        );
    };
    let dir = prepare(bench, shared.work, job)?;
    let texts = prompt_texts(bench, job)?;
    write_scratch(&dir, &texts)?;
    let author = CliAuthor {
        bench,
        command: shared.command,
        model: &job.model,
        scratch: dir.scratch(),
        work: shared.work,
        children: shared.children,
        bound: bench.limits.model_call,
    };
    let start = match shared.mode {
        Mode::Models => Start::Model {
            author: &author,
            prompt: full_prompt(&bench.framing, &texts),
        },
        Mode::Dry => Start::Reference(reference_text(
            bench,
            &bench.reference_surface(arm, &job.task),
            &job.task,
        )?),
        Mode::Mutants => {
            place_reference(bench, &dir, job)?;
            Start::Reference(mutant_text(bench, &job.task)?)
        }
    };
    let (oracle, clauses) = oracle_for(bench, &job.task, &dir.package)?;
    let judge = CargoJudge {
        cargo: shared.cargo,
        dir: &dir,
        oracle,
    };
    let finished = run_trial(
        Inputs {
            bench,
            job,
            task,
            trial_id: dir.id.clone(),
            clauses,
            max_repairs: shared.max_repairs,
            commit: shared.commit,
            start,
        },
        &judge,
    )?;
    write_outputs(
        &dir,
        &finished.code,
        &finished.raw,
        &finished.build_log,
        &finished.test_output,
    )?;
    Ok(finished.record)
}

/// One finished trial's progress line.
fn progress(record: &Record) -> String {
    format!(
        "{:28} {:8} {:14} {:13} t{} compiled={} oracle={}/{}",
        record.model,
        record.arm,
        record.task,
        if record.profile.is_empty() {
            "-"
        } else {
            record.profile.as_str()
        },
        record.trial,
        record.compiled,
        record.pass_count(),
        record.total()
    )
}

/// Runs trials from the queue until it is empty or the run is stopping.
///
/// A trial that cannot be run is a recorded crash, never a missing record; a
/// record that cannot be kept stops the worker, because a run that cannot keep
/// its evidence has no business producing more of it.
fn work_through(shared: &Shared<'_>) {
    loop {
        if shared.children.is_stopping() {
            return;
        }
        let Some(job) = guard(&shared.queue).pop_front() else {
            return;
        };
        let record = match execute(shared, &job) {
            Ok(found) => found,
            Err(error) => Record::crashed(
                &job,
                shared.commit,
                &shared.bench.tokens,
                error.to_string(),
            ),
        };
        if let Err(error) = store::append(shared.dir, &record) {
            guard(&shared.errors).push(error);
            return;
        }
        shared.console.say(&progress(&record));
        guard(&shared.records).push(record);
    }
}

/// The mutants no oracle clause caught, which are a hole in the oracle.
fn report_mutants(console: &Console, records: &[Record]) -> usize {
    console.say("\nmutant verdicts (which oracle clause each mutant fails):");
    let mut survivors = 0_usize;
    for record in records {
        let failed: Vec<&str> = record
            .oracle
            .iter()
            .filter(|clause| *clause.1 == Verdict::Fail)
            .map(|clause| clause.0.as_str())
            .collect();
        let ran = record.pass_count().saturating_add(failed.len());
        let caught = record.compiled && !failed.is_empty();
        if !caught {
            survivors = survivors.saturating_add(1);
        }
        let named = if caught {
            failed.join(", ")
        } else if record.compiled {
            "(none — the oracle did NOT catch the mutant)".to_owned()
        } else {
            "(the mutant did not compile, so it proves nothing)".to_owned()
        };
        console.say(&format!(
            "  {:14} failed {}/{ran}: {named}",
            record.task,
            failed.len()
        ));
    }
    survivors
}

/// Runs the trials `mode` asks for and writes the run's results; the code is the
/// exit status.
pub(super) fn run(
    bench: &Bench,
    options: &Options,
    mode: Mode,
    stamp: &str,
    console: &Console,
    children: &Children,
) -> Result<u8, BenchError> {
    let choices = choose(bench, options);
    if mode == Mode::Models {
        ensure_sandbox(bench)?;
    }
    let planned = plan(bench, mode, &choices.selection)?;
    let commit = head_commit(bench, children)?;
    let places = places(bench, options, mode, stamp, &commit)?;
    let (mut records, remaining) = split_done(planned.jobs, &places.done);
    for skipped in &planned.skipped {
        console.say(&format!(
            "skipped {}/{}: {}",
            skipped.task, skipped.arm, skipped.why
        ));
    }
    if options.resume.is_some() {
        let ids: Vec<String> = remaining.iter().map(trial_id).collect();
        let moved = store::set_aside(&places.work, &ids)?;
        console.say(&format!(
            "resuming: {} trial(s) kept, {} to run, {moved} interrupted directory(ies) set aside, {} torn log line(s) dropped",
            records.len(),
            remaining.len(),
            places.torn
        ));
    }
    let cargo = Cargo::new(
        &places.work,
        children,
        bench.limits.build,
        bench.limits.oracle,
    );
    let workers = choices.parallel.max(1).min(remaining.len().max(1));
    let shared = Shared {
        bench,
        mode,
        dir: &places.dir,
        work: &places.work,
        cargo: &cargo,
        children,
        console,
        commit: &commit,
        command: &choices.command,
        max_repairs: choices.max_repairs,
        queue: Mutex::new(remaining.into()),
        records: Mutex::new(Vec::new()),
        errors: Mutex::new(Vec::new()),
    };
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| work_through(&shared));
        }
    });
    let Shared {
        records: ran,
        errors,
        ..
    } = shared;
    let mut stopped = match errors.into_inner() {
        Ok(found) => found,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(first) = stopped.pop() {
        return Err(first);
    }
    records.extend(match ran.into_inner() {
        Ok(found) => found,
        Err(poisoned) => poisoned.into_inner(),
    });
    records.sort_by(|left, right| {
        (&left.model, &left.arm, &left.task, &left.profile, left.trial).cmp(&(
            &right.model,
            &right.arm,
            &right.task,
            &right.profile,
            right.trial,
        ))
    });
    let profiles: Vec<String> = if options.profiles.is_empty() {
        bench.profiles.iter().map(|item| item.name.clone()).collect()
    } else {
        options.profiles.clone()
    };
    let summary = render(&[
        ("generated_by", Value::from(GENERATED_BY)),
        ("lgwks_bot_sha", Value::from(commit.as_str())),
        (
            "protocol",
            protocol(bench, &profiles, choices.max_repairs, &planned.skipped),
        ),
        ("groups", Value::Array(summarize(&records))),
    ]);
    store::finish(&places.dir, &records, &summary)?;
    console.say(&format!("\nwrote {}", places.dir.join("results.jsonl").display()));
    console.say(&format!("wrote {}", places.dir.join("summary.json").display()));
    let crashed = records.iter().filter(|record| record.error.is_some()).count();
    if crashed > 0 {
        console.warn(&format!(
            "ai-authoring: {} trial(s) crashed and are recorded as such",
            wide(crashed)
        ));
    }
    let survivors = if mode == Mode::Mutants {
        report_mutants(console, &records)
    } else {
        0
    };
    Ok(u8::from(crashed > 0 || survivors > 0))
}
