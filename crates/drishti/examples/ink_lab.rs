//! Open the Drishti ink lab: the eleven brand effects on the GPU, through GPUI.
//!
//! ```text
//! cargo run --release --features ui --example ink_lab
//! cargo run --release --features ui --example ink_lab -- --frames 600 --json
//! ```
//!
//! Flags: `--seed N`, `--reduce-motion`, `--dark`, `--frames N` (quit after N
//! frames and print the session report), `--json` (print the outcome as one
//! JSON object). Closing the window also prints the report.
//!
//! On macOS GPUI ends the process when the application quits, so the exit
//! status is 0 even when the host failed to start. A failure is printed as
//! `ink_lab: <reason>` on stderr, or as `{"error": ...}` with `--json`.

use std::io::Write as _;
use std::process::ExitCode;

use drishti::native::{Launch, NativeError, SessionReport};

/// Usage text.
const USAGE: &str = "usage: ink_lab [--seed N] [--reduce-motion] [--dark] [--frames N] [--json]";

/// Parse arguments, run the window, and print the session outcome.
fn main() -> ExitCode {
    let (launch, json) = match parse(std::env::args().skip(1)) {
        Ok(parsed) => parsed,
        Err(message) => {
            return match emit(&format!("ink_lab: {message}\n{USAGE}"), false) {
                Ok(()) => ExitCode::from(2),
                Err(_) => ExitCode::FAILURE,
            };
        }
    };
    drishti::native::run(launch, move |outcome| {
        let to_stdout = json || outcome.is_ok();
        let line = describe(&outcome, json);
        if emit(&line, to_stdout).is_err() && to_stdout {
            // A closed stdout (a finished pipe) must not lose the report.
            let _unreported = emit(&line, false);
        }
    });
    ExitCode::SUCCESS
}

/// Write one line to a locked stdout or stderr.
fn emit(line: &str, stdout: bool) -> std::io::Result<()> {
    if stdout {
        let mut handle = std::io::stdout().lock();
        writeln!(handle, "{line}")
    } else {
        let mut handle = std::io::stderr().lock();
        writeln!(handle, "{line}")
    }
}

/// Why the command line was refused.
#[derive(Debug)]
enum ArgError {
    /// `-h` or `--help`: the usage is the answer.
    Help,
    /// An argument the lab does not take.
    Unknown(String),
    /// A flag with no value after it.
    Missing(&'static str),
    /// A flag whose value is not a whole number.
    NotANumber {
        /// The flag.
        flag: &'static str,
        /// What was given.
        given: String,
    },
}

impl std::fmt::Display for ArgError {
    /// One line naming the problem.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Help => formatter.write_str("help requested"),
            Self::Unknown(ref arg) => write!(formatter, "unknown argument {arg:?}"),
            Self::Missing(flag) => write!(formatter, "{flag} needs a value"),
            Self::NotANumber { flag, ref given } => {
                write!(formatter, "{flag} takes a whole number, not {given:?}")
            }
        }
    }
}

/// Parse the command line into a launch and the output format.
fn parse(mut args: impl Iterator<Item = String>) -> Result<(Launch, bool), ArgError> {
    let mut launch = Launch::new();
    let mut json = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seed" => launch.seed = number(&mut args, "--seed")?,
            "--frames" => launch.frame_limit = Some(number(&mut args, "--frames")?),
            "--reduce-motion" => launch.reduce_motion = true,
            "--dark" => launch.dark = true,
            "--json" => json = true,
            "-h" | "--help" => {
                lgwks_std::trace::debug!(%arg, "ink_lab: help requested");
                return Err(ArgError::Help);
            }
            _ => {
                lgwks_std::trace::debug!(%arg, "ink_lab: unknown argument");
                return Err(ArgError::Unknown(arg));
            }
        }
    }
    Ok((launch, json))
}

/// Read the numeric value that follows `flag`.
fn number(args: &mut impl Iterator<Item = String>, flag: &'static str) -> Result<u64, ArgError> {
    let given = args.next().ok_or(ArgError::Missing(flag))?;
    match given.parse::<u64>() {
        Ok(value) => Ok(value),
        Err(error) => {
            lgwks_std::trace::debug!(flag, %given, %error, "ink_lab: flag value refused");
            Err(ArgError::NotANumber { flag, given })
        }
    }
}

/// The outcome as one line of JSON or a short human summary.
fn describe(outcome: &Result<SessionReport, NativeError>, json: bool) -> String {
    let session = match *outcome {
        Ok(ref session) => session,
        Err(ref error) if json => {
            return format!("{{\"error\":{:?}}}", error.to_string());
        }
        Err(ref error) => return format!("ink_lab: {error}"),
    };
    let totals = session.totals;
    let frames = session.frame_interval_ms;
    let fields = [
        ("frames_presented", totals.frames_presented.to_string()),
        ("idle_frames", totals.idle_frames.to_string()),
        ("receipts", totals.receipts.to_string()),
        ("reused", totals.reused.to_string()),
        ("uploaded_bytes", totals.uploaded_bytes.to_string()),
        ("epoch", totals.epoch.to_string()),
        ("refusals", session.refusals.to_string()),
        ("paint_failures", session.paint_failures.to_string()),
        ("wall_ms", format!("{:.1}", session.wall_ms)),
        ("frame_samples", frames.samples.to_string()),
        ("frame_p50_ms", format!("{:.2}", frames.p50)),
        ("frame_p95_ms", format!("{:.2}", frames.p95)),
        ("frame_p99_ms", format!("{:.2}", frames.p99)),
        ("frame_max_ms", format!("{:.2}", frames.max)),
    ];
    let (open, separator, close) = if json {
        ("{", ",", "}")
    } else {
        ("ink lab: ", " ", "")
    };
    let body: Vec<String> = fields
        .iter()
        .map(|&(key, ref value)| {
            if json {
                format!("\"{key}\":{value}")
            } else {
                format!("{key}={value}")
            }
        })
        .collect();
    format!("{open}{}{close}", body.join(separator))
}
