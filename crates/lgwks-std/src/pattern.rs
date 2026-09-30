//! `pattern` owns compiled regular expression matching. A single search has
//! worst-case `O(m * n)` cost, where `m` is compiled pattern size and `n` is
//! haystack byte length. Complete `find_all`, `split`, and replacement
//! iteration can cost `O(m * n^2)`. A short-circuiting iterator avoids
//! producing later results but does not promise that only the returned prefix
//! was searched. Use [`crate::pattern::BoundedRegex`] for resource ceilings on
//! untrusted data.

use std::borrow::Cow;

/// A compiled regular expression.
///
/// Wraps `regex::Regex` with a smaller surface for matching and replacement.
/// Single search is worst-case `O(m * n)`; full iteration may be `O(m * n^2)`.
pub struct Regex(regex::Regex);

/// Error returned when a pattern fails to compile.
///
/// Access rejected input through the accessors; formatted text is escaped and
/// must not be parsed for policy decisions.
#[non_exhaustive]
pub struct PatternError {
    /// The rejected pattern, kept private so callers cannot mutate error state.
    pattern: String,
    /// The regex engine's description of the refusal.
    message: String,
    /// Machine-readable class for the refusal.
    kind: PatternErrorKind,
}

impl PatternError {
    /// Returns the rejected pattern.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Returns the engine's refusal description.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the machine-readable refusal class.
    #[must_use]
    pub fn kind(&self) -> PatternErrorKind {
        self.kind
    }
}

impl core::fmt::Debug for PatternError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PatternError")
            .field("pattern", &self.pattern)
            .field("message", &self.message)
            .field("kind", &self.kind)
            .finish()
    }
}

/// Machine-readable class for a pattern compilation refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PatternErrorKind {
    /// Source pattern exceeded its configured byte ceiling.
    PatternTooLarge {
        /// Configured source pattern ceiling in bytes.
        limit: usize,
        /// Actual source pattern size in bytes.
        actual: usize,
    },
    /// Compiled representation exceeded its configured size ceiling.
    CompiledTooLarge,
    /// Invalid syntax or parser nesting refusal.
    Syntax,
    /// Another engine compilation refusal.
    Other,
}

/// Explicit resource ceilings for pattern compilation and matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PatternConfig {
    /// Maximum source pattern size in bytes, checked before the engine parses it.
    pattern_limit: usize,
    /// Maximum compiled regex size in bytes.
    compiled_size_limit: usize,
    /// Maximum regex parser nesting depth.
    nesting_limit: u32,
    /// Maximum input haystack size in bytes.
    input_limit: usize,
    /// Maximum replacement output size in bytes.
    output_limit: usize,
}

impl PatternConfig {
    /// Construct one complete set of compile, input, and replacement limits.
    #[must_use]
    pub fn with_limits(
        pattern_limit: usize,
        compiled_size_limit: usize,
        nesting_limit: u32,
        input_limit: usize,
        output_limit: usize,
    ) -> Self {
        Self {
            pattern_limit,
            compiled_size_limit,
            nesting_limit,
            input_limit,
            output_limit,
        }
    }
}

impl Default for PatternConfig {
    fn default() -> Self {
        Self {
            pattern_limit: 64 * 1024,
            compiled_size_limit: 10 * 1024 * 1024,
            nesting_limit: 250,
            input_limit: 1024 * 1024,
            output_limit: 4 * 1024 * 1024,
        }
    }
}

/// Typed refusal from a bounded matching or replacement operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PatternRunError {
    /// Input exceeded its configured byte ceiling.
    InputTooLarge {
        /// Configured input ceiling in bytes.
        limit: usize,
        /// Actual input size in bytes.
        actual: usize,
    },
    /// Replacement output would exceed its byte ceiling.
    OutputTooLarge {
        /// Configured replacement ceiling in bytes.
        limit: usize,
        /// Size that the next append would produce.
        attempted: usize,
    },
    /// The allocator refused an output reservation.
    AllocationFailed,
    /// The engine violated its group-zero capture invariant.
    MissingWholeMatch,
}

impl core::fmt::Display for PatternRunError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::InputTooLarge { limit, actual } => {
                write!(f, "input is {actual} bytes; limit is {limit}")
            }
            Self::OutputTooLarge { limit, attempted } => write!(
                f,
                "replacement would produce {attempted} bytes; limit is {limit}"
            ),
            Self::AllocationFailed => f.write_str("replacement output allocation failed"),
            Self::MissingWholeMatch => {
                f.write_str("regex engine returned captures without group zero")
            }
        }
    }
}
impl std::error::Error for PatternRunError {}

impl core::fmt::Display for PatternError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The pattern is caller-supplied and `message` quotes the offending
        // part of it, so both are escaped: a pattern containing CR/LF must not
        // forge a second line in whatever log interpolates this error.
        write!(
            f,
            "invalid pattern `{}`: {}",
            self.pattern.escape_debug(),
            self.message.escape_debug()
        )
    }
}

impl std::error::Error for PatternError {}

impl Regex {
    /// Compile a pattern. Returns an error if the syntax is invalid.
    pub fn new(pattern: &str) -> Result<Self, PatternError> {
        regex::Regex::new(pattern)
            .map(Self)
            .map_err(|err| pattern_error(pattern, err))
    }

    /// Reports whether the pattern matches anywhere in `text`.
    ///
    /// This single search has worst-case `O(m * n)` cost.
    #[must_use]
    pub fn is_match(&self, text: &str) -> bool {
        self.0.is_match(text)
    }

    /// Returns the first match in `text`, or `None`.
    ///
    /// This single search has worst-case `O(m * n)` cost.
    /// Offsets are byte offsets into `text`; the borrowed slice is the matched
    /// text itself, so a caller that only needs the span still pays nothing for
    /// the `Match`.
    #[must_use]
    pub fn find<'t>(&self, text: &'t str) -> Option<Match<'t>> {
        self.0.find(text).map(|matched| Match {
            text: matched.as_str(),
            start: matched.start(),
            end: matched.end(),
        })
    }

    /// Returns all non-overlapping matches in `text`, lazily.
    ///
    /// Iterates rather than collecting: a caller that wants the count, the
    /// first match, or a short-circuiting predicate should not have to allocate
    /// one `Match` per occurrence in the haystack to get it, and an iterator
    /// can be collected at the call site when a `Vec` is what is wanted.
    ///
    /// No `#[must_use]` is written here: an iterator already carries the
    /// attribute, so a second one would only satisfy the eye.
    pub fn find_all<'t>(&self, text: &'t str) -> impl Iterator<Item = Match<'t>> {
        self.0.find_iter(text).map(|matched| {
            #[cfg(test)]
            count_produced_match();
            convert_match(matched)
        })
    }

    /// Returns the capture groups for the first match, or `None`.
    ///
    /// The search has worst-case `O(m * n)` cost.
    /// Index 0 is the whole match. An unmatched group is `None`; a group that
    /// participated and matched empty is `Some("").`
    #[must_use]
    pub fn captures<'t>(&self, text: &'t str) -> Option<Vec<Option<&'t str>>> {
        self.0.captures(text).map(|caps| {
            (0..caps.len())
                .map(|i| caps.get(i).map(|matched| matched.as_str()))
                .collect()
        })
    }

    /// Replace the first match with `replacement`.
    ///
    /// Matching has worst-case `O(m * n)` cost, plus output construction.
    #[must_use]
    pub fn replace(&self, text: &str, replacement: &str) -> String {
        self.0.replace(text, replacement).into_owned()
    }

    /// Replace the first match, borrowing `text` when there is no match.
    #[must_use]
    pub fn replace_borrowed<'t>(&self, text: &'t str, replacement: &str) -> Cow<'t, str> {
        self.0.replace(text, replacement)
    }

    /// The text with every non-overlapping match replaced by `replacement`;
    /// `$name` and `${name}` in the replacement expand to capture groups, and
    /// an empty match at the cursor advances rather than looping.
    ///
    /// Complete matching has worst-case `O(m * n^2)` cost, plus output construction.
    #[must_use]
    pub fn replace_all(&self, text: &str, replacement: &str) -> String {
        self.0.replace_all(text, replacement).into_owned()
    }

    /// Replace all matches, borrowing `text` when there is no match.
    #[must_use]
    pub fn replace_all_borrowed<'t>(&self, text: &'t str, replacement: &str) -> Cow<'t, str> {
        self.0.replace_all(text, replacement)
    }

    /// Split `text` by occurrences of the pattern, lazily.
    ///
    /// The separators are removed and the borrowed pieces are the text between
    /// them; a pattern that can match empty splits between every character. The
    /// pieces are produced one at a time. A first greedy separator can still
    /// inspect the full suffix, and complete iteration can cost `O(m * n^2)`.
    ///
    /// No `#[must_use]` is written here: an iterator already carries the
    /// attribute, so a second one would only satisfy the eye.
    pub fn split<'t>(&self, text: &'t str) -> impl Iterator<Item = &'t str> {
        self.0.split(text)
    }
}

impl Regex {
    /// Compile with explicit compilation, input, and output ceilings.
    /// The returned [`BoundedRegex`] enforces those limits on every operation.
    pub fn with_config(pattern: &str, config: PatternConfig) -> Result<BoundedRegex, PatternError> {
        if pattern.len() > config.pattern_limit {
            return Err(PatternError {
                pattern: pattern.chars().take(64).collect(),
                message: format!(
                    "pattern is {} bytes; limit is {}",
                    pattern.len(),
                    config.pattern_limit
                ),
                kind: PatternErrorKind::PatternTooLarge {
                    limit: config.pattern_limit,
                    actual: pattern.len(),
                },
            });
        }
        let mut builder = regex::RegexBuilder::new(pattern);
        builder
            .size_limit(config.compiled_size_limit)
            .nest_limit(config.nesting_limit);
        builder
            .build()
            .map(|engine| BoundedRegex { engine, config })
            .map_err(|err| pattern_error(pattern, err))
    }
}

/// Preserve the engine refusal as a machine-readable facade class.
fn pattern_error(pattern: &str, source: regex::Error) -> PatternError {
    let kind = match source.clone() {
        regex::Error::Syntax(_) => PatternErrorKind::Syntax,
        regex::Error::CompiledTooBig(_) => PatternErrorKind::CompiledTooLarge,
        _ => PatternErrorKind::Other,
    };
    PatternError {
        pattern: pattern.to_owned(),
        message: source.to_string(),
        kind,
    }
}

impl core::fmt::Debug for Regex {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("Regex").field(&self.0.as_str()).finish()
    }
}

/// A compiled regex whose operations enforce [`PatternConfig`] limits.
pub struct BoundedRegex {
    /// Compiled engine instance.
    engine: regex::Regex,
    /// Immutable limits used for every operation.
    config: PatternConfig,
}

impl core::fmt::Debug for BoundedRegex {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BoundedRegex")
            .field("pattern", &self.engine.as_str())
            .field("config", &self.config)
            .finish()
    }
}

impl BoundedRegex {
    /// Reject input before handing it to the regex engine.
    fn check_input(&self, text: &str) -> Result<(), PatternRunError> {
        if text.len() > self.config.input_limit {
            return Err(PatternRunError::InputTooLarge {
                limit: self.config.input_limit,
                actual: text.len(),
            });
        }
        Ok(())
    }

    /// Reports whether the pattern matches a permitted input.
    pub fn is_match(&self, text: &str) -> Result<bool, PatternRunError> {
        self.check_input(text)?;
        Ok(self.engine.is_match(text))
    }

    /// Returns the first match in a permitted input, or `None`.
    pub fn find<'t>(&self, text: &'t str) -> Result<Option<Match<'t>>, PatternRunError> {
        self.check_input(text)?;
        Ok(self.engine.find(text).map(convert_match))
    }

    /// Returns a lazy iterator after checking the complete input byte length.
    pub fn find_all<'r, 't>(
        &'r self,
        text: &'t str,
    ) -> Result<impl Iterator<Item = Match<'t>> + 'r, PatternRunError>
    where
        't: 'r,
    {
        self.check_input(text)?;
        Ok(self.engine.find_iter(text).map(|matched| {
            #[cfg(test)]
            count_produced_match();
            convert_match(matched)
        }))
    }

    /// Returns captures for the first match in a permitted input.
    pub fn captures<'t>(
        &self,
        text: &'t str,
    ) -> Result<Option<Vec<Option<&'t str>>>, PatternRunError> {
        self.check_input(text)?;
        Ok(self.engine.captures(text).map(|caps| {
            (0..caps.len())
                .map(|index| caps.get(index).map(|matched| matched.as_str()))
                .collect()
        }))
    }

    /// Splits a permitted input lazily.
    pub fn split<'r, 't>(
        &'r self,
        text: &'t str,
    ) -> Result<impl Iterator<Item = &'t str> + 'r, PatternRunError>
    where
        't: 'r,
    {
        self.check_input(text)?;
        Ok(self.engine.split(text))
    }

    /// Replaces the first match within the configured output ceiling.
    pub fn replace<'t>(
        &self,
        text: &'t str,
        replacement: &str,
    ) -> Result<Cow<'t, str>, PatternRunError> {
        self.replace_matches(text, replacement, 1)
    }

    /// Replaces every match within the configured output ceiling.
    pub fn replace_all<'t>(
        &self,
        text: &'t str,
        replacement: &str,
    ) -> Result<Cow<'t, str>, PatternRunError> {
        self.replace_matches(text, replacement, 0)
    }

    /// Build a complete replacement or return an error without exposing a prefix.
    fn replace_matches<'t>(
        &self,
        text: &'t str,
        replacement: &str,
        count: usize,
    ) -> Result<Cow<'t, str>, PatternRunError> {
        self.check_input(text)?;
        let mut captures = self.engine.captures_iter(text);
        let Some(first) = captures.next() else {
            if text.len() > self.config.output_limit {
                return Err(PatternRunError::OutputTooLarge {
                    limit: self.config.output_limit,
                    attempted: text.len(),
                });
            }
            return Ok(Cow::Borrowed(text));
        };
        let mut output = BoundedString::new(self.config.output_limit);
        let mut cursor = 0;
        let mut remaining = count;
        let mut current = Some(first);
        while let Some(groups) = current {
            let whole = groups.get(0).ok_or(PatternRunError::MissingWholeMatch)?;
            output.push_str(&text[cursor..whole.start()])?;
            expand_replacement(&groups, replacement, &mut output)?;
            cursor = whole.end();
            if count != 0 {
                remaining = remaining.saturating_sub(1);
                if remaining == 0 {
                    break;
                }
            }
            current = captures.next();
        }
        output.push_str(&text[cursor..])?;
        Ok(Cow::Owned(output.value))
    }
}

/// Convert the engine's byte-span match into the facade result.
fn convert_match<'t>(matched: regex::Match<'t>) -> Match<'t> {
    Match {
        text: matched.as_str(),
        start: matched.start(),
        end: matched.end(),
    }
}

#[cfg(test)]
thread_local! {
    static PRODUCED_MATCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count_produced_match() {
    PRODUCED_MATCHES.with(|count| count.set(count.get().saturating_add(1)));
}

/// String builder that checks its byte ceiling before reserving capacity.
struct BoundedString {
    /// Complete output accumulated so far.
    value: String,
    /// Maximum allowed output size in bytes.
    limit: usize,
}
impl BoundedString {
    /// Create an empty output buffer with a fixed byte ceiling.
    fn new(limit: usize) -> Self {
        Self {
            value: String::new(),
            limit,
        }
    }
    /// Append text only if the full append fits and allocation succeeds.
    fn push_str(&mut self, value: &str) -> Result<(), PatternRunError> {
        let attempted = match self.value.len().checked_add(value.len()) {
            Some(attempted) => attempted,
            None => usize::MAX,
        };
        if attempted > self.limit {
            return Err(PatternRunError::OutputTooLarge {
                limit: self.limit,
                attempted,
            });
        }
        self.value
            .try_reserve_exact(value.len())
            .map_err(|_| PatternRunError::AllocationFailed)?;
        self.value.push_str(value);
        Ok(())
    }
}

/// Expand regex replacement references directly into bounded output storage.
fn expand_replacement(
    captures: &regex::Captures<'_>,
    replacement: &str,
    output: &mut BoundedString,
) -> Result<(), PatternRunError> {
    let bytes = replacement.as_bytes();
    let mut cursor = 0;
    let mut literal_start = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'$' {
            cursor = cursor.saturating_add(1);
            continue;
        }
        let reference_start = cursor.saturating_add(1);
        if reference_start < bytes.len() && bytes[reference_start] == b'$' {
            output.push_str(&replacement[literal_start..cursor])?;
            output.push_str("$")?;
            cursor = cursor.saturating_add(2);
            literal_start = cursor;
            continue;
        }
        let (reference_end, name_start, braced) = if reference_start < bytes.len()
            && bytes[reference_start] == b'{'
        {
            let name_start = reference_start.saturating_add(1);
            match bytes[name_start..].iter().position(|byte| *byte == b'}') {
                Some(offset) => (name_start.saturating_add(offset), name_start, true),
                None => {
                    cursor = cursor.saturating_add(1);
                    continue;
                }
            }
        } else {
            let mut end = reference_start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end = end.saturating_add(1);
            }
            if end == reference_start {
                cursor = cursor.saturating_add(1);
                continue;
            }
            (end, reference_start, false)
        };
        output.push_str(&replacement[literal_start..cursor])?;
        let name = &replacement[name_start..reference_end];
        let matched = if !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit()) {
            name.parse::<usize>()
                .ok()
                .and_then(|index| captures.get(index))
        } else {
            captures.name(name)
        };
        if let Some(matched) = matched {
            output.push_str(matched.as_str())?;
        }
        cursor = if braced {
            reference_end.saturating_add(1)
        } else {
            reference_end
        };
        literal_start = cursor;
    }
    output.push_str(&replacement[literal_start..])
}

/// A single match result.
///
/// `text` is the matched slice of the haystack and the offsets are the byte
/// range it occupies, so `start` and `end` stay meaningful for a caller that
/// only kept the numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Match<'t> {
    /// The matched text.
    pub text: &'t str,
    /// Byte offset of the start.
    pub start: usize,
    /// Byte offset of the end (exclusive).
    pub end: usize,
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn config(input_limit: usize, output_limit: usize) -> PatternConfig {
        PatternConfig::with_limits(64 * 1024, 1_000_000, 250, input_limit, output_limit)
    }

    fn produced_matches() -> usize {
        PRODUCED_MATCHES.with(std::cell::Cell::get)
    }

    fn reset_produced_matches() {
        PRODUCED_MATCHES.with(|count| count.set(0));
    }

    /// Result and measured vector growth from the intentionally eager oracle.
    struct EagerResult<'t> {
        /// Every produced match.
        matches: Vec<Match<'t>>,
        /// Number of capacity-growth allocation events observed.
        growth_events: usize,
        /// Capacity bytes acquired across those growth events.
        capacity_bytes: usize,
    }

    fn eager_mutant<'t>(re: &BoundedRegex, text: &'t str) -> EagerResult<'t> {
        let mut matches = Vec::new();
        let mut growth_events = 0usize;
        let mut capacity_bytes = 0usize;
        for matched in re.engine.find_iter(text) {
            count_produced_match();
            let old_capacity = matches.capacity();
            matches.push(convert_match(matched));
            let new_capacity = matches.capacity();
            if new_capacity > old_capacity {
                growth_events = growth_events.saturating_add(1);
                capacity_bytes = capacity_bytes.saturating_add(
                    new_capacity
                        .saturating_sub(old_capacity)
                        .saturating_mul(std::mem::size_of::<Match<'t>>()),
                );
            }
        }
        EagerResult {
            matches,
            growth_events,
            capacity_bytes,
        }
    }

    #[test]
    fn producer_counter_detects_eager_collection_mutant() -> Result<(), Box<dyn std::error::Error>>
    {
        let re = Regex::with_config(r"\d", config(32, 32))?;
        reset_produced_matches();
        let first = re.find_all("1x2x3x4x5x6x7x8")?.take(1).next();
        assert_eq!(
            first.map(|item| (item.text, item.start, item.end)),
            Some(("1", 0, 1))
        );
        assert_eq!(produced_matches(), 1);

        reset_produced_matches();
        let eager = eager_mutant(&re, "1x2x3x4x5x6x7x8");
        assert_eq!(eager.matches.len(), 8);
        assert_eq!(produced_matches(), 8);
        assert!(eager.growth_events > 0);
        assert!(eager.capacity_bytes >= 8 * std::mem::size_of::<Match<'_>>());
        Ok(())
    }

    #[test]
    fn configured_limits_refuse_input_and_amplified_output()
    -> Result<(), Box<dyn std::error::Error>> {
        let bounded = Regex::with_config("a", config(2, 5))?;
        assert_eq!(
            bounded.is_match("aaa"),
            Err(PatternRunError::InputTooLarge {
                limit: 2,
                actual: 3
            })
        );
        let amplify = Regex::with_config("a", config(8, 5))?;
        assert_eq!(
            amplify.replace_all("aaa", "XX"),
            Err(PatternRunError::OutputTooLarge {
                limit: 5,
                attempted: 6
            })
        );
        Ok(())
    }

    #[test]
    fn replacement_borrows_no_match_and_preserves_capture_rules()
    -> Result<(), Box<dyn std::error::Error>> {
        let no_match = Regex::with_config("z", config(32, 32))?;
        let input = String::from("é-data");
        let output = no_match.replace_all(&input, "x")?;
        assert!(matches!(output, Cow::Borrowed(value) if value == input));
        let narrow_output = Regex::with_config("z", config(32, 3))?;
        assert_eq!(
            narrow_output.replace_all(&input, "x"),
            Err(PatternRunError::OutputTooLarge {
                limit: 3,
                attempted: input.len()
            })
        );

        let captures = Regex::with_config(r"(a)?()", config(32, 32))?
            .captures("b")?
            .ok_or("empty capture missing")?;
        assert_eq!(captures[1], None);
        assert_eq!(captures[2], Some(""));
        let expansion = Regex::with_config(r"(?<left>a)(?<right>b)", config(32, 32))?;
        assert_eq!(expansion.replace("ab", "${right}$left$$")?, "ba$");
        let unicode = Regex::with_config("é", config(32, 32))?
            .find("xéy")?
            .ok_or("match missing")?;
        assert_eq!((unicode.text, unicode.start, unicode.end), ("é", 1, 3));
        assert_eq!(
            Regex::with_config("", config(32, 32))?.replace_all("é", "-")?,
            "-é-"
        );
        Ok(())
    }

    #[test]
    fn greedy_adversary_and_literal_control_keep_exact_match_workloads()
    -> Result<(), Box<dyn std::error::Error>> {
        let adversary = Regex::with_config(r".*[^A-Z]|[A-Z]", config(16, 16))?;
        let spans = adversary
            .find_all("AAAA")?
            .map(|item| (item.start, item.end))
            .collect::<Vec<_>>();
        assert_eq!(spans, vec![(0, 1), (1, 2), (2, 3), (3, 4)]);
        assert_eq!(adversary.replace_all("AAAA", "X")?, "XXXX");
        assert_eq!(
            adversary.split("AAAA")?.collect::<Vec<_>>(),
            vec!["", "", "", "", ""]
        );

        let literal = Regex::with_config("A", config(16, 16))?;
        let literal_spans = literal
            .find_all("AAAA")?
            .map(|item| (item.start, item.end))
            .collect::<Vec<_>>();
        assert_eq!(literal_spans, vec![(0, 1), (1, 2), (2, 3), (3, 4)]);
        Ok(())
    }

    #[test]
    fn configured_pattern_compile_size_and_nesting_limits_are_enforced()
    -> Result<(), Box<dyn std::error::Error>> {
        let source_error =
            Regex::with_config("long", PatternConfig::with_limits(3, 1_000, 250, 16, 16))
                .err()
                .ok_or("source pattern limit was ignored")?;
        assert_eq!(
            source_error.kind(),
            PatternErrorKind::PatternTooLarge {
                limit: 3,
                actual: 4
            }
        );
        assert_eq!(source_error.pattern(), "long");
        let large_source = "x".repeat(1_000);
        let excerpt_error = Regex::with_config(
            &large_source,
            PatternConfig::with_limits(3, 1_000, 250, 16, 16),
        )
        .err()
        .ok_or("oversize source pattern was compiled")?;
        assert_eq!(
            excerpt_error.kind(),
            PatternErrorKind::PatternTooLarge {
                limit: 3,
                actual: 1_000
            }
        );
        assert!(excerpt_error.pattern().len() <= 64);

        let compile_error =
            Regex::with_config(r"\w", PatternConfig::with_limits(1024, 1, 250, 16, 16))
                .err()
                .ok_or("compiled-size limit was ignored")?;
        assert_eq!(compile_error.kind(), PatternErrorKind::CompiledTooLarge);

        let nesting_error =
            Regex::with_config("ab", PatternConfig::with_limits(1024, 10_000, 0, 16, 16))
                .err()
                .ok_or("nesting limit was ignored")?;
        assert_eq!(nesting_error.kind(), PatternErrorKind::Syntax);
        Ok(())
    }

    #[test]
    fn compiles_valid_pattern() {
        assert!(Regex::new(r"\d+").is_ok());
    }

    #[test]
    fn rejects_invalid_pattern() {
        assert!(Regex::new(r"[unclosed").is_err());
    }

    // A test that must observe a refusal returns `Result` and binds the
    // refusal explicitly, so a successful compile fails the test naming the
    // expectation it broke instead of panicking inside an `unwrap_err`.
    #[test]
    fn error_display_escapes_control_characters() -> Result<(), Box<dyn std::error::Error>> {
        // An invalid pattern containing a newline must render as an escaped
        // `\n`, not a raw byte that forges a second line in a caller's log.
        let Err(error) = Regex::new("(\n") else {
            return Err("a pattern with an unclosed group must not compile".into());
        };
        let rendered = error.to_string();
        assert!(
            !rendered.contains('\n'),
            "raw newline survived: {rendered:?}"
        );
        assert!(rendered.contains("invalid pattern"));
        let valid_control_pattern = Regex::with_config("[\n]", PatternConfig::default())?;
        let debug = format!("{valid_control_pattern:?}");
        assert!(!debug.contains('\n'));
        assert!(debug.contains("\\n"));
        Ok(())
    }

    #[test]
    fn is_match_finds_substring() -> Result<(), PatternError> {
        let re = Regex::new(r"\d+")?;
        assert!(re.is_match("abc123def"));
        assert!(!re.is_match("abcdef"));
        Ok(())
    }

    #[test]
    fn find_returns_first_match() -> Result<(), Box<dyn std::error::Error>> {
        let re = Regex::new(r"\d+")?;
        let matched = re
            .find("abc123def456")
            .ok_or("the digit pattern must match abc123def456")?;
        assert_eq!(matched.text, "123");
        assert_eq!(matched.start, 3);
        assert_eq!(matched.end, 6);
        Ok(())
    }

    #[test]
    fn find_all_returns_every_match() -> Result<(), PatternError> {
        let re = Regex::new(r"\d+")?;
        let matches: Vec<Match<'_>> = re.find_all("a1b22c333").collect();
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].text, "1");
        assert_eq!(matches[1].text, "22");
        assert_eq!(matches[2].text, "333");
        Ok(())
    }

    /// The iterator is lazy, so stopping early leaves the rest of the haystack
    /// unvisited. Liveness is observed through a counter the closure
    /// increments: `.take(1)` must yield one match and the predicate must run
    /// for one match only, which a collected `Vec` could not have shown.
    #[test]
    fn find_all_stops_where_the_caller_stops() -> Result<(), PatternError> {
        let re = Regex::new(r"\d+")?;
        let mut seen = 0usize;
        let first = re
            .find_all("a1b22c333d4444")
            .inspect(|_| seen = seen.saturating_add(1))
            .take(1)
            .next();
        assert_eq!(first.map(|matched| matched.text), Some("1"));
        assert_eq!(seen, 1);
        Ok(())
    }

    #[test]
    fn captures_extracts_groups() -> Result<(), Box<dyn std::error::Error>> {
        let re = Regex::new(r"(\w+)@(\w+)\.(\w+)")?;
        let captures = re
            .captures("user@host.com")
            .ok_or("the three-group pattern must match user@host.com")?;
        assert_eq!(captures[1], Some("user"));
        assert_eq!(captures[2], Some("host"));
        assert_eq!(captures[3], Some("com"));
        Ok(())
    }

    #[test]
    fn replace_substitutes_first() -> Result<(), PatternError> {
        let re = Regex::new(r"\d+")?;
        assert_eq!(re.replace("a1b2c3", "X"), "aXb2c3");
        Ok(())
    }

    #[test]
    fn replace_all_substitutes_every_match() -> Result<(), PatternError> {
        let re = Regex::new(r"\d+")?;
        assert_eq!(re.replace_all("a1b2c3", "X"), "aXbXcX");
        Ok(())
    }

    #[test]
    fn split_divides_on_pattern() -> Result<(), PatternError> {
        let re = Regex::new(r"[,;]\s*")?;
        assert_eq!(
            re.split("a, b; c,d").collect::<Vec<&str>>(),
            vec!["a", "b", "c", "d"]
        );
        Ok(())
    }

    #[test]
    fn error_includes_pattern_text() -> Result<(), Box<dyn std::error::Error>> {
        let Err(error) = Regex::new(r"(unclosed") else {
            return Err("a pattern with an unclosed group must not compile".into());
        };
        assert!(error.pattern().contains("unclosed"));
        assert!(!error.message().is_empty());
        Ok(())
    }
}
