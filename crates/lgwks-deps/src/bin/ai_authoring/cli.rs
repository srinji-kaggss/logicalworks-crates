//! The command line.

use std::path::PathBuf;

use super::plan::Mode;
use super::{BenchError, fail};

/// The benchmark directory when `--bench` is not given, relative to the
/// repository root the command is run from.
const DEFAULT_BENCH: &str = "bench/ai-authoring";

/// The usage text.
pub(super) const USAGE: &str = "\
ai-authoring: the AI authoring benchmark's runner. Say what to run:

  ai-authoring --dry-run            canned reference solutions stand in for a model (free)
  ai-authoring --mutants            each task's deliberate mutant: does the oracle catch it? (free)
  ai-authoring --tables RUN_DIR     the README tables for a committed run (read-only, free)
  ai-authoring --matrix             the model matrix: it calls the models and costs money

  --bench DIR       the benchmark directory (default bench/ai-authoring)
  --models A,B      model ids (default: the manifest's)
  --apis A,B        arms (default: every arm)
  --tasks A,B       tasks (default: every task)
  --profiles A,B    profiles (default: every profile)
  --trials N        trials per cell (default: the manifest's)
  --parallel N      trials in flight (default: the manifest's)
  --max-repairs N   compiler repairs a trial gets (default: the manifest's)
  --cmd PROGRAM     the model command (default: the manifest's)
  --out DIR         results directory (default: <bench>/runs/<UTC stamp>-<mode>)
  --work DIR        trial work root (default: a fresh directory under $TMPDIR)
  --resume RUN_DIR  finish an interrupted run, at the same commit
  --help            this text
";

/// What the command was asked to do.
pub(super) enum Action {
    /// Print the usage.
    Help,
    /// Print the tables of a committed run.
    Tables(PathBuf),
    /// Run trials.
    Run(Mode),
}

/// What the command line chose.
pub(super) struct Options {
    /// The benchmark directory.
    pub(super) bench: PathBuf,
    /// What to do.
    pub(super) action: Action,
    /// An interrupted run's directory to finish.
    pub(super) resume: Option<PathBuf>,
    /// The models; empty means the manifest's.
    pub(super) models: Vec<String>,
    /// The arms; empty means every arm.
    pub(super) arms: Vec<String>,
    /// The tasks; empty means every task.
    pub(super) tasks: Vec<String>,
    /// The profiles; empty means every profile.
    pub(super) profiles: Vec<String>,
    /// Trials per cell; `None` means the manifest's.
    pub(super) trials: Option<u32>,
    /// Trials in flight; `None` means the manifest's.
    pub(super) parallel: Option<usize>,
    /// Repairs per trial; `None` means the manifest's.
    pub(super) max_repairs: Option<u32>,
    /// The model command; `None` means the manifest's.
    pub(super) command: Option<String>,
    /// The results directory.
    pub(super) out: Option<PathBuf>,
    /// The work root.
    pub(super) work: Option<PathBuf>,
}

/// A refusal of the command line, logged where it is made.
fn usage<T>(message: String) -> Result<T, BenchError> {
    fail(BenchError::Usage(message), "usage")
}

/// The value after a flag.
fn value_of<'argument>(
    remaining: &mut impl Iterator<Item = &'argument String>,
    flag: &str,
) -> Result<String, BenchError> {
    match remaining.next() {
        Some(found) => Ok(found.clone()),
        None => usage(format!("ai-authoring: {flag} needs a value")),
    }
}

/// The names in a comma-separated list.
fn names(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A whole number above zero.
fn count(text: &str, flag: &str) -> Result<u32, BenchError> {
    match text.parse::<u32>() {
        Ok(found) if found > 0 => Ok(found),
        Ok(_) | Err(_) => usage(format!(
            "ai-authoring: {flag} wants a whole number above zero, got {text:?}"
        )),
    }
}

/// Reads the command line.
pub(super) fn parse(arguments: &[String]) -> Result<Options, BenchError> {
    let mut options = Options {
        bench: PathBuf::from(DEFAULT_BENCH),
        action: Action::Help,
        resume: None,
        models: Vec::new(),
        arms: Vec::new(),
        tasks: Vec::new(),
        profiles: Vec::new(),
        trials: None,
        parallel: None,
        max_repairs: None,
        command: None,
        out: None,
        work: None,
    };
    let mut modes: Vec<Mode> = Vec::new();
    let mut tables: Option<PathBuf> = None;
    let mut help = false;
    let mut remaining = arguments.iter();
    while let Some(argument) = remaining.next() {
        let flag = argument.as_str();
        match flag {
            "--help" | "-h" => help = true,
            "--dry-run" => modes.push(Mode::Dry),
            "--mutants" => modes.push(Mode::Mutants),
            "--matrix" => modes.push(Mode::Models),
            "--tables" => tables = Some(PathBuf::from(value_of(&mut remaining, flag)?)),
            "--bench" => options.bench = PathBuf::from(value_of(&mut remaining, flag)?),
            "--resume" => options.resume = Some(PathBuf::from(value_of(&mut remaining, flag)?)),
            "--out" => options.out = Some(PathBuf::from(value_of(&mut remaining, flag)?)),
            "--work" => options.work = Some(PathBuf::from(value_of(&mut remaining, flag)?)),
            "--cmd" => options.command = Some(value_of(&mut remaining, flag)?),
            "--models" => options.models = names(&value_of(&mut remaining, flag)?),
            "--apis" => options.arms = names(&value_of(&mut remaining, flag)?),
            "--tasks" => options.tasks = names(&value_of(&mut remaining, flag)?),
            "--profiles" => options.profiles = names(&value_of(&mut remaining, flag)?),
            "--trials" => options.trials = Some(count(&value_of(&mut remaining, flag)?, flag)?),
            "--max-repairs" => {
                options.max_repairs = Some(count(&value_of(&mut remaining, flag)?, flag)?);
            }
            "--parallel" => {
                let workers = count(&value_of(&mut remaining, flag)?, flag)?;
                options.parallel = match usize::try_from(workers) {
                    Ok(found) => Some(found),
                    Err(_) => return usage(format!("ai-authoring: {flag} {workers} does not fit")),
                };
            }
            other => return usage(format!("ai-authoring: unknown argument {other:?}")),
        }
    }
    let many = "ai-authoring: --dry-run, --mutants, --matrix and --tables are separate runs; pick one";
    options.action = if help {
        Action::Help
    } else if let Some(run_dir) = tables {
        if !modes.is_empty() {
            return usage(many.to_owned());
        }
        Action::Tables(run_dir)
    } else {
        match (modes.first(), modes.len()) {
            (Some(&mode), 1) => Action::Run(mode),
            (None, _) => {
                return usage(
                    "ai-authoring: say what to run: --dry-run, --mutants, --matrix or --tables RUN_DIR (--help lists them)"
                        .to_owned(),
                );
            }
            (Some(_), _) => return usage(many.to_owned()),
        }
    };
    Ok(options)
}
