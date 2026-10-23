//! `check-docs` holds the documentation set to its two standards, mechanically:
//! every architecture decision record against ISO/IEC/IEEE 42010, and every
//! product requirement against ISO/IEC/IEEE 29148 (#732).
//!
//! A governance document that is missing the field a reader, a reviewer or an
//! auditor needs is a document nobody can act on, and prose review does not
//! catch it: two files can both look complete while one names no stakeholders
//! and the other states no fit criterion. So the fields `docs/adr/README.md` and
//! `docs/prd/README.md` require are read here, and a missing one is a refusal
//! naming the file and the field.
//!
//! The clauses this encodes, kept deliberately narrow:
//!
//! * 42010 — an architecture description identifies its stakeholders and their
//!   concerns, the viewpoint its views are expressed in, the elements and
//!   relations it describes, its rationale, and how it corresponds to the other
//!   records. Each of those is a required part of every record.
//! * 29148 — a requirement statement is singular, unambiguous, verifiable and
//!   traceable, and carries its identifier, rationale, source, priority, fit
//!   criterion, verification method and trace. Each of those is a required
//!   attribute of every requirement.
//!
//! Usage: `check-docs [ROOT]` (default: the working directory). Exit 2 on any
//! finding.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The directory holding the architecture decision records.
const ADR_DIR: &str = "docs/adr";

/// The directory holding the product requirement documents.
const PRD_DIR: &str = "docs/prd";

/// The sections every 42010 record must hold, spelled as the heading text after
/// `## `.
const ADR_SECTIONS: [&str; 6] = [
    "Context",
    "Decision",
    "Alternatives rejected",
    "Consequences",
    "Correspondence",
    "Verification",
];

/// The header fields every 42010 record must state before its first section.
const ADR_HEADER_FIELDS: [&str; 5] = [
    "Stakeholders:",
    "Concerns:",
    "Requirements:",
    "Viewpoint:",
    "Elements and relations:",
];

/// The statuses a record may hold. A record is superseded, never rewritten.
const ADR_STATUSES: [&str; 5] = [
    "proposed",
    "accepted",
    "superseded",
    "rejected",
    "deprecated",
];

/// The sections every product requirement document must hold.
const PRD_SECTIONS: [&str; 4] = [
    "Stakeholders and operational concept",
    "Requirements",
    "Assumptions and constraints",
    "Exclusions",
];

/// The attributes every 29148 requirement block must carry, spelled
/// `**Attribute.**`.
const PRD_ATTRIBUTES: [&str; 7] = [
    "Statement",
    "Rationale",
    "Source",
    "Priority",
    "Fit criterion",
    "Verified by",
    "Traces to",
];

/// The priority vocabulary, lowest bar first.
const PRD_PRIORITIES: [&str; 3] = ["must", "should", "could"];

/// Why the check could not run.
#[derive(Debug)]
enum CheckError {
    /// A directory or file could not be read.
    Io {
        /// What was being read.
        path: PathBuf,
        /// What the operating system said.
        cause: io::Error,
    },
    /// A verdict could not be written to a handle.
    Write {
        /// What the operating system said.
        cause: io::Error,
    },
}

impl fmt::Display for CheckError {
    /// Names the path.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Io {
                ref path,
                ref cause,
            } => write!(formatter, "{}: {cause}", path.display()),
            Self::Write { ref cause } => write!(formatter, "cannot write the verdict: {cause}"),
        }
    }
}

impl std::error::Error for CheckError {
    /// The operating system's error.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Io { ref cause, .. } | Self::Write { ref cause } => Some(cause),
        }
    }
}

/// One record: its path relative to the root, and its whole text.
struct Record {
    /// The path relative to the root.
    name: String,
    /// The whole file.
    text: String,
}

/// The documents under `directory`, sorted by name.
fn records(root: &Path, directory: &str) -> Result<Vec<Record>, CheckError> {
    let path = root.join(directory);
    let entries = fs::read_dir(&path).map_err(|cause| CheckError::Io {
        path: path.clone(),
        cause,
    })?;
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|cause| CheckError::Io {
            path: path.clone(),
            cause,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".md") && name != "README.md" {
            names.push(name);
        }
    }
    names.sort();
    let mut found = Vec::new();
    for name in names {
        let file = path.join(&name);
        let text = fs::read_to_string(&file).map_err(|cause| CheckError::Io {
            path: file.clone(),
            cause,
        })?;
        found.push(Record {
            name: format!("{directory}/{name}"),
            text,
        });
    }
    Ok(found)
}

/// The text of a `## ` heading.
fn section_heading(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("## ")?;
    Some(rest.trim())
}

/// Whether `haystack` holds `needle`, ignoring case.
fn holds(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// Whether `line` holds a date spelled `YYYY-MM-DD`.
fn holds_date(line: &str) -> bool {
    let bytes = line.as_bytes();
    for start in 0..bytes.len() {
        let Some(window) = bytes.get(start..start.saturating_add(10)) else {
            continue;
        };
        let shaped = window.iter().enumerate().all(|(offset, byte)| {
            if offset == 4 || offset == 7 {
                *byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        });
        if shaped {
            return true;
        }
    }
    false
}

/// The findings of a `Status:` line, which every record must carry.
fn status_line(text: &str) -> Vec<String> {
    let Some(line) = text.lines().find(|line| line.starts_with("Status:")) else {
        return vec![
            "no `Status:` line: a record states its status and the date it was decided".to_owned(),
        ];
    };
    let mut findings = Vec::new();
    let lowered = line.to_lowercase();
    if !ADR_STATUSES.iter().any(|status| lowered.contains(status)) {
        findings.push(format!(
            "`Status:` names no defined status ({}): {line}",
            ADR_STATUSES.join(", ")
        ));
    }
    if !holds_date(line) {
        findings.push(format!(
            "`Status:` names no `YYYY-MM-DD` date, so the decision has no date: {line}"
        ));
    }
    findings
}

/// The `NNNN` a record's file name starts with, or `None` when it is not
/// numbered: numbering is how two records are cited apart.
fn numbered(name: &str) -> Option<u32> {
    let file = name.rsplit('/').next()?;
    let digits = file.get(..4)?;
    if !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u32>().ok()
}

/// Whether the record's first line is `# ADR NNNN: title` / `# PRD NNNN: title`.
fn titled(text: &str, kind: &str, number: u32) -> bool {
    let Some(first) = text.lines().next() else {
        return false;
    };
    let expected = format!("# {kind} {number:04}: ");
    first.starts_with(&expected) && first.len() > expected.len()
}

/// The heading findings for one numbered record.
fn heading(number: Option<u32>, text: &str, kind: &str, name: &str) -> Vec<String> {
    match number {
        Some(value) => {
            if titled(text, kind, value) {
                Vec::new()
            } else {
                vec![format!(
                    "the first line must read `# {kind} {value:04}: <title>`"
                )]
            }
        }
        None => vec![format!("{name} must start with a four-digit number")],
    }
}

/// Checks one 42010 record.
fn check_adr(record: &Record) -> Vec<String> {
    let mut findings = heading(numbered(&record.name), &record.text, "ADR", &record.name);
    findings.extend(status_line(&record.text));
    for field in ADR_HEADER_FIELDS {
        match record.text.lines().find(|line| line.starts_with(field)) {
            Some(line) if line.trim().len() > field.len() => {}
            Some(_) => findings.push(format!("`{field}` names nothing")),
            None => findings.push(format!(
                "no `{field}` line: ISO/IEC/IEEE 42010 requires the record to identify it"
            )),
        }
    }
    let present: Vec<&str> = record.text.lines().filter_map(section_heading).collect();
    for section in ADR_SECTIONS {
        if !present.contains(&section) {
            findings.push(format!("no `## {section}` section"));
        }
    }
    let traces = record
        .text
        .lines()
        .any(|line| line.starts_with("Requirements:") && line.contains('R'));
    if !traces {
        findings.push(
            "`Requirements:` names no requirement id, so the decision traces to nothing".to_owned(),
        );
    }
    findings
}

/// One requirement block of a product requirement document.
struct Block<'text> {
    /// The heading's own text.
    heading: &'text str,
    /// The block's lines, heading included.
    lines: Vec<&'text str>,
}

/// Splits a PRD into `### ` blocks, each ending at the next heading.
fn blocks(text: &str) -> Vec<Block<'_>> {
    let mut found = Vec::new();
    let mut current: Option<Block<'_>> = None;
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("### ") {
            if let Some(finished) = current.take() {
                found.push(finished);
            }
            current = Some(Block {
                heading: heading.trim(),
                lines: vec![line],
            });
            continue;
        }
        if line.starts_with("## ") {
            if let Some(finished) = current.take() {
                found.push(finished);
            }
            continue;
        }
        if let Some(ref mut open) = current {
            open.lines.push(line);
        }
    }
    if let Some(finished) = current {
        found.push(finished);
    }
    found
}

/// The value of a block's `**Attribute.** …` line.
fn attribute(block: &Block<'_>, name: &str) -> Option<String> {
    let marker = format!("**{name}.**");
    for line in &block.lines {
        if let Some(rest) = line.trim().strip_prefix(&marker) {
            return Some(rest.trim().to_owned());
        }
    }
    None
}

/// The identifier a requirement heading starts with.
fn requirement_id(heading: &str) -> String {
    match heading.split_whitespace().next() {
        Some(id) => id.to_owned(),
        None => String::new(),
    }
}

/// Checks one 29148 product requirement document.
fn check_prd(record: &Record) -> Vec<String> {
    let mut findings = heading(numbered(&record.name), &record.text, "PRD", &record.name);
    findings.extend(status_line(&record.text));
    let present: Vec<&str> = record.text.lines().filter_map(section_heading).collect();
    for section in PRD_SECTIONS {
        if !present.contains(&section) {
            findings.push(format!("no `## {section}` section"));
        }
    }
    let blocks = blocks(&record.text);
    if blocks.is_empty() {
        findings
            .push("no `### ` requirement block: a PRD with no requirement is not one".to_owned());
        return findings;
    }
    let mut seen: Vec<String> = Vec::new();
    for block in &blocks {
        let id = requirement_id(block.heading);
        if !id.starts_with("PR-") || !id.contains('.') {
            findings.push(format!(
                "`### {}` is not `### PR-<document>.<n> <title>`: a requirement has one identifier",
                block.heading
            ));
        }
        if seen.contains(&id) {
            findings.push(format!("{id} is used twice: identifiers are unique"));
        }
        seen.push(id.clone());
        for attribute_name in PRD_ATTRIBUTES {
            match attribute(block, attribute_name) {
                Some(value) if !value.is_empty() => {
                    if attribute_name == "Statement" && !holds(&value, "must") {
                        findings.push(format!(
                            "{id}: the statement is not normative: a requirement states what the \
                             system MUST do"
                        ));
                    }
                    if attribute_name == "Priority"
                        && !PRD_PRIORITIES.iter().any(|level| value.starts_with(level))
                    {
                        findings.push(format!(
                            "{id}: priority `{value}` is not one of {}",
                            PRD_PRIORITIES.join(", ")
                        ));
                    }
                }
                _ => findings.push(format!(
                    "{id}: no `**{attribute_name}.**` value: ISO/IEC/IEEE 29148 requires it"
                )),
            }
        }
    }
    findings
}

/// Writes one line to a handle, typing the write failure.
fn say(handle: &mut impl Write, arguments: fmt::Arguments<'_>) -> Result<(), CheckError> {
    writeln!(handle, "{arguments}").map_err(|cause| CheckError::Write { cause })
}

/// Runs the check over `root`, writing the verdict to the two handles.
fn execute(
    root: &Path,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<ExitCode, CheckError> {
    let mut findings: Vec<String> = Vec::new();
    let mut counted = 0_usize;
    for record in records(root, ADR_DIR)? {
        counted = counted.saturating_add(1);
        for finding in check_adr(&record) {
            findings.push(format!("{}: {finding}", record.name));
        }
    }
    let mut requirements = 0_usize;
    for record in records(root, PRD_DIR)? {
        counted = counted.saturating_add(1);
        requirements = requirements.saturating_add(blocks(&record.text).len());
        for finding in check_prd(&record) {
            findings.push(format!("{}: {finding}", record.name));
        }
    }
    if counted == 0 {
        say(
            err,
            format_args!(
                "REFUSED  {ADR_DIR} and {PRD_DIR} hold no record: the documentation set is the \
                 governance surface, not an ornament"
            ),
        )?;
        return Ok(ExitCode::from(2));
    }
    if findings.is_empty() {
        say(
            out,
            format_args!(
                "OK  {counted} records — every ADR holds its 42010 fields, every one of the \
                 {requirements} PRD requirements holds its 29148 attributes"
            ),
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    say(
        err,
        format_args!(
            "REFUSED  {} documentation finding(s) across {counted} records\n",
            findings.len()
        ),
    )?;
    for finding in &findings {
        say(err, format_args!("  {finding}"))?;
    }
    say(
        err,
        format_args!(
            "\nThe field list is docs/adr/README.md (ISO/IEC/IEEE 42010) and \
             docs/prd/README.md (ISO/IEC/IEEE 29148). A record missing a field is not a record."
        ),
    )?;
    Ok(ExitCode::from(2))
}

/// The directory named on the command line, else the working directory.
fn root_argument() -> PathBuf {
    match std::env::args_os().nth(1) {
        Some(argument) => PathBuf::from(argument),
        None => PathBuf::from("."),
    }
}

/// Runs the check and reports its exit code.
fn main() -> ExitCode {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let stderr = io::stderr();
    let mut err = stderr.lock();
    match execute(&root_argument(), &mut out, &mut err) {
        Ok(code) => code,
        Err(error) => {
            if say(&mut err, format_args!("check-docs: {error}")).is_err() {
                return ExitCode::from(2);
            }
            ExitCode::from(2)
        }
    }
}

/// The reader's own tests.
#[cfg(test)]
mod tests {
    use super::{
        Record, blocks, check_adr, check_prd, holds_date, numbered, requirement_id,
        section_heading, titled,
    };

    /// The header of an accepted 42010 record.
    const ADR_HEAD: &str = "\
# ADR 0001: an example decision

Status: **accepted**, 2026-10-22. Issue: #732.

Stakeholders: the Director, the maintainer, a consumer
Concerns: whether the capability is reachable
Requirements: R1, R13
Viewpoint: the capability viewpoint, one view per crate
Elements and relations: crates, modules and cargo features
";

    /// The sections of an accepted 42010 record.
    const ADR_BODY: &str = "\
## Context

The context.

## Decision

The decision.

## Alternatives rejected

The alternatives.

## Consequences

The consequences.

## Correspondence

Supersedes nothing; relates to ADR 0002.

## Verification

The lane.
";

    /// One accepted requirement block, reused by the judgements below.
    const PRD_BLOCK: &str = "\
### PR-0001.1 The catalog answers a lookup

**Statement.** The system MUST print every capability it owns with its owner path.

**Rationale.** A capability nobody can find is rebuilt by hand.

**Source.** Director, 2026-10-22.

**Priority.** must

**Fit criterion.** The command exits zero and its output names 30 capabilities.

**Verified by.** `catalog --check`.

**Traces to.** R13.
";

    /// A complete 42010 record, which the checks must accept.
    fn accepted_adr() -> Record {
        Record {
            name: "docs/adr/0001-example.md".to_owned(),
            text: format!("{ADR_HEAD}\n{ADR_BODY}"),
        }
    }

    /// A complete 29148 requirement document, which the checks must accept.
    fn accepted_prd() -> Record {
        Record {
            name: "docs/prd/0001-example.md".to_owned(),
            text: format!(
                "# PRD 0001: an example requirement set\n\nStatus: **accepted**, 2026-10-22.\n\n\
                 ## Stakeholders and operational concept\n\nThe operator runs the command.\n\n\
                 ## Requirements\n\n{PRD_BLOCK}\n\
                 ## Assumptions and constraints\n\nNone.\n\n\
                 ## Exclusions\n\nNone.\n"
            ),
        }
    }

    /// The accepted records pass, so a refusal below is the field under test.
    #[test]
    fn a_complete_record_passes() {
        assert_eq!(check_adr(&accepted_adr()), Vec::<String>::new());
        assert_eq!(check_prd(&accepted_prd()), Vec::<String>::new());
    }

    /// A record with no stakeholders is refused: 42010 requires the concern to
    /// be attributable to someone.
    #[test]
    fn an_adr_without_stakeholders_is_refused() {
        let mut record = accepted_adr();
        record.text = record
            .text
            .lines()
            .filter(|line| !line.starts_with("Stakeholders:"))
            .collect::<Vec<_>>()
            .join("\n");
        let findings = check_adr(&record);
        assert!(
            findings.iter().any(|line| line.contains("Stakeholders:")),
            "the missing field is named: {findings:?}"
        );
    }

    /// A record with no rationale section is refused: 42010 requires it.
    #[test]
    fn an_adr_without_alternatives_is_refused() {
        let mut record = accepted_adr();
        record.text = ADR_HEAD.to_owned();
        let findings = check_adr(&record);
        assert!(
            findings
                .iter()
                .any(|line| line.contains("## Alternatives rejected")),
            "the missing section is named: {findings:?}"
        );
    }

    /// A requirement with no fit criterion is refused: 29148 requires it to be
    /// verifiable.
    #[test]
    fn a_requirement_without_a_fit_criterion_is_refused() {
        let mut record = accepted_prd();
        record.text = record
            .text
            .lines()
            .filter(|line| !line.starts_with("**Fit criterion.**"))
            .collect::<Vec<_>>()
            .join("\n");
        let findings = check_prd(&record);
        assert!(
            findings.iter().any(|line| line.contains("Fit criterion")),
            "the missing attribute is named: {findings:?}"
        );
    }

    /// A statement that is not normative is refused, and so is a priority
    /// outside the vocabulary.
    #[test]
    fn a_statement_without_must_and_an_unknown_priority_are_refused() {
        let mut record = accepted_prd();
        record.text = record
            .text
            .replace("MUST print", "may print")
            .replace("**Priority.** must", "**Priority.** eventually");
        let findings = check_prd(&record);
        assert!(
            findings.iter().any(|line| line.contains("not normative")),
            "the statement is judged: {findings:?}"
        );
        assert!(
            findings.iter().any(|line| line.contains("priority")),
            "the priority is judged: {findings:?}"
        );
    }

    /// Two requirements with one identifier are refused: an identifier names one
    /// requirement.
    #[test]
    fn a_repeated_requirement_identifier_is_refused() {
        let mut record = accepted_prd();
        record.text.push('\n');
        record.text.push_str(PRD_BLOCK);
        let findings = check_prd(&record);
        assert!(
            findings.iter().any(|line| line.contains("used twice")),
            "a repeated identifier is judged: {findings:?}"
        );
    }

    /// A status with no date, a file with no number and a heading of the wrong
    /// level are each read exactly.
    #[test]
    fn a_status_a_number_and_a_heading_are_read_exactly() {
        assert!(holds_date("Status: accepted, 2026-10-22."));
        assert!(!holds_date("Status: accepted"));
        assert_eq!(numbered("docs/adr/0007-x.md"), Some(7));
        assert_eq!(numbered("docs/adr/x.md"), None);
        assert!(titled("# ADR 0007: x", "ADR", 7));
        assert!(!titled("# ADR 7: x", "ADR", 7));
        assert_eq!(section_heading("## Decision"), Some("Decision"));
        assert_eq!(section_heading("### Decision"), None);
    }

    /// The block reader cuts at the next section, so an attribute cannot be
    /// borrowed from the requirement after it.
    #[test]
    fn a_block_ends_at_the_next_section() {
        let text = "## Requirements\n\n### PR-1.1 One\n**Statement.** MUST\n\n## Exclusions\n";
        let found = blocks(text);
        assert_eq!(found.len(), 1, "one block");
        assert_eq!(found.first().map(|block| block.heading), Some("PR-1.1 One"));
        assert_eq!(requirement_id("PR-1.1 One"), "PR-1.1");
    }
}
