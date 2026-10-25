//! Detector ALLOW-SILENCE in a package manifest: a `[lints]` table that lowers
//! a lint to `allow` silences it for every item in the package, which is the
//! same act as `#[allow(..)]` written once per item (#699).
//!
//! Cargo reads manifests as TOML. This reader is deliberately one table wide: it
//! reads the section headers `[lints]`, `[lints.<tool>]`, `[workspace.lints]`
//! and `[workspace.lints.<tool>]`, a `name = "level"` declaration under them, the
//! `name = { level = "level", priority = -1 }` Cargo writes a lint group as, a
//! dotted key that reaches a lint table from the top of the file or from
//! `[workspace]`, and the `workspace = true` line that inherits a workspace's
//! table. A table is named by its parsed key path, so spacing and quoting
//! (`[ lints . "rust" ]`) do not change which table it is, and a `lints.*` key
//! under another table (`[package.metadata.x]`) is not a lint. Every other shape
//! in a lint table — an inline table with no `level`, an array, a string with an
//! escape, a bare expression — is reported as a declaration this reader cannot
//! read, because a gate that passes what it cannot read reports success for the
//! one condition it exists to catch.
//!
//! A package that must lower a lint records it, the way the source gate records
//! an approved architecture widening: a `# scan-approved: <reason>` comment on
//! the declaration's line, or in the comment block directly above it, makes the
//! silence a reviewed exception, and the same marker clears a declaration the
//! reader cannot read. Only a comment approves: the marker inside a string value
//! approves nothing. An approval with no reason, or one that no longer sits above
//! the declaration it approved, approves nothing.
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
    let mut table: Vec<String> = Vec::new();
    let mut approved_above = false;
    for (index, raw) in text.lines().enumerate() {
        if raw.trim().is_empty() {
            // A blank line ends the comment block above a declaration, so a
            // marker separated from it by one approves nothing.
            approved_above = false;
            continue;
        }
        let (code, comment) = split_comment(raw);
        let line = code.trim();
        if line.is_empty() {
            approved_above |= comment.is_some_and(approves);
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            table = header_path(header);
            approved_above = false;
            continue;
        }
        let approved = approved_above || comment.is_some_and(approves);
        approved_above = false;
        let (key, value) = match line.split_once('=') {
            Some((key, value)) => (key, Some(value.trim())),
            None => (line, None),
        };
        let mut path = table.clone();
        path.extend(key_path(key));
        if !is_lint_path(&path) {
            continue;
        }
        // A declaration is reported under the table that holds it, or under its
        // own full key when the table is not a lint table (a dotted key).
        let shown = if is_lint_path(&table) { &table } else { &path };
        let scope = format!("[{}]", shown.join("."));
        let number = index.saturating_add(1);
        match value.and_then(|value| declaration(&path, value)) {
            Some(Declared::Level(level)) if SILENCING_LEVELS.contains(&level.as_str()) => {
                if !approved {
                    hits.push(Hit::new(RULE, number, format!("{scope} {line}")));
                }
            }
            Some(Declared::Level(_) | Declared::Inherited) => {}
            None => {
                if !approved {
                    hits.push(Hit::new(
                        RULE,
                        number,
                        format!("{scope} {line} — a lint declaration this reader cannot read"),
                    ));
                }
            }
        }
    }
    hits
}

/// Whether a comment carries an approval with a reason.
fn approves(comment: &str) -> bool {
    let Some((_, reason)) = comment.split_once(APPROVAL) else {
        return false;
    };
    reason.trim().chars().count() >= APPROVAL_MIN_CHARS
}

/// The key path of the table a header opens, given the text after its `[`. An
/// array of tables (`[[bin]]`) and a header that never closes open no table this
/// reader reads, which an empty first segment stands for: no lint path starts
/// with it.
fn header_path(header: &str) -> Vec<String> {
    match header.strip_suffix(']') {
        Some(name) if !name.starts_with('[') => key_path(name),
        _ => vec![String::new()],
    }
}

/// Whether a key path reaches a lint table: `lints`, or `workspace.lints`, and
/// anything beneath either.
fn is_lint_path(path: &[String]) -> bool {
    let at = |index: usize, name: &str| path.get(index).is_some_and(|segment| segment == name);
    at(0, "lints") || (at(0, "workspace") && at(1, "lints"))
}

/// What the value of the key at `path` declares, or `None` when it is not a
/// declaration this reader models.
fn declaration(path: &[String], value: &str) -> Option<Declared> {
    if value == "true" {
        return path
            .last()
            .is_some_and(|name| name == "workspace")
            .then_some(Declared::Inherited);
    }
    if let Some(body) = value
        .strip_prefix('{')
        .and_then(|inside| inside.strip_suffix('}'))
    {
        return level_in(body).map(Declared::Level);
    }
    string_value(value).map(Declared::Level)
}

/// The `level` an inline table sets (`{ level = "warn", priority = -1 }`). The
/// other entries — `priority`, `check-cfg` — cannot change the level, so they are
/// skipped; a table with no `level` of its own (`{ dead_code = "allow" }`) names
/// lints by key, which this reader does not follow.
fn level_in(body: &str) -> Option<String> {
    entries(body).into_iter().find_map(|entry| {
        let (key, value) = entry.split_once('=')?;
        if unquote(key) != "level" {
            return None;
        }
        string_value(value.trim())
    })
}

/// The entries of an inline table's body, split at the commas outside strings and
/// nested groups.
fn entries(body: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut quoting = Quoting::default();
    let mut depth = 0_usize;
    let mut start = 0;
    for (at, character) in body.char_indices() {
        if quoting.inside(character) {
            continue;
        }
        match character {
            '[' | '{' => depth = depth.saturating_add(1),
            ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                found.push(part(body, start, at));
                start = at.saturating_add(1);
            }
            _ => {}
        }
    }
    found.push(part(body, start, body.len()));
    found
}

/// The characters of a TOML string, or `None` when the value is not one.
///
/// A basic string with an escape (`"allow"`) spells its level in a way this
/// does not decode, and so does a multi-line string or anything after the closing
/// quote; those are reported, not guessed at.
fn string_value(value: &str) -> Option<String> {
    let quote = value
        .chars()
        .next()
        .filter(|&open| open == '"' || open == '\'')?;
    let rest = value.strip_prefix(quote)?;
    let (body, tail) = rest.split_at_checked(rest.find(quote)?)?;
    if !tail.get(quote.len_utf8()..)?.trim().is_empty() {
        return None;
    }
    if quote == '"' && body.contains('\\') {
        return None;
    }
    Some(body.to_owned())
}

/// The segments of a dotted key as written, quotes and spacing folded:
/// `lints . "rust"` is `lints`, `rust`.
fn key_path(key: &str) -> Vec<String> {
    let mut quoting = Quoting::default();
    let mut segments = Vec::new();
    let mut start = 0;
    for (at, character) in key.char_indices() {
        if !quoting.inside(character) && character == '.' {
            segments.push(unquote(part(key, start, at)));
            start = at.saturating_add(1);
        }
    }
    segments.push(unquote(part(key, start, key.len())));
    segments
}

/// A key segment without its spacing and its surrounding quotes.
fn unquote(segment: &str) -> String {
    let trimmed = segment.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = trimmed
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner.to_owned();
        }
    }
    trimmed.to_owned()
}

/// `text` between two byte offsets, or the empty string for a range that is not
/// on character boundaries (an offset taken from a scan of this very text never
/// is).
fn part(text: &str, start: usize, end: usize) -> &str {
    let Some(found) = text.get(start..end) else {
        return "";
    };
    found
}

/// A line split into its code and the comment after it, when it has one. A `#`
/// outside a string opens the comment.
fn split_comment(line: &str) -> (&str, Option<&str>) {
    let mut quoting = Quoting::default();
    for (at, character) in line.char_indices() {
        if !quoting.inside(character) && character == '#' {
            return (
                part(line, 0, at),
                Some(part(line, at.saturating_add(1), line.len())),
            );
        }
    }
    (line, None)
}

/// Whether a scan over one TOML line is inside a string: basic (`"`, where a
/// backslash escapes the next character) or literal (`'`, where nothing does).
#[derive(Default)]
struct Quoting {
    /// The quote that opened the string being read.
    open: Option<char>,
    /// Whether the previous character was a backslash inside a basic string.
    escaped: bool,
}

impl Quoting {
    /// Feed the next character; true while it belongs to a string, quotes
    /// included.
    fn inside(&mut self, character: char) -> bool {
        match self.open {
            Some(quote) => {
                if self.escaped {
                    self.escaped = false;
                } else if character == '\\' && quote == '"' {
                    self.escaped = true;
                } else if character == quote {
                    self.open = None;
                }
                true
            }
            None if character == '"' || character == '\'' => {
                self.open = Some(character);
                true
            }
            None => false,
        }
    }
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

    /// The Cargo book's own spelling of a lint group.
    #[test]
    fn a_lint_group_written_as_an_inline_table_is_read_by_its_level() {
        let warned = "[lints.clippy]\npedantic = { level = \"warn\", priority = -1 }\n";
        assert_eq!(evidence(warned), Vec::new());
        let allowed = "[lints.clippy]\npedantic = { level = \"allow\", priority = -1 }\n";
        assert_eq!(
            evidence(allowed),
            vec![(
                2,
                "[lints.clippy] pedantic = { level = \"allow\", priority = -1 }".to_owned()
            )]
        );
        let approved = "[lints.clippy]\n# scan-approved: the corpus needs every pedantic lint off\npedantic = { level = \"allow\", priority = -1 }\n";
        assert_eq!(evidence(approved), Vec::new());
    }

    #[test]
    fn extra_keys_in_the_inline_table_cannot_change_the_level() {
        let text = "[lints.rust]\nunexpected_cfgs = { level = \"warn\", check-cfg = ['cfg(a)', 'cfg(b)'] }\nunused = { check-cfg = [\"a\", \"b\"], level = \"allow\" }\n";
        let found = evidence(text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found.first().map(|found| found.0), Some(3));
    }

    #[test]
    fn a_header_is_a_key_path_whatever_its_spacing_and_quotes() {
        for header in [
            "[lints.rust]",
            "[ lints.rust ]",
            "[lints . rust]",
            "[lints.\"rust\"]",
            "[\"lints\".'rust']",
        ] {
            let text = format!("{header}\ndead_code = \"allow\"\n");
            assert_eq!(
                evidence(&text),
                vec![(2, "[lints.rust] dead_code = \"allow\"".to_owned())],
                "{header}"
            );
        }
    }

    #[test]
    fn a_lints_key_is_a_lint_only_where_its_table_makes_it_one() {
        let elsewhere = "[package.metadata.x]\nlints.rust.dead_code = \"allow\"\n";
        assert_eq!(evidence(elsewhere), Vec::new());
        let workspace = "[workspace]\nlints.rust.dead_code = \"allow\"\n";
        assert_eq!(
            evidence(workspace),
            vec![(
                2,
                "[workspace.lints.rust.dead_code] lints.rust.dead_code = \"allow\"".to_owned()
            )]
        );
    }

    #[test]
    fn a_marker_inside_a_value_approves_nothing() {
        let text = "[lints.rust]\ndead_code = { level = \"allow\", note = \"scan-approved: this is a string, not a comment\" }\n";
        assert_eq!(evidence(text).len(), 1, "{:?}", evidence(text));
    }

    #[test]
    fn an_approval_clears_a_declaration_the_reader_cannot_read() {
        let text = "[lints]\n# scan-approved: reviewed, the corpus names lints by key\nrust = { dead_code = \"allow\" }\n";
        assert_eq!(evidence(text), Vec::new());
    }

    #[test]
    fn a_level_with_an_escape_is_unreadable_and_a_literal_string_is_read() {
        let escaped = "[lints.rust]\ndead_code = \"al\\u006cow\"\n";
        let found = evidence(escaped);
        assert!(
            found
                .first()
                .is_some_and(|found| found.1.contains("cannot read")),
            "{found:?}"
        );
        let literal = "[lints.rust]\ndead_code = 'allow'\n";
        assert_eq!(evidence(literal).len(), 1, "{:?}", evidence(literal));
        assert!(
            evidence(literal)
                .iter()
                .all(|found| !found.1.contains("cannot read")),
            "{:?}",
            evidence(literal)
        );
    }

    #[test]
    fn a_hash_inside_a_string_is_not_a_comment() {
        let text =
            "[lints.rust]\ndead_code = \"warn\" # \"allow\"\nunused = \"allow # not a comment\"\n";
        assert_eq!(evidence(text), Vec::new());
    }
}
