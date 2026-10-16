//! Cargo for the trials, one process at a time.
//!
//! Two cargo processes sharing one target directory are not a measurement, so
//! every build and every oracle run holds an exclusive lock on one file in the
//! work root. The lock is on the open file, so it excludes another thread of
//! this process as well as another process.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::Path;
use std::time::{Duration, Instant};

use super::children::{Children, Launch, TIMED_OUT, forget_launched_crate, inherited_env};
use super::grade::peak_rss_bytes;
use super::{BenchError, fail};

/// The lock file's name in the work root.
const GATE_FILE: &str = ".cargo-gate.lock";

/// The tool that reports the oracle's peak resident set. A host without it still
/// grades, and reports no peak.
const TIME_TOOL: &str = "/usr/bin/time";

/// The tool's flag for that report: BSD and macOS `time -l` prints bytes, GNU
/// `time -v` prints kilobytes, and GNU `time` refuses `-l` outright, which would
/// leave the oracle unrun on every Linux host that has it.
const TIME_FLAG: &str = if cfg!(target_vendor = "apple") {
    "-l"
} else {
    "-v"
};

/// The result of one `cargo build`.
pub(super) struct Built {
    /// Whether it succeeded.
    pub(super) ok: bool,
    /// Everything it printed.
    pub(super) log: String,
    /// How long it took, in milliseconds.
    pub(super) wall_ms: f64,
}

/// The result of one oracle run.
pub(super) struct Tested {
    /// Everything it printed.
    pub(super) output: String,
    /// How long it took, in milliseconds.
    pub(super) wall_ms: f64,
    /// The peak resident set of the run, when the host reports one.
    pub(super) peak_rss: Option<u64>,
    /// Whether the oracle was still running at its bound.
    pub(super) timed_out: bool,
}

/// Cargo under one run's work root.
pub(super) struct Cargo<'run> {
    /// The work root; its `target` is the one every trial builds into.
    work: &'run Path,
    /// The children the run owns.
    children: &'run Children,
    /// The environment every cargo process sees.
    env: BTreeMap<String, String>,
    /// How long one build may take.
    build_bound: Duration,
    /// How long one oracle run may take.
    oracle_bound: Duration,
}

/// A command line from string slices.
fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

/// `milliseconds` since `started`.
fn milliseconds(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

impl<'run> Cargo<'run> {
    /// Cargo building into `work/target`, under the bounds given.
    pub(super) fn new(
        work: &'run Path,
        children: &'run Children,
        build_bound: Duration,
        oracle_bound: Duration,
    ) -> Self {
        let mut env = inherited_env();
        forget_launched_crate(&mut env);
        env.insert(
            "CARGO_TARGET_DIR".to_owned(),
            work.join("target").display().to_string(),
        );
        Self {
            work,
            children,
            env,
            build_bound,
            oracle_bound,
        }
    }

    /// The exclusive lock, held until the returned file is dropped.
    pub(super) fn gate(&self) -> Result<File, BenchError> {
        let path = self.work.join(GATE_FILE);
        let file = match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(file) => file,
            Err(cause) => return fail(BenchError::Io { path, cause }, "gate"),
        };
        match file.lock() {
            Ok(()) => Ok(file),
            Err(cause) => fail(BenchError::Io { path, cause }, "gate"),
        }
    }

    /// `cargo build --locked --offline` in `trial`.
    pub(super) fn build(&self, trial: &Path) -> Result<Built, BenchError> {
        let _gate = self.gate()?;
        let log_path = trial.join("build.log");
        let log = match File::create(&log_path) {
            Ok(file) => file,
            Err(cause) => {
                return fail(
                    BenchError::Io {
                        path: log_path,
                        cause,
                    },
                    "build",
                );
            }
        };
        let command = argv(&["cargo", "build", "--locked", "--offline"]);
        let started = Instant::now();
        let code = self.children.run(
            &Launch {
                argv: &command,
                root: trial,
                env: &self.env,
                stdin: None,
                bound: self.build_bound,
            },
            Some(&log),
        );
        let wall_ms = milliseconds(started);
        let bytes = match fs::read(&log_path) {
            Ok(bytes) => bytes,
            Err(cause) => {
                return fail(
                    BenchError::Io {
                        path: log_path,
                        cause,
                    },
                    "build",
                );
            }
        };
        Ok(Built {
            ok: code == 0,
            log: String::from_utf8_lossy(&bytes).into_owned(),
            wall_ms,
        })
    }

    /// `cargo test --locked --offline -- --test-threads=1` in `trial`, timed.
    pub(super) fn test(&self, trial: &Path) -> Result<Tested, BenchError> {
        let _gate = self.gate()?;
        let timed = Path::new(TIME_TOOL).is_file();
        let mut command = if timed {
            argv(&[TIME_TOOL, TIME_FLAG, "cargo"])
        } else {
            argv(&["cargo"])
        };
        command.extend(argv(&[
            "test",
            "--locked",
            "--offline",
            "--",
            "--test-threads=1",
        ]));
        let started = Instant::now();
        let captured = self.children.capture(&Launch {
            argv: &command,
            root: trial,
            env: &self.env,
            stdin: None,
            bound: self.oracle_bound,
        });
        let wall_ms = milliseconds(started);
        let peak_rss = if timed {
            peak_rss_bytes(&captured.stderr)
        } else {
            None
        };
        Ok(Tested {
            output: format!("{}{}", captured.stdout, captured.stderr),
            wall_ms,
            peak_rss,
            timed_out: captured.code == TIMED_OUT,
        })
    }
}
