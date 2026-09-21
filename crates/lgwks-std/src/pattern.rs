//! `pattern` owns compiled regular expression matching and enforces
//! INV-PATTERN-SAFE: patterns compile once, match in linear time, and never
//! panic on untrusted input. Wraps the `regex` crate behind a narrow API so
//! consumers have one import path for regex operations.

/// A compiled regular expression.
///
/// Wraps `regex::Regex` with a smaller surface: match, find, captures, replace.
/// Construction validates the pattern; matching is guaranteed linear-time.
pub struct Regex(regex::Regex);

/// Error returned when a pattern fails to compile.
///
/// The two fields are the caller's pattern and the regex engine's own
/// description of what it rejected. `#[non_exhaustive]` lets a later revision
/// carry a structured span or a category without breaking a caller that
/// destructures the struct.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PatternError {
    /// The pattern that failed.
    pub pattern: String,
    /// The underlying error message.
    pub message: String,
}

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
            .map_err(|err| PatternError {
                pattern: pattern.to_owned(),
                message: err.to_string(),
            })
    }

    /// Reports whether the pattern matches anywhere in `text`.
    #[must_use]
    pub fn is_match(&self, text: &str) -> bool {
        self.0.is_match(text)
    }

    /// Returns the first match in `text`, or `None`.
    ///
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

    /// Returns all non-overlapping matches in `text`.
    #[must_use]
    pub fn find_all<'t>(&self, text: &'t str) -> Vec<Match<'t>> {
        self.0
            .find_iter(text)
            .map(|matched| Match {
                text: matched.as_str(),
                start: matched.start(),
                end: matched.end(),
            })
            .collect()
    }

    /// Returns the capture groups for the first match, or `None`.
    ///
    /// Index 0 is the whole match. A group that did not participate in the
    /// match is `Some("")` when it matched empty and `None` when it did not
    /// participate; the length is always the pattern's group count plus one.
    #[must_use]
    pub fn captures<'t>(&self, text: &'t str) -> Option<Vec<Option<&'t str>>> {
        self.0.captures(text).map(|caps| {
            (0..caps.len())
                .map(|i| caps.get(i).map(|matched| matched.as_str()))
                .collect()
        })
    }

    /// Replace the first match with `replacement`.
    #[must_use]
    pub fn replace(&self, text: &str, replacement: &str) -> String {
        self.0.replace(text, replacement).into_owned()
    }

    /// The text with every non-overlapping match replaced by `replacement`;
    /// `$name` and `${name}` in the replacement expand to capture groups, and
    /// an empty match at the cursor advances rather than looping.
    #[must_use]
    pub fn replace_all(&self, text: &str, replacement: &str) -> String {
        self.0.replace_all(text, replacement).into_owned()
    }

    /// Split `text` by occurrences of the pattern.
    ///
    /// The separators are removed and the borrowed pieces are the text between
    /// them; a pattern that can match empty splits between every character.
    #[must_use]
    pub fn split<'t>(&self, text: &'t str) -> Vec<&'t str> {
        self.0.split(text).collect()
    }
}

impl core::fmt::Debug for Regex {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Regex({})", self.0.as_str())
    }
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
        let matches = re.find_all("a1b22c333");
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].text, "1");
        assert_eq!(matches[1].text, "22");
        assert_eq!(matches[2].text, "333");
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
        assert_eq!(re.split("a, b; c,d"), vec!["a", "b", "c", "d"]);
        Ok(())
    }

    #[test]
    fn error_includes_pattern_text() -> Result<(), Box<dyn std::error::Error>> {
        let Err(error) = Regex::new(r"(unclosed") else {
            return Err("a pattern with an unclosed group must not compile".into());
        };
        assert!(error.pattern.contains("unclosed"));
        assert!(!error.message.is_empty());
        Ok(())
    }
}
