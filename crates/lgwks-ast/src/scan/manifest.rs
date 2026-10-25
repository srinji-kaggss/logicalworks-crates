//! Detector ALLOW-SILENCE in a package manifest: a `[lints]` table that lowers
//! a lint to `allow` silences it for every item in the package, which is the
//! same act as `#[allow(..)]` written once per item (#699).
//!
//! Cargo reads manifests as TOML. This reader is deliberately one table wide: it
//! reads the section headers `[lints]`, `[lints.<tool>]`, `[workspace.lints]`
//! and `[workspace.lints.<tool>]`, a `name = "level"` declaration under them (or
//! a top-level `lints.<table>` / `lints.workspace` key, which is the same
//! declaration written as a dotted key), and the `workspace = true` line that
//! inherits a workspace's table. Every other shape in those sections — an inline
//! table, an array, a bare expression — is reported as a declaration this reader
//! cannot read, because a gate that passes what it cannot read reports success
//! for the one condition it exists to catch.
//!
//! A package that must lower a lint records it, the way the source gate records
//! an approved architecture widening: a `# scan-approved: <reason>` comment on
//! the declaration's line, or in the comment block directly above it, makes the
//! silence a reviewed exception. An approval with no reason, or one that no
//! longer sits above the declaration it approved, approves nothing.
//!
//! `lgwks_deps` keeps its own fail-closed TOML subset behind its binaries
//! (`bin/shared/toml_subset.rs`); publishing it from the library would widen
//! another crate's supported surface for a reader whose only caller reads one
//! table, so the gap is stated here rather than papered over.

use super::Hit;

/// The lint levels that silence. Cargo accepts `allow`, `warn`, `deny` and
/// `forbid` in a `[lints]` table; `expect` is not one of them, and a manifest
/// that writes it is already refused by cargo, so it is read as no level here.
const SILENCING_LEVELS: [&str; 1] = ["allow"];

/// The rule the manifest findings carry, the same one a source suppression does.
const RULE: &str = "ALLOW-SILENCE";

/// The comment that records a reviewed exception, as the source gate's
/// `// lgwks-architecture-approved: <reason>` does for a widening.
const APPROVAL: &str = "scan-approved:";

/// The shortest reason an approval may carry. A marker with nothing after it is
/// a rubber stamp, and a gate that accepts one accepts anything.
const APPROVAL_MIN_CHARS: usize = 8;

/// What one line of a lint table declares.
enum Declared {
    /// A lint level, as written.
    Level(String),
    /// `workspace = true`: the package inherits its workspace's table, which is
    /// read when that workspace's own manifest is scanned.
    Inherited,
}

/// Scans one `Cargo.toml` for lint levels that silence, in source order.
#[must_use]
pub fn scan_manifest(text: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut table: Option<String> = None;
    let mut approved_above = false;
    for (index, raw) in text.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            // A blank line ends the comment block above a declaration, so a
            // marker separated from it by one approves nothing.
            approved_above = false;
            continue;
        }
        if trimmed.starts_with('#') {
            approved_above |= approvals(raw);
            continue;
        }
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            table = header
                .strip_suffix(']')
                .filter(|name| lint_table(name))
                .map(str::to_owned);
            approved_above = false;
            continue;
        }
        let number = index.saturating_add(1);
        let approved = approved_above || approvals(raw);
        approved_above = false;
        let Some(scope) = scope_of(table.as_deref(), line) else {
            continue;
        };
        match declaration(line) {
            Some(Declared::Level(level)) if SILENCING_LEVELS.contains(&level.as_str()) => {
                if !approved {
                    hits.push(Hit::new(RULE, number, format!("{scope} {line}")));
                }
            }
            Some(Declared::Level(_) | Declared::Inherited) => {}
            None => hits.push(Hit::new(
                RULE,
                number,
                format!("{scope} {line} — a lint declaration this reader cannot read"),
            )),
        }
    }
    hits
}

/// Whether a comment line carries an approval with a reason.
fn approvals(line: &str) -> bool {
    let Some((_, reason)) = line.split_once(APPROVAL) else {
        return false;
    };
    reason.trim().chars().count() >= APPROVAL_MIN_CHARS
}

/// The scope a declaration is reported under: the lint table in hand, or the
/// top level for a `lints` dotted key written outside any table.
///
/// `None` means the line is not a lint declaration at all: outside a lint table
/// every line but a `lints` key belongs to some other table and is skipped.
fn scope_of(table: Option<&str>, line: &str) -> Option<String> {
    if let Some(name) = table {
        return Some(format!("[{name}]"));
    }
    let key = match line.split_once('=') {
        Some((key, _)) => key.trim(),
        None => line.trim(),
    };
    lint_table(key).then(|| format!("[{key}]"))
}

/// Whether a section header names a lint table.
fn lint_table(name: &str) -> bool {
    name == "lints"
        || name.starts_with("lints.")
        || name == "workspace.lints"
        || name.starts_with("workspace.lints.")
}

/// What `line` declares, or `None` when it is not a declaration this reader
/// models.
fn declaration(line: &str) -> Option<Declared> {
    let (key, value) = line.split_once('=')?;
    if key.trim().is_empty() {
        return None;
    }
    let value = value.trim();
    if value == "true" {
        return Some(Declared::Inherited);
    }
    string_value(value).map(Declared::Level)
}

/// The characters of a double-quoted TOML string, or `None` when the value is
/// not one.
///
/// An inline table (`{ dead_code = "allow" }`) and an array are the shapes this
/// deliberately does not read: they are reported, not guessed at.
fn string_value(value: &str) -> Option<String> {
    let rest = value.strip_prefix('"')?;
    let end = rest.find('"')?;
    rest.get(..end).map(str::to_owned)
}

/// `line` without its trailing comment: a `#` outside a string opens one.
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    for (at, character) in line.char_indices() {
        match character {
            '"' => quoted = !quoted,
            '#' if !quoted => {
                return match line.get(..at) {
                    Some(head) => head,
                    None => line,
                };
            }
            _ => {}
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::scan_manifest;

    /// The line and evidence of every finding, in order.
    fn evidence(text: &str) -> Vec<(usize, String)> {
        scan_manifest(text)
            .iter()
            .map(|hit| (hit.line(), hit.snippet().to_owned()))
            .collect()
    }

    #[test]
    fn a_package_allow_is_silence_and_a_forbid_is_not() {
        let text = "[lints.rust]\ndead_code = \"allow\"\nunsafe_code = \"forbid\"\n";
        let found = evidence(text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found.first().map(|found| found.0), Some(2));
        assert!(
            found
                .first()
                .is_some_and(|found| found.1.contains("dead_code = \"allow\"")),
            "{found:?}"
        );
    }

    #[test]
    fn a_workspace_table_and_a_tool_table_are_both_read() {
        let text = "[workspace.lints.clippy]\nunwrap_used = \"allow\"\n[lints]\nworkspace = true\n";
        let found = evidence(text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found
                .first()
                .is_some_and(|found| found.1.starts_with("[workspace.lints.clippy]")),
            "the scope is the table that declared it: {found:?}"
        );
    }

    #[test]
    fn an_inline_table_is_a_declaration_the_reader_reports_rather_than_passes() {
        let text = "[lints]\nrust = { dead_code = \"allow\" }\n";
        let found = evidence(text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found
                .first()
                .is_some_and(|found| found.1.contains("cannot read")),
            "an unreadable declaration is a finding, never a pass: {found:?}"
        );
    }

    #[test]
    fn a_line_outside_a_lint_table_is_not_a_declaration() {
        let text = "[package]\nname = \"x\"\n[dependencies]\nx = { path = \"../x\" }\n";
        assert_eq!(evidence(text), Vec::new());
    }

    #[test]
    fn a_trailing_comment_is_not_part_of_the_level() {
        let text = "[lints.clippy]\nunwrap_used = \"allow\" # the corpus needs it\n";
        let found = evidence(text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found
                .first()
                .is_some_and(|found| found.1.contains("unwrap_used = \"allow\"")),
            "{found:?}"
        );
    }

    #[test]
    fn a_dotted_lint_key_outside_a_table_is_read() {
        let text = "lints.rust.dead_code = \"allow\"\n";
        let found = evidence(text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found
                .first()
                .is_some_and(|found| found.1.contains("dead_code = \"allow\"")),
            "{found:?}"
        );
    }

    #[test]
    fn an_array_of_tables_is_not_a_lint_table() {
        let text = "[[bin]]\nname = \"x\"\nallow = \"allow\"\n";
        assert_eq!(evidence(text), Vec::new());
    }

    #[test]
    fn an_approved_silence_is_a_reviewed_exception_and_not_a_finding() {
        let text = "[lints.rust]\n# scan-approved: INV-STD-UNSAFE-1, the FFI modules\nunsafe_code = \"allow\"\n";
        assert_eq!(evidence(text), Vec::new());
    }

    #[test]
    fn an_approval_with_no_reason_approves_nothing() {
        let text = "[lints.rust]\n# scan-approved:   \nunsafe_code = \"allow\"\n";
        assert_eq!(
            evidence(text),
            vec![(3, "[lints.rust] unsafe_code = \"allow\"".to_owned())]
        );
    }

    #[test]
    fn an_approval_further_up_than_the_declaration_approves_nothing() {
        let text = "[lints.rust]\n# scan-approved: a reason that does not belong to this line\ndead_code = \"warn\"\nunsafe_code = \"allow\"\n";
        assert_eq!(
            evidence(text),
            vec![(4, "[lints.rust] unsafe_code = \"allow\"".to_owned())]
        );
    }
}
