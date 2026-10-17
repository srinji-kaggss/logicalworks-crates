//! The per-file ceiling the #526 gates share: a count per file that may fall and
//! never rise, read from one table of `contract/PUBLIC_SURFACE.toml` and judged
//! the same way whatever it counts.
//!
//! A file with no entry is held to zero. The count is exact: a ceiling above the
//! count is stale and is a finding too, so lowering a number lowers its line in
//! the same change and the table never drifts from the tree. A contract that
//! cannot be read is a refusal, never an empty table.

use std::collections::BTreeMap;
use std::path::Path;

use super::toml_subset::{raw, read_tables};
use super::{AuditError, fail, read};

/// The contract file, relative to the repository root.
pub(super) const CONTRACT: &str = "contract/PUBLIC_SURFACE.toml";

/// What one ratchet counts, where its ceilings live and how its findings read.
pub(super) struct Ratchet {
    /// The finding's tag, e.g. `SURFACE`.
    pub(super) label: &'static str,
    /// The table of the contract that holds the ceilings.
    pub(super) table: &'static str,
    /// What a count counts, as a plural noun phrase for the over-ceiling finding.
    pub(super) counted: &'static str,
    /// What a file with a ceiling and a zero count does not hold, for the stale finding.
    pub(super) none: &'static str,
    /// Why a rise is refused.
    pub(super) refused: &'static str,
}

/// File -> its ceiling in the ratchet's table, as the contract writes it.
pub(super) fn ceilings(
    repo: &Path,
    ratchet: &Ratchet,
) -> Result<BTreeMap<String, usize>, AuditError> {
    let path = repo.join(CONTRACT);
    let tables = read_tables(&read(&path)?);
    let mut found = BTreeMap::new();
    for table in tables.iter().filter(|table| table.path == ratchet.table) {
        for entry in &table.entries {
            let number = raw(table, &entry.key).and_then(|text| text.trim().parse::<usize>().ok());
            match number {
                Some(ceiling) => {
                    found.insert(entry.key.clone(), ceiling);
                }
                None => {
                    return fail(
                        AuditError::Manifest {
                            path,
                            missing: "a ceiling that is not a whole number",
                        },
                        "ceilings",
                    );
                }
            }
        }
    }
    Ok(found)
}

/// The finding for a file that holds `count` against a `ceiling`.
fn over(ratchet: &Ratchet, file: &str, count: usize, ceiling: usize) -> String {
    format!(
        "{file} — {}: {count} {}, ceiling {ceiling}; {}",
        ratchet.label, ratchet.counted, ratchet.refused
    )
}

/// The finding for a ceiling that is above the `count` its file holds.
fn stale(ratchet: &Ratchet, file: &str, ceiling: usize, count: usize) -> String {
    format!(
        "{file} — {}: ceiling {ceiling} is above the {count} declared; lower it in {CONTRACT} so \
         the table stays exact",
        ratchet.label
    )
}

/// The findings from holding what the tree declares to what the contract allows.
pub(super) fn judge(
    ratchet: &Ratchet,
    declared: &BTreeMap<String, usize>,
    allowed: &BTreeMap<String, usize>,
) -> Vec<String> {
    let mut findings = Vec::new();
    for (file, count) in declared {
        match allowed.get(file) {
            Some(&ceiling) if *count > ceiling => {
                findings.push(over(ratchet, file, *count, ceiling));
            }
            Some(_) => {}
            None => findings.push(over(ratchet, file, *count, 0)),
        }
    }
    for (file, ceiling) in allowed {
        match declared.get(file) {
            Some(&count) if *ceiling > count => {
                findings.push(stale(ratchet, file, *ceiling, count));
            }
            Some(_) => {}
            None => findings.push(format!(
                "{file} — {}: ceiling {ceiling} for a file that {}; remove the line from \
                 {CONTRACT} so the table stays exact",
                ratchet.label, ratchet.none
            )),
        }
    }
    findings
}
