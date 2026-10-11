//! `check`: audit a repository against its register and report the verdict.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lgwks_deps::{
    Refusal, Verdict, check_invariants, check_verdict,
    contract::Contract,
    invariants::{
        Audit as InvariantAudit, Register as InvariantRegister, SCOPE as INVARIANT_SCOPE,
    },
    metadata::Collected,
    repository_root,
};

use super::receipt::print_check_json;
use crate::{PolicyMode, refuse, subject_path};

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

/// The receipt-bearing result of one `check` invocation.
pub(crate) struct CheckOutcome<'a> {
    /// Repository root, or `None` when root discovery failed.
    pub(super) root: Option<&'a Path>,
    /// Whether the register was committed enforcement or a `--contract` override.
    pub(super) mode: PolicyMode,
    /// The audited verdict, or the flattened error that prevented one.
    pub(super) audit: Result<&'a Verdict, &'a str>,
}

/// Optional invariant data added to the existing machine-readable check shape.
pub(crate) struct InvariantJson<'a> {
    /// Parsed register, when the optional file was readable.
    pub(super) register: Option<&'a InvariantRegister>,
    /// Resolved verdicts, when the optional file was readable.
    pub(super) audit: Option<&'a InvariantAudit>,
    /// Register error, when parsing or workspace scope discovery failed.
    pub(super) error: Option<&'a str>,
}

/// Parsed invariant register and its resolved audit, carried together.
#[derive(Clone, Copy)]
pub(crate) struct InvariantReport<'a> {
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
pub(crate) fn report_audited_check(
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
pub(crate) fn report_check_with_invariants(
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
pub(crate) fn report_check_with_dependency_error(
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
pub(crate) fn report_check_with_invariant_error(
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
pub(crate) fn write_invariant_register_error(
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
pub(crate) fn write_both_registers_unaudited(
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
pub(crate) fn write_receipt(
    outcome: &CheckOutcome<'_>,
    writer: &mut impl io::Write,
) -> io::Result<()> {
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
pub(crate) fn write_dependency_refusals(
    refusals: &[Refusal],
    err: &mut impl io::Write,
) -> io::Result<()> {
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
pub(crate) fn write_invariant_refusals(
    audit: &InvariantAudit,
    err: &mut impl io::Write,
) -> io::Result<()> {
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
pub(crate) fn write_register_detail(
    err: &mut impl io::Write,
    register: &str,
    detail: &str,
) -> io::Result<()> {
    writeln!(err, "  {register} register: {detail}")
}

/// Renders the verdict in the requested mode and returns the exit code.
///
/// One function rather than a branch at each call site: the human and machine
/// renderings differ in bytes and must not differ in *verdict*, and the only way
/// to guarantee that is for a single place to compute it. Both modes exit 0 for
/// an admitted tree and 2 for any refusal, including a stand-down of refusals.
pub(crate) fn report_check(
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
        let admitted = match outcome.audit {
            Ok(verdict) => verdict.refusals().is_empty(),
            Err(_unread) => false,
        };
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
pub(crate) fn finish_verdict(
    outcome: &CheckOutcome<'_>,
    writer: &mut impl io::Write,
    code: ExitCode,
) -> io::Result<ExitCode> {
    write_receipt(outcome, writer)?;
    writeln!(writer, "SCOPE  {INVARIANT_SCOPE}")?;
    Ok(code)
}
