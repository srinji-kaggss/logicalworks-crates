//! The `check` command's reports: the receipt, the human text and the `--json`
//! document, for every way a check can end.

use std::io;
use std::path::Path;
use std::process::ExitCode;

use lgwks_deps::{
    Refusal, Verdict,
    contract::Contract,
    invariants::{
        Audit as InvariantAudit, Register as InvariantRegister, SCOPE as INVARIANT_SCOPE,
    },
};

use crate::{PolicyMode, refuse, report_ok, report_refusals};

/// The receipt-bearing result of one `check` invocation.
pub(crate) struct CheckOutcome<'a> {
    /// Repository root, or `None` when root discovery failed.
    pub(crate) root: Option<&'a Path>,
    /// Whether the register was committed enforcement or a `--contract` override.
    pub(crate) mode: PolicyMode,
    /// The audited verdict, or the flattened error that prevented one.
    pub(crate) audit: Result<&'a Verdict, &'a str>,
}

/// Optional invariant data added to the existing machine-readable check shape.
pub(crate) struct InvariantJson<'a> {
    /// Parsed register, when the optional file was readable.
    pub(crate) register: Option<&'a InvariantRegister>,
    /// Resolved verdicts, when the optional file was readable.
    pub(crate) audit: Option<&'a InvariantAudit>,
    /// Register error, when parsing or workspace scope discovery failed.
    pub(crate) error: Option<&'a str>,
}

/// Parsed invariant register and its resolved audit, carried together.
#[derive(Clone, Copy)]
pub(crate) struct InvariantReport<'a> {
    /// Parsed invariant register.
    pub(crate) register: &'a InvariantRegister,
    /// Resolved invariant audit.
    pub(crate) audit: &'a InvariantAudit,
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
pub(crate) fn finish_verdict(
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
pub(crate) fn print_check_json(
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
pub(crate) fn serde_json_number(count: usize) -> lgwks_std::json::Number {
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
pub(crate) fn schema_number(version: u32) -> lgwks_std::json::Number {
    lgwks_std::json::Number::from(i64::from(version))
}
