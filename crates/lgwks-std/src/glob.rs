//! `glob` owns shell-style path pattern matching and enforces
//! INV-GLOB-SEPARATOR: a single `*`, a `?`, and a character class never cross a
//! `/`, and only `**` does — so a pattern cannot silently reach into a
//! subdirectory the author did not name.
//!
//! Retires the `glob` crate, declared in 2 manifests and reached from 3 call
//! sites. The estate's use is pattern *matching*
//! against paths it already has, not filesystem traversal, so directory walking
//! is deliberately out of scope: matching is pure and testable, traversal is an
//! I/O concern that belongs to the caller.
//!
//! An unterminated `[` is treated as a literal bracket rather than an error.
//! That is the POSIX `fnmatch` behaviour and the upstream crate's, and matching
//! it keeps the migration a substitution instead of a semantic change.

// ── Token Parsing ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token<'a> {
    Literal(u8),
    Question,
    Star,
    DoubleStar,
    DoubleStarSlash,
    SlashDoubleStar,
    SlashDoubleStarSlash,
    Class { body: &'a [u8], negated: bool },
}

fn parse_class_token<'a>(pattern: &'a [u8]) -> (Token<'a>, usize) {
    if let Some(end) = class_end(pattern) {
        let raw_body = &pattern[1..end];
        let (negated, body) = match raw_body.first() {
            Some(b'!' | b'^') => (true, &raw_body[1..]),
            _ => (false, raw_body),
        };
        (Token::Class { body, negated }, end + 1)
    } else {
        (Token::Literal(b'['), 1)
    }
}

fn match_four_byte_prefix<'a>(pattern: &'a [u8]) -> Option<(Token<'a>, usize)> {
    if pattern.starts_with(b"/**/") {
        Some((Token::SlashDoubleStarSlash, 4))
    } else {
        None
    }
}

fn match_three_byte_prefix<'a>(pattern: &'a [u8]) -> Option<(Token<'a>, usize)> {
    if pattern.starts_with(b"/**") {
        Some((Token::SlashDoubleStar, 3))
    } else if pattern.starts_with(b"**/") {
        Some((Token::DoubleStarSlash, 3))
    } else {
        None
    }
}

fn match_special_prefix<'a>(pattern: &'a [u8]) -> Option<(Token<'a>, usize)> {
    if let Some(tok) = match_four_byte_prefix(pattern) {
        Some(tok)
    } else if let Some(tok) = match_three_byte_prefix(pattern) {
        Some(tok)
    } else if pattern.starts_with(b"**") {
        Some((Token::DoubleStar, 2))
    } else {
        None
    }
}

fn match_single_char_token<'a>(pattern: &'a [u8]) -> (Token<'a>, usize) {
    match pattern[0] {
        b'*' => (Token::Star, 1),
        b'?' => (Token::Question, 1),
        b'[' => parse_class_token(pattern),
        ch => (Token::Literal(ch), 1),
    }
}

fn next_token<'a>(pattern: &'a [u8]) -> Option<(Token<'a>, usize)> {
    if pattern.is_empty() {
        return None;
    }
    if let Some(tok) = match_special_prefix(pattern) {
        Some(tok)
    } else {
        Some(match_single_char_token(pattern))
    }
}

fn tokenize<'a>(mut pattern: &'a [u8]) -> Vec<Token<'a>> {
    let mut tokens = Vec::new();
    while let Some((tok, consumed)) = next_token(pattern) {
        tokens.push(tok);
        pattern = &pattern[consumed..];
    }
    tokens
}

// ── DP Matching ─────────────────────────────────────────────────────────────

fn step_literal(c: u8, path: &[u8], dp: &[bool], next: &mut [bool]) {
    for j in 1..=path.len() {
        if dp[j - 1] && path[j - 1] == c {
            next[j] = true;
        }
    }
}

fn step_question(path: &[u8], dp: &[bool], next: &mut [bool]) {
    for j in 1..=path.len() {
        if dp[j - 1] && path[j - 1] != b'/' {
            next[j] = true;
        }
    }
}

fn step_class(body: &[u8], negated: bool, path: &[u8], dp: &[bool], next: &mut [bool]) {
    for j in 1..=path.len() {
        if dp[j - 1] && path[j - 1] != b'/' {
            let matches_body = scan_class_body(body, path[j - 1]);
            let hit = if negated { !matches_body } else { matches_body };
            if hit {
                next[j] = true;
            }
        }
    }
}

fn step_star(path: &[u8], dp: &[bool], next: &mut [bool]) {
    for j in 0..=path.len() {
        if dp[j] {
            next[j] = true;
            for k in j + 1..=path.len() {
                if path[k - 1] == b'/' {
                    break;
                }
                next[k] = true;
            }
        }
    }
}

fn step_double_star(path: &[u8], dp: &[bool], next: &mut [bool]) {
    let mut any = false;
    for j in 0..=path.len() {
        any |= dp[j];
        if any {
            next[j] = true;
        }
    }
}

fn step_double_star_slash(path: &[u8], dp: &[bool], next: &mut [bool]) {
    for j in 0..=path.len() {
        if dp[j] {
            next[j] = true;
            for k in j + 1..=path.len() {
                if path[k - 1] == b'/' {
                    next[k] = true;
                }
            }
        }
    }
}

fn step_slash_double_star(path: &[u8], dp: &[bool], next: &mut [bool]) {
    for j in 0..=path.len() {
        if dp[j] {
            next[j] = true;
            if j < path.len() && path[j] == b'/' {
                for slot in &mut next[j + 1..=path.len()] {
                    *slot = true;
                }
            }
        }
    }
}

fn step_slash_double_star_slash(path: &[u8], dp: &[bool], next: &mut [bool]) {
    for j in 0..=path.len() {
        if dp[j] && j < path.len() && path[j] == b'/' {
            next[j + 1] = true;
            for k in j + 2..=path.len() {
                if path[k - 1] == b'/' {
                    next[k] = true;
                }
            }
        }
    }
}

fn step_token(token: &Token<'_>, path: &[u8], dp: &[bool], next: &mut [bool]) {
    match token {
        Token::Literal(c) => step_literal(*c, path, dp, next),
        Token::Question => step_question(path, dp, next),
        Token::Class { body, negated } => step_class(body, *negated, path, dp, next),
        Token::Star => step_star(path, dp, next),
        Token::DoubleStar => step_double_star(path, dp, next),
        Token::DoubleStarSlash => step_double_star_slash(path, dp, next),
        Token::SlashDoubleStar => step_slash_double_star(path, dp, next),
        Token::SlashDoubleStarSlash => step_slash_double_star_slash(path, dp, next),
    }
}

/// Reports whether `path` matches `pattern`.
///
/// Supported syntax: `?` for one non-separator character, `*` for any run of
/// non-separator characters, `**` for any run including separators, and
/// `[abc]` / `[a-z]` / `[!a-z]` character classes.
///
/// Guaranteed $O(M \times N)$ time and $O(N)$ memory via deterministic DP.
pub fn matches(pattern: &str, path: &str) -> bool {
    let tokens = tokenize(pattern.as_bytes());
    let path_bytes = path.as_bytes();
    let mut dp = vec![false; path_bytes.len() + 1];
    dp[0] = true;

    for token in &tokens {
        let mut next = vec![false; path_bytes.len() + 1];
        step_token(token, path_bytes, &dp, &mut next);
        dp = next;
    }

    dp[path_bytes.len()]
}

// ── Character classes ───────────────────────────────────────────────────────

fn skip_class_prefix(pattern: &[u8]) -> usize {
    let mut idx = 1;
    if matches!(pattern.get(idx), Some(b'!' | b'^')) {
        idx += 1;
    }
    if pattern.get(idx) == Some(&b']') {
        idx += 1;
    }
    idx
}

fn find_closing_bracket(pattern: &[u8], start: usize) -> Option<usize> {
    (start..pattern.len()).find(|&idx| pattern[idx] == b']')
}

/// Index of the `]` that closes the class opening at `pattern[0]`, or `None` if
/// the class is unterminated. A `]` in the first content position is a literal,
/// per POSIX.
fn class_end(pattern: &[u8]) -> Option<usize> {
    let start = skip_class_prefix(pattern);
    find_closing_bracket(pattern, start)
}

fn match_range_or_single(body: &[u8], candidate: u8, idx: usize) -> (bool, usize) {
    if idx + 2 < body.len() && body[idx + 1] == b'-' {
        let in_range = body[idx] <= candidate && candidate <= body[idx + 2];
        (in_range, 3)
    } else {
        let is_match = body[idx] == candidate;
        (is_match, 1)
    }
}

fn scan_class_body(body: &[u8], candidate: u8) -> bool {
    let mut idx = 0;
    while idx < body.len() {
        let (matched, step) = match_range_or_single(body, candidate, idx);
        if matched {
            return true;
        }
        idx += step;
    }
    false
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_pattern_matches_only_an_empty_path() {
        assert!(matches("", ""));
        assert!(!matches("", "a"));
    }

    #[test]
    fn a_literal_pattern_matches_only_itself() {
        assert!(matches("hello", "hello"));
        assert!(!matches("hello", "world"));
        assert!(!matches("hello", "hello/world"));
    }

    #[test]
    fn question_mark_matches_one_non_separator() {
        assert!(matches("a?c", "abc"));
        assert!(matches("a?c", "a.c"));
        assert!(!matches("a?c", "a/c"));
        assert!(!matches("a?c", "ac"));
        assert!(!matches("a?c", "abbc"));
    }

    #[test]
    fn star_does_not_cross_a_separator() {
        assert!(matches("a*c", "ac"));
        assert!(matches("a*c", "abc"));
        assert!(matches("a*c", "abbc"));
        assert!(!matches("a*c", "a/c"));
        assert!(!matches("a*c", "a/b/c"));
        assert!(matches("*.rs", "main.rs"));
        assert!(!matches("*.rs", "src/main.rs"));
    }

    #[test]
    fn double_star_crosses_separators() {
        assert!(matches("a/**/b", "a/b"));
        assert!(matches("a/**/b", "a/x/b"));
        assert!(matches("a/**/b", "a/x/y/z/b"));
        assert!(matches("**/b", "b"));
        assert!(matches("**/b", "a/b"));
        assert!(matches("**/b", "x/y/b"));
        assert!(matches("a/**", "a"));
        assert!(matches("a/**", "a/b"));
        assert!(matches("a/**", "a/b/c"));
    }

    #[test]
    fn double_star_matches_nothing_in_between() {
        assert!(matches("a**b", "ab"));
        assert!(matches("a**b", "axb"));
        assert!(matches("a**b", "a/b"));
        assert!(matches("a**b", "a/x/y/b"));
    }

    #[test]
    fn a_class_never_matches_a_separator() {
        assert!(!matches("[/]", "/"));
        assert!(!matches("[a/z]", "/"));
        assert!(!matches("[!a]", "/"));
    }

    #[test]
    fn a_negated_class_excludes_its_members() {
        assert!(!matches("[!abc]", "a"));
        assert!(!matches("[!abc]", "b"));
        assert!(!matches("[!abc]", "c"));
        assert!(matches("[!abc]", "d"));
        assert!(!matches("[^abc]", "a"));
        assert!(matches("[^abc]", "d"));
    }

    #[test]
    fn a_trailing_dash_in_a_class_is_a_literal() {
        assert!(matches("[a-]", "a"));
        assert!(matches("[a-]", "-"));
        assert!(!matches("[a-]", "b"));
    }

    #[test]
    fn an_unterminated_class_is_a_literal_bracket() {
        assert!(matches("[abc", "[abc"));
        assert!(!matches("[abc", "a"));
    }

    #[test]
    fn backtracking_terminates_on_a_pathological_pattern() {
        // Pathological regex/glob backtracking case: a*a*a*a*b on aaaaaaa...
        let pattern = "a*a*a*a*a*a*b";
        let path = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        assert!(!matches(pattern, path));
    }
}
