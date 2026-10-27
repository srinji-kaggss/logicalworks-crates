//! The dependency report: every edge's mortgage, every deferral's trigger,
//! every pin's expiry and every blocked provider's cover, as human text or
//! JSON.
//!
//! The gate refuses; this command explains. It reads the same registers
//! `lgwks-deps check` enforces — the approvals, the deferral catalog and the
//! supply-chain ledger — beside the resolved lockfile, and prints the quarterly
//! review page: crate, owner, consumers, replacement, removal trigger and last
//! review for every edge. A JSON document under stable keys is what `--json`
//! emits for lanes that diff two reports.
//!
//! Every write goes through the explicit handle passed down from `main`, and a
//! closed reader is a clean exit, like every other binary in this crate.

use std::io;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use lgwks_deps::{check_verdict, deferral, ledger, lock, patches, repository_root};

/// The command summary printed by `--help` and by an invocation that refuses
/// its own arguments.
pub(crate) const USAGE: &str = "\
ledger: the dependency mortgage report

USAGE
  ledger [PATH]               print the lifecycle, deferral, patch and TLS
                              provider report for the repo at PATH (default:
                              the working directory's repository)
              [--json]        emit one JSON object on stdout instead of text

EXIT
  0  the report rendered and the verdict it was read beside carries no refusal
  2  a refusal, a missing register, or an unreadable ledger
";

/// What one parse of `ledger`'s arguments resolved to.
enum Request {
    /// Print the report for this target, as JSON when asked.
    Report {
        /// Repository to report on. `None` means the working directory.
        target: Option<PathBuf>,
        /// Whether to render JSON instead of text.
        json: bool,
    },
    /// Print the usage block and exit successfully.
    Help,
}

/// Why `ledger`'s arguments were refused.
#[derive(Debug)]
enum ArgError {
    /// A `--flag` this command does not define.
    UnknownFlag {
        /// The unrecognised token, verbatim.
        flag: String,
    },
    /// A second positional argument.
    SurplusTarget {
        /// The extra token, verbatim.
        value: String,
    },
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::UnknownFlag { ref flag } => {
                write!(formatter, "unknown option for `ledger`: {flag}")
            }
            Self::SurplusTarget { ref value } => {
                write!(
                    formatter,
                    "`ledger` reports on one repository, and {value:?} is a second path"
                )
            }
        }
    }
}

/// Parses `ledger`'s arguments in one consuming pass.
///
/// Each refusal is logged where it is made, like every other command in this
/// crate: the caller sees a signal in the trace, not only a value on stderr.
fn parse_args(args: &[String]) -> Result<Request, ArgError> {
    let mut target: Option<PathBuf> = None;
    let mut json = false;
    for argument in args {
        match argument.as_str() {
            "--help" | "-h" => return Ok(Request::Help),
            "--json" => json = true,
            flag if flag.starts_with("--") => {
                let refusal = Err(ArgError::UnknownFlag {
                    flag: flag.to_owned(),
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse_args: returning an error to the caller");
                return refusal;
            }
            path => {
                if target.is_some() {
                    let refusal = Err(ArgError::SurplusTarget {
                        value: path.to_owned(),
                    });
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse_args: returning an error to the caller");
                    return refusal;
                }
                target = Some(PathBuf::from(path));
            }
        }
    }
    Ok(Request::Report { target, json })
}

/// Turns a command's write result into the process exit code.
///
/// `BrokenPipe` is success: the reader closed the pipe on purpose. Any other
/// write error is a real fault and reports `2`.
fn settle(result: io::Result<ExitCode>) -> ExitCode {
    match result {
        Ok(code) => code,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(2),
    }
}

/// Refuses with `message` on stderr.
fn refuse(message: &str, err: &mut impl io::Write) -> io::Result<ExitCode> {
    writeln!(err, "ledger: {message}")?;
    Ok(ExitCode::from(2))
}

/// Runs the report: loads the verdict the gate would reach, then renders the
/// registers beside it.
fn run(
    target: Option<PathBuf>,
    json: bool,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let start = match target {
        Some(path) => path,
        None => PathBuf::from("."),
    };
    let root = match repository_root(&start) {
        Ok(root) => root,
        Err(error) => return refuse(&error.to_string(), err),
    };
    let collected = match check_verdict(&root, None) {
        Ok(collected) => collected,
        Err(error) => return refuse(&error.to_string(), err),
    };
    let (verdict, cleanup) = collected.into_parts();
    if let Some(warning) = cleanup {
        writeln!(err, "ledger: WARN {warning}")?;
    }
    let lock_text = match std::fs::read_to_string(root.join("Cargo.lock")) {
        Ok(text) => text,
        Err(cause) => {
            return refuse(
                &format!("cannot read {}: {cause}", root.join("Cargo.lock").display()),
                err,
            );
        }
    };
    let locked = match lock::parse(&lock_text) {
        Ok(locked) => locked,
        Err(error) => return refuse(&error.to_string(), err),
    };
    let deferral = match deferral::load(&root) {
        Ok(deferral) => deferral,
        Err(refusal) => return refuse(&refusal.to_string(), err),
    };
    let supply = match patches::load(&root) {
        Ok(supply) => supply,
        Err(refusal) => return refuse(&refusal.to_string(), err),
    };
    let lifecycle = match lgwks_deps::lifecycle::load(&root) {
        Ok(lifecycle) => lifecycle,
        Err(refusal) => return refuse(&refusal.to_string(), err),
    };
    let today = patches::today_utc();
    let input = ledger::Input::new(
        verdict.register(),
        deferral.as_ref(),
        supply.as_ref(),
        lifecycle.as_ref(),
        &locked,
        &today,
    );
    if json {
        writeln!(
            out,
            "{}",
            ledger::render_json(&input).map_err(io::Error::other)?
        )?;
    } else {
        write!(
            out,
            "{}",
            ledger::render_text(&input).map_err(io::Error::other)?
        )?;
    }
    if verdict.refusals().is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(2))
    }
}

fn main() -> ExitCode {
    settle(run_main())
}

/// Parses the process arguments and runs the report on the process streams.
fn run_main() -> io::Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = io::stdout();
    let mut handle = out.lock();
    let errors = io::stderr();
    let mut error_handle = errors.lock();
    match parse_args(&args) {
        Ok(Request::Help) => {
            write!(handle, "{USAGE}")?;
            Ok(ExitCode::SUCCESS)
        }
        Ok(Request::Report { target, json }) => run(target, json, &mut handle, &mut error_handle),
        Err(error) => refuse(&error.to_string(), &mut error_handle),
    }
}
