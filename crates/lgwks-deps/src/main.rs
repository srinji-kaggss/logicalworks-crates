//! `lgwks-deps` is the human-facing half of INV-DEP-EDGE-OWNED: the same audit
//! the build script runs, runnable by hand, plus the two commands that make the
//! admission process a path rather than a folk practice.
//!
//! This binary is a doctor, not an authority. `check` diagnoses, `request`
//! prints a block for a human to fill in, and `init` writes a fail-closed
//! starting register. None of them can approve anything: approval is a diff
//! with a name on it, which is the whole point of the contract.
//!
//! `vendor check` is the physical counterpart: the register says which edges
//! are owned, the vendor tree says which bytes the offline build resolves,
//! and the subcommand proves the lockfile is fully covered by the tree.
//!
//! ## Output and the exit code
//!
//! Every command writes through an `io::Write` handle passed down from `main`
//! rather than through `print!`/`println!`. The two are the same bytes on a
//! terminal, but only the explicit handle lets this binary decide what a closed
//! reader means: `lgwks-deps check . | head -1` is ordinary usage, so a broken
//! pipe is a clean exit, not a panic. `print!` cannot express that, and it also
//! trips `clippy::print_stdout`, which the workspace forbids outright. The
//! policy lives in one place (`settle`) instead of at each write site.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lgwks_deps::{
    invariants::Register as InvariantRegister,
    invariants::{Audit as InvariantAudit, SCOPE as INVARIANT_SCOPE},
    repository_root,
};

/// How a `check` run reached its register, which is what the receipt binds.
///
/// Committed enforcement reads `PATH/contract/APPROVED.toml` beside the code;
/// diagnosis reads a caller-named register and is never what a build uses. Two
/// modes with one verdict computation, so the receipt can say which it was
/// without either mode getting a laxer rule.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PolicyMode {
    /// The register committed beside the audited code.
    Enforcement,
    /// A register named with `--contract`, for diagnosis only.
    Diagnosis,
}

impl PolicyMode {
    /// The stable receipt spelling.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Enforcement => "enforcement",
            Self::Diagnosis => "diagnosis",
        }
    }

    /// The mode a `--contract` override selects.
    fn of(contract_override: &Option<PathBuf>) -> Self {
        if contract_override.is_some() {
            Self::Diagnosis
        } else {
            Self::Enforcement
        }
    }
}

/// Exit code for a write that failed for a reason other than the reader going
/// away.
///
/// The crate's generic failure code, already used for an unknown command and a
/// refused audit. It is deliberately not `1`: `freshness` owns `1` for a stale
/// dependency, and a consumer scripting on that number must not see a write
/// fault spelled the same way as an out-of-date lockfile.
const EXIT_IO: u8 = 2;

/// The command summary printed by `--help`, by an unknown command, and by a
/// subcommand invoked with the wrong number of arguments.
const USAGE: &str = "\
lgwks-deps: dependency admission for the core surface

USAGE
  lgwks-deps check [PATH]              audit the repo at PATH (default: cwd)
             [--contract FILE]         read the register from FILE instead of
                                       PATH/contract/APPROVED.toml. Diagnosis
                                       only — a build always reads the register
                                       committed beside the code it builds.
             [--json]                  emit one JSON object on stdout; stderr
                                       carries only a WARN line when a Cargo
                                       capture file could not be removed. The
                                       exit code still carries the verdict
  `check` parses its own arguments in one pass, and refuses rather than
  guesses: a missing or repeated --contract value, an unknown --flag, or a
  second PATH exits 2 without auditing anything. --contract's FILE is a
  register and never the audit target, whichever order the two appear in.
  lgwks-deps invariants [PATH]         audit the optional invariant register
  lgwks-deps request <CRATE> <VERSION> print an approval block to fill in
  lgwks-deps init [PATH]               write a fail-closed starting register
   lgwks-deps tiers                     print the admission ladder
   lgwks-deps freshness [PATH]          check resolved deps against crates.io
              [--json]                  output as JSON instead of a table
   lgwks-deps vendor check [PATH]       prove every locked package resolves
                                        to the shared vendor tree
   lgwks-deps scan                      moved to `lgwks-ast scan` (#408);
                                        exits 2 naming the command
   lgwks-deps debug [PATH]              run the cargo-doctor-style debugger
             [--json]                   emit the doctor report as JSON

EXIT
   0  every authored external edge has an admitted semantic owner
   2  a refusal, a missing register, or an unparseable one
";

/// Turns a command's write result into the process exit code.
///
/// `BrokenPipe` is success: the reader closed the pipe on purpose, which is
/// what `| head -1` does, and the command's own verdict was never in question.
/// Any other write error is a real fault and reports ``EXIT_IO``. This is the
/// single place the policy is written down; the commands themselves propagate
/// their first write failure with `?` and never inspect its kind.
fn settle(result: io::Result<ExitCode>) -> ExitCode {
    match result {
        Ok(code) => code,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(EXIT_IO),
    }
}

/// What `check` was asked to do, after one consuming pass over its arguments.
///
/// The two path fields are separate and separately typed. An earlier revision
/// read every token that did not start with `--` as positional, which made the
/// *value* of `--contract` a candidate audit target as well as the register:
/// `check --contract other/contract/APPROVED.toml` then discovered its
/// repository root inside `other/` and reported a verdict for a tree nobody
/// asked about. The register and the subject are different things and are now
/// different fields, so no parse can conflate them.
struct CheckArgs {
    /// Repository to audit. `None` means the process working directory, which
    /// is the documented default for an omitted `PATH`.
    target: Option<PathBuf>,
    /// Register to read instead of the target's own `contract/APPROVED.toml`.
    /// Diagnosis only: a build always reads the register beside the code.
    contract: Option<PathBuf>,
    /// True when the verdict is rendered as one JSON object on stdout.
    json: bool,
}

/// What one parse of `check`'s arguments resolved to.
///
/// An enum rather than a `help` flag on [`CheckArgs`]: a request to print the
/// usage block has no audit target, and a bool would leave every consumer
/// deciding what an empty target means when the flag is set.
enum CheckRequest {
    /// Audit a repository.
    Audit(CheckArgs),
    /// Print the usage block and exit successfully.
    Help,
}

/// Why `check`'s arguments were refused.
///
/// Refusals rather than guesses. Every variant names the token that caused it,
/// because the operator's next action is to look at that token, and every one
/// of them used to be silently tolerated — which is how an option's value
/// became the audited repository.
#[derive(Debug)]
enum CheckArgError {
    /// An option that takes a value was the last argument.
    MissingValue {
        /// The option that needed a value.
        flag: &'static str,
    },
    /// An option that takes a value was handed another option.
    ///
    /// `--contract --json` is a missing value, not a path named `--json`:
    /// treating it as a path would silently redirect the register to a
    /// filename that does not exist and turn a diagnosis into a refusal about
    /// the wrong file. No usable path begins with `--`.
    ValueLooksLikeOption {
        /// The option that needed a value.
        flag: &'static str,
        /// The token it was handed instead.
        value: String,
    },
    /// A value-taking option appeared twice.
    DuplicateOverride {
        /// The option that was repeated.
        flag: &'static str,
    },
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

impl fmt::Display for CheckArgError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::MissingValue { flag } => {
                write!(formatter, "{flag} needs a value: {flag} FILE")
            }
            Self::ValueLooksLikeOption { flag, ref value } => write!(
                formatter,
                "{flag} was given {value:?}, which is another option, not a path"
            ),
            Self::DuplicateOverride { flag } => write!(
                formatter,
                "{flag} was given more than once; `check` reads one register"
            ),
            Self::UnknownFlag { ref flag } => {
                write!(formatter, "unknown option for `check`: {flag}")
            }
            Self::SurplusTarget { ref value } => write!(
                formatter,
                "`check` audits one repository, and {value:?} is a second path"
            ),
        }
    }
}

/// Parses `check`'s arguments in one consuming pass.
///
/// The pass consumes each option's value, so a value can never also be read as
/// the positional target. `PATH` may appear before or after the options and
/// means the same thing either way; omitting it leaves [`CheckArgs::target`]
/// as `None`, which [`run_check`] resolves to the working directory.
///
/// `--json` may repeat: it is a mode, and repeating a mode cannot change what
/// the command does. `--contract` may not: two registers are two policies, and
/// picking either one silently would make the verdict depend on argument order.
/// The `--contract=FILE` spelling is not accepted — it is reported as an
/// unknown option rather than parsed into a path, so a caller that guessed the
/// wrong spelling is told instead of audited against the wrong file.
fn parse_check_args(args: &[String]) -> Result<CheckRequest, CheckArgError> {
    let mut target: Option<PathBuf> = None;
    let mut contract: Option<PathBuf> = None;
    let mut json = false;
    let mut cursor = args.iter();
    while let Some(argument) = cursor.next() {
        match argument.as_str() {
            "--help" | "-h" => return Ok(CheckRequest::Help),
            "--json" => json = true,
            "--contract" => {
                if contract.is_some() {
                    let refusal = Err(CheckArgError::DuplicateOverride { flag: "--contract" });
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse_check_args: returning an error to the caller");
                    return refusal;
                }
                let Some(value) = cursor.next() else {
                    let refusal = Err(CheckArgError::MissingValue { flag: "--contract" });
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse_check_args: returning an error to the caller");
                    return refusal;
                };
                if value.starts_with("--") {
                    let refusal = Err(CheckArgError::ValueLooksLikeOption {
                        flag: "--contract",
                        value: value.clone(),
                    });
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse_check_args: returning an error to the caller");
                    return refusal;
                }
                contract = Some(PathBuf::from(value));
            }
            flag if flag.starts_with("--") => {
                let refusal = Err(CheckArgError::UnknownFlag {
                    flag: flag.to_owned(),
                });
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse_check_args: returning an error to the caller");
                return refusal;
            }
            // A single `-` token is a path, not an option: the only short flag
            // this command defines is `-h`, which is matched above.
            path => {
                if target.is_some() {
                    let refusal = Err(CheckArgError::SurplusTarget {
                        value: path.to_owned(),
                    });
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse_check_args: returning an error to the caller");
                    return refusal;
                }
                target = Some(PathBuf::from(path));
            }
        }
    }
    Ok(CheckRequest::Audit(CheckArgs {
        target,
        contract,
        json,
    }))
}

/// Runs `check`, keeping the argument parsing out of the audit path.
///
/// A refused invocation prints the reason and the usage block and exits 2
/// without auditing anything: the one thing it must never do is fall back to
/// auditing something that parses.
fn handle_check(
    args: &[String],
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let request = match parse_check_args(args) {
        Ok(request) => request,
        Err(error) => {
            writeln!(err, "lgwks-deps: {error}\n")?;
            write!(err, "{USAGE}")?;
            return Ok(ExitCode::from(2));
        }
    };
    match request {
        CheckRequest::Help => handle_help(out),
        CheckRequest::Audit(parsed) => {
            run_check(parsed.target, parsed.contract, parsed.json, out, err)
        }
    }
}

/// Prints the usage block and reports success.
fn handle_help(out: &mut impl io::Write) -> io::Result<ExitCode> {
    write!(out, "{USAGE}")?;
    Ok(ExitCode::SUCCESS)
}

/// Prints the admission ladder and reports success.
fn handle_tiers(out: &mut impl io::Write) -> io::Result<ExitCode> {
    write!(out, "{LADDER}")?;
    Ok(ExitCode::SUCCESS)
}

/// Audits the optional invariant register without changing dependency-gate
/// behaviour for repositories that have not authored one.
fn handle_invariants(
    args: &[String],
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    // No PATH operand names the current directory, the documented default.
    let target = match args.iter().find(|argument| !argument.starts_with("--")) {
        Some(operand) => PathBuf::from(operand),
        None => PathBuf::from("."),
    };
    let root = match repository_root(&target) {
        Ok(root) => root,
        Err(error) => return refuse(&error.to_string(), err),
    };
    match audit_invariant_root(&root, err)? {
        Ok(None) => {
            writeln!(
                out,
                "OK  {} — no invariant register (optional)",
                root.display()
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Ok(Some((register, audit))) => report_invariant_audit(&root, &register, &audit, out, err),
        Err(error) => refuse(&error, err),
    }
}

/// Prints an invariant-register verdict. Any refusal is non-zero for this
/// explicit audit command, even while a register is in adoption mode.
///
/// The command is a doctor: it says what it resolved, never that an invariant
/// was enforced. [`INVARIANT_SCOPE`] is printed on both paths so a reader
/// cannot mistake a pass here for a run that observed anything.
fn report_invariant_audit(
    root: &Path,
    register: &InvariantRegister,
    audit: &InvariantAudit,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    if audit.refusals().is_empty() {
        writeln!(
            out,
            "OK  {} — {} invariants resolve ({} resolved, {} attested by a recorded run)",
            root.display(),
            audit.registered(),
            audit.resolved(),
            audit.attested()
        )?;
        writeln!(out, "SCOPE  {INVARIANT_SCOPE}")?;
        return Ok(ExitCode::SUCCESS);
    }
    writeln!(
        err,
        "REFUSED  {} — {} invariant violations\n",
        root.display(),
        audit.refusals().len()
    )?;
    for refusal in audit.refusals() {
        writeln!(err, "  invariant register: {refusal}")?;
    }
    writeln!(err, "SCOPE  {INVARIANT_SCOPE}")?;
    if !register.enforce {
        writeln!(
            err,
            "\nNOTE  [policy] enforce = false applies to the combined check; the explicit invariants audit still fails."
        )?;
    }
    Ok(ExitCode::from(2))
}

/// Reports an unrecognised command on stderr with the usage block.
///
/// Exits 2: an unknown command is a failure of the invocation, not a refused
/// audit, but both are "this did not do what you asked" and share the code the
/// usage block documents.
///
/// The two refusals are distinct messages because they are distinct operator
/// mistakes: naming a command this binary does not run, and naming none at all.
fn handle_unknown(other: Option<&str>, err: &mut impl io::Write) -> io::Result<ExitCode> {
    match other {
        Some(name) => writeln!(err, "lgwks-deps: unknown command {name:?}\n")?,
        None => writeln!(err, "lgwks-deps: no command given\n")?,
    }
    write!(err, "{USAGE}")?;
    Ok(ExitCode::from(2))
}

/// Routes the first argument to its command, with the remaining arguments.
fn dispatch(
    command: Option<&str>,
    args: &[String],
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    match command {
        Some("check") => handle_check(&args[1..], out, err),
        Some("invariants") => handle_invariants(&args[1..], out, err),
        Some("request") => run_request(args.get(1), args.get(2), out, err),
        Some("init") => run_init(args.get(1).map(PathBuf::from), out, err),
        Some("tiers") => handle_tiers(out),
        Some("freshness") => handle_freshness(&args[1..], out, err),
        Some("vendor") => handle_vendor(&args[1..], out, err),
        Some("scan") => handle_scan(&args[1..], out, err),
        Some("debug") => handle_debug(&args[1..], out, err),
        Some("-h" | "--help" | "help") => handle_help(out),
        other => handle_unknown(other, err),
    }
}

/// Locks both output handles once and runs the requested command.
///
/// The handles are locked for the life of the process rather than per line:
/// `check` and `freshness` write one line per edge, and re-acquiring the lock
/// for each would be pure overhead. [`settle`] turns the command's write result
/// into the exit code.
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // An absent command reaches the same refusal as an unrecognised one, with
    // its own message: the operator asked for nothing this binary can run, and
    // the usage block is what tells them what it can.
    let command = args.first().map(String::as_str);
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let stderr = io::stderr();
    let mut err = stderr.lock();
    settle(dispatch(command, &args, &mut out, &mut err))
}

/// Prints a one-line refusal on stderr and returns the refusal exit code.
///
/// Every refusal path in this binary funnels through here so exit code 2 means
/// exactly one thing regardless of which check produced it.
fn refuse(message: &str, err: &mut impl io::Write) -> io::Result<ExitCode> {
    writeln!(err, "REFUSED  {message}")?;
    Ok(ExitCode::from(2))
}

/// Returns a value or emits a command refusal and exits the current handler.
macro_rules! unwrap_or_refuse {
    ($result:expr, $err:expr) => {
        match $result {
            Ok(value) => value,
            Err(message) => return crate::refuse(&message, $err),
        }
    };
}

/// Resolves a repository root into the command's string-refusal vocabulary.
fn resolve_repository_root(start: &Path) -> Result<PathBuf, String> {
    repository_root(start).map_err(|error| error.to_string())
}

/// The subject a command audits: the path it named, or the directory the
/// operator is standing in.
///
/// A command given no path is not auditing nothing: it audits the repository
/// the process was started in, which is the one subject that is always a
/// repository. Every command resolves its target here so that rule is one fact
/// about the CLI rather than a substitution repeated at each call site.
fn subject_path(named: Option<&Path>) -> PathBuf {
    match named {
        Some(path) => path.to_path_buf(),
        None => PathBuf::from("."),
    }
}

/// Reads the lockfile text for commands that audit the resolved dependency set.
fn read_lock_text(root: &Path) -> Result<String, String> {
    let lock_path = root.join("Cargo.lock");
    std::fs::read_to_string(&lock_path)
        .map_err(|error| format!("cannot read {}: {error}", lock_path.display()))
}

// ── Commands past dispatch, one file each ───────────────────────────────────

/// Each command's implementation, split by command under the size law (#408).
mod cli {
    pub(crate) mod check;
    pub(crate) mod debug;
    pub(crate) mod freshness;
    pub(crate) mod receipt;
    pub(crate) mod register;
}

use cli::check::{audit_invariant_root, run_check};
use cli::debug::handle_debug;
use cli::freshness::handle_freshness;
use cli::register::{LADDER, handle_scan, handle_vendor, run_init, run_request};
