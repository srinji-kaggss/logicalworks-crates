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
    Refusal, Verdict, check_invariants, check_verdict,
    contract::Contract,
    invariants::Register as InvariantRegister,
    invariants::{Audit as InvariantAudit, SCOPE as INVARIANT_SCOPE},
    metadata::Collected,
    repository_root,
};

/// How a `check` run reached its register, which is what the receipt binds.
///
/// Committed enforcement reads `PATH/contract/APPROVED.toml` beside the code;
/// diagnosis reads a caller-named register and is never what a build uses. Two
/// modes with one verdict computation, so the receipt can say which it was
/// without either mode getting a laxer rule.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PolicyMode {
    /// The register committed beside the audited code.
    Enforcement,
    /// A register named with `--contract`, for diagnosis only.
    Diagnosis,
}

impl PolicyMode {
    /// The stable receipt spelling.
    pub(crate) const fn as_str(self) -> &'static str {
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
pub(crate) const EXIT_IO: u8 = 2;

/// The command summary printed by `--help`, by an unknown command, and by a
/// subcommand invoked with the wrong number of arguments.
pub(crate) const USAGE: &str = "\
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
pub(crate) fn handle_help(out: &mut impl io::Write) -> io::Result<ExitCode> {
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
    let target = match args.iter().find(|argument| !argument.starts_with("--")) {
        Some(named) => PathBuf::from(named),
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

/// Says where `scan` went: the zero-gate detectors read Rust through `lgwks_ast`
/// now (ADR 0002, #408), and this binary no longer carries a second Rust parser.
fn handle_scan_moved(err: &mut impl io::Write) -> io::Result<ExitCode> {
    writeln!(
        err,
        "lgwks-deps: `scan` moved to `lgwks-ast scan` (feature `scan`; ADR 0002, #408)"
    )?;
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
        Some("scan") => handle_scan_moved(err),
        Some("debug") => handle_debug(&args[1..], out, err),
        Some("-h" | "--help" | "help") => handle_help(out),
        other => handle_unknown(other, err),
    }
}

/// Locks both output handles once and runs the requested command.
///
/// The handles are locked for the life of the process rather than per line, so a
/// long report does not re-acquire the lock for every line it writes. [`settle`] turns the command's write result
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

// ── check ───────────────────────────────────────────────────────────────────

/// Audits `root`, optionally against a register held elsewhere.
///
/// The error is flattened to a `String` here because every caller only ever
/// prints it; the structured [`lgwks_deps::GateError`] is preserved for
/// library embedders who branch on the failure mode.
pub(crate) fn audit_root(
    root: &Path,
    contract_override: &Option<PathBuf>,
    err: &mut impl io::Write,
) -> io::Result<Result<Verdict, String>> {
    let outcome = check_verdict(root, contract_override.as_deref());
    match outcome {
        Ok(collected) => warn_unresolved(collected, err).map(Ok),
        Err(error) => Ok(Err(error.to_string())),
    }
}

/// The verdict from a complete Cargo metadata read, printing any capture
/// cleanup that could not be confirmed after it.
///
/// The warning goes to stderr in both output modes, so `--json` stdout stays
/// one document. The verdict and exit code do not change: the graph was read
/// in full, and the leftover path is named so an operator can remove it.
pub(crate) fn warn_unresolved<T>(
    collected: Collected<T>,
    err: &mut impl io::Write,
) -> io::Result<T> {
    let (value, unresolved) = collected.into_parts();
    if let Some(cleanup) = unresolved {
        writeln!(err, "WARN  cargo metadata: {cleanup}")?;
    }
    Ok(value)
}

/// Reads the optional invariant register, flattening its structured error for
/// the human-facing command while retaining the structured library API.
pub(crate) fn audit_invariant_root(
    root: &Path,
    err: &mut impl io::Write,
) -> io::Result<Result<Option<(InvariantRegister, InvariantAudit)>, String>> {
    match check_invariants(root) {
        Ok(Some(collected)) => warn_unresolved(collected, err).map(|audit| Ok(Some(audit))),
        Ok(None) => Ok(Ok(None)),
        Err(error) => Ok(Err(error.to_string())),
    }
}

/// Prints every refusal and returns the verdict's exit code.
///
/// Always exit 2 when there is at least one refusal. Adoption mode no longer
/// returns success from this path: an `enforce = false` register on a tree that
/// carries refusals is itself refused, so arriving here with a non-empty list
/// means the gate found something, and the note explains which posture it was
/// read under.
pub(crate) fn report_refusals(
    root: &Path,
    register: &Contract,
    refusals: &[Refusal],
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    writeln!(
        err,
        "REFUSED  {} — {} dependency-edge violations\n",
        root.display(),
        refusals.len()
    )?;
    for refusal in refusals {
        writeln!(err, "  {refusal}")?;
    }
    writeln!(
        err,
        "\nEach one is a decision, not a paperwork step. Climb the ladder first \
         (`lgwks-deps tiers`);\nif the answer is still a dependency, \
         `lgwks-deps request <crate> <version>` prints the block."
    )?;
    if !register.enforce {
        writeln!(
            err,
            "\nNOTE  [policy] enforce = false does not make these pass. Adoption mode \
             is a reviewable posture over a *clean* tree; a stand-down over a violating \
             tree is refused above, so fix the named edges or set enforce = true."
        )?;
    }
    Ok(ExitCode::from(2))
}

/// Prints the admitted-edge summary and returns success.
///
/// `count` is the number of approvals in the register, which on this path is
/// the number of entries every one of which was matched by a real edge.
pub(crate) fn report_ok(
    root: &Path,
    count: usize,
    out: &mut impl io::Write,
) -> io::Result<ExitCode> {
    writeln!(
        out,
        "OK  {} — {} semantic approvals, every authored external edge is owned",
        root.display(),
        count
    )?;
    Ok(ExitCode::SUCCESS)
}

/// Resolves the repository root, audits it, and reports the verdict.
///
/// A failure to find a lock file or to read the register is a refusal with exit
/// code 2, never a pass: the gate is fail-closed, so "could not check" and
/// "checked and refused" are the same verdict.
///
/// `json_output` changes the *rendering* only. The verdict, the exit code, and
/// the fail-closed behaviour are identical in both modes, so a consumer that
/// switches to `--json` cannot accidentally get a laxer gate.
pub(crate) fn run_check(
    path: Option<PathBuf>,
    contract_override: Option<PathBuf>,
    json_output: bool,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let start = subject_path(path.as_deref());
    let mode = PolicyMode::of(&contract_override);
    let root = match repository_root(&start) {
        Ok(root) => root,
        Err(error) => {
            return report_check(
                &CheckOutcome {
                    root: None,
                    mode,
                    audit: Err(&error.to_string()),
                },
                json_output,
                out,
                err,
            );
        }
    };
    let dependency = audit_root(&root, &contract_override, err)?;
    match audit_invariant_root(&root, err)? {
        Ok(None) => report_check(
            &CheckOutcome {
                root: Some(&root),
                mode,
                audit: dependency.as_ref().map_err(String::as_str),
            },
            json_output,
            out,
            err,
        ),
        Ok(Some((invariant_register, invariant_audit))) => report_check_with_invariants(
            &root,
            mode,
            dependency.as_ref().map_err(String::as_str),
            InvariantReport {
                register: &invariant_register,
                audit: &invariant_audit,
            },
            json_output,
            out,
            err,
        ),
        Err(invariant_error) => report_check_with_invariant_error(
            &root,
            mode,
            dependency.as_ref().map_err(String::as_str),
            &invariant_error,
            json_output,
            out,
            err,
        ),
    }
}

/// Prints a one-line refusal on stderr and returns the refusal exit code.
///
/// Every refusal path in this binary funnels through here so exit code 2 means
/// exactly one thing regardless of which check produced it.
pub(crate) fn refuse(message: &str, err: &mut impl io::Write) -> io::Result<ExitCode> {
    writeln!(err, "REFUSED  {message}")?;
    Ok(ExitCode::from(2))
}

/// Returns a value or emits a command refusal and exits the current handler.
macro_rules! unwrap_or_refuse {
    ($result:expr, $err:expr) => {
        match $result {
            Ok(value) => value,
            Err(message) => return refuse(&message, $err),
        }
    };
}

mod check_report;
mod debug_cmd;
mod freshness_cmd;
mod request_cmd;

use check_report::{
    CheckOutcome, InvariantReport, report_check, report_check_with_invariant_error,
    report_check_with_invariants,
};
use debug_cmd::handle_debug;
use freshness_cmd::handle_freshness;
use request_cmd::{run_init, run_request};

/// Resolves a repository root into the command's string-refusal vocabulary.
pub(crate) fn resolve_repository_root(start: &Path) -> Result<PathBuf, String> {
    repository_root(start).map_err(|error| error.to_string())
}

/// The subject a command audits: the path it named, or the directory the
/// operator is standing in.
///
/// A command given no path is not auditing nothing: it audits the repository
/// the process was started in, which is the one subject that is always a
/// repository. Every command resolves its target here so that rule is one fact
/// about the CLI rather than a substitution repeated at each call site.
pub(crate) fn subject_path(named: Option<&Path>) -> PathBuf {
    match named {
        Some(path) => path.to_path_buf(),
        None => PathBuf::from("."),
    }
}

/// Reads the lockfile text for commands that audit the resolved dependency set.
pub(crate) fn read_lock_text(root: &Path) -> Result<String, String> {
    let lock_path = root.join("Cargo.lock");
    std::fs::read_to_string(&lock_path)
        .map_err(|error| format!("cannot read {}: {error}", lock_path.display()))
}

// ── vendor ──────────────────────────────────────────────────────────────────

/// Handles `vendor check`, rejecting any other `vendor` subcommand.
fn handle_vendor(
    args: &[String],
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    if args.first().map(String::as_str) != Some("check") {
        write!(err, "{USAGE}")?;
        return Ok(ExitCode::from(2));
    }
    let positional: Vec<&String> = args[1..]
        .iter()
        .filter(|arg| !arg.starts_with("--"))
        .collect();
    let start = subject_path(
        positional
            .first()
            .map(|candidate| Path::new(candidate.as_str())),
    );
    run_vendor_check(&start, out, err)
}

/// Writes the refusal for a lock file the vendor tree does not cover: how many
/// locked packages are missing, each one by name and version, and the repair.
fn write_missing_packages(
    root: &Path,
    tree: &Path,
    report: &lgwks_deps::vendor::Report,
    err: &mut impl io::Write,
) -> io::Result<()> {
    writeln!(
        err,
        "REFUSED  {} — {} of {} locked packages missing from {}\n",
        root.display(),
        report.missing().len(),
        // Both operands are counts of entries in one lock file, so the
        // sum is bounded by its package count and cannot overflow.
        report.covered().saturating_add(report.missing().len()),
        tree.display()
    )?;
    for missing in report.missing() {
        writeln!(err, "  {} {}", missing.name(), missing.version())?;
    }
    writeln!(
        err,
        "\nRe-run the vendor sync for this repo, then re-check."
    )
}

/// Proves every locked package resolves to the shared vendor tree.
///
/// Exit 0 when the tree covers the lock file and 2 when any locked package is
/// missing from it, since an uncovered package means the offline build would
/// reach the network.
fn run_vendor_check(
    start: &Path,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let root = unwrap_or_refuse!(resolve_repository_root(start), err);
    let tree = match lgwks_deps::vendor::tree_for(&root) {
        Ok(tree) => tree,
        Err(error) => return refuse(&error.to_string(), err),
    };
    let lock_text = unwrap_or_refuse!(read_lock_text(&root), err);
    match lgwks_deps::vendor::check_coverage(&lock_text, &tree) {
        Ok(report) if report.is_complete() => {
            writeln!(
                out,
                "OK  {} — {} locked packages covered by {}, {} local skipped",
                root.display(),
                report.covered(),
                tree.display(),
                report.skipped_local()
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Ok(report) => {
            write_missing_packages(&root, &tree, &report, err)?;
            Ok(ExitCode::from(2))
        }
        Err(error) => refuse(&error.to_string(), err),
    }
}

// ── Ladder text ─────────────────────────────────────────────────────────────

/// The admission ladder printed by `tiers` and prefixed to an approval
/// template.
///
/// Read lowest rung first: each step up is an escalation that needs a stated
/// reason, and only the last two rungs (`VENDOR` and `BOUNDARY`) produce a
/// register entry at all.
pub(crate) const LADDER: &str = "\
The core admission ladder (INV-DEP-EDGE-OWNED)

Every direct dependency authored in a workspace manifest must be accounted for.
Transitive closure remains lockfile provenance, not package-level authority.
Lower rungs are preferred; each step up is an escalation that requires a reason.

  1. Rust standard library (std / core / alloc)
     Preferred unconditionally. Zero dependencies, zero supply-chain risk.

  2. Workspace stdlib+ (`lgwks_std`)
     The common substrate: id (uuid v4), hex, time (RFC 3339), glob, fs, leb128, task.
     The workspace facade owns its audited external implementations behind one API.

  3. ELIMINATE
     Crates whose functionality belongs in `lgwks_std` or std.
     Target for removal: write the minimal zero-dependency implementation.

  4. CONSOLIDATE
     Multiple crates solving the same problem.
     Target for convergence: pick one, retire the rest.

  5. VENDOR
     A mature, audit-clean dependency whose code is reviewed and checked into the
     workspace rather than resolved through crates.io at build time.

  6. BOUNDARY
     A third-party dependency approved for use across an external boundary
     (e.g., protocol parsers, hardware drivers, cryptographic primitives).
     Must be declared in `contract/APPROVED.toml` with a human sign-off.
";
