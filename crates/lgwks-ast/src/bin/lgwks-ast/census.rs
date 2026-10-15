//! `lgwks-ast script census`: which plain-Rust shapes recur across `script!`
//! flows (SL-7, #389).
//!
//! A word is admitted when passthrough Rust keeps recurring
//! (`docs/script-word-admission.md`). The census reads every `script!` under
//! the paths through the one parser, groups the lines no word reads by shape,
//! and prints the shapes most-recurrent first, with how many lines and files
//! have each and where. A shape that recurs at three or more sites is a
//! candidate word. The census measures and never gates: a gated share would be
//! gamed by moving code out of scripts, which is the opposite of the goal.
//!
//! A shape is the line with its bindings, receivers, arguments and literals
//! blanked and its keywords, types, called names and punctuation kept, so
//! `let a = site.get(id)` and `let b = page.get(n)` are one shape,
//! `let _ = _.get(0)`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use lgwks_ast::script::{FlowShape, StepShape};

use super::{FileReading, Totals};

/// The rows printed when `--top` is not given.
pub(super) const DEFAULT_TOP: usize = 20;

/// The most rows `--top` may ask for: the table is for a reader, and a census
/// of a whole estate has fewer shapes than this that recur at all.
const MAX_TOP: usize = 1_000;

/// How many sites a row names.
const SITES_SHOWN: usize = 3;

/// Rust's strict and reserved keywords, which carry a line's structure.
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];

/// Punctuation written with no space before it.
const TIGHT_BEFORE: &str = ".,;?)]:";

/// Punctuation written with no space after it.
const TIGHT_AFTER: &str = ".([&!";

/// The row count `--top` names.
///
/// # Errors
///
/// `value` is not a whole number from 1 to [`MAX_TOP`].
pub(super) fn rows(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|count| (1..=MAX_TOP).contains(count))
        .ok_or_else(|| format!("--top takes a row count from 1 to {MAX_TOP}, not `{value}`"))
}

/// Print the census of everything `readings` read.
///
/// # Errors
///
/// The writer's own error.
pub(super) fn write(
    readings: &[FileReading],
    top: usize,
    out: &mut impl Write,
) -> io::Result<ExitCode> {
    let census = count(readings);
    let refused = Totals::of(readings).refused;
    for line in render(&census, refused, top) {
        writeln!(out, "{line}")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// What the census counted.
#[derive(Default)]
struct Counts {
    /// `script!` invocations found, accepted or not.
    invocations: usize,
    /// Flows in the invocations the language accepted.
    flows: usize,
    /// Steps a word reads, nested ones included.
    steps: usize,
    /// Lines no word reads.
    passthrough: usize,
}

/// Every passthrough line counted, grouped by shape.
struct Census {
    /// Each shape and the `path:line` of every line that has it, in read order.
    sites: BTreeMap<String, Vec<String>>,
    /// The files that held a script the language accepted.
    files: BTreeSet<String>,
    /// The counts.
    totals: Counts,
}

impl Census {
    /// Count one flow's steps and passthrough lines, read from `path`.
    fn add_flow(&mut self, path: &str, shape: &FlowShape) {
        self.totals.flows = self.totals.flows.saturating_add(1);
        self.totals.steps = self.totals.steps.saturating_add(steps_in(shape.steps()));
        for passed in shape.passthrough() {
            self.totals.passthrough = self.totals.passthrough.saturating_add(1);
            self.sites
                .entry(shape_of(passed.text()))
                .or_default()
                .push(format!("{path}:{}", passed.line()));
        }
    }
}

/// `steps` and the steps nested in them.
fn steps_in(steps: &[StepShape]) -> usize {
    steps.iter().fold(0_usize, |total, step| {
        total
            .saturating_add(1)
            .saturating_add(steps_in(step.children()))
    })
}

/// The path as the caller named it, without a leading `./`.
fn named(path: &Path) -> String {
    let shown = path.display().to_string();
    let trimmed = shown.strip_prefix("./").map(str::to_owned);
    match trimmed {
        Some(rest) => rest,
        None => shown,
    }
}

/// Group every passthrough line the accepted scripts in `readings` hold.
fn count(readings: &[FileReading]) -> Census {
    let mut census = Census {
        sites: BTreeMap::new(),
        files: BTreeSet::new(),
        totals: Counts::default(),
    };
    for reading in readings {
        let Ok(ref invocations) = reading.outcome else {
            continue;
        };
        let path = named(&reading.path);
        for invocation in invocations {
            census.totals.invocations = census.totals.invocations.saturating_add(1);
            let Ok(script) = invocation.read() else {
                continue;
            };
            census.files.insert(path.clone());
            for flow in script.flows() {
                census.add_flow(&path, flow.shape());
            }
        }
    }
    census
}

/// `part` of `whole` as a percentage with one decimal, rounded half up, or
/// `n/a` when there is no whole to take a share of.
fn percent(part: usize, whole: usize) -> String {
    let scaled = part.saturating_mul(2_000).saturating_add(whole);
    let tenths = scaled.checked_div(whole.saturating_mul(2));
    match tenths.map(|total| (total.checked_div(10), total.checked_rem(10))) {
        Some((Some(units), Some(tenth))) => format!("{units}.{tenth}%"),
        _ => "n/a".to_owned(),
    }
}

/// The census as Markdown lines: the totals, the table of the `top` shapes
/// that recur most, and the files read. A `|` in a shape is escaped, since a
/// closure or an `||` would otherwise end its table cell.
fn render(census: &Census, refused: usize, top: usize) -> Vec<String> {
    let totals = &census.totals;
    let read = totals.steps.saturating_add(totals.passthrough);
    let mut lines = vec![
        format!(
            "{} `script!` in {} files, {} flows; {} word steps and {} passthrough lines \
             (passthrough share {}); {refused} refused or unreadable and not read; \
             {} distinct shapes.",
            totals.invocations,
            census.files.len(),
            totals.flows,
            totals.steps,
            totals.passthrough,
            percent(totals.passthrough, read),
            census.sites.len(),
        ),
        String::new(),
        "| # | Lines | Files | Shape | Sites (first three) |".to_owned(),
        "|---|---|---|---|---|".to_owned(),
    ];
    let mut ranked: Vec<(&String, &Vec<String>)> = census.sites.iter().collect();
    ranked.sort_by(|left, right| {
        right
            .1
            .len()
            .cmp(&left.1.len())
            .then_with(|| left.0.cmp(right.0))
    });
    for (index, &(shape, places)) in ranked.iter().take(top).enumerate() {
        let files: BTreeSet<&str> = places
            .iter()
            .filter_map(|site| site.rsplit_once(':').map(|(file, _)| file))
            .collect();
        let shown: Vec<String> = places
            .iter()
            .take(SITES_SHOWN)
            .map(|site| format!("`{site}`"))
            .collect();
        lines.push(format!(
            "| {} | {} | {} | `{}` | {} |",
            index.saturating_add(1),
            places.len(),
            files.len(),
            shape.replace('|', "\\|"),
            shown.join(", "),
        ));
    }
    lines.push(String::new());
    lines.push("Files read:".to_owned());
    lines.extend(census.files.iter().map(|file| format!("- `{file}`")));
    lines
}

/// The line with its names and literals blanked, tokens joined the way
/// rustfmt spaces them.
fn shape_of(text: &str) -> String {
    let words = tokens(text);
    let mut kept = Vec::with_capacity(words.len());
    for (index, word) in words.iter().enumerate() {
        let following = word_at(&words, index.saturating_add(1));
        let beyond = word_at(&words, index.saturating_add(2));
        kept.push(blank(word, following, beyond));
    }
    join(&kept)
}

/// The token at `index`, or an empty string past either end.
fn word_at(words: &[String], index: usize) -> &str {
    match words.get(index) {
        Some(word) => word.as_str(),
        None => "",
    }
}

/// What `token` becomes in a shape, given the two tokens after it: a literal
/// or a name is blanked, a keyword, a type, a called name or punctuation stays.
fn blank(token: &str, following: &str, beyond: &str) -> String {
    let Some(first) = token.chars().next() else {
        return String::new();
    };
    if first == '"' || first == '\'' || token.starts_with("b\"") || token.starts_with("b'") {
        // A lifetime longer than one letter is structure; a character, a
        // string and `'a` are literals.
        let lifetime = token.starts_with('\'') && token.chars().count() > 3;
        return if lifetime {
            token.to_owned()
        } else {
            "_".to_owned()
        };
    }
    if first.is_ascii_digit() {
        return "0".to_owned();
    }
    if first.is_alphabetic() || first == '_' || token.starts_with("r#") {
        let macro_call = following == "!" && matches!(beyond, "(" | "[" | "{");
        let path = following == ":" && beyond == ":";
        let called = following == "(" || macro_call || path;
        return if KEYWORDS.contains(&token) || first.is_uppercase() || called {
            token.to_owned()
        } else {
            "_".to_owned()
        };
    }
    token.to_owned()
}

/// Whether `word` is one character, and one of `set`.
fn one_of(word: &str, set: &str) -> bool {
    word.chars().count() == 1 && set.contains(word)
}

/// Whether `word` ends in a letter or a digit.
fn ends_alphanumeric(word: &str) -> bool {
    word.chars().last().is_some_and(char::is_alphanumeric)
}

/// Tokens laid out the way rustfmt spaces them: `a.b(c, d)?`, `x: T`, `a::b`,
/// `f!(x)`, and one space around everything else.
fn join(tokens: &[String]) -> String {
    let mut rendered = String::new();
    for (index, token) in tokens.iter().enumerate() {
        let previous = match index.checked_sub(1) {
            Some(before) => word_at(tokens, before),
            None => "",
        };
        let next = word_at(tokens, index.saturating_add(1));
        let colon_path = token == ":" && (previous == ":" || next == ":");
        let opens_call = matches!(token.as_str(), "(" | "[")
            && (ends_alphanumeric(previous) || one_of(previous, "_!)]>"));
        let macro_bang = token == "!" && ends_alphanumeric(previous) && matches!(next, "(" | "[");
        let glued = rendered.is_empty()
            || one_of(token, TIGHT_BEFORE)
            || one_of(previous, TIGHT_AFTER)
            || opens_call
            || (previous == ":" && rendered.ends_with("::"))
            || colon_path
            || macro_bang;
        if !glued {
            rendered.push(' ');
        }
        rendered.push_str(token);
    }
    rendered
}

/// The length in characters of the quoted literal that opens at `start`,
/// closing quote included, when `chars` holds one. A backslash escapes the
/// character after it, and a line break is not escapable.
fn quoted(chars: &[char], start: usize, quote: char) -> Option<usize> {
    let mut at = start.saturating_add(1);
    loop {
        let current = *chars.get(at)?;
        if current == quote {
            return Some(at.saturating_sub(start).saturating_add(1));
        }
        at = if current == '\\' {
            match chars.get(at.saturating_add(1)) {
                Some(&escaped) if escaped != '\n' => at.saturating_add(2),
                _ => return None,
            }
        } else {
            at.saturating_add(1)
        };
    }
}

/// The length of the character literal that opens at `start`, when `chars`
/// holds one: one character, or one escaped character, between quotes.
fn character(chars: &[char], start: usize) -> Option<usize> {
    let first = *chars.get(start.saturating_add(1))?;
    let width = match first {
        '\\' => match chars.get(start.saturating_add(2)) {
            Some(&escaped) if escaped != '\n' => 2,
            _ => return None,
        },
        '\'' => return None,
        _ => 1,
    };
    let close = start.saturating_add(1).saturating_add(width);
    if chars.get(close) == Some(&'\'') {
        Some(close.saturating_sub(start).saturating_add(1))
    } else {
        None
    }
}

/// The length of a string or character literal at `at`, with its `b` prefix.
fn literal(chars: &[char], at: usize) -> Option<usize> {
    let first = *chars.get(at)?;
    let prefix = usize::from(first == 'b');
    let open = at.saturating_add(prefix);
    let length = match *chars.get(open)? {
        '"' => quoted(chars, open, '"'),
        '\'' => character(chars, open),
        _ => None,
    }?;
    Some(length.saturating_add(prefix))
}

/// Whether `ch` can start an identifier.
const fn starts_identifier(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

/// Whether `ch` can continue an identifier.
const fn continues_identifier(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// Whether `ch` can continue a number's digits.
const fn digit_or_underscore(ch: char) -> bool {
    ch.is_ascii_digit() || ch == '_'
}

/// How many characters from `from` on satisfy `keep`, in a row.
fn run_of(chars: &[char], from: usize, keep: fn(char) -> bool) -> usize {
    chars
        .iter()
        .skip(from)
        .copied()
        .take_while(|ch| keep(*ch))
        .count()
}

/// The length of a lifetime at `start`: `'` and an identifier.
fn lifetime(chars: &[char], start: usize) -> Option<usize> {
    let name = start.saturating_add(1);
    if !chars.get(name).copied().is_some_and(starts_identifier) {
        return None;
    }
    Some(run_of(chars, name, continues_identifier).saturating_add(1))
}

/// The length of the number at `start`: digits, a fraction, an exponent and a
/// type suffix.
fn number(chars: &[char], start: usize) -> usize {
    let mut end = start.saturating_add(run_of(chars, start, digit_or_underscore));
    let fraction = end.saturating_add(1);
    if chars.get(end) == Some(&'.') && chars.get(fraction).is_some_and(char::is_ascii_digit) {
        end = fraction.saturating_add(run_of(chars, fraction, digit_or_underscore));
    }
    if matches!(chars.get(end), Some(&('e' | 'E'))) {
        let signed = usize::from(matches!(
            chars.get(end.saturating_add(1)),
            Some(&('+' | '-'))
        ));
        let digits_at = end.saturating_add(1).saturating_add(signed);
        let digits = run_of(chars, digits_at, |ch| ch.is_ascii_digit());
        if digits > 0 {
            end = digits_at.saturating_add(digits);
        }
    }
    end.saturating_add(run_of(chars, end, continues_identifier))
        .saturating_sub(start)
}

/// The length of the token that starts at `at`: a literal, a lifetime, a
/// number, an identifier, or one character of anything else.
fn token_length(chars: &[char], at: usize) -> usize {
    let Some(&first) = chars.get(at) else {
        return 0;
    };
    if let Some(length) = literal(chars, at) {
        return length;
    }
    if first == '\''
        && let Some(length) = lifetime(chars, at)
    {
        return length;
    }
    if first.is_ascii_digit() {
        return number(chars, at);
    }
    let raw = at.saturating_add(2);
    if first == 'r'
        && chars.get(at.saturating_add(1)) == Some(&'#')
        && chars.get(raw).copied().is_some_and(starts_identifier)
    {
        return run_of(chars, raw, continues_identifier).saturating_add(2);
    }
    if starts_identifier(first) {
        return run_of(chars, at, continues_identifier);
    }
    1
}

/// The tokens of `text`, whitespace dropped.
fn tokens(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    let mut at = 0_usize;
    while let Some(&current) = chars.get(at) {
        if current.is_whitespace() {
            at = at.saturating_add(1);
            continue;
        }
        let end = at.saturating_add(token_length(&chars, at).max(1));
        match chars.get(at..end) {
            Some(slice) => found.push(slice.iter().collect()),
            None => break,
        }
        at = end;
    }
    found
}

#[cfg(test)]
mod tests;
