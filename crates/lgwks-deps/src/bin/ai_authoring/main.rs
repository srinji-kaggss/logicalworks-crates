//! The AI authoring benchmark's runner (#270).
//!
//! Each trial asks one fixed author to write one held-out orchestration task
//! against one API surface, under one prompt skeleton, one hidden oracle and one
//! repair budget, and records everything: tokens, compiler repairs, consumer
//! lines, orchestration sites, oracle verdicts, wall time and peak memory. Every
//! trial is recorded; no best run is selected.
//!
//! What the benchmark is, in `bench/ai-authoring/bench.toml`, is that file's
//! business; this binary holds none of it. The runs it makes:
//!
//! ```text
//! ai-authoring --dry-run            reference solutions stand in for a model (free)
//! ai-authoring --mutants            each task's mutant must fail the oracle (free)
//! ai-authoring --tables RUN_DIR     the README tables of a committed run
//! ai-authoring --matrix             the model matrix (calls the models; costs money)
//! ```
//!
//! Exit codes: 0 the run finished clean; 1 it finished with a crashed trial or a
//! mutant the oracle did not catch; 2 usage, manifest or host refusal.

mod author;
mod bench;
mod budget;
mod cargo;
#[path = "../shared/children.rs"]
mod children;
mod cli;
#[path = "../shared/console.rs"]
mod console;
mod grade;
mod opencode;
mod plan;
mod record;
mod run;
mod scaffold;
mod stats;
mod store;
mod summary;
mod table;
#[path = "../shared/toml_subset.rs"]
mod toml_subset;
mod trial;
#[path = "../shared/utc.rs"]
mod utc;

// The simulations: the estate's one seed substrate and replay sweep, included by
// path as `tests/it` includes them, and the runner's own fixtures over them.
#[cfg(test)]
#[path = "../../../tests/support/replay.rs"]
mod replay;
#[cfg(test)]
#[path = "../../../../lgwks-bot/tests/sim/seed.rs"]
mod seed;
#[cfg(test)]
#[path = "../../../../lgwks-bot/tests/sim/seed_helpers.rs"]
mod seed_helpers;
#[cfg(test)]
#[path = "../../../tests/support/sim.rs"]
mod sim;
#[cfg(test)]
mod sim_cargo;
#[cfg(test)]
mod sim_cli;
#[cfg(test)]
mod sim_fence;
#[cfg(test)]
mod sim_kit;
#[cfg(test)]
mod sim_plan;
#[cfg(test)]
mod sim_record;
#[cfg(test)]
mod sim_scaffold;
#[cfg(test)]
mod sim_stats;
#[cfg(test)]
mod sim_store;
#[cfg(test)]
mod sim_trial;

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use bench::Bench;
use children::Children;
use cli::{Action, USAGE};
use console::Console;

/// What a run's summary says generated it.
const GENERATED_BY: &str = match option_env!("CARGO_BIN_NAME") {
    Some(name) => name,
    None => "ai-authoring",
};

/// Why the runner could not do what it was asked.
#[derive(Debug)]
pub(crate) enum BenchError {
    /// A file or directory could not be read, written or made.
    Io {
        /// What was being touched.
        path: PathBuf,
        /// What the operating system said.
        cause: io::Error,
    },
    /// The manifest, a run's own files or a record is malformed.
    Manifest(String),
    /// The command line is wrong.
    Usage(String),
    /// The host or the run's state refuses the request.
    Refused(String),
    /// The call asks what an earlier call of the run already asked, so it is not made.
    Repeated,
}

impl fmt::Display for BenchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Io {
                ref path,
                ref cause,
            } => write!(formatter, "ai-authoring: {}: {cause}", path.display()),
            Self::Manifest(ref what) | Self::Usage(ref what) | Self::Refused(ref what) => {
                write!(formatter, "{what}")
            }
            Self::Repeated => write!(
                formatter,
                "this call repeats an earlier one in meaning, so it is not made"
            ),
        }
    }
}

impl std::error::Error for BenchError {}

/// Logs `error` at the site that produced it, then hands it back as the `Err`.
pub(crate) fn fail<T>(error: BenchError, site: &'static str) -> Result<T, BenchError> {
    lgwks_std::trace::debug!(error = ?error, site, "returning an error to the caller");
    Err(error)
}

/// Reads a file whole, naming it when it cannot be read.
pub(crate) fn read(path: &Path) -> Result<String, BenchError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(cause) => fail(
            BenchError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "read",
        ),
    }
}

/// Reads a file whole as text, replacing bytes that are not UTF-8, so a log a kill
/// tore inside a multi-byte character still reads and the torn line is dropped.
pub(crate) fn read_lossy(path: &Path) -> Result<String, BenchError> {
    match fs::read(path) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Err(cause) => fail(
            BenchError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "read_lossy",
        ),
    }
}

/// Writes a file whole, naming it when it cannot be written.
pub(crate) fn write(path: &Path, text: &str) -> Result<(), BenchError> {
    match fs::write(path, text) {
        Ok(()) => Ok(()),
        Err(cause) => fail(
            BenchError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "write",
        ),
    }
}

/// Makes one new directory; an existing one is a refusal, never reused.
pub(crate) fn create_dir(path: &Path) -> Result<(), BenchError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(cause) => fail(
            BenchError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "create_dir",
        ),
    }
}

/// Makes a directory and its parents; an existing one is fine.
pub(crate) fn create_dir_all(path: &Path) -> Result<(), BenchError> {
    match fs::create_dir_all(path) {
        Ok(()) => Ok(()),
        Err(cause) => fail(
            BenchError::Io {
                path: path.to_path_buf(),
                cause,
            },
            "create_dir_all",
        ),
    }
}

/// Prints the tables of a committed run.
fn tables(run_dir: &Path, console: &Console) -> Result<u8, BenchError> {
    let records = record::read_results(&read(&run_dir.join("results.jsonl"))?)?;
    console.say(&table::render_tables(&records));
    Ok(0)
}

/// Does what `arguments` ask and reports to `console`; the code is the exit status.
pub(crate) fn execute(arguments: &[String], console: &Console) -> u8 {
    let options = match cli::parse(arguments) {
        Ok(options) => options,
        Err(error) => {
            console.warn(&error.to_string());
            return 2;
        }
    };
    let outcome = match options.action {
        Action::Help => {
            console.say(USAGE);
            Ok(0)
        }
        Action::Tables(ref run_dir) => tables(run_dir, console),
        Action::Run(mode) => match Bench::load(&options.bench) {
            Ok(loaded) => {
                let children = Children::new(true);
                let done = run::run(&loaded, &options, mode, &utc::stamp(), console, &children);
                children.stop_all();
                children.finish();
                done
            }
            Err(error) => Err(error),
        },
    };
    match outcome {
        Ok(code) => code,
        Err(error) => {
            console.warn(&error.to_string());
            2
        }
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let console = Console::new(Box::new(io::stdout()), Box::new(io::stderr()));
    ExitCode::from(execute(&arguments, &console))
}
