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

/// One lexed element of a glob pattern.
///
/// Tokens borrow from the pattern bytes, so tokenizing allocates nothing per
/// token and the whole token list dies with the caller's `&str`. The separator
/// policy of INV-GLOB-SEPARATOR is encoded in *which* token a sequence becomes:
/// [`Token::Star`] and [`Token::Question`] refuse to cross `/`, and only the
/// four double-star forms may.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Token<'a> {
    /// A single byte that must equal the path byte at this position.
    Literal(u8),
    /// `?` — exactly one path byte, never `/`.
    Question,
    /// `*` — zero or more path bytes, stopping at any `/`.
    Star,
    /// `**` — zero or more path bytes, `/` included.
    DoubleStar,
    /// `**/` — a leading `**` that also consumes the `/` after it, so the
    /// enclosing directory may be omitted entirely.
    DoubleStarSlash,
    /// `/**` — a trailing `**` preceded by the `/` that introduces it.
    SlashDoubleStar,
    /// `/**/` — one or more path segments; the separators on both sides are
    /// part of the token, which is what lets `a/**/b` match `a/b`.
    SlashDoubleStarSlash,
    /// A `[...]` class, or a literal `[` when the class is unterminated.
    Class {
        /// The class contents between the brackets, with any leading `!`/`^`
        /// negation marker already stripped. Ranges (`a-z`) are kept verbatim
        /// and interpreted later by `match_range_or_single`.
        body: &'a [u8],
        /// Whether a member of `body` *fails* the match, as written `[!...]`
        /// or `[^...]`.
        negated: bool,
    },
}

/// Lexes a `[...]` class starting at `pattern[0]`.
///
/// Returns [`Token::Class`] and the number of bytes consumed, or a literal `[`
/// consuming one byte when no closing `]` exists. The literal-bracket fallback
/// is POSIX `fnmatch` behaviour and keeps an unterminated class from turning a
/// whole pattern into an error.
///
/// The consumed count is `end + 1` (the bracket plus its closing `]`); `end` is
/// a valid index below `pattern.len()`, so the addition cannot overflow, and
/// saturating expresses that bound without a panic path.
fn parse_class_token<'a>(pattern: &'a [u8]) -> (Token<'a>, usize) {
    if let Some(end) = class_end(pattern) {
        let raw_body = &pattern[1..end];
        // A leading `!` or `^` negates the class and is not part of its body.
        let (negated, body) = match raw_body.first() {
            Some(&(b'!' | b'^')) => (true, &raw_body[1..]),
            _ => (false, raw_body),
        };
        (Token::Class { body, negated }, end.saturating_add(1))
    } else {
        (Token::Literal(b'['), 1)
    }
}

/// Recognises the four-byte `/**/` token.
///
/// Checked before the three-byte forms because `/**/` shares its `/**` prefix
/// with [`Token::SlashDoubleStar`]; the longest form must win or `a/**/b` would
/// be lexed as `/**` followed by a literal `/`.
///
/// Returns the token and its length, or `None` when the pattern does not start
/// with `/**/`.
fn match_four_byte_prefix<'a>(pattern: &'a [u8]) -> Option<(Token<'a>, usize)> {
    if pattern.starts_with(b"/**/") {
        Some((Token::SlashDoubleStarSlash, 4))
    } else {
        None
    }
}

/// Recognises the three-byte `/**` and `**/` tokens.
///
/// Both are tried here because each needs the neighbour separator that a bare
/// `**` would otherwise consume, and the token list must carry that separator
/// as part of the token so the DP steps stay single-pass.
///
/// Returns the token and its length, or `None` when neither prefix is present.
fn match_three_byte_prefix<'a>(pattern: &'a [u8]) -> Option<(Token<'a>, usize)> {
    if pattern.starts_with(b"/**") {
        Some((Token::SlashDoubleStar, 3))
    } else if pattern.starts_with(b"**/") {
        Some((Token::DoubleStarSlash, 3))
    } else {
        None
    }
}

/// Recognises any multi-character `*`-based token at the head of a pattern.
///
/// Applies the prefix matchers longest-first (`/**/`, then `/**` or `**/`,
/// then a bare `**`) so no shorter token can shadow a longer one.
///
/// Returns the token and its length, or `None` when the head is not a
/// double-star form and the caller must fall back to single-character lexing.
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

/// Lexes the single-character token at `pattern[0]`.
///
/// `*` and `?` are one byte; `[` delegates to [`parse_class_token`] so a class
/// is consumed whole; every other byte — including `/`, which is an ordinary
/// literal here — becomes [`Token::Literal`].
///
/// # Panics
///
/// Panics if `pattern` is empty. Every caller checks for an empty slice or has
/// already consumed a token boundary, so `pattern[0]` always exists.
fn match_single_char_token<'a>(pattern: &'a [u8]) -> (Token<'a>, usize) {
    match pattern[0] {
        b'*' => (Token::Star, 1),
        b'?' => (Token::Question, 1),
        b'[' => parse_class_token(pattern),
        ch => (Token::Literal(ch), 1),
    }
}

/// Lexes the next token from the front of `pattern`.
///
/// Returns the token and the number of bytes it consumed, or `None` at the end
/// of the pattern. Multi-byte `*` forms are tried first so the longest match
/// always wins.
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

/// Lexes an entire pattern into a token list.
///
/// Consumes the pattern front to back, and advances by exactly the byte count
/// each token reports, so the list covers the pattern without gaps or overlap.
/// The returned tokens borrow from `pattern`; nothing is copied.
///
/// Terminates because every token consumes at least one byte.
fn tokenize<'a>(mut pattern: &'a [u8]) -> Vec<Token<'a>> {
    let mut tokens = Vec::new();
    while let Some((tok, consumed)) = next_token(pattern) {
        tokens.push(tok);
        pattern = &pattern[consumed..];
    }
    tokens
}

// ── DP Matching ─────────────────────────────────────────────────────────────

/// Advances the DP row for one [`Token::Literal`].
///
/// `dp` is the previous row: `dp[n]` means "the first `n` path bytes matched the
/// tokens consumed so far". A literal only extends a match whose next byte
/// equals it exactly, and — because it is a literal — it may be a `/`.
///
/// `next` must be at least as long as `dp` and indexed by the same offsets; it
/// is only ever written, never read, so a caller may pass a fresh `false` row.
/// Both `j - 1` subtractions are guarded by the loop starting at 1, so they are
/// saturating only to keep the arithmetic total, not to correct an underflow.
fn step_literal(literal: u8, path: &[u8], dp: &[bool], next: &mut [bool]) {
    for j in 1..=path.len() {
        if dp[j.saturating_sub(1)] && path[j.saturating_sub(1)] == literal {
            next[j] = true;
        }
    }
}

/// Advances the DP row for one [`Token::Question`].
///
/// `?` matches exactly one path byte, and never `/` — that refusal is what
/// INV-GLOB-SEPARATOR requires of a single wildcard, so a `?` cannot silently
/// step into a subdirectory. See [`step_literal`] for the row convention.
fn step_question(path: &[u8], dp: &[bool], next: &mut [bool]) {
    for j in 1..=path.len() {
        if dp[j.saturating_sub(1)] && path[j.saturating_sub(1)] != b'/' {
            next[j] = true;
        }
    }
}

/// Advances the DP row for one [`Token::Class`].
///
/// A class consumes exactly one non-separator byte, like [`Token::Question`],
/// and then asks `scan_class_body` whether that byte is a member. `negated`
/// inverts the membership test, so `[!a-z]` accepts anything outside the range
/// except `/`. See [`step_literal`] for the row convention.
fn step_class(body: &[u8], negated: bool, path: &[u8], dp: &[bool], next: &mut [bool]) {
    for j in 1..=path.len() {
        if dp[j.saturating_sub(1)] && path[j.saturating_sub(1)] != b'/' {
            let matches_body = scan_class_body(body, path[j.saturating_sub(1)]);
            let hit = if negated { !matches_body } else { matches_body };
            if hit {
                next[j] = true;
            }
        }
    }
}

/// Advances the DP row for one [`Token::Star`].
///
/// `*` matches a run of zero or more bytes but stops at the first `/`, so from
/// each reachable offset the star can carry the match forward only until the
/// next separator. The inner loop breaks at that separator, leaving every byte
/// beyond it unreachable — which is exactly the separator invariant.
///
/// `for reach in j + 1..=path.len()`: `j` is at most `path.len()`, so the start
/// stays within the length-plus-one indexing range of `next` and cannot
/// overflow.
fn step_star(path: &[u8], dp: &[bool], next: &mut [bool]) {
    for j in 0..=path.len() {
        if dp[j] {
            next[j] = true;
            for reach in j.saturating_add(1)..=path.len() {
                if path[reach.saturating_sub(1)] == b'/' {
                    break;
                }
                next[reach] = true;
            }
        }
    }
}

/// Advances the DP row for one [`Token::DoubleStar`].
///
/// A bare `**` matches any run of bytes including separators, so once any offset
/// is reachable every later offset is reachable too. The scan therefore carries
/// a single `any` flag forward instead of looping per offset, which keeps the
/// step linear in the path length.
fn step_double_star(path: &[u8], dp: &[bool], next: &mut [bool]) {
    let mut any = false;
    for j in 0..=path.len() {
        any |= dp[j];
        if any {
            next[j] = true;
        }
    }
}

/// Advances the DP row for one [`Token::DoubleStarSlash`] (`**/`).
///
/// The token owns the `/` that follows the `**`, so from each reachable offset
/// it can resume at that offset (covering zero directories) or at any offset
/// after a separator. Unlike [`Token::Star`] the inner scan does not break on
/// `/` — `**/` is allowed to cross separators, which is what makes `**/b` match
/// `x/y/b`.
///
/// `for reach in j + 1..=path.len()` is bounded by the path length, so it
/// cannot overflow and always indexes within `next`.
fn step_double_star_slash(path: &[u8], dp: &[bool], next: &mut [bool]) {
    for j in 0..=path.len() {
        if dp[j] {
            next[j] = true;
            for reach in j.saturating_add(1)..=path.len() {
                if path[reach.saturating_sub(1)] == b'/' {
                    next[reach] = true;
                }
            }
        }
    }
}

/// Advances the DP row for one [`Token::SlashDoubleStar`] (`/**`).
///
/// The leading `/` is part of the token, so this step is a *literal* separator:
/// it extends only to immediately after a `/`, and from there the trailing `**`
/// accepts everything to the end of the path. That is why `a/**` matches `a`
/// (zero trailing segments) as well as `a/b/c`.
///
/// `next[j + 1..=path.len()]` is guarded by `j < path.len()`, so the range is
/// non-empty and both bounds are within `next`.
fn step_slash_double_star(path: &[u8], dp: &[bool], next: &mut [bool]) {
    for j in 0..=path.len() {
        if dp[j] {
            next[j] = true;
            if j < path.len() && path[j] == b'/' {
                for slot in &mut next[j.saturating_add(1)..=path.len()] {
                    *slot = true;
                }
            }
        }
    }
}

/// Advances the DP row for one [`Token::SlashDoubleStarSlash`] (`/**/`).
///
/// Both separators belong to the token, so it requires at least one full
/// segment: a `/` must follow the reachable offset, and the match resumes
/// immediately after it. The trailing scan then allows any further bytes,
/// including more separators, so `a/**/b` matches `a/x/y/b`.
///
/// The `next[j + 1]` write is guarded by `j < path.len()`, and the loop
/// `reach in j + 2..=path.len()` is bounded by the path length, so neither index
/// can exceed the row.
fn step_slash_double_star_slash(path: &[u8], dp: &[bool], next: &mut [bool]) {
    for j in 0..=path.len() {
        if dp[j] && j < path.len() && path[j] == b'/' {
            next[j.saturating_add(1)] = true;
            for reach in j.saturating_add(2)..=path.len() {
                if path[reach.saturating_sub(1)] == b'/' {
                    next[reach] = true;
                }
            }
        }
    }
}

/// Dispatches one token to the step function that knows how to advance it.
///
/// Every token advances exactly one DP row, so the traversal in [`matches`]
/// stays a single pass over the token list regardless of which token is next.
/// Matching on `*token` reads the token by value: every field of [`Token`] is
/// `Copy`, so this copies a byte or a pair of small values and never moves
/// borrowed pattern data.
fn step_token(token: &Token<'_>, path: &[u8], dp: &[bool], next: &mut [bool]) {
    match *token {
        Token::Literal(literal) => step_literal(literal, path, dp, next),
        Token::Question => step_question(path, dp, next),
        Token::Class { body, negated } => step_class(body, negated, path, dp, next),
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
#[must_use]
pub fn matches(pattern: &str, path: &str) -> bool {
    let tokens = tokenize(pattern.as_bytes());
    let path_bytes = path.as_bytes();
    // Row `n` of the DP means "the first `n` path bytes are matched by the
    // tokens consumed so far", so the row needs one slot past the path length.
    // A path length is at most `isize::MAX`, which leaves room for the extra
    // slot; saturating keeps the expression total on a length that large.
    let row_len = path_bytes.len().saturating_add(1);
    let mut dp = vec![false; row_len];
    // The empty prefix always matches, before any token has been consumed.
    dp[0] = true;

    for token in &tokens {
        let mut next = vec![false; row_len];
        step_token(token, path_bytes, &dp, &mut next);
        dp = next;
    }

    dp[path_bytes.len()]
}

// ── Character classes ───────────────────────────────────────────────────────

/// Returns the offset of the first content byte of a class opened at
/// `pattern[0]`.
///
/// Skips the opening `[`, then an optional `!` or `^` negation marker, then a
/// leading `]` — which POSIX treats as a literal member rather than the class
/// terminator. That last case is what makes `[]]` a class containing `]`.
///
/// The increments are bounded by `pattern.len()`, so neither can overflow;
/// saturating_add keeps them total without a panic path.
fn skip_class_prefix(pattern: &[u8]) -> usize {
    let mut idx = 1;
    if matches!(pattern.get(idx), Some(b'!' | b'^')) {
        idx = idx.saturating_add(1);
    }
    if pattern.get(idx) == Some(&b']') {
        idx = idx.saturating_add(1);
    }
    idx
}

/// Finds the next `]` at or after `start`.
///
/// Returns its offset, or `None` when the pattern ends first. `start` is
/// clamped by the range expression itself, so an out-of-range start simply
/// yields `None` rather than panicking.
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

/// Tries to read a `a-z` range at `body[idx]`, falling back to a single member.
///
/// Returns whether the candidate was accepted, together with how many body
/// bytes the attempt consumed: three for a range (`lo`, `-`, `hi`) and one for
/// every other byte. Consuming one byte on a non-range is what lets a trailing
/// dash such as `[a-]` be read as the literal `-` on the next iteration.
///
/// A range is only recognised when `idx + 2` is a valid body index, which is
/// exactly the guard below; saturating keeps that guard total, and because a
/// saturated offset always fails the comparison it degrades to the single-byte
/// branch rather than misreading a range.
fn match_range_or_single(body: &[u8], candidate: u8, idx: usize) -> (bool, usize) {
    if idx.saturating_add(2) < body.len() && body[idx.saturating_add(1)] == b'-' {
        let in_range = body[idx] <= candidate && candidate <= body[idx.saturating_add(2)];
        (in_range, 3)
    } else {
        let is_match = body[idx] == candidate;
        (is_match, 1)
    }
}

/// Reports whether `candidate` is a member of a class body.
///
/// Walks left to right so that ranges and literals are interpreted in the order
/// they were written, stopping at the first member that accepts `candidate`.
/// The step is always at least one byte, so the scan terminates without
/// consuming more than the body; a saturated offset would end the loop, which
/// is the correct outcome for a body that long.
fn scan_class_body(body: &[u8], candidate: u8) -> bool {
    let mut idx = 0;
    while idx < body.len() {
        let (matched, step) = match_range_or_single(body, candidate, idx);
        if matched {
            return true;
        }
        idx = idx.saturating_add(step);
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
