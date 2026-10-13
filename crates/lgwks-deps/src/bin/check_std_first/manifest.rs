//! Reading the two kinds of TOML the audit judges: crate manifests
//! (`Cargo.toml`) and the approval register (`contract/APPROVED.toml`).
//!
//! This is not a general TOML parser. It reads tables, array-of-tables headers,
//! `key = value` lines, strings (basic, literal, and their multi-line forms),
//! arrays and inline tables as text, which is every construct those files use.

use std::collections::{BTreeMap, BTreeSet};

/// A value as the audit needs it.
pub(super) enum Value {
    /// A string.
    Text(String),
    /// An array's string elements.
    List(Vec<String>),
    /// Anything else (a number, a boolean, an inline table), as written.
    Raw(String),
}

/// One `key = value` line.
pub(super) struct Entry {
    /// The key as written, with surrounding quotes removed.
    pub(super) key: String,
    /// The value.
    pub(super) value: Value,
}

/// One table or array-of-tables element.
pub(super) struct Table {
    /// The header's path, e.g. `dependencies` or `target.'cfg(unix)'.dependencies`.
    pub(super) path: String,
    /// Whether the header was `[[path]]`.
    pub(super) array: bool,
    /// The entries under the header.
    pub(super) entries: Vec<Entry>,
}

/// One approval of the register.
pub(super) struct Record {
    /// The owning surface.
    pub(super) owner: Option<String>,
    /// The capability the edge provides.
    pub(super) capability: Option<String>,
    /// The admission tier.
    pub(super) tier: Option<String>,
    /// The approver's reason.
    pub(super) reason: Option<String>,
    /// The crates admitted to reach the edge.
    pub(super) consumers: BTreeSet<String>,
}

/// The lexical state of the scalar being scanned.
enum Mode {
    /// Outside any string.
    Normal,
    /// Inside `"…"`.
    Basic,
    /// Inside `'…'`.
    Literal,
    /// Inside `"""…"""`.
    MultiBasic,
    /// Inside `'''…'''`.
    MultiLiteral,
}

/// Whether `chars` at `at` starts with `needle`.
fn starts_at(chars: &[char], at: usize, needle: &str) -> bool {
    needle
        .chars()
        .enumerate()
        .all(|(offset, expected)| chars.get(at.saturating_add(offset)) == Some(&expected))
}

/// The value text of the line starting at `lines[*cursor]` after `=`, with
/// comments stripped, continued over following lines while a string, array or
/// inline table is open. Advances `cursor` to the last line consumed.
fn collect_value(lines: &[&str], cursor: &mut usize, first: &str) -> String {
    let mut raw = String::new();
    let mut mode = Mode::Normal;
    let mut depth = 0_i64;
    let mut current = first.to_owned();
    loop {
        let chars: Vec<char> = current.chars().collect();
        let mut index = 0_usize;
        while index < chars.len() {
            let ch = chars[index];
            match mode {
                Mode::Normal => {
                    if starts_at(&chars, index, "\"\"\"") {
                        mode = Mode::MultiBasic;
                        raw.push_str("\"\"\"");
                        index = index.saturating_add(3);
                        continue;
                    }
                    if starts_at(&chars, index, "'''") {
                        mode = Mode::MultiLiteral;
                        raw.push_str("'''");
                        index = index.saturating_add(3);
                        continue;
                    }
                    match ch {
                        '#' => break,
                        '"' => mode = Mode::Basic,
                        '\'' => mode = Mode::Literal,
                        '[' | '{' => depth = depth.saturating_add(1),
                        ']' | '}' => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                    raw.push(ch);
                }
                Mode::Basic => {
                    raw.push(ch);
                    if ch == '\\' {
                        index = index.saturating_add(1);
                        if let Some(next) = chars.get(index) {
                            raw.push(*next);
                        }
                    } else if ch == '"' {
                        mode = Mode::Normal;
                    }
                }
                Mode::Literal => {
                    raw.push(ch);
                    if ch == '\'' {
                        mode = Mode::Normal;
                    }
                }
                Mode::MultiBasic => {
                    if ch == '\\' {
                        raw.push(ch);
                        index = index.saturating_add(1);
                        if let Some(next) = chars.get(index) {
                            raw.push(*next);
                        }
                    } else if starts_at(&chars, index, "\"\"\"") {
                        raw.push_str("\"\"\"");
                        index = index.saturating_add(3);
                        mode = Mode::Normal;
                        continue;
                    } else {
                        raw.push(ch);
                    }
                }
                Mode::MultiLiteral => {
                    if starts_at(&chars, index, "'''") {
                        raw.push_str("'''");
                        index = index.saturating_add(3);
                        mode = Mode::Normal;
                        continue;
                    }
                    raw.push(ch);
                }
            }
            index = index.saturating_add(1);
        }
        if depth <= 0 && matches!(mode, Mode::Normal) {
            return raw;
        }
        raw.push('\n');
        *cursor = cursor.saturating_add(1);
        match lines.get(*cursor) {
            Some(next) => {
                current.clear();
                current.push_str(next);
            }
            None => return raw,
        }
    }
}

/// A basic string's text, with escapes undone and line continuations folded.
fn unescape(raw: &str, multiline: bool) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('\n') if multiline => {
                while chars.peek().is_some_and(|next| next.is_whitespace()) {
                    chars.next();
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// `body` without one leading newline.
fn without_first_newline(body: String) -> String {
    match body.strip_prefix('\n') {
        Some(rest) => rest.to_owned(),
        None => body,
    }
}

/// Every string in `raw` (basic or literal, single or multi-line), in order.
fn strings_in(raw: &str) -> Vec<String> {
    let chars: Vec<char> = raw.chars().collect();
    let mut found = Vec::new();
    let mut index = 0_usize;
    while index < chars.len() {
        let ch = chars[index];
        if starts_at(&chars, index, "\"\"\"") || starts_at(&chars, index, "'''") {
            let quote = if ch == '"' { "\"\"\"" } else { "'''" };
            let start = index.saturating_add(3);
            let mut end = start;
            while end < chars.len() && !starts_at(&chars, end, quote) {
                end = end.saturating_add(if ch == '"' && chars[end] == '\\' { 2 } else { 1 });
            }
            let body: String = chars.iter().skip(start).take(end.saturating_sub(start)).collect();
            let body = without_first_newline(body);
            found.push(if ch == '"' { unescape(&body, true) } else { body });
            index = end.saturating_add(3);
        } else if ch == '"' {
            let start = index.saturating_add(1);
            let mut end = start;
            while end < chars.len() && chars[end] != '"' {
                end = end.saturating_add(if chars[end] == '\\' { 2 } else { 1 });
            }
            let body: String = chars.iter().skip(start).take(end.saturating_sub(start)).collect();
            found.push(unescape(&body, false));
            index = end.saturating_add(1);
        } else if ch == '\'' {
            let start = index.saturating_add(1);
            let mut end = start;
            while end < chars.len() && chars[end] != '\'' {
                end = end.saturating_add(1);
            }
            found.push(chars.iter().skip(start).take(end.saturating_sub(start)).collect());
            index = end.saturating_add(1);
        } else {
            index = index.saturating_add(1);
        }
    }
    found
}

/// Classifies a collected value.
fn classify(raw: &str) -> Value {
    let trimmed = raw.trim();
    if trimmed.starts_with('[') {
        return Value::List(strings_in(trimmed));
    }
    if trimmed.starts_with(['"', '\'']) {
        let mut strings = strings_in(trimmed);
        if !strings.is_empty() {
            return Value::Text(strings.swap_remove(0));
        }
    }
    Value::Raw(trimmed.to_owned())
}

/// The header path of a `[table]` or `[[table]]` line, and whether it is an array.
fn header(trimmed: &str) -> Option<(String, bool)> {
    if let Some(inner) = trimmed.strip_prefix("[[") {
        let (path, _) = inner.split_once("]]")?;
        return Some((path.trim().to_owned(), true));
    }
    let inner = trimmed.strip_prefix('[')?;
    let (path, _) = inner.rsplit_once(']')?;
    Some((path.trim().to_owned(), false))
}

/// The key of a `key = value` line, quotes removed, and the text after `=`.
fn key_value(trimmed: &str) -> Option<(String, &str)> {
    let mut quote: Option<char> = None;
    for (index, ch) in trimmed.char_indices() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '=' => {
                let key = trimmed.get(..index)?.trim().trim_matches(['"', '\'']).to_owned();
                let rest = trimmed.get(index.saturating_add(1)..)?;
                return Some((key, rest));
            }
            None => {}
        }
    }
    None
}

/// Reads every table of `text`, starting with the headerless top level.
pub(super) fn read_tables(text: &str) -> Vec<Table> {
    let lines: Vec<&str> = text.lines().collect();
    let mut tables = vec![Table {
        path: String::new(),
        array: false,
        entries: Vec::new(),
    }];
    let mut cursor = 0_usize;
    while let Some(line) = lines.get(cursor) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            cursor = cursor.saturating_add(1);
            continue;
        }
        if trimmed.starts_with('[')
            && let Some((path, array)) = header(trimmed)
        {
            tables.push(Table {
                path,
                array,
                entries: Vec::new(),
            });
            cursor = cursor.saturating_add(1);
            continue;
        }
        if let Some((key, rest)) = key_value(trimmed) {
            let raw = collect_value(&lines, &mut cursor, rest);
            if let Some(table) = tables.last_mut() {
                table.entries.push(Entry {
                    key,
                    value: classify(&raw),
                });
            }
        }
        cursor = cursor.saturating_add(1);
    }
    tables
}

/// The text of the entry `key` of the table `path`, if present.
fn text_of(tables: &[Table], path: &str, key: &str) -> Option<String> {
    tables
        .iter()
        .filter(|table| table.path == path && !table.array)
        .flat_map(|table| table.entries.iter())
        .find(|entry| entry.key == key)
        .and_then(|entry| match entry.value {
            Value::Text(ref text) => Some(text.clone()),
            Value::List(_) | Value::Raw(_) => None,
        })
}

/// A crate's import name: its package name, dashes to underscores.
pub(super) fn crate_name(tables: &[Table]) -> Option<String> {
    text_of(tables, "package", "name").map(|name| name.replace('-', "_"))
}

/// The members a workspace manifest lists.
pub(super) fn workspace_members(tables: &[Table]) -> Vec<String> {
    for table in tables.iter().filter(|table| table.path == "workspace") {
        for entry in &table.entries {
            if entry.key == "members"
                && let Value::List(ref members) = entry.value
            {
                return members.clone();
            }
        }
    }
    Vec::new()
}

/// The segments of a dotted header path, with quoted segments kept whole.
fn segments(path: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in path.chars() {
        match quote {
            Some(open) => {
                if ch == open {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '.' => parts.push(std::mem::take(&mut current)),
            None => current.push(ch),
        }
    }
    parts.push(current);
    parts
}

/// The dependency table kinds.
const DEPENDENCY_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// Every crate a manifest declares, across every dependency table.
///
/// Target-specific tables (`[target.'cfg(unix)'.dependencies]`) are included: an
/// edge is an edge whether or not this host compiles it. Both the inline form
/// (`name = { … }`) and the sub-table form (`[dependencies.name]`) name a crate.
pub(super) fn declared_dependencies(tables: &[Table]) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for table in tables {
        let parts = segments(&table.path);
        let Some(first) = parts.first() else {
            continue;
        };
        let base = if first == "target" { 2_usize } else { 0_usize };
        let Some(kind) = parts.get(base) else {
            continue;
        };
        if !DEPENDENCY_TABLES.contains(&kind.as_str()) {
            continue;
        }
        match parts.len().saturating_sub(base) {
            1 => {
                for entry in &table.entries {
                    if let Some(name) = entry.key.split('.').next() {
                        found.insert(name.replace('-', "_"));
                    }
                }
            }
            2 => {
                if let Some(name) = parts.get(base.saturating_add(1)) {
                    found.insert(name.replace('-', "_"));
                }
            }
            _ => {}
        }
    }
    found
}

/// The crates a comma-separated `allowed_consumers` names, dashes normalised.
pub(super) fn consumers_of(raw: &str) -> BTreeSet<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| name.replace('-', "_"))
        .collect()
}

/// The text or raw value of the entry `key` of an array-of-tables element.
fn field(table: &Table, key: &str) -> Option<String> {
    table
        .entries
        .iter()
        .find(|entry| entry.key == key)
        .and_then(|entry| match entry.value {
            Value::Text(ref text) | Value::Raw(ref text) => Some(text.clone()),
            Value::List(_) => None,
        })
}

/// `contract/APPROVED.toml` keyed by crate name, dashes normalised to underscores.
///
/// One crate can be approved once per owner (`tracing` is registered for
/// `lgwks_std` and again for `lgwks_ast`). Keying by name alone made the last
/// record win and silently dropped the earlier consumer, so the records are
/// merged: the first keeps its owner and every record's consumers are admitted.
pub(super) fn approvals(tables: &[Table]) -> BTreeMap<String, Record> {
    let mut merged: BTreeMap<String, Record> = BTreeMap::new();
    for table in tables.iter().filter(|table| table.path == "approved" && table.array) {
        let Some(raw_name) = field(table, "crate") else {
            continue;
        };
        let name = raw_name.replace('-', "_");
        let consumers = match field(table, "allowed_consumers") {
            Some(raw) => consumers_of(&raw),
            None => BTreeSet::new(),
        };
        if let Some(existing) = merged.get_mut(&name) {
            existing.consumers.extend(consumers);
            continue;
        }
        merged.insert(
            name,
            Record {
                owner: field(table, "owner"),
                capability: field(table, "capability"),
                tier: field(table, "tier"),
                reason: field(table, "reason"),
                consumers,
            },
        );
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::{approvals, crate_name, declared_dependencies, read_tables, workspace_members};

    const MANIFEST: &str = "[package]\nname = \"lgwks-demo\"\n\n\
        [dependencies]\nserde = { version = \"1\", features = [\"derive\"] }\nlgwks_std.workspace = true\n\n\
        [dev-dependencies.proptest]\nversion = \"1\"\n\n\
        [target.'cfg(unix)'.dependencies]\nrustix = { version = \"1\" }\n\n\
        [features]\nfull = [\"a\"]\n";

    #[test]
    fn a_manifest_names_its_crate_and_every_declared_dependency() {
        let tables = read_tables(MANIFEST);
        assert_eq!(crate_name(&tables).as_deref(), Some("lgwks_demo"), "dashes become underscores");
        assert_eq!(
            declared_dependencies(&tables).into_iter().collect::<Vec<_>>(),
            vec!["lgwks_std", "proptest", "rustix", "serde"],
            "inline, dotted, sub-table and target forms all count; features do not"
        );
    }

    #[test]
    fn a_workspace_lists_its_members_across_lines() {
        let tables = read_tables("[workspace]\nmembers = [\n  \"crates/a\", # one\n  \"crates/b\",\n]\n");
        assert_eq!(workspace_members(&tables), vec!["crates/a", "crates/b"], "a multi-line array");
    }

    #[test]
    fn approvals_merge_per_crate_and_keep_the_first_owner() {
        let register = "[[approved]]\ncrate = \"tracing\"\nowner = \"lgwks_std\"\nreason = \"\"\"a \\\n   b\"\"\"\nallowed_consumers = \"lgwks_std\"\n\n\
            [[approved]]\ncrate = \"tracing\"\nowner = \"lgwks_ast\"\nallowed_consumers = \"lgwks-ast, lgwks_std\"\n";
        let merged = approvals(&read_tables(register));
        let record = merged.get("tracing");
        assert_eq!(record.map(|found| found.owner.as_deref()), Some(Some("lgwks_std")), "first owner wins");
        assert_eq!(
            record.map(|found| found.consumers.iter().cloned().collect::<Vec<_>>()),
            Some(vec!["lgwks_ast".to_owned(), "lgwks_std".to_owned()]),
            "consumers are the union"
        );
        assert_eq!(record.and_then(|found| found.reason.clone()).as_deref(), Some("a b"), "a continued line folds");
    }
}
