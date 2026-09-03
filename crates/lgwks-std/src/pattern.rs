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
#[derive(Debug, Clone)]
pub struct PatternError {
    /// The pattern that failed.
    pub pattern: String,
    /// The underlying error message.
    pub message: String,
}

impl core::fmt::Display for PatternError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid pattern `{}`: {}", self.pattern, self.message)
    }
}

impl std::error::Error for PatternError {}

impl Regex {
    /// Compile a pattern. Returns an error if the syntax is invalid.
    pub fn new(pattern: &str) -> Result<Self, PatternError> {
        regex::Regex::new(pattern)
            .map(Self)
            .map_err(|e| PatternError {
                pattern: pattern.to_string(),
                message: e.to_string(),
            })
    }

    /// Reports whether the pattern matches anywhere in `text`.
    pub fn is_match(&self, text: &str) -> bool {
        self.0.is_match(text)
    }

    /// Returns the first match in `text`, or `None`.
    pub fn find<'t>(&self, text: &'t str) -> Option<Match<'t>> {
        self.0.find(text).map(|m| Match {
            text: m.as_str(),
            start: m.start(),
            end: m.end(),
        })
    }

    /// Returns all non-overlapping matches in `text`.
    pub fn find_all<'t>(&self, text: &'t str) -> Vec<Match<'t>> {
        self.0
            .find_iter(text)
            .map(|m| Match {
                text: m.as_str(),
                start: m.start(),
                end: m.end(),
            })
            .collect()
    }

    /// Returns the capture groups for the first match, or `None`.
    pub fn captures<'t>(&self, text: &'t str) -> Option<Vec<Option<&'t str>>> {
        self.0.captures(text).map(|caps| {
            (0..caps.len())
                .map(|i| caps.get(i).map(|m| m.as_str()))
                .collect()
        })
    }

    /// Replace the first match with `replacement`.
    pub fn replace(&self, text: &str, replacement: &str) -> String {
        self.0.replace(text, replacement).into_owned()
    }

    /// Replace all matches with `replacement`.
    pub fn replace_all(&self, text: &str, replacement: &str) -> String {
        self.0.replace_all(text, replacement).into_owned()
    }

    /// Split `text` by occurrences of the pattern.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    #[test]
    fn is_match_finds_substring() {
        let re = Regex::new(r"\d+").unwrap();
        assert!(re.is_match("abc123def"));
        assert!(!re.is_match("abcdef"));
    }

    #[test]
    fn find_returns_first_match() {
        let re = Regex::new(r"\d+").unwrap();
        let m = re.find("abc123def456").unwrap();
        assert_eq!(m.text, "123");
        assert_eq!(m.start, 3);
        assert_eq!(m.end, 6);
    }

    #[test]
    fn find_all_returns_every_match() {
        let re = Regex::new(r"\d+").unwrap();
        let matches = re.find_all("a1b22c333");
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].text, "1");
        assert_eq!(matches[1].text, "22");
        assert_eq!(matches[2].text, "333");
    }

    #[test]
    fn captures_extracts_groups() {
        let re = Regex::new(r"(\w+)@(\w+)\.(\w+)").unwrap();
        let caps = re.captures("user@host.com").unwrap();
        assert_eq!(caps[1], Some("user"));
        assert_eq!(caps[2], Some("host"));
        assert_eq!(caps[3], Some("com"));
    }

    #[test]
    fn replace_substitutes_first() {
        let re = Regex::new(r"\d+").unwrap();
        assert_eq!(re.replace("a1b2c3", "X"), "aXb2c3");
    }

    #[test]
    fn replace_all_substitutes_every_match() {
        let re = Regex::new(r"\d+").unwrap();
        assert_eq!(re.replace_all("a1b2c3", "X"), "aXbXcX");
    }

    #[test]
    fn split_divides_on_pattern() {
        let re = Regex::new(r"[,;]\s*").unwrap();
        assert_eq!(re.split("a, b; c,d"), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn error_includes_pattern_text() {
        let err = Regex::new(r"(unclosed").unwrap_err();
        assert!(err.pattern.contains("unclosed"));
        assert!(!err.message.is_empty());
    }
}
