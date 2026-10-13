//! The module paths and items the sources define, which the invariants lane
//! resolves every `enforced by:` reference against.
//!
//! Module nesting is derived from the crate layout, which is what Rust itself
//! does: `src/lib.rs` and `src/main.rs` are the crate root, `src/<name>.rs` is
//! the module `<name>` at the root, `src/<dir>/mod.rs` is the module `<dir>`, and
//! `src/<dir>/<name>.rs` is `<dir>::<name>`. The `src` directory is not itself a
//! module, so it is dropped, and the crate directory keeps its on-disk spelling
//! (`lgwks-std`) while a reference may write the Rust spelling (`lgwks_std::`);
//! the resolver compares the two through one normalisation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::builtins::{relative, rust_files_under, tail};
use super::{GateError, read};

/// Module path -> the item names that module's own source defines.
pub(super) type Modules = BTreeMap<String, BTreeSet<String>>;

/// `line` without its `pub` or `pub(...)` visibility, when it has one.
fn without_visibility(line: &str) -> &str {
    let Some(after) = line.strip_prefix("pub") else {
        return line;
    };
    let after_scope = match after.strip_prefix('(') {
        Some(inside) => match inside.split_once(')') {
            Some((_, rest)) => rest,
            None => return line,
        },
        None => after,
    };
    let spaced = after_scope.trim_start();
    if spaced.len() == after_scope.len() {
        line
    } else {
        spaced
    }
}

/// The text after `keyword` and the whitespace that must follow it.
fn after_word<'line>(line: &'line str, keyword: &str) -> Option<&'line str> {
    let rest = line.strip_prefix(keyword)?;
    let spaced = rest.trim_start();
    if spaced.len() == rest.len() {
        None
    } else {
        Some(spaced)
    }
}

/// A lowercase identifier at the front of `text`, and what follows it.
fn identifier(text: &str) -> Option<(String, &str)> {
    let name: String = text
        .chars()
        .take_while(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || *ch == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        let rest = tail(text, name.len());
        Some((name, rest))
    }
}

/// A `mod name;` or `mod name {` declaration: the name, and whether it has a body.
fn module_declaration(line: &str) -> Option<(String, bool)> {
    let (name, rest) = identifier(after_word(without_visibility(line), "mod")?)?;
    let opener = rest.trim_start();
    if opener.starts_with('{') {
        Some((name, true))
    } else if opener.starts_with(';') {
        Some((name, false))
    } else {
        None
    }
}

/// The text after the optional `async`, `const` and `unsafe` qualifiers.
fn without_qualifiers(line: &str) -> &str {
    let mut rest = without_visibility(line);
    for qualifier in ["async", "const", "unsafe"] {
        if let Some(after) = after_word(rest, qualifier) {
            rest = after;
        }
    }
    rest
}

/// A `fn name` item: the name.
fn function_item(line: &str) -> Option<String> {
    identifier(after_word(without_qualifiers(line), "fn")?).map(|found| found.0)
}

/// A `fn name` or `mod name` item: the name.
fn inline_item(line: &str) -> Option<String> {
    let rest = without_qualifiers(line);
    let after = after_word(rest, "fn").or_else(|| after_word(rest, "mod"))?;
    identifier(after).map(|found| found.0)
}

/// A `mod name {` opener: the name, and the offset of its `{` in characters.
fn module_opener(line: &str) -> Option<(String, usize)> {
    let (name, brace) = module_declaration(line)?;
    if !brace {
        return None;
    }
    let (front, _) = line.split_once('{')?;
    Some((name, front.chars().count()))
}

/// A bench function: `fn name(` at the start of a line.
fn bench_function(line: &str) -> Option<String> {
    let (name, rest) = identifier(after_word(line, "fn")?)?;
    if rest.trim_start().starts_with('(') {
        Some(name)
    } else {
        None
    }
}

/// The position after a raw-string opener at `at`, and its closing delimiter.
fn raw_opener(chars: &[char], at: usize) -> Option<(usize, String)> {
    let mut cursor = at;
    if chars.get(cursor) == Some(&'b') {
        cursor = cursor.saturating_add(1);
    }
    if chars.get(cursor) != Some(&'r') {
        return None;
    }
    cursor = cursor.saturating_add(1);
    let mut hashes = 0_usize;
    while chars.get(cursor) == Some(&'#') {
        hashes = hashes.saturating_add(1);
        cursor = cursor.saturating_add(1);
    }
    if chars.get(cursor) != Some(&'"') {
        return None;
    }
    Some((
        cursor.saturating_add(1),
        format!("\"{}", "#".repeat(hashes)),
    ))
}

/// The first position at or after `from` where `needle` starts.
fn find_at(chars: &[char], needle: &str, from: usize) -> Option<usize> {
    let wanted: Vec<char> = needle.chars().collect();
    (from..chars.len())
        .find(|&at| chars.get(at..at.saturating_add(wanted.len())) == Some(wanted.as_slice()))
}

/// The position after `count` hex digits starting at `start`.
fn after_hex(chars: &[char], start: usize, count: usize) -> Option<usize> {
    let digits = chars
        .iter()
        .skip(start)
        .take(count)
        .filter(|ch| ch.is_ascii_hexdigit())
        .count();
    if digits == count {
        Some(start.saturating_add(count))
    } else {
        None
    }
}

/// The position after a `\u{...}` escape whose `u` is at `u_at`.
fn after_unicode(chars: &[char], u_at: usize) -> Option<usize> {
    if chars.get(u_at.saturating_add(1)) != Some(&'{') {
        return None;
    }
    let digits_at = u_at.saturating_add(2);
    let run = chars
        .iter()
        .skip(digits_at)
        .take_while(|ch| ch.is_ascii_hexdigit())
        .count();
    let close = digits_at.saturating_add(run);
    if run > 0 && chars.get(close) == Some(&'}') {
        Some(close.saturating_add(1))
    } else {
        None
    }
}

/// How many characters a character literal at `at` spans, or `None` for a lifetime.
fn char_literal_len(chars: &[char], at: usize) -> Option<usize> {
    let first = chars.get(at.saturating_add(1)).copied()?;
    if first != '\\' {
        return if first != '\'' && chars.get(at.saturating_add(2)) == Some(&'\'') {
            Some(3)
        } else {
            None
        };
    }
    let escape_at = at.saturating_add(2);
    let escaped = chars.get(escape_at).copied()?;
    let hex_form = if escaped == 'x' {
        after_hex(chars, escape_at.saturating_add(1), 2)
    } else {
        None
    };
    let unicode_form = if escaped == 'u' {
        after_unicode(chars, escape_at)
    } else {
        None
    };
    let plain_form = if escaped == '\n' {
        None
    } else {
        Some(escape_at.saturating_add(1))
    };
    [hex_form, unicode_form, plain_form]
        .into_iter()
        .flatten()
        .find(|end| chars.get(*end) == Some(&'\''))
        .map(|end| end.saturating_add(1).saturating_sub(at))
}

/// The items each inline `mod name { … }` in `source` defines, by nesting path.
///
/// Braces are matched on the code alone: string literals (raw ones included),
/// character literals and comments are skipped, so a `{` in a format string or a
/// doc comment cannot close a module early. A lifetime (`'a`) is not a character
/// literal, and is told apart by its missing closing quote.
pub(super) fn inline_module_items(source: &str) -> BTreeMap<Vec<String>, BTreeSet<String>> {
    let chars: Vec<char> = source.chars().collect();
    let length = chars.len();
    let mut found: BTreeMap<Vec<String>, BTreeSet<String>> = BTreeMap::new();
    let mut stack: Vec<(String, i64)> = Vec::new();
    let mut depth = 0_i64;
    let mut index = 0_usize;
    let mut at_line_start = true;
    while index < length {
        let ch = chars[index];
        if ch == '/' && chars.get(index.saturating_add(1)) == Some(&'/') {
            match find_at(&chars, "\n", index) {
                Some(end) => index = end,
                None => index = length,
            }
            continue;
        }
        if ch == '/' && chars.get(index.saturating_add(1)) == Some(&'*') {
            match find_at(&chars, "*/", index.saturating_add(2)) {
                Some(end) => index = end.saturating_add(2),
                None => index = length,
            }
            continue;
        }
        let ident_before = index > 0
            && chars
                .get(index.saturating_sub(1))
                .is_some_and(|prev| prev.is_alphanumeric() || *prev == '_');
        if !ident_before && let Some((body_start, closing)) = raw_opener(&chars, index) {
            match find_at(&chars, &closing, body_start) {
                Some(end) => index = end.saturating_add(closing.chars().count()),
                None => index = length,
            }
            continue;
        }
        if ch == '"' {
            let mut cursor = index.saturating_add(1);
            while cursor < length && chars[cursor] != '"' {
                cursor = cursor.saturating_add(if chars[cursor] == '\\' { 2 } else { 1 });
            }
            index = cursor.saturating_add(1);
            continue;
        }
        if ch == '\'' {
            match char_literal_len(&chars, index) {
                Some(span) => index = index.saturating_add(span),
                None => index = index.saturating_add(1),
            }
            continue;
        }
        if ch == '\n' {
            at_line_start = true;
            index = index.saturating_add(1);
            continue;
        }
        if at_line_start && !ch.is_whitespace() {
            at_line_start = false;
            let line: String = chars
                .iter()
                .skip(index)
                .take_while(|next| **next != '\n')
                .collect();
            if let Some((name, brace_at)) = module_opener(&line) {
                stack.push((name, depth));
                found
                    .entry(stack.iter().map(|entry| entry.0.clone()).collect())
                    .or_default();
                index = index.saturating_add(brace_at);
                continue;
            }
            if let Some(name) = inline_item(&line)
                && !stack.is_empty()
            {
                found
                    .entry(stack.iter().map(|entry| entry.0.clone()).collect())
                    .or_default()
                    .insert(name);
            }
        }
        if ch == '{' {
            depth = depth.saturating_add(1);
        } else if ch == '}' {
            depth = depth.saturating_sub(1);
            while stack.last().is_some_and(|entry| entry.1 == depth) {
                stack.pop();
            }
        }
        index = index.saturating_add(1);
    }
    found
}

/// The module path a source file defines, from its place under `crates/`.
fn module_prefix(root: &Path, path: &Path) -> Vec<String> {
    let crates = root.join("crates");
    let inner = if let Ok(stripped) = path.strip_prefix(&crates) {
        stripped
    } else {
        path
    };
    let mut parts: Vec<String> = inner
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .filter(|part| part != "src")
        .collect();
    let Some(last) = parts.pop() else {
        return parts;
    };
    if last == "lib.rs" || last == "main.rs" || last == "mod.rs" {
        return parts;
    }
    match last.strip_suffix(".rs") {
        Some(stem) => parts.push(stem.to_owned()),
        None => parts.push(last),
    }
    parts
}

/// Maps every module path to the item names its own source defines.
///
/// An earlier version built one flat set of every `mod` and `fn` name and
/// resolved a qualified reference by its leaf, so a real function in the wrong
/// module certified as resolved. Keeping the module path is what makes a
/// misattributed enforcement claim falsifiable.
pub(super) fn collect_module_paths(root: &Path) -> Result<Modules, GateError> {
    let mut modules: Modules = BTreeMap::new();
    for path in rust_files_under(&root.join("crates"), &["target"])? {
        let prefix = module_prefix(root, &path);
        let source = read(&path)?;
        let module_path = prefix.join("::");
        let mut items = BTreeSet::new();
        let mut has_inline_tests = false;
        for line in source.lines().map(str::trim_start) {
            if let Some((name, brace)) = module_declaration(line) {
                has_inline_tests = has_inline_tests || (brace && name == "tests");
                items.insert(name);
            }
            if let Some(name) = function_item(line) {
                items.insert(name);
            }
        }
        modules
            .entry(module_path.clone())
            .or_default()
            .extend(items.iter().cloned());
        if has_inline_tests {
            let tests_path = if module_path.is_empty() {
                "tests".to_owned()
            } else {
                format!("{module_path}::tests")
            };
            modules.entry(tests_path).or_default().extend(items);
        }
        for (nested, inner) in inline_module_items(&source) {
            let mut full = prefix.clone();
            full.extend(nested);
            modules.entry(full.join("::")).or_default().extend(inner);
        }
    }
    // A bench rig's invariant names the runner function that enforces it, so the
    // bench's own definitions resolve too: unindexed, every such reference reads
    // as missing, and the only way to pass would be to stop checking the claim.
    for path in rust_files_under(&root.join("bench"), &["target", "runs"])? {
        let shown = relative(root, &path);
        let stem = if let Some(stripped) = shown.strip_suffix(".rs") {
            stripped
        } else {
            shown.as_str()
        };
        let items = modules.entry(stem.replace('/', "::")).or_default();
        items.extend(
            read(&path)?
                .lines()
                .map(str::trim_start)
                .filter_map(bench_function),
        );
    }
    Ok(modules)
}

#[cfg(test)]
mod tests {
    use super::{function_item, inline_module_items, module_declaration, module_prefix};
    use std::path::Path;

    #[test]
    fn items_are_found_with_their_visibility_and_qualifiers() {
        assert_eq!(
            function_item("pub(crate) async fn load_it(x: u8)"),
            Some("load_it".to_owned()),
            "scoped and async"
        );
        assert_eq!(
            function_item("const unsafe fn zero()"),
            Some("zero".to_owned()),
            "const unsafe"
        );
        assert_eq!(
            function_item("fn Upper()"),
            None,
            "uppercase names are not collected"
        );
        assert_eq!(
            module_declaration("pub mod wire;"),
            Some(("wire".to_owned(), false)),
            "a file module"
        );
        assert_eq!(
            module_declaration("mod tests {"),
            Some(("tests".to_owned(), true)),
            "an inline module"
        );
        assert_eq!(module_declaration("modest;"), None, "a longer word");
    }

    #[test]
    fn a_nested_inline_module_holds_exactly_the_items_written_inside_its_braces() {
        let found = inline_module_items(
            "mod outer {\n    const BRACE: &str = \"}\";\n    // a stray } in a comment\n    fn helper() -> char { '}' }\n    #[cfg(test)]\n    mod tests {\n        fn inner_case<'a>(text: &'a str) -> &'a str { text }\n    }\n}\nfn outside() {}\n",
        );
        let outer = found.get(&vec!["outer".to_owned()]);
        let inner = found.get(&vec!["outer".to_owned(), "tests".to_owned()]);
        assert_eq!(
            outer.map(|items| items.iter().cloned().collect::<Vec<_>>()),
            Some(vec!["helper".to_owned()]),
            "outer"
        );
        assert_eq!(
            inner.map(|items| items.iter().cloned().collect::<Vec<_>>()),
            Some(vec!["inner_case".to_owned()]),
            "inner"
        );
    }

    #[test]
    fn raw_strings_hide_their_braces() {
        let found = inline_module_items(
            "mod holder {\n    const TEXT: &str = r#\"}\"#;\n    fn after() {}\n}\n",
        );
        assert_eq!(
            found
                .get(&vec!["holder".to_owned()])
                .map(|items| items.contains("after")),
            Some(true),
            "the raw string's brace does not close the module"
        );
    }

    #[test]
    fn a_source_path_becomes_its_module_path() {
        let root = Path::new("/repo");
        assert_eq!(
            module_prefix(root, Path::new("/repo/crates/lgwks-std/src/hex.rs")),
            vec!["lgwks-std", "hex"],
            "a module file"
        );
        assert_eq!(
            module_prefix(root, Path::new("/repo/crates/lgwks-std/src/lib.rs")),
            vec!["lgwks-std"],
            "the crate root"
        );
        assert_eq!(
            module_prefix(root, Path::new("/repo/crates/lgwks-std/src/fs/mod.rs")),
            vec!["lgwks-std", "fs"],
            "a directory module"
        );
    }
}
