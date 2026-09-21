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
    CONTRACT_PATH, Refusal, check_dependencies, check_dependencies_against, check_invariants,
    contract::Contract, invariants::Refusal as InvariantRefusal,
    invariants::Register as InvariantRegister, repository_root,
};

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
             [--json]                  emit one JSON object on stdout and
                                       nothing on stderr; the exit code still
                                       carries the verdict
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
                    return Err(CheckArgError::DuplicateOverride { flag: "--contract" });
                }
                let Some(value) = cursor.next() else {
                    return Err(CheckArgError::MissingValue { flag: "--contract" });
                };
                if value.starts_with("--") {
                    return Err(CheckArgError::ValueLooksLikeOption {
                        flag: "--contract",
                        value: value.clone(),
                    });
                }
                contract = Some(PathBuf::from(value));
            }
            flag if flag.starts_with("--") => {
                return Err(CheckArgError::UnknownFlag {
                    flag: flag.to_owned(),
                });
            }
            // A single `-` token is a path, not an option: the only short flag
            // this command defines is `-h`, which is matched above.
            path => {
                if target.is_some() {
                    return Err(CheckArgError::SurplusTarget {
                        value: path.to_owned(),
                    });
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
    match audit_invariant_root(&root) {
        Ok(None) => {
            writeln!(
                out,
                "OK  {} — no invariant register (optional)",
                root.display()
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Ok(Some((register, refusals))) => {
            report_invariant_audit(&root, &register, &refusals, out, err)
        }
        Err(error) => refuse(&error, err),
    }
}

/// Prints an invariant-register verdict. Any refusal is non-zero for this
/// explicit audit command, even while a register is in adoption mode.
fn report_invariant_audit(
    root: &Path,
    register: &InvariantRegister,
    refusals: &[InvariantRefusal],
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    if refusals.is_empty() {
        writeln!(
            out,
            "OK  {} — {} invariants are registered and enforced",
            root.display(),
            register.entries.len()
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    writeln!(
        err,
        "REFUSED  {} — {} invariant violations\n",
        root.display(),
        refusals.len()
    )?;
    for refusal in refusals {
        writeln!(err, "  invariant register: {refusal}")?;
    }
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
fn handle_unknown(other: &str, err: &mut impl io::Write) -> io::Result<ExitCode> {
    writeln!(err, "lgwks-deps: unknown command {other:?}\n")?;
    write!(err, "{USAGE}")?;
    Ok(ExitCode::from(2))
}

/// Routes the first argument to its command, with the remaining arguments.
fn dispatch(
    command: &str,
    args: &[String],
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    match command {
        "check" => handle_check(&args[1..], out, err),
        "invariants" => handle_invariants(&args[1..], out, err),
        "request" => run_request(args.get(1), args.get(2), out, err),
        "init" => run_init(args.get(1).map(PathBuf::from), out, err),
        "tiers" => handle_tiers(out),
        "freshness" => handle_freshness(&args[1..], out, err),
        "vendor" => handle_vendor(&args[1..], out, err),
        "scan" => handle_scan(&args[1..], out, err),
        "-h" | "--help" | "help" => handle_help(out),
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
    let command = args.first().map(String::as_str).unwrap_or("");
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
) -> Result<(Contract, Vec<Refusal>), String> {
    let outcome = match contract_override.as_ref() {
        Some(path) => check_dependencies_against(root, path),
        None => check_dependencies(root),
    };
    outcome.map_err(|error| error.to_string())
}

/// Reads the optional invariant register, flattening its structured error for
/// the human-facing command while retaining the structured library API.
fn audit_invariant_root(
    root: &Path,
) -> Result<Option<(InvariantRegister, Vec<InvariantRefusal>)>, String> {
    check_invariants(root).map_err(|error| error.to_string())
}

/// Prints every refusal and returns the verdict's exit code.
///
/// Exit 0 when `[policy] enforce = false`: the register has stood enforcement
/// down deliberately, so the refusals are reported as adoption guidance and the
/// build still passes. Exit 2 otherwise.
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
            "\nNOTE  [policy] enforce = false, so builds still pass. This is adoption-only."
        )?;
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(2))
    }
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
    let start = path.unwrap_or_else(|| PathBuf::from("."));
    let root = match repository_root(&start) {
        Ok(root) => root,
        Err(error) => {
            return report_check(
                None,
                None,
                &[],
                Some(&error.to_string()),
                json_output,
                out,
                err,
            );
        }
    };
    let dependency = audit_root(&root, &contract_override);
    match audit_invariant_root(&root) {
        Ok(None) => match dependency {
            Ok((register, refusals)) => report_check(
                Some(&root),
                Some(&register),
                &refusals,
                None,
                json_output,
                out,
                err,
            ),
            Err(err_msg) => report_check(None, None, &[], Some(&err_msg), json_output, out, err),
        },
        Ok(Some((invariant_register, invariant_refusals))) => report_check_with_invariants(
            &root,
            dependency,
            &invariant_register,
            &invariant_refusals,
            json_output,
            out,
            err,
        ),
        Err(invariant_error) => report_check_with_invariant_error(
            &root,
            dependency,
            &invariant_error,
            json_output,
            out,
            err,
        ),
    }
}

/// Optional invariant data added to the existing machine-readable check shape.
struct InvariantJson<'a> {
    /// Parsed register, when the optional file was readable.
    register: Option<&'a InvariantRegister>,
    /// Semantic invariant refusals.
    refusals: &'a [InvariantRefusal],
    /// Register error, when parsing or workspace scope discovery failed.
    error: Option<&'a str>,
}

/// Reports one check verdict after both registers have been audited.
fn report_check_with_invariants(
    root: &Path,
    dependency: Result<(Contract, Vec<Refusal>), String>,
    invariant_register: &InvariantRegister,
    invariant_refusals: &[InvariantRefusal],
    json_output: bool,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    match dependency {
        Ok((register, refusals)) => {
            if json_output {
                print_check_json(
                    Some(root),
                    Some(&register),
                    &refusals,
                    None,
                    Some(InvariantJson {
                        register: Some(invariant_register),
                        refusals: invariant_refusals,
                        error: None,
                    }),
                    out,
                )?;
                let dependencies_pass = refusals.is_empty() || !register.enforce;
                let invariants_pass = invariant_refusals.is_empty() || !invariant_register.enforce;
                return Ok(if dependencies_pass && invariants_pass {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(2)
                });
            }
            if refusals.is_empty() && invariant_refusals.is_empty() {
                writeln!(
                    out,
                    "OK  {} — {} semantic approvals and {} invariants are enforced",
                    root.display(),
                    register.entries.len(),
                    invariant_register.entries.len()
                )?;
                return Ok(ExitCode::SUCCESS);
            }
            writeln!(
                err,
                "REFUSED  {} — {} dependency-edge violations, {} invariant violations\n",
                root.display(),
                refusals.len(),
                invariant_refusals.len()
            )?;
            for refusal in &refusals {
                writeln!(err, "  dependency register: {refusal}")?;
            }
            if refusals.is_empty() {
                writeln!(err, "  dependency register: 0 refusals")?;
            }
            for refusal in invariant_refusals {
                writeln!(err, "  invariant register: {refusal}")?;
            }
            if invariant_refusals.is_empty() {
                writeln!(err, "  invariant register: 0 refusals")?;
            }
            writeln!(
                err,
                "\nBoth registers are reviewed contracts; repair each named refusal before delivery."
            )?;
            let dependencies_pass = refusals.is_empty() || !register.enforce;
            let invariants_pass = invariant_refusals.is_empty() || !invariant_register.enforce;
            Ok(if dependencies_pass && invariants_pass {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            })
        }
        Err(dependency_error) => report_check_with_dependency_error(
            root,
            &dependency_error,
            invariant_register,
            invariant_refusals,
            json_output,
            out,
            err,
        ),
    }
}

/// Reports a dependency-register error while preserving invariant findings.
fn report_check_with_dependency_error(
    root: &Path,
    dependency_error: &str,
    invariant_register: &InvariantRegister,
    invariant_refusals: &[InvariantRefusal],
    json_output: bool,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    if json_output {
        print_check_json(
            Some(root),
            None,
            &[],
            Some(dependency_error),
            Some(InvariantJson {
                register: Some(invariant_register),
                refusals: invariant_refusals,
                error: None,
            }),
            out,
        )?;
        return Ok(ExitCode::from(2));
    }
    writeln!(
        err,
        "REFUSED  {} — dependency register error, {} invariant violations\n",
        root.display(),
        invariant_refusals.len()
    )?;
    writeln!(err, "  dependency register: {dependency_error}")?;
    for refusal in invariant_refusals {
        writeln!(err, "  invariant register: {refusal}")?;
    }
    if invariant_refusals.is_empty() {
        writeln!(err, "  invariant register: 0 refusals")?;
    }
    Ok(ExitCode::from(2))
}

/// Reports an invariant-register error while preserving any dependency result.
fn report_check_with_invariant_error(
    root: &Path,
    dependency: Result<(Contract, Vec<Refusal>), String>,
    invariant_error: &str,
    json_output: bool,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    match dependency {
        Ok((register, refusals)) => {
            if json_output {
                print_check_json(
                    Some(root),
                    Some(&register),
                    &refusals,
                    None,
                    Some(InvariantJson {
                        register: None,
                        refusals: &[],
                        error: Some(invariant_error),
                    }),
                    out,
                )?;
                return Ok(ExitCode::from(2));
            }
            writeln!(
                err,
                "REFUSED  {} — {} dependency-edge violations, invariant register error\n",
                root.display(),
                refusals.len()
            )?;
            for refusal in &refusals {
                writeln!(err, "  dependency register: {refusal}")?;
            }
            if refusals.is_empty() {
                writeln!(err, "  dependency register: 0 refusals")?;
            }
            writeln!(err, "  invariant register: {invariant_error}")?;
            Ok(ExitCode::from(2))
        }
        Err(dependency_error) => {
            if json_output {
                print_check_json(
                    Some(root),
                    None,
                    &[],
                    Some(&dependency_error),
                    Some(InvariantJson {
                        register: None,
                        refusals: &[],
                        error: Some(invariant_error),
                    }),
                    out,
                )?;
                return Ok(ExitCode::from(2));
            }
            writeln!(
                err,
                "REFUSED  {} — both registers could not be audited",
                root.display()
            )?;
            writeln!(err, "  dependency register: {dependency_error}")?;
            writeln!(err, "  invariant register: {invariant_error}")?;
            Ok(ExitCode::from(2))
        }
    }
}

/// Renders the verdict in the requested mode and returns the exit code.
///
/// One function rather than a branch at each call site: the human and machine
/// renderings differ in bytes and must not differ in *verdict*, and the only way
/// to guarantee that is for a single place to compute it. Both modes exit 0 for
/// an admitted tree, 0 for refusals under `enforce = false`, and 2 otherwise.
fn report_check(
    root: Option<&Path>,
    register: Option<&Contract>,
    refusals: &[Refusal],
    error: Option<&str>,
    json_output: bool,
    out: &mut impl io::Write,
    err: &mut impl io::Write,
) -> io::Result<ExitCode> {
    if json_output {
        print_check_json(root, register, refusals, error, None, out)?;
        // A gate that could not reach a verdict is a refusal, and exits 2.
        // This arm is first because the code below would otherwise *pass*: with
        // no register, `enforce` defaults to true and `refusals` is empty, so
        // `refusals.is_empty() || !enforced` is satisfied and the gate would
        // report success for a tree it never read. That is the one failure this
        // crate's fail-closed rule exists to prevent, and it was reachable only
        // through `--json`.
        if error.is_some() {
            return Ok(ExitCode::from(2));
        }
        // `enforce = false` is adoption-only: refusals are reported and the
        // build still passes, exactly as in the human path. The two modes must
        // not disagree about what an exit code means.
        let enforced = register.is_none_or(|contract| contract.enforce);
        return Ok(if refusals.is_empty() || !enforced {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(2)
        });
    }

    if let Some(message) = error {
        return refuse(message, err);
    }
    // Unreachable through `run_check`, which refuses on the error path rather
    // than passing `None` here. The arm exists because the type admits it, and
    // the one thing it must not do is report success for a register it never
    // read.
    let Some(contract) = register else {
        return refuse("no register was read", err);
    };
    let named = root.unwrap_or(Path::new("."));
    if refusals.is_empty() {
        report_ok(named, contract.entries.len(), out)
    } else {
        report_refusals(named, contract, refusals, err)
    }
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
    root: Option<&Path>,
    register: Option<&Contract>,
    refusals: &[Refusal],
    error: Option<&str>,
    invariant: Option<InvariantJson<'_>>,
    out: &mut impl io::Write,
) -> io::Result<()> {
    use lgwks_std::json::{Map, Value};

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
        match root {
            Some(path) => Value::String(format!("{}", path.display())),
            None => Value::Null,
        },
    );
    payload.insert(
        "enforce".to_owned(),
        match register {
            Some(contract) => Value::Bool(contract.enforce),
            None => Value::Null,
        },
    );
    let dependency_admitted = error.is_none() && refusals.is_empty();
    let invariant_admitted = invariant.as_ref().is_none_or(|audit| {
        audit.error.is_none()
            && (audit.refusals.is_empty()
                || audit.register.is_none_or(|register| !register.enforce))
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
        Value::Number(serde_json_number(
            register.map_or(0, |contract| contract.entries.len()),
        )),
    );
    payload.insert("refusals".to_owned(), Value::Array(refusal_rows));
    payload.insert(
        "error".to_owned(),
        match error {
            Some(message) => Value::String(message.to_owned()),
            None => Value::Null,
        },
    );
    if let Some(audit) = invariant {
        let mut invariant_rows = Vec::with_capacity(audit.refusals.len());
        for refusal in audit.refusals {
            let mut row = Map::new();
            row.insert("id".to_owned(), Value::String(refusal.id().to_owned()));
            row.insert("detail".to_owned(), Value::String(refusal.to_string()));
            invariant_rows.push(Value::Object(row));
        }
        let mut invariant_payload = Map::new();
        invariant_payload.insert(
            "enforce".to_owned(),
            match audit.register {
                Some(register) => Value::Bool(register.enforce),
                None => Value::Null,
            },
        );
        invariant_payload.insert(
            "registered".to_owned(),
            Value::Number(serde_json_number(
                audit.register.map_or(0, |register| register.entries.len()),
            )),
        );
        invariant_payload.insert("refusals".to_owned(), Value::Array(invariant_rows));
        invariant_payload.insert(
            "error".to_owned(),
            match audit.error {
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
        Err(_) => lgwks_std::json::Number::from(i64::MAX),
    }
}

/// Prints a one-line refusal on stderr and returns the refusal exit code.
///
/// Every refusal path in this binary funnels through here so exit code 2 means
/// exactly one thing regardless of which check produced it.
fn refuse(message: &str, err: &mut impl io::Write) -> io::Result<ExitCode> {
    writeln!(err, "REFUSED  {message}")?;
    Ok(ExitCode::from(2))
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
    let start = path.unwrap_or_else(|| PathBuf::from("."));
    let root = match repository_root(&start) {
        Ok(root) => root,
        Err(error) => return refuse(&error.to_string(), err),
    };
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
    let start = positional
        .first()
        .map(|candidate| PathBuf::from(candidate.as_str()))
        .unwrap_or_else(|| PathBuf::from("."));

    let root = match repository_root(&start) {
        Ok(root) => root,
        Err(error) => return refuse(&error.to_string(), err),
    };

    let lock_path = root.join("Cargo.lock");
    let lock_text = match std::fs::read_to_string(&lock_path) {
        Ok(text) => text,
        Err(error) => {
            return refuse(
                &format!("cannot read {}: {error}", lock_path.display()),
                err,
            );
        }
    };

    let resolved = match lgwks_deps::lock::parse(&lock_text) {
        Ok(parsed) => parsed,
        Err(error) => return refuse(&format!("Cargo.lock: {error}"), err),
    };

    let registry: Vec<&lgwks_deps::lock::Resolved> =
        resolved.iter().filter(|package| !package.local).collect();

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
    /// Latest version crates.io reports, or empty when unknown.
    latest: String,
    /// Upstream repository URL reported by crates.io, or empty.
    repository: String,
    /// Whether `latest` is a real version newer than `resolved`.
    stale: bool,
    /// Why the lookup failed, when it did.
    error: Option<String>,
}

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
        if !seen.insert(&package.name) {
            continue;
        }

        let completed = std::process::Command::new("curl")
            .args([
                "-sf",
                "--max-time",
                "10",
                "-H",
                "User-Agent: lgwks-deps/0.1 (srinji@logicalworks.ca)",
                &format!("https://crates.io/api/v1/crates/{}", package.name),
            ])
            .output();

        match completed {
            Ok(response) if response.status.success() => {
                let body = String::from_utf8_lossy(&response.stdout);
                let (latest, repo) = parse_crate_response(&body);
                let stale = !latest.is_empty() && latest != package.version;
                results.push(FreshnessResult {
                    name: package.name.clone(),
                    resolved: package.version.clone(),
                    latest,
                    repository: repo,
                    stale,
                    error: None,
                });
            }
            Ok(response) => {
                results.push(FreshnessResult {
                    name: package.name.clone(),
                    resolved: package.version.clone(),
                    latest: String::new(),
                    repository: String::new(),
                    stale: false,
                    error: Some(format!("HTTP {}", response.status)),
                });
            }
            Err(error) => {
                results.push(FreshnessResult {
                    name: package.name.clone(),
                    resolved: package.version.clone(),
                    latest: String::new(),
                    repository: String::new(),
                    stale: false,
                    error: Some(error.to_string()),
                });
            }
        }
    }
    results
}

/// INV-GATE-ZERO-DEPS: no JSON parser; extract fields by line scan.
fn parse_crate_response(body: &str) -> (String, String) {
    let newest = extract_json_string(body, "newest_version");
    let repo = extract_json_string(body, "repository");
    (newest, repo)
}

/// Reads the value of `key` out of a JSON object by scanning for its quoted
/// form.
///
/// Both `"key":"value"` and `"key": "value"` spacing are accepted, and a key
/// that is absent yields an empty string rather than an error: freshness is a
/// best-effort advisory, and a missing field must not turn a successful HTTP
/// lookup into a failure. This is deliberately not a JSON parser; see the
/// zero-deps invariant above.
fn extract_json_string(body: &str, key: &str) -> String {
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
    match start {
        Some(value_start) => body[value_start..]
            .find('"')
            // `end_offset` indexes the closing quote inside the slice that
            // begins at `value_start`, so the sum stays within `body`.
            .map(|end_offset| body[value_start..value_start.saturating_add(end_offset)].to_string())
            .unwrap_or_default(),
        None => String::new(),
    }
}

// Each label is a column value; the format string owns the alignment. The final
// column is spelled inside the format string rather than passed as an argument:
// a trailing string literal in a `{}` slot is what `clippy::write_literal`
// flags, and the rendered line is byte-identical either way.
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
                result.name, result.resolved, result.latest, stale_mark, result.repository
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
        row.insert("latest".to_owned(), Value::String(result.latest.clone()));
        row.insert("stale".to_owned(), Value::Bool(result.stale));
        row.insert(
            "repository".to_owned(),
            Value::String(result.repository.clone()),
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
    let start = positional
        .first()
        .map(|candidate| PathBuf::from(candidate.as_str()))
        .unwrap_or_else(|| PathBuf::from("."));
    run_vendor_check(&start, out, err)
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
    let root = match lgwks_deps::repository_root(start) {
        Ok(root) => root,
        Err(error) => return refuse(&error.to_string(), err),
    };
    let tree = match lgwks_deps::vendor::tree_for(&root) {
        Ok(tree) => tree,
        Err(error) => return refuse(&error.to_string(), err),
    };
    let lock_path = root.join("Cargo.lock");
    let lock_text = match std::fs::read_to_string(&lock_path) {
        Ok(text) => text,
        Err(error) => {
            return refuse(
                &format!("cannot read {}: {error}", lock_path.display()),
                err,
            );
        }
    };
    match lgwks_deps::vendor::check_coverage(&lock_text, &tree) {
        Ok(report) if report.missing.is_empty() => {
            writeln!(
                out,
                "OK  {} — {} locked packages covered by {}, {} local skipped",
                root.display(),
                report.covered,
                tree.display(),
                report.skipped_local
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Ok(report) => {
            writeln!(
                err,
                "REFUSED  {} — {} of {} locked packages missing from {}\n",
                root.display(),
                report.missing.len(),
                // Both operands are counts of entries in one lock file, so the
                // sum is bounded by its package count and cannot overflow.
                report.covered.saturating_add(report.missing.len()),
                tree.display()
            )?;
            for missing in &report.missing {
                writeln!(err, "  {} {}", missing.name, missing.version)?;
            }
            writeln!(
                err,
                "\nRe-run the vendor sync for this repo, then re-check."
            )?;
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
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if matches!(
            name,
            "target" | "vendor" | "node_modules" | "third_party" | ".venv" | ".git" | ".jj"
        ) {
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
