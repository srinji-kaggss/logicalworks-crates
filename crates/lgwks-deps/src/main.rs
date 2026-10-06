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
    CONTRACT_PATH, Refusal, Verdict, check_invariants, check_verdict,
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
   lgwks-deps scan [PATH]...            run the zero-gate source detectors
                                        (error swallow, unlogged returns,
                                        lint allowances, try chains, docs)
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
    let target = args
        .iter()
        .find(|argument| !argument.starts_with("--"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
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
/// `scan` writes one line per finding, and re-acquiring the lock thousands of
/// times would be pure overhead. [`settle`] turns the command's write result
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
fn audit_root(
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
fn warn_unresolved<T>(collected: Collected<T>, err: &mut impl io::Write) -> io::Result<T> {
    let (value, unresolved) = collected.into_parts();
    if let Some(cleanup) = unresolved {
        writeln!(err, "WARN  cargo metadata: {cleanup}")?;
    }
    Ok(value)
}

/// Reads the optional invariant register, flattening its structured error for
/// the human-facing command while retaining the structured library API.
fn audit_invariant_root(
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
fn report_refusals(
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
fn report_ok(root: &Path, count: usize, out: &mut impl io::Write) -> io::Result<ExitCode> {
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
fn run_check(
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

/// The receipt-bearing result of one `check` invocation.
struct CheckOutcome<'a> {
    /// Repository root, or `None` when root discovery failed.
    root: Option<&'a Path>,
    /// Whether the register was committed enforcement or a `--contract` override.
    mode: PolicyMode,
    /// The audited verdict, or the flattened error that prevented one.
    audit: Result<&'a Verdict, &'a str>,
}

/// Optional invariant data added to the existing machine-readable check shape.
struct InvariantJson<'a> {
    /// Parsed register, when the optional file was readable.
    register: Option<&'a InvariantRegister>,
    /// Resolved verdicts, when the optional file was readable.
    audit: Option<&'a InvariantAudit>,
    /// Register error, when parsing or workspace scope discovery failed.
    error: Option<&'a str>,
}

/// Parsed invariant register and its resolved audit, carried together.
#[derive(Clone, Copy)]
struct InvariantReport<'a> {
    /// Parsed invariant register.
    register: &'a InvariantRegister,
    /// Resolved invariant audit.
    audit: &'a InvariantAudit,
}

impl<'a> InvariantReport<'a> {
    /// Converts this pair into the JSON payload shape.
    const fn json(self) -> InvariantJson<'a> {
        InvariantJson {
            register: Some(self.register),
            audit: Some(self.audit),
            error: None,
        }
    }

    /// Computes the combined check exit code for this invariant report.
    ///
    /// Both halves are pure functions of the refusal count. `enforce = false`
    /// no longer reaches this verdict: `audit_direct` refuses a stand-down
    /// outright when the tree carries violations, so an adoption-mode register
    /// on a violating tree arrives here with a non-empty list and exits 2
    /// exactly as `enforce = true` would. No token remains whose flip changes
    /// the verdict.
    fn check_exit_code(self, refusals: &[Refusal]) -> ExitCode {
        if refusals.is_empty() && self.audit.refusals().is_empty() {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(2)
        }
    }
}

/// Reports a check whose dependency register audited, beside its invariant audit.
fn report_audited_check(
    root: &Path,
    mode: PolicyMode,
    verdict: &Verdict,
    invariant: InvariantReport<'_>,
    json_output: bool,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let register = verdict.register();
    let refusals = verdict.refusals();
    if json_output {
        print_check_json(
            &CheckOutcome {
                root: Some(root),
                mode,
                audit: Ok(verdict),
            },
            Some(invariant.json()),
            out,
        )?;
        return Ok(invariant.check_exit_code(refusals));
    }
    if refusals.is_empty() && invariant.audit.refusals().is_empty() {
        writeln!(
            out,
            "OK  {} — {} semantic approvals; {} invariants resolve ({} resolved, {} attested by a recorded run)",
            root.display(),
            register.entry_count(),
            invariant.audit.registered(),
            invariant.audit.resolved(),
            invariant.audit.attested()
        )?;
        write_receipt(
            &CheckOutcome {
                root: Some(root),
                mode,
                audit: Ok(verdict),
            },
            out,
        )?;
        writeln!(out, "SCOPE  {INVARIANT_SCOPE}")?;
        return Ok(ExitCode::SUCCESS);
    }
    writeln!(
        err,
        "REFUSED  {} — {} dependency-edge violations, {} invariant violations\n",
        root.display(),
        refusals.len(),
        invariant.audit.refusals().len()
    )?;
    write_dependency_refusals(refusals, err)?;
    write_invariant_refusals(invariant.audit, err)?;
    write_receipt(
        &CheckOutcome {
            root: Some(root),
            mode,
            audit: Ok(verdict),
        },
        err,
    )?;
    writeln!(err, "SCOPE  {INVARIANT_SCOPE}")?;
    writeln!(
        err,
        "\nBoth registers are reviewed contracts; repair each named refusal before delivery."
    )?;
    Ok(invariant.check_exit_code(refusals))
}

/// Reports one check verdict after both registers have been audited.
fn report_check_with_invariants(
    root: &Path,
    mode: PolicyMode,
    audit: Result<&Verdict, &str>,
    invariant: InvariantReport<'_>,
    json_output: bool,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    match audit {
        Ok(verdict) => report_audited_check(root, mode, verdict, invariant, json_output, out, err),
        Err(dependency_error) => report_check_with_dependency_error(
            root,
            mode,
            dependency_error,
            invariant,
            json_output,
            out,
            err,
        ),
    }
}

/// Reports a dependency-register error while preserving invariant findings.
fn report_check_with_dependency_error(
    root: &Path,
    mode: PolicyMode,
    dependency_error: &str,
    invariant: InvariantReport<'_>,
    json_output: bool,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let outcome = CheckOutcome {
        root: Some(root),
        mode,
        audit: Err(dependency_error),
    };
    if json_output {
        print_check_json(&outcome, Some(invariant.json()), out)?;
        return Ok(ExitCode::from(2));
    }
    writeln!(
        err,
        "REFUSED  {} — dependency register error, {} invariant violations\n",
        root.display(),
        invariant.audit.refusals().len()
    )?;
    write_register_detail(err, "dependency", dependency_error)?;
    write_invariant_refusals(invariant.audit, err)?;
    write_receipt(&outcome, err)?;
    writeln!(err, "SCOPE  {INVARIANT_SCOPE}")?;
    Ok(ExitCode::from(2))
}

/// Reports an invariant-register error while preserving any dependency result.
fn report_check_with_invariant_error(
    root: &Path,
    mode: PolicyMode,
    audit: Result<&Verdict, &str>,
    invariant_error: &str,
    machine_output: bool,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let outcome = CheckOutcome {
        root: Some(root),
        mode,
        audit,
    };
    let broken_invariant = InvariantJson {
        register: None,
        audit: None,
        error: Some(invariant_error),
    };
    if machine_output {
        print_check_json(&outcome, Some(broken_invariant), stdout)?;
        return Ok(ExitCode::from(2));
    }
    match audit {
        Ok(verdict) => write_invariant_register_error(root, verdict, invariant_error, stderr)?,
        Err(dependency_error) => {
            write_both_registers_unaudited(root, dependency_error, invariant_error, stderr)?;
        }
    }
    write_receipt(&outcome, stderr)?;
    Ok(ExitCode::from(2))
}

/// Writes the human refusal for a tree whose dependency register audited and
/// whose invariant register did not, naming every dependency refusal it found.
fn write_invariant_register_error(
    root: &Path,
    verdict: &Verdict,
    invariant_error: &str,
    writer: &mut impl io::Write,
) -> io::Result<()> {
    writeln!(
        writer,
        "REFUSED  {} — {} dependency-edge violations, invariant register error\n",
        root.display(),
        verdict.refusals().len()
    )?;
    write_dependency_refusals(verdict.refusals(), writer)?;
    write_register_detail(writer, "invariant", invariant_error)
}

/// Writes the human refusal for a tree on which neither register could be
/// audited, naming why each one could not.
fn write_both_registers_unaudited(
    root: &Path,
    dependency_error: &str,
    invariant_error: &str,
    writer: &mut impl io::Write,
) -> io::Result<()> {
    writeln!(
        writer,
        "REFUSED  {} — both registers could not be audited",
        root.display()
    )?;
    write_register_detail(writer, "dependency", dependency_error)?;
    write_register_detail(writer, "invariant", invariant_error)
}

/// Writes the receipt that binds the verdict to what it was reached against.
///
/// `contract` names the register identity and version, `subject` the exact
/// metadata graph, and `mode` whether the register was committed enforcement or
/// a `--contract` diagnosis. The root is printed by the verdict line; this adds
/// the identities a receipt needs and a re-run can compare.
fn write_receipt(outcome: &CheckOutcome<'_>, writer: &mut impl io::Write) -> io::Result<()> {
    match outcome.audit {
        Ok(verdict) => writeln!(
            writer,
            "CONTRACT  {} schema={} entries={}",
            verdict.register().digest(),
            verdict.register().schema(),
            verdict.register().entry_count()
        )?,
        Err(error) => writeln!(writer, "CONTRACT  unresolved: {error}")?,
    }
    match outcome.audit {
        Ok(verdict) => writeln!(
            writer,
            "SUBJECT   {} edges={}",
            verdict.subject().digest(),
            verdict.subject().edges()
        )?,
        Err(_) => writeln!(writer, "SUBJECT   unresolved")?,
    }
    writeln!(writer, "MODE      {}", outcome.mode.as_str())
}

/// Writes dependency-register refusals with an explicit zero line.
fn write_dependency_refusals(refusals: &[Refusal], err: &mut impl io::Write) -> io::Result<()> {
    if refusals.is_empty() {
        write_register_detail(err, "dependency", "0 refusals")
    } else {
        for refusal in refusals {
            write_register_detail(err, "dependency", &refusal.to_string())?;
        }
        Ok(())
    }
}

/// Writes invariant-register refusals with an explicit zero line.
fn write_invariant_refusals(audit: &InvariantAudit, err: &mut impl io::Write) -> io::Result<()> {
    if audit.refusals().is_empty() {
        write_register_detail(err, "invariant", "0 refusals")
    } else {
        for refusal in audit.refusals() {
            write_register_detail(err, "invariant", &refusal.to_string())?;
        }
        Ok(())
    }
}

/// Writes one register detail line.
fn write_register_detail(err: &mut impl io::Write, register: &str, detail: &str) -> io::Result<()> {
    writeln!(err, "  {register} register: {detail}")
}

/// Renders the verdict in the requested mode and returns the exit code.
///
/// One function rather than a branch at each call site: the human and machine
/// renderings differ in bytes and must not differ in *verdict*, and the only way
/// to guarantee that is for a single place to compute it. Both modes exit 0 for
/// an admitted tree and 2 for any refusal, including a stand-down of refusals.
fn report_check(
    outcome: &CheckOutcome<'_>,
    json_output: bool,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    if json_output {
        print_check_json(outcome, None, out)?;
        // A gate that could not reach a verdict is a refusal, and exits 2.
        // This arm is first because the code below would otherwise *pass*: with
        // no register, `enforce` defaults to true and `refusals` is empty, so
        // `refusals.is_empty()` is satisfied and the gate would
        // report success for a tree it never read. That is the one failure this
        // crate's fail-closed rule exists to prevent, and it was reachable only
        // through `--json`.
        if outcome.audit.is_err() {
            return Ok(ExitCode::from(2));
        }
        // The verdict is the refusal count alone. `enforce = false` is no
        // longer consulted: an adoption-mode register on a violating tree is
        // itself refused by `audit_direct`, so it arrives here with a non-empty
        // `refusals` and exits 2 -- identical to the human path, which is the
        // property `--json` must never lose.
        let admitted = outcome
            .audit
            .is_ok_and(|verdict| verdict.refusals().is_empty());
        return Ok(if admitted {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(2)
        });
    }

    let verdict = match outcome.audit {
        Ok(verdict) => verdict,
        Err(message) => return refuse(message, err),
    };
    // The audited subject is the root the collection resolved. A collection
    // that reached a verdict without one is not a collection this module can
    // have produced, and naming the operator's own directory would print a
    // receipt for a repository nobody asked about — the defect `check_cli`
    // pins. It is therefore reported as an unreadable outcome rather than
    // rendered against an invented subject.
    let Some(named) = outcome.root else {
        let message = "the audit reached a verdict without naming a repository";
        lgwks_std::trace::debug!(error = ?message, "report: the audit reached a verdict without a root");
        return refuse(message, err);
    };
    if verdict.refusals().is_empty() {
        report_ok(named, verdict.register().entry_count(), out)?;
        finish_verdict(outcome, out, ExitCode::SUCCESS)
    } else {
        let code = report_refusals(named, verdict.register(), verdict.refusals(), err)?;
        finish_verdict(outcome, err, code)
    }
}

/// Closes a human verdict with its receipt and the scope the verdict covers,
/// then hands back the exit code the verdict earned.
fn finish_verdict(
    outcome: &CheckOutcome<'_>,
    writer: &mut impl io::Write,
    code: ExitCode,
) -> io::Result<ExitCode> {
    write_receipt(outcome, writer)?;
    writeln!(writer, "SCOPE  {INVARIANT_SCOPE}")?;
    Ok(code)
}

/// Writes the verdict as one JSON object on `out`.
///
/// Built through `lgwks_std::json` rather than by hand. This crate enforces the
/// rule that JSON comes through the workspace facade, and emitting its own JSON
/// by string concatenation would make the checker the first thing that violates
/// it, which is exactly how `print_freshness_json` came to escape only the
/// double quote and produce invalid output for any string carrying a backslash.
///
/// The shape is contractual and every key is always present, so a consumer can
/// read `refusals` without first testing for its existence. `error` is `null`
/// unless the gate could not reach a verdict at all.
fn print_check_json(
    outcome: &CheckOutcome<'_>,
    invariant: Option<InvariantJson<'_>>,
    out: &mut impl io::Write,
) -> io::Result<()> {
    use lgwks_std::json::{Map, Value};

    let verdict = outcome.audit.ok();
    let register = verdict.map(Verdict::register);
    let refusals: &[Refusal] = verdict.map_or(&[], Verdict::refusals);
    let error = outcome.audit.err();

    let mut refusal_rows = Vec::with_capacity(refusals.len());
    for refusal in refusals {
        let mut row = Map::new();
        row.insert(
            "crate".to_owned(),
            Value::String(refusal.krate().to_owned()),
        );
        row.insert("detail".to_owned(), Value::String(refusal.to_string()));
        refusal_rows.push(Value::Object(row));
    }

    let mut payload = Map::new();
    payload.insert(
        "root".to_owned(),
        match outcome.root {
            Some(path) => Value::String(format!("{}", path.display())),
            None => Value::Null,
        },
    );
    // The receipt's stable, agent-readable fields: which policy mode reached
    // the verdict, and the two identities it is bound to.
    payload.insert(
        "mode".to_owned(),
        Value::String(outcome.mode.as_str().to_owned()),
    );
    let mut contract = Map::new();
    contract.insert(
        "digest".to_owned(),
        match register {
            Some(register) => Value::String(register.digest().to_owned()),
            None => Value::Null,
        },
    );
    contract.insert(
        "schema".to_owned(),
        match register {
            Some(register) => Value::Number(schema_number(register.schema())),
            None => Value::Null,
        },
    );
    contract.insert(
        "entries".to_owned(),
        Value::Number(serde_json_number(register.map_or(0, Contract::entry_count))),
    );
    contract.insert(
        "repository".to_owned(),
        match register.and_then(|register| register.repository.as_ref()) {
            Some(repository) => Value::String(repository.clone()),
            None => Value::Null,
        },
    );
    // The licence policy the verdict was judged by, as the register declared
    // it, so a consumer reading the receipt sees the set that admitted or
    // refused each edge rather than inferring one the gate does not hold.
    contract.insert(
        "accepted_licenses".to_owned(),
        match register.and_then(Contract::accepted_licenses) {
            Some(accepted) => Value::Array(
                accepted
                    .iter()
                    .map(|licence| Value::String(licence.clone()))
                    .collect(),
            ),
            None => Value::Null,
        },
    );
    payload.insert("contract".to_owned(), Value::Object(contract));
    let mut subject = Map::new();
    subject.insert(
        "digest".to_owned(),
        match verdict {
            Some(verdict) => Value::String(verdict.subject().digest().to_owned()),
            None => Value::Null,
        },
    );
    subject.insert(
        "edges".to_owned(),
        match verdict {
            Some(verdict) => Value::Number(serde_json_number(verdict.subject().edges())),
            None => Value::Null,
        },
    );
    subject.insert("resolved".to_owned(), Value::Bool(verdict.is_some()));
    payload.insert("subject".to_owned(), Value::Object(subject));
    payload.insert(
        "scope".to_owned(),
        Value::String(INVARIANT_SCOPE.to_owned()),
    );
    payload.insert(
        "enforce".to_owned(),
        match register {
            Some(contract) => Value::Bool(contract.enforce),
            None => Value::Null,
        },
    );
    let dependency_admitted = error.is_none() && refusals.is_empty();
    let invariant_admitted = invariant.as_ref().is_none_or(|json| {
        json.error.is_none()
            && (json.audit.is_none_or(|audit| audit.refusals().is_empty())
                || json.register.is_none_or(|register| !register.enforce))
    });
    // `admitted` is the same predicate the exit code carries: a tree the gate
    // could not read is not admitted, so `error.is_some()` must make this false
    // even though there are no refusals to list. Otherwise a consumer reading
    // the payload instead of the exit code would see `admitted: true` beside an
    // error, which is the fail-open reading this field must never support.
    payload.insert(
        "admitted".to_owned(),
        Value::Bool(dependency_admitted && invariant_admitted),
    );
    payload.insert(
        "approvals".to_owned(),
        // Bounded by the register's entry count, which is a file length.
        Value::Number(serde_json_number(register.map_or(0, Contract::entry_count))),
    );
    payload.insert("refusals".to_owned(), Value::Array(refusal_rows));
    payload.insert(
        "error".to_owned(),
        match error {
            Some(message) => Value::String(message.to_owned()),
            None => Value::Null,
        },
    );
    if let Some(json) = invariant {
        let mut invariant_rows = Vec::new();
        if let Some(audit) = json.audit {
            for refusal in audit.refusals() {
                let mut row = Map::new();
                row.insert("id".to_owned(), Value::String(refusal.id().to_owned()));
                row.insert("detail".to_owned(), Value::String(refusal.to_string()));
                invariant_rows.push(Value::Object(row));
            }
        }
        let mut outcome_rows = Vec::new();
        if let Some(audit) = json.audit {
            for outcome in audit.outcomes() {
                let mut row = Map::new();
                row.insert("id".to_owned(), Value::String(outcome.id().to_owned()));
                row.insert(
                    "status".to_owned(),
                    Value::String(outcome.status.as_str().to_owned()),
                );
                outcome_rows.push(Value::Object(row));
            }
        }
        let mut invariant_payload = Map::new();
        invariant_payload.insert(
            "enforce".to_owned(),
            match json.register {
                Some(register) => Value::Bool(register.enforce),
                None => Value::Null,
            },
        );
        invariant_payload.insert(
            "registered".to_owned(),
            Value::Number(serde_json_number(
                json.register.map_or(0, InvariantRegister::entry_count),
            )),
        );
        // Counts, not a verdict. There is no `enforced` key and there must
        // never be one: this command reaches these numbers by reading files and
        // manifests, so it has no standing to say an invariant holds. The
        // `scope` string is emitted beside them for the same reason the human
        // rendering prints it.
        invariant_payload.insert(
            "resolved".to_owned(),
            Value::Number(serde_json_number(
                json.audit.map_or(0, InvariantAudit::resolved),
            )),
        );
        invariant_payload.insert(
            "attested".to_owned(),
            Value::Number(serde_json_number(
                json.audit.map_or(0, InvariantAudit::attested),
            )),
        );
        invariant_payload.insert(
            "scope".to_owned(),
            Value::String(INVARIANT_SCOPE.to_owned()),
        );
        invariant_payload.insert("outcomes".to_owned(), Value::Array(outcome_rows));
        invariant_payload.insert("refusals".to_owned(), Value::Array(invariant_rows));
        invariant_payload.insert(
            "error".to_owned(),
            match json.error {
                Some(message) => Value::String(message.to_owned()),
                None => Value::Null,
            },
        );
        payload.insert("invariants".to_owned(), Value::Object(invariant_payload));
    }

    let rendered =
        lgwks_std::json::to_string_pretty(&Value::Object(payload)).map_err(io::Error::other)?;
    writeln!(out, "{rendered}")
}

/// Convert a `usize` count into a JSON number.
///
/// `serde_json::Number` is `i64`-backed unless the `arbitrary_precision` feature
/// is on, which it is not. A register with more than `i64::MAX` entries cannot
/// exist (the file would have to be exabytes long), so the conversion is total
/// in practice, and the fallback keeps it total in the type system too rather
/// than reaching for a cast the workspace forbids.
fn serde_json_number(count: usize) -> lgwks_std::json::Number {
    match i64::try_from(count) {
        Ok(exact) => lgwks_std::json::Number::from(exact),
        // A count past `i64::MAX` is reported as the largest number JSON can
        // hold, so a receipt says the count is not exactly countable rather than
        // reporting a smaller count as if it were the whole.
        Err(_) => lgwks_std::json::Number::from(i64::MAX),
    }
}

/// A JSON number for the register's declared schema version.
///
/// A `u32` is at most `u32::MAX`, which `i64` holds exactly, so the version is
/// rendered from the value itself. This is not the count path: a schema version
/// is never large enough to need a clamp, and clamping one would report a
/// register with a version this binary cannot name as the version it declared.
fn schema_number(version: u32) -> lgwks_std::json::Number {
    lgwks_std::json::Number::from(i64::from(version))
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
            Err(message) => return refuse(&message, $err),
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

// ── debug ──────────────────────────────────────────────────────────────────

/// Parsed arguments for the debugger doctor.
struct DebugArgs {
    /// Repository to inspect. `None` means the process working directory.
    target: Option<PathBuf>,
    /// True when the doctor report is written as JSON.
    json: bool,
}

/// A check that must hold for the debugger to be default-on.
struct DebugSurface {
    /// `default = [...]` includes `trace`.
    default_includes_trace: bool,
    /// `trace = [...]` includes `dep:tracing`.
    trace_includes_tracing: bool,
    /// `trace = [...]` includes `dep:tracing-subscriber`.
    trace_includes_subscriber: bool,
    /// The `tracing` dependency is declared.
    tracing_declared: bool,
    /// The `tracing-subscriber` dependency is declared.
    subscriber_declared: bool,
}

impl DebugSurface {
    /// True when no caller has to select an optional feature to get debugging.
    const fn passes(&self) -> bool {
        self.default_includes_trace
            && self.trace_includes_tracing
            && self.trace_includes_subscriber
            && self.tracing_declared
            && self.subscriber_declared
    }
}

/// Complete doctor report.
struct DebugReport {
    /// Repository root inspected by the doctor.
    root: PathBuf,
    /// Service name installed into the debugger.
    service_name: String,
    /// Filter directive installed into the debugger.
    filter: String,
    /// Output format installed into the debugger.
    format: lgwks_std::trace::DebugFormat,
    /// Manifest-level checks.
    surface: DebugSurface,
    /// True when the report should render as JSON.
    json: bool,
}

/// Parses `lgwks-deps debug` arguments.
fn parse_debug_args(args: &[String]) -> Result<DebugArgs, String> {
    let mut target: Option<PathBuf> = None;
    let mut json = false;
    for argument in args {
        match argument.as_str() {
            "--json" => json = true,
            flag if flag.starts_with("--") => {
                let refusal = Err(format!("unknown option for `debug`: {flag}"));
                lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse_debug_args: returning an error to the caller");
                return refusal;
            }
            value => {
                if target.is_some() {
                    let refusal = Err(format!(
                        "`debug` inspects one repository, and {value:?} is a second path"
                    ));
                    lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "parse_debug_args: returning an error to the caller");
                    return refusal;
                }
                target = Some(PathBuf::from(value));
            }
        }
    }
    Ok(DebugArgs { target, json })
}

/// Runs the debugger doctor and returns the report to render.
fn run_debug(args: &[String]) -> Result<DebugReport, String> {
    let request = parse_debug_args(args)?;
    let start = subject_path(request.target.as_deref());
    let root = repository_root(&start).map_err(|error| error.to_string())?;
    let mut config = lgwks_std::trace::DebugConfig::from_env("lgwks-deps")
        .map_err(|error| format!("cannot configure debugger: {error}"))?;
    if request.json {
        config = config.with_ansi(false);
    }
    let service_name = config.service_name().to_owned();
    let filter = config.filter().to_owned();
    let format = config.format();
    config
        .install()
        .map_err(|error| format!("cannot install debugger: {error}"))?;
    let surface = inspect_debug_surface(&root)?;
    let root_text = format!("{}", root.display());
    lgwks_std::trace::info!(
        service_name = service_name.as_str(),
        repository_root = root_text.as_str(),
        otel_schema_url = lgwks_std::trace::OTEL_SCHEMA_URL,
        default_debugger = surface.passes(),
        "debug doctor completed"
    );
    Ok(DebugReport {
        root,
        service_name,
        filter,
        format,
        surface,
        json: request.json,
    })
}

/// Inspects the standard-library manifest for the default debugger surface.
fn inspect_debug_surface(root: &Path) -> Result<DebugSurface, String> {
    let manifest_path = root.join("crates/lgwks-std/Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
    // A manifest that assigns no `default` or no `trace` list declares no
    // features there, so the question the surface answers is "does that list
    // include this entry" and an absent list answers it for itself.
    let default_value = assignment_value(&manifest, "default");
    let trace_value = assignment_value(&manifest, "trace");
    Ok(DebugSurface {
        default_includes_trace: default_value.is_some_and(|list| list.contains("\"trace\"")),
        trace_includes_tracing: trace_value
            .as_ref()
            .is_some_and(|list| list.contains("\"dep:tracing\"")),
        trace_includes_subscriber: trace_value
            .as_ref()
            .is_some_and(|list| list.contains("\"dep:tracing-subscriber\"")),
        tracing_declared: dependency_declared(&manifest, "tracing"),
        subscriber_declared: dependency_declared(&manifest, "tracing-subscriber"),
    })
}

/// Reads one TOML assignment value as text, including a multi-line array.
fn assignment_value(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} =");
    let mut lines = text.lines().map(str::trim);
    while let Some(line) = lines.next() {
        let Some(value) = line.strip_prefix(&prefix) else {
            continue;
        };
        let mut collected = value.to_owned();
        let mut opens = value.matches('[').count();
        let mut closes = value.matches(']').count();
        while opens > closes {
            let Some(next) = lines.next() else {
                break;
            };
            collected.push(' ');
            collected.push_str(next);
            opens = opens.saturating_add(next.matches('[').count());
            closes = closes.saturating_add(next.matches(']').count());
        }
        return Some(collected);
    }
    None
}

/// True when a direct dependency assignment exists in a manifest.
fn dependency_declared(text: &str, name: &str) -> bool {
    let prefix = format!("{name} =");
    text.lines()
        .map(str::trim)
        .any(|line| line.starts_with(&prefix))
}

/// Handles the cargo-doctor-style debugger command.
fn handle_debug(
    args: &[String],
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let report = match run_debug(args) {
        Ok(report) => report,
        Err(message) => return refuse(&message, err),
    };
    if report.json {
        print_debug_json(&report, out)?;
    } else if report.surface.passes() {
        print_debug_human(&report, "OK", out)?;
    } else {
        print_debug_human(&report, "REFUSED", err)?;
    }
    Ok(if report.surface.passes() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

/// Prints the human debugger report.
fn print_debug_human(
    report: &DebugReport,
    status: &str,
    writer: &mut impl io::Write,
) -> io::Result<()> {
    writeln!(
        writer,
        "{status}  {} — lgwks_std debugger bootstrap is default-on",
        report.root.display()
    )?;
    writeln!(writer, "SERVICE  {}", report.service_name)?;
    writeln!(writer, "FILTER   {}", report.filter)?;
    writeln!(writer, "FORMAT   {}", report.format.as_str())?;
    writeln!(writer, "SCHEMA   {}", lgwks_std::trace::OTEL_SCHEMA_URL)?;
    debug_check_line(
        writer,
        report.surface.default_includes_trace,
        "default feature includes trace",
    )?;
    debug_check_line(
        writer,
        report.surface.trace_includes_tracing,
        "trace includes tracing facade",
    )?;
    debug_check_line(
        writer,
        report.surface.trace_includes_subscriber,
        "trace includes default subscriber",
    )?;
    debug_check_line(
        writer,
        report.surface.tracing_declared,
        "tracing dependency declared",
    )?;
    debug_check_line(
        writer,
        report.surface.subscriber_declared,
        "tracing-subscriber dependency declared",
    )
}

/// Prints one debugger check.
fn debug_check_line(writer: &mut impl io::Write, pass: bool, label: &str) -> io::Result<()> {
    let mark = if pass { "yes" } else { "no" };
    writeln!(writer, "CHECK    {mark:<3} {label}")
}

/// Prints the JSON debugger report.
fn print_debug_json(report: &DebugReport, out: &mut impl io::Write) -> io::Result<()> {
    let payload = lgwks_std::json::json!({
        "command": "debug",
        "admitted": report.surface.passes(),
        "root": format!("{}", report.root.display()),
        "service_name": report.service_name.as_str(),
        "filter": report.filter.as_str(),
        "format": report.format.as_str(),
        "otel": {
            "schema_url": lgwks_std::trace::OTEL_SCHEMA_URL
        },
        "checks": {
            "default_includes_trace": report.surface.default_includes_trace,
            "trace_includes_tracing": report.surface.trace_includes_tracing,
            "trace_includes_tracing_subscriber": report.surface.trace_includes_subscriber,
            "tracing_declared": report.surface.tracing_declared,
            "tracing_subscriber_declared": report.surface.subscriber_declared
        }
    });
    let rendered = lgwks_std::json::to_string_pretty(&payload).map_err(io::Error::other)?;
    writeln!(out, "{rendered}")
}

// ── request ─────────────────────────────────────────────────────────────────

/// Prints the ladder followed by an approval block pre-filled with the crate
/// and version.
///
/// The blank fields are the point: this command writes a template a human must
/// complete and commit, and it deliberately cannot write the register itself.
fn print_request_template(krate: &str, version: &str, out: &mut impl io::Write) -> io::Result<()> {
    write!(out, "{LADDER}")?;
    writeln!(
        out,
        "\n\
         If every rung above still leaves a dependency, append this to {CONTRACT_PATH},\n\
         fill in the blanks, and commit it. The commit is the approval.\n\n\
         [[approved]]\n\
         crate = \"{krate}\"\n\
         tier = \"boundary\"          # boundary | vendor — see the ladder above\n\
         version = \"{version}\"\n\
         owner = \"\"                 # workspace crate responsible for the capability\n\
         capability = \"\"            # stable semantic capability name\n\
         license = \"\"              # SPDX expression the package declares\n\
         source = \"registry\"        # registry | git | path\n\
         allowed_consumers = \"\"     # comma-separated workspace crate names\n\
         allowed_kinds = \"normal\"   # normal, build, and/or dev\n\
         reason = \"\"                # one sentence naming what std cannot do\n\
         approved_by = \"\"           # the human who decided\n\
         approved_on = \"\"           # YYYY-MM-DD\n\
         review = \"\"                # path or URL to the evidence\n"
    )
}

/// Prints an approval template, or the usage block when either argument is
/// missing.
///
/// Both arguments are required (a template with no crate name is not useful),
/// so a partial invocation is a usage error on stderr with exit code 2.
fn run_request(
    krate: Option<&String>,
    version: Option<&String>,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let (Some(krate), Some(version)) = (krate, version) else {
        write!(err, "{USAGE}")?;
        return Ok(ExitCode::from(2));
    };
    print_request_template(krate, version, out)?;
    Ok(ExitCode::SUCCESS)
}

// ── init ────────────────────────────────────────────────────────────────────

/// The fail-closed register written by `init`.
///
/// `enforce = true` is the default on purpose: a repository brought onto the
/// gate starts refusing unregistered edges immediately, and standing enforcement
/// down is an explicit, reviewable edit rather than a default someone forgot to
/// change.
const STARTER: &str = "\
# Approved dependency edges — the semantic contract for INV-DEP-EDGE-OWNED.
#
# A crate reaches this file only after the ladder in `lgwks-deps tiers` has been
# climbed and every rung above a dependency was rejected for a stated reason.
# Adding a block here is an approval; the commit that adds it is the signature.
#
# ELIMINATE and CONSOLIDATE crates never appear here. They become a module in
# `lgwks_std` instead, which is why `tier` admits only `boundary` and `vendor`.

[policy]
# Fail-closed. Set to false only while a repo is being brought onto the gate,
# and only in a diff a human signed off — refusals then report as warnings.
enforce = true
# Canonical repository URL. A workspace member declaring a different repository
# is a copied foreign crate and must be consumed as a released dependency.
repository = \"\"
";

/// Refuses when `init` would overwrite an existing register.
///
/// An existing register may carry a human's approvals; `init` never clobbers
/// one. The caller reports the returned message as a refusal.
fn check_target_exists(target: &Path) -> Result<(), String> {
    if target.exists() {
        Err(format!(
            "{} already exists; init will not overwrite it",
            target.display()
        ))
    } else {
        Ok(())
    }
}

/// Creates the register's parent directory if it does not exist.
///
/// A path with no parent (a bare filename) needs no directory and is not an
/// error; `create_dir_all` is idempotent, so an existing directory is fine.
fn create_parent_dirs(target: &Path) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))
    } else {
        Ok(())
    }
}

/// Writes the starter register, refusing if one is already present.
fn prepare_init_file(target: &Path) -> Result<(), String> {
    check_target_exists(target)?;
    create_parent_dirs(target)?;
    std::fs::write(target, STARTER)
        .map_err(|error| format!("cannot write {}: {error}", target.display()))
}

/// Writes a fail-closed register at the repository root.
fn run_init(
    path: Option<PathBuf>,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let start = subject_path(path.as_deref());
    let root = unwrap_or_refuse!(resolve_repository_root(&start), err);
    let target = root.join(CONTRACT_PATH);
    if let Err(msg) = prepare_init_file(&target) {
        return refuse(&msg, err);
    }
    writeln!(out, "OK  initialized {}", target.display())?;
    Ok(ExitCode::SUCCESS)
}

// ── freshness ──────────────────────────────────────────────────────────────

/// Reports every registry dependency in the lock file against crates.io.
///
/// Exit 0 when nothing is stale and 1 when at least one package is behind, so
/// a cron job can distinguish "up to date" from "action needed". A local or
/// path package is skipped rather than queried. `--json` selects the stable
/// object-array form over the human table; both carry the same fields.
fn handle_freshness(
    args: &[String],
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let json_output = args.iter().any(|arg| arg == "--json");
    let positional: Vec<&String> = args.iter().filter(|arg| !arg.starts_with("--")).collect();
    let start = subject_path(
        positional
            .first()
            .map(|candidate| Path::new(candidate.as_str())),
    );

    let root = unwrap_or_refuse!(resolve_repository_root(&start), err);
    let lock_text = unwrap_or_refuse!(read_lock_text(&root), err);

    let resolved = match lgwks_deps::lock::parse(&lock_text) {
        Ok(parsed) => parsed,
        Err(error) => return refuse(&format!("Cargo.lock: {error}"), err),
    };

    let registry: Vec<&lgwks_deps::lock::Resolved> = resolved
        .iter()
        .filter(|package| !package.is_local())
        .collect();

    if registry.is_empty() {
        writeln!(out, "no registry dependencies in Cargo.lock")?;
        return Ok(ExitCode::SUCCESS);
    }

    let results = query_crates_io(&registry);

    if json_output {
        print_freshness_json(&results, out)?;
    } else {
        print_freshness_table(&results, out)?;
    }

    let stale_count = results.iter().filter(|result| result.stale).count();
    if stale_count > 0 {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

/// One registry package's freshness verdict.
///
/// A package whose lookup failed carries an `error` and is not stale: a network
/// fault is not evidence that the dependency is out of date, and reporting it as
/// stale would fail a build for a reason the lockfile cannot support.
struct FreshnessResult {
    /// Package name as it appears in `Cargo.lock`.
    name: String,
    /// Version the lock file resolved.
    resolved: String,
    /// Latest version crates.io reports, or `None` when it published none.
    latest: Option<String>,
    /// Upstream repository URL crates.io reports, or `None` when it published
    /// none. A published version carries no repository, and an unpublished one
    /// carries neither; neither is an empty string crates.io sent.
    repository: Option<String>,
    /// Whether `latest` is a real version newer than `resolved`.
    stale: bool,
    /// Why the lookup failed, when it did.
    error: Option<String>,
}

impl FreshnessResult {
    /// Successful registry lookup.
    fn found(
        package: &lgwks_deps::lock::Resolved,
        latest: Option<String>,
        repository: Option<String>,
    ) -> Self {
        // A registry that publishes no version for a crate cannot be compared,
        // so nothing is claimed: `stale` is only ever decided between two
        // versions the registry and the lock both named.
        let stale = match latest.as_deref() {
            Some(latest) => latest != package.version(),
            None => false,
        };
        Self {
            name: package.name().to_owned(),
            resolved: package.version().to_owned(),
            latest,
            repository,
            stale,
            error: None,
        }
    }

    /// Failed registry lookup, which is reported but not treated as stale.
    fn lookup_failed(package: &lgwks_deps::lock::Resolved, error: String) -> Self {
        Self {
            name: package.name().to_owned(),
            resolved: package.version().to_owned(),
            latest: None,
            repository: None,
            stale: false,
            error: Some(error),
        }
    }
}

/// The `User-Agent` crates.io's crawler policy asks for: this tool's name and
/// version and where to reach its maintainers, read from the crate's own
/// manifest. Compiled from `Cargo.toml` rather than written here, so the
/// version cannot go stale and no person's address is baked into every copy
/// of a published binary.
const USER_AGENT: &str = concat!(
    "User-Agent: lgwks-deps/",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CARGO_PKG_REPOSITORY"),
    ")"
);

/// Queries crates.io for each distinct package name.
///
/// Names are de-duplicated first: a lock file commonly resolves several
/// versions of one package, and the registry answer is per name. The lookup
/// shells out to `curl` rather than pulling an HTTP client (INV-GATE-ZERO-DEPS),
/// with a ten-second cap so an unreachable registry cannot hang the command.
fn query_crates_io(packages: &[&lgwks_deps::lock::Resolved]) -> Vec<FreshnessResult> {
    let mut seen = std::collections::HashSet::new();
    let mut results = Vec::new();

    for package in packages {
        if !seen.insert(package.name()) {
            continue;
        }

        let completed = std::process::Command::new("curl")
            .args([
                "-sf",
                "--max-time",
                "10",
                "-H",
                USER_AGENT,
                &format!("https://crates.io/api/v1/crates/{}", package.name()),
            ])
            .output();

        match completed {
            Ok(response) if response.status.success() => {
                let body = String::from_utf8_lossy(&response.stdout);
                let (latest, repo) = parse_crate_response(&body);
                results.push(FreshnessResult::found(package, latest, repo));
            }
            Ok(response) => {
                results.push(FreshnessResult::lookup_failed(
                    package,
                    format!("HTTP {}", response.status),
                ));
            }
            Err(error) => {
                results.push(FreshnessResult::lookup_failed(package, error.to_string()));
            }
        }
    }
    results
}

/// INV-GATE-ZERO-DEPS: no JSON parser; extract fields by line scan.
fn parse_crate_response(body: &str) -> (Option<String>, Option<String>) {
    let newest = extract_json_string(body, "newest_version");
    let repo = extract_json_string(body, "repository");
    (newest, repo)
}

/// Reads the value of `key` out of a JSON object by scanning for its quoted
/// form.
///
/// Both `"key":"value"` and `"key": "value"` spacing are accepted, and a key
/// that is absent yields `None` rather than an empty string: freshness is a
/// best-effort advisory, so a missing field is not a failure, and an empty
/// string is a value crates.io never sent. `None` is what the caller reports as
/// "not published", and an unterminated value is refused the same way, because
/// a half-read value is not a shorter one. This is deliberately not a JSON
/// parser; see the zero-deps invariant above.
fn extract_json_string(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\":\"", key);
    let alt_needle = format!("\"{}\": \"", key);
    // Each `index` is a byte offset at which `body` matched `needle`, so the
    // sum is a position inside `body` and cannot exceed `body.len()`; the
    // additions therefore cannot overflow `usize`.
    let start = body
        .find(&needle)
        .map(|index| index.saturating_add(needle.len()))
        .or_else(|| {
            body.find(&alt_needle)
                .map(|index| index.saturating_add(alt_needle.len()))
        });
    // Three refusals, one answer: the key is absent, the value is unterminated,
    // and the key is absent under the spaced spelling. None of them is a value
    // crates.io sent.
    let value_start = start?;
    // `end_offset` indexes the closing quote inside the slice that begins at
    // `value_start`, so the sum stays within `body`.
    let end_offset = body[value_start..].find('"')?;
    Some(body[value_start..value_start.saturating_add(end_offset)].to_string())
}

// Each label is a column value; the format string owns the alignment. The final
// column is spelled inside the format string rather than passed as an argument:
// a trailing string literal in a `{}` slot is what `clippy::write_literal`
// flags, and the rendered line is byte-identical either way.
/// The table cell for a field crates.io may not have published.
///
/// `?` is the marker a failed lookup already prints, and a version the registry
/// never published is the same fact to a reader: the value is not known. Naming
/// the mapping keeps one cell spelling for "not known" instead of an empty column
/// that reads as a value crates.io sent.
fn or_unpublished(value: Option<&str>) -> &str {
    match value {
        Some(value) => value,
        None => "?",
    }
}

/// Prints the human-readable freshness table.
///
/// A failed lookup is shown as `?` / `err` rather than as a version, so an
/// unreachable registry cannot be misread as an up-to-date dependency.
fn print_freshness_table(results: &[FreshnessResult], out: &mut impl io::Write) -> io::Result<()> {
    writeln!(
        out,
        "{:<30} {:<12} {:<12} {:<5} repository",
        "crate", "resolved", "latest", "stale"
    )?;
    writeln!(out, "{}", "-".repeat(90))?;
    for result in results {
        if let Some(failure) = result.error.as_ref() {
            writeln!(
                out,
                "{:<30} {:<12} {:<12} {:<5} {}",
                result.name, result.resolved, "?", "err", failure
            )?;
        } else {
            let stale_mark = if result.stale { "YES" } else { "" };
            writeln!(
                out,
                "{:<30} {:<12} {:<12} {:<5} {}",
                result.name,
                result.resolved,
                or_unpublished(result.latest.as_deref()),
                stale_mark,
                or_unpublished(result.repository.as_deref())
            )?;
        }
    }
    let stale_count = results.iter().filter(|result| result.stale).count();
    let err_count = results
        .iter()
        .filter(|result| result.error.is_some())
        .count();
    writeln!(
        out,
        "\n{} checked, {} stale, {} errors",
        results.len(),
        stale_count,
        err_count
    )?;
    Ok(())
}

/// Prints the freshness results as a JSON array of objects.
///
/// The shape is contractual: one object per checked package, in the order they
/// were queried, with `error` present only when the lookup failed. A stale
/// count is not emitted; consumers read `stale` per row and the process exit
/// code carries the aggregate.
///
/// Built through `lgwks_std::json`, like `check --json`. The previous version
/// assembled JSON by concatenation and escaped only the double quote, so any
/// `repository` or `error` string containing a backslash produced a payload no
/// parser would accept, from the crate whose whole purpose is enforcing that
/// dependencies go through the facade.
fn print_freshness_json(results: &[FreshnessResult], out: &mut impl io::Write) -> io::Result<()> {
    use lgwks_std::json::{Map, Value};

    let mut rows = Vec::with_capacity(results.len());
    for result in results {
        let mut row = Map::new();
        row.insert("name".to_owned(), Value::String(result.name.clone()));
        row.insert(
            "resolved".to_owned(),
            Value::String(result.resolved.clone()),
        );
        // `null` for a field crates.io did not publish, so a consumer reads
        // "not published" rather than a version nobody published.
        row.insert(
            "latest".to_owned(),
            match result.latest.as_ref() {
                Some(latest) => Value::String(latest.clone()),
                None => Value::Null,
            },
        );
        row.insert("stale".to_owned(), Value::Bool(result.stale));
        row.insert(
            "repository".to_owned(),
            match result.repository.as_ref() {
                Some(repository) => Value::String(repository.clone()),
                None => Value::Null,
            },
        );
        // Always present, `null` when the lookup succeeded: a consumer reads a
        // fixed set of keys rather than testing for the existence of each.
        row.insert(
            "error".to_owned(),
            match result.error.as_ref() {
                Some(failure) => Value::String(failure.clone()),
                None => Value::Null,
            },
        );
        rows.push(Value::Object(row));
    }

    let rendered =
        lgwks_std::json::to_string_pretty(&Value::Array(rows)).map_err(io::Error::other)?;
    writeln!(out, "{rendered}")
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

// ── scan ────────────────────────────────────────────────────────────────────

/// Collects `.rs` files under a path, skipping build output, vendored
/// sources, and virtualenv-style trees, the same external set the CI gate
/// excludes, so local and remote verdicts agree.
#[cfg(feature = "scan")]
fn collect_rs_files(root: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    dirs.sort();
    files.sort();
    for dir in dirs {
        // A directory name that is not UTF-8 is not one of the excluded names:
        // it compares equal to none of them, so it is walked. Reading it as an
        // empty name would compare equal to none of them too, but would do so
        // by inventing a name the filesystem does not carry.
        let excluded = dir
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                matches!(
                    name,
                    "target" | "vendor" | "node_modules" | "third_party" | ".venv" | ".git" | ".jj"
                )
            });
        if excluded {
            continue;
        }
        collect_rs_files(&dir, out);
    }
    out.extend(files);
}

/// Runs the zero-gate detectors over every collected `.rs` file.
///
/// Findings are printed one per line and the run exits 2 if there were any, so
/// a CI job can gate on the exit code alone. An unreadable file is reported as
/// a scan error rather than skipped: a detector that silently drops a file
/// reports a clean tree it never looked at.
#[cfg(feature = "scan")]
fn handle_scan(
    args: &[String],
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let positional: Vec<&String> = args.iter().filter(|arg| !arg.starts_with("--")).collect();
    let mut files: Vec<PathBuf> = Vec::new();
    if positional.is_empty() {
        collect_rs_files(&PathBuf::from("."), &mut files);
    } else {
        for target in positional {
            let path = PathBuf::from(target.as_str());
            if path.is_dir() {
                collect_rs_files(&path, &mut files);
            } else {
                files.push(path);
            }
        }
    }
    let mut total = 0usize;
    for file in &files {
        match lgwks_deps::scan::scan_path(file) {
            Ok(hits) => {
                for hit in &hits {
                    writeln!(
                        out,
                        "{}:{}: [{}] {}",
                        file.display(),
                        hit.line,
                        hit.rule,
                        hit.snippet
                    )?;
                }
                // Both operands are lengths of an in-memory vector and a
                // single file's findings, so the sum cannot overflow `usize`.
                total = total.saturating_add(hits.len());
            }
            Err(error) => {
                writeln!(err, "scan error: {error}")?;
                return Ok(ExitCode::from(2));
            }
        }
    }
    if total == 0 {
        writeln!(out, "OK  scan clean — {} files, zero findings", files.len())?;
        Ok(ExitCode::SUCCESS)
    } else {
        writeln!(
            err,
            "REFUSED  scan: {total} findings across {} files",
            files.len()
        )?;
        Ok(ExitCode::from(2))
    }
}

/// Reports that the `scan` subcommand needs its feature, which is on by
/// default.
///
/// Only reachable from a build with `--no-default-features`, where `scan` was
/// compiled out deliberately; the message names the reason rather than failing
/// silently.
#[cfg(not(feature = "scan"))]
fn handle_scan(
    _args: &[String],
    _out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    writeln!(
        err,
        "lgwks-deps: scan needs the `scan` feature (default on)"
    )?;
    Ok(ExitCode::from(2))
}

// ── Ladder text ─────────────────────────────────────────────────────────────

/// The admission ladder printed by `tiers` and prefixed to an approval
/// template.
///
/// Read lowest rung first: each step up is an escalation that needs a stated
/// reason, and only the last two rungs (`VENDOR` and `BOUNDARY`) produce a
/// register entry at all.
const LADDER: &str = "\
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
