//! `check --json`: the stable receipt an agent reads instead of the text.

use std::io;

use lgwks_deps::{Refusal, Verdict, contract::Contract, invariants::SCOPE as INVARIANT_SCOPE};

use super::check::{CheckOutcome, InvariantJson};

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
    let refusals: &[Refusal] = match verdict {
        Some(verdict) => verdict.refusals(),
        None => &[],
    };
    // An unread register approves nothing: zero entries, not an absent count.
    let approvals = match register {
        Some(register) => register.entry_count(),
        None => 0,
    };
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
        Value::Number(serde_json_number(approvals)),
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
        Value::Number(serde_json_number(approvals)),
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
        // An unread register or audit counts zero entries, never a guess.
        let (registered, resolved, attested) = (
            match json.register {
                Some(register) => register.entry_count(),
                None => 0,
            },
            match json.audit {
                Some(audit) => audit.resolved(),
                None => 0,
            },
            match json.audit {
                Some(audit) => audit.attested(),
                None => 0,
            },
        );
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
            Value::Number(serde_json_number(registered)),
        );
        // Counts, not a verdict. There is no `enforced` key and there must
        // never be one: this command reaches these numbers by reading files and
        // manifests, so it has no standing to say an invariant holds. The
        // `scope` string is emitted beside them for the same reason the human
        // rendering prints it.
        invariant_payload.insert(
            "resolved".to_owned(),
            Value::Number(serde_json_number(resolved)),
        );
        invariant_payload.insert(
            "attested".to_owned(),
            Value::Number(serde_json_number(attested)),
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
