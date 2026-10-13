//! The TOML subset the gate binaries read: crate manifests, the approval
//! register and the lane table.
//!
//! This is not a general TOML parser. It reads tables, array-of-tables headers,
//! `key = value` lines, strings (basic, literal, and their multi-line forms),
//! arrays and inline tables as text, which is every construct those files use.
//! One reader serves every binary that includes this file, so a fix to it is a
//! fix to all of them.

/// A value as the gate binaries need it.
pub(crate) enum Value {
    /// A string.
    Text(String),
    /// An array's string elements.
    List(Vec<String>),
    /// Anything else (a number, a boolean, an inline table), as written.
    Raw(String),
}

/// One `key = value` line.
pub(crate) struct Entry {
    /// The key as written, with surrounding quotes removed.
    pub(crate) key: String,
    /// The value.
    pub(crate) value: Value,
}

/// One table or array-of-tables element.
pub(crate) struct Table {
    /// The header's path, e.g. `dependencies` or `target.'cfg(unix)'.dependencies`.
    pub(crate) path: String,
    /// Whether the header was `[[path]]`.
    pub(crate) array: bool,
    /// The entries under the header.
    pub(crate) entries: Vec<Entry>,
}

/// The string under `key` of a table.
pub(crate) fn text<'table>(table: &'table Table, key: &str) -> Option<&'table str> {
    table
        .entries
        .iter()
        .find(|entry| entry.key == key)
        .and_then(|entry| match entry.value {
            Value::Text(ref found) => Some(found.as_str()),
            Value::List(_) | Value::Raw(_) => None,
        })
}

/// The value under `key` of a table as it was written: a number, a boolean or an
/// inline table.
pub(crate) fn raw<'table>(table: &'table Table, key: &str) -> Option<&'table str> {
    table
        .entries
        .iter()
        .find(|entry| entry.key == key)
        .and_then(|entry| match entry.value {
            Value::Raw(ref found) => Some(found.as_str()),
            Value::Text(_) | Value::List(_) => None,
        })
}

/// The strings under `key` of a table.
pub(crate) fn list<'table>(table: &'table Table, key: &str) -> Option<&'table [String]> {
    table
        .entries
        .iter()
        .find(|entry| entry.key == key)
        .and_then(|entry| match entry.value {
            Value::List(ref found) => Some(found.as_slice()),
            Value::Text(_) | Value::Raw(_) => None,
        })
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
pub(crate) fn read_tables(text: &str) -> Vec<Table> {
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
