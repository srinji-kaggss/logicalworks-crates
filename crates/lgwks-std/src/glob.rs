//! Pure matching of Unicode text against the `lgwks_std` glob dialect.
//!
//! `?` and classes consume one Unicode scalar value; `*` consumes zero or more
//! scalar values except `/`; `**` may consume `/`. Matching is case-sensitive,
//! does not normalize Unicode, and treats a backslash as an ordinary literal.
//! Leading dots are ordinary characters. These are text semantics, not
//! grapheme-cluster or native `OsStr` path semantics.
//!
//! The checked
//! [GlobPattern::compile](crate::glob::GlobPattern::compile) entry accepts the
//! strict dialect:
//! unclosed or empty classes, descending ranges, and `**` embedded inside a
//! path component are errors.
//! [GlobDialect::Legacy](crate::glob::GlobDialect::Legacy) preserves the
//! earlier boolean matcher's literal unclosed brackets, descending-range no-match, and
//! `**` anywhere behavior. The one-call [`matches`] function continues to use
//! that named legacy dialect for source compatibility.
//!
//! This module replaces only the matching portion of the upstream `glob`
//! crate. It does not walk directories or implement POSIX shell expansion.
//! The upstream crate rejects unclosed classes and constrains `**`; this
//! dialect deliberately keeps the documented `a/**/b`, `**/b`, `a/**`, and
//! legacy `a**b` forms. `globset` targets compiled sets of filesystem patterns,
//! which is a different job and adds an external dependency. A compiled NFA
//! with two rolling rows keeps this crate's zero-dependency single-pattern
//! contract while bounding each token transition to one path scan.

use core::fmt;

/// The rolling DP rows the matcher retains: the previous token's row and the
/// one being written. A third would buy nothing, since only these two are ever
/// read.
const ROLLING_ROWS: usize = 2;

/// Selects the syntax accepted while compiling a glob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GlobDialect {
    /// Reject malformed classes, descending ranges, and component-embedded `**`.
    Strict,
    /// Preserve the historical `matches` behavior for migration.
    Legacy,
}

/// A reason a strict glob pattern could not be compiled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PatternErrorKind {
    /// A `[` had no matching `]`.
    UnclosedClass,
    /// A range's first scalar sorts after its last scalar.
    DescendingRange,
    /// A `**` sequence was not a complete path component.
    DoubleStarPlacement,
}

/// A pattern compilation failure with a byte offset into the pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PatternError {
    /// The syntax error category.
    pub kind: PatternErrorKind,
    /// The UTF-8 byte offset where the invalid syntax begins.
    pub byte_offset: usize,
}

impl fmt::Display for PatternError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.kind {
            PatternErrorKind::UnclosedClass => "character class is not closed",
            PatternErrorKind::DescendingRange => "character class range is descending",
            PatternErrorKind::DoubleStarPlacement => "** must occupy a complete path component",
        };
        write!(formatter, "{reason} at byte {}", self.byte_offset)
    }
}

impl std::error::Error for PatternError {}

/// One inclusive Unicode scalar interval compiled from a class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScalarRange {
    /// Lowest included Unicode scalar.
    start: char,
    /// Highest included Unicode scalar.
    end: char,
}

/// One compiled transition in the pattern automaton.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    /// One exact Unicode scalar.
    Literal(char),
    /// Exactly one non-separator scalar.
    Question,
    /// Zero or more non-separator scalars.
    Star,
    /// Zero or more scalars, including separators.
    DoubleStar,
    /// Zero or more scalars ending at a separator, or no directory prefix.
    DoubleStarSlash,
    /// A separator followed by zero or more scalars.
    SlashDoubleStar,
    /// A separator-delimited run with an endpoint after a separator.
    SlashDoubleStarSlash,
    /// One scalar in or outside the inclusive ranges.
    Class {
        /// Sorted scalar ranges accepted by this class.
        ranges: Vec<ScalarRange>,
        /// Whether a scalar outside the ranges is accepted.
        negated: bool,
    },
}

/// A reusable, validated glob pattern.
///
/// Pattern storage is O(M), including the token list and compiled class
/// intervals. A caller that matches many paths should retain this value and a
/// [`GlobScratch`] to amortize compilation and matching allocations.
///
/// # Sharing one pattern across threads
///
/// A `GlobPattern` is `Send + Sync` and holds no caller data, so one compiled
/// pattern serves any number of concurrent callers. The mutable half is
/// [`GlobScratch`], which [`is_match_with`](Self::is_match_with) takes by
/// `&mut`: the pattern is what is shared, and the scratch is what a caller owns.
/// Two callers sharing a scratch share one caller's path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobPattern {
    /// The compiled sequence of matching transitions.
    tokens: Vec<Token>,
}

/// The compile-time assertion that one pattern is shareable across threads.
///
/// A compile error, not a runtime note, if a future edit gives [`GlobPattern`]
/// interior mutability or a non-`Send` field: the whole sharing contract above
/// is that claim. It is asserted once here, and the same assertion is written
/// independently against the published type by
/// `tests/sim_shared_policy_tiers.rs`, which is what makes this one a check
/// rather than a note. The test module below carries the matching runtime
/// probe.
#[cfg(test)]
const fn assert_shared_across_threads() {
    const fn assert<T: Send + Sync>() {}
    assert::<GlobPattern>();
    assert::<GlobScratch>();
}

impl GlobPattern {
    /// Compiles the strict dialect and returns malformed syntax as a typed error.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use lgwks_std::glob::{GlobPattern, GlobScratch};
    ///
    /// let pattern = GlobPattern::compile("src/**/[a-z]?.rs")?;
    /// let mut scratch = GlobScratch::new();
    /// assert!(pattern.is_match_with("src/l1.rs", &mut scratch));
    /// assert!(pattern.is_match_with("src/sub/l1.rs", &mut scratch));
    /// assert!(!pattern.is_match_with("src/sub/lib1.rs", &mut scratch));
    /// # Ok::<(), lgwks_std::glob::PatternError>(())
    /// ```
    pub fn compile(pattern: &str) -> Result<Self, PatternError> {
        Self::compile_with_dialect(pattern, GlobDialect::Strict)
    }

    /// Compiles a pattern under an explicitly selected syntax policy.
    ///
    /// Use [`GlobDialect::Legacy`] when migrating callers that rely on the old
    /// permissive forms. New consumers should prefer [`Self::compile`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use lgwks_std::glob::{GlobDialect, GlobPattern};
    ///
    /// let old_pattern = GlobPattern::compile_with_dialect("a**b", GlobDialect::Legacy)?;
    /// assert!(old_pattern.is_match("a/x/b"));
    /// # Ok::<(), lgwks_std::glob::PatternError>(())
    /// ```
    pub fn compile_with_dialect(pattern: &str, dialect: GlobDialect) -> Result<Self, PatternError> {
        let mut work = Work::default();
        compile_pattern(pattern, dialect, &mut work)
    }

    /// Matches a path using newly allocated temporary matching scratch.
    #[must_use]
    pub fn is_match(&self, path: &str) -> bool {
        self.is_match_with(path, &mut GlobScratch::new())
    }

    /// Matches a path while reusing scalar indexing and rolling-row storage.
    ///
    /// After the scratch capacities are sufficient for the path, matching
    /// performs no allocation per token. The caller owns the scratch lifetime;
    /// scratch contains only the last path and is overwritten on the next call.
    #[must_use]
    pub fn is_match_with(&self, path: &str, scratch: &mut GlobScratch) -> bool {
        scratch.match_path(self, path)
    }

    /// Returns the number of compiled tokens.
    ///
    /// Pattern storage is `O(M)` in the pattern length, and this is the count
    /// that sizes it. It is reported separately from the `O(N)` matching
    /// scratch so a caller sizing its own memory is not misled by a whole-call
    /// figure that blends the two.
    #[must_use]
    pub const fn token_count(&self) -> usize {
        self.tokens.len()
    }
}

/// Reusable matching memory owned by the caller.
///
/// Its buffers are O(N): Unicode scalar indexing plus two rolling DP rows.
/// Reusing it avoids allocating a row for each token or retaining path state in
/// the compiled pattern.
#[derive(Debug, Default)]
pub struct GlobScratch {
    /// Unicode scalar indexing for the current path.
    scalars: Vec<char>,
    /// Reachable offsets after the previous token.
    previous: Vec<u8>,
    /// Reachable offsets while processing the current token.
    next: Vec<u8>,
    /// Test-only measurements of parser and transition work.
    work: Work,
}

impl GlobScratch {
    /// Creates empty scratch buffers.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            scalars: Vec::new(),
            previous: Vec::new(),
            next: Vec::new(),
            work: Work::new(),
        }
    }

    /// Returns the retained byte capacity of the scalar index.
    ///
    /// This is `O(N)` in the path's scalar count and is the largest single
    /// buffer the scratch owns; `char` is four bytes on every target this
    /// workspace supports.
    #[must_use]
    pub fn scalar_capacity(&self) -> usize {
        self.scalars.capacity()
    }

    /// Returns the retained byte capacity of each rolling row.
    ///
    /// There are exactly two, and each is one byte per scalar plus the
    /// terminating position.
    #[must_use]
    pub fn row_capacity(&self) -> usize {
        self.previous.capacity()
    }

    /// Returns the number of rolling rows the scratch retains.
    ///
    /// Always two. It is reported rather than assumed so a caller accounting
    /// for its own `O(N)` storage has the count from the type instead of from
    /// the module's prose.
    #[must_use]
    pub const fn row_count(&self) -> usize {
        ROLLING_ROWS
    }

    /// Returns the total retained capacity across all three buffers.
    ///
    /// The scratch is caller-owned and outlives the call, so a caller sizing
    /// its own steady-state memory needs the scalar index, the two rolling
    /// rows, and this sum reported separately from the pattern's own `O(M)`
    /// storage ([`GlobPattern::token_count`]).
    #[must_use]
    pub fn storage_bytes(&self) -> usize {
        self.scalars
            .capacity()
            .saturating_mul(core::mem::size_of::<char>())
            .saturating_add(self.previous.capacity())
            .saturating_add(self.next.capacity())
    }

    /// Runs the two-row automaton, counting each inspected state as work.
    fn match_path(&mut self, pattern: &GlobPattern, path: &str) -> bool {
        self.scalars.clear();
        self.scalars.extend(path.chars());
        let row_len = self.scalars.len().saturating_add(1);
        self.previous.resize(row_len, 0);
        self.next.resize(row_len, 0);
        self.previous.fill(0);
        self.previous[0] = 1;
        self.work.reset();

        for token in &pattern.tokens {
            self.next.fill(0);
            step_token(
                token,
                &self.scalars,
                &self.previous,
                &mut self.next,
                &mut self.work,
            );
            core::mem::swap(&mut self.previous, &mut self.next);
        }
        self.previous[self.scalars.len()] != 0
    }
}

/// Counts deterministic parser and transition operations in test builds only.
#[derive(Debug, Default)]
struct Work {
    /// Parser inspections, included only in unit-test builds.
    #[cfg(test)]
    parse: usize,
    /// DP and class lookup inspections, included only in unit-test builds.
    #[cfg(test)]
    transition: usize,
}

impl Work {
    /// Creates an empty operation counter.
    const fn new() -> Self {
        Self {
            #[cfg(test)]
            parse: 0,
            #[cfg(test)]
            transition: 0,
        }
    }

    /// Clears operation counts before one observable match.
    fn reset(&mut self) {
        #[cfg(test)]
        {
            self.transition = 0;
        }
    }

    /// Records one parser inspection in test builds.
    fn parser_step(&mut self) {
        #[cfg(test)]
        {
            self.parse = self.parse.saturating_add(1);
        }
    }

    /// Records one transition inspection in test builds.
    fn transition_step(&mut self) {
        #[cfg(test)]
        {
            self.transition = self.transition.saturating_add(1);
        }
    }
}

/// Compiles chars and class ranges once; tokens borrow no caller data.
fn compile_pattern(
    pattern: &str,
    dialect: GlobDialect,
    work: &mut Work,
) -> Result<GlobPattern, PatternError> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut next_close = vec![chars.len(); chars.len().saturating_add(1)];
    let mut nearest = chars.len();
    for index in (0..chars.len()).rev() {
        work.parser_step();
        if chars[index] == ']' {
            nearest = index;
        }
        next_close[index] = nearest;
    }

    let mut star_cursor = 0;
    while star_cursor < chars.len() {
        work.parser_step();
        if chars[star_cursor] == '[' {
            let (_, _, close) = class_bounds(&chars, &next_close, star_cursor);
            star_cursor = if close < chars.len() {
                close.saturating_add(1)
            } else if dialect == GlobDialect::Strict {
                chars.len()
            } else {
                star_cursor.saturating_add(1)
            };
        } else if chars[star_cursor] == '*'
            && chars.get(star_cursor.saturating_add(1)) == Some(&'*')
        {
            let run_start = star_cursor;
            while chars.get(star_cursor) == Some(&'*') {
                work.parser_step();
                star_cursor = star_cursor.saturating_add(1);
            }
            let left_boundary = run_start == 0 || chars[run_start.saturating_sub(1)] == '/';
            let right_boundary = star_cursor == chars.len() || chars.get(star_cursor) == Some(&'/');
            if dialect == GlobDialect::Strict
                && (star_cursor.saturating_sub(run_start) != 2 || !left_boundary || !right_boundary)
            {
                return Err(PatternError {
                    kind: PatternErrorKind::DoubleStarPlacement,
                    byte_offset: byte_offset(&chars, run_start),
                });
            }
        } else {
            star_cursor = star_cursor.saturating_add(1);
        }
    }

    let mut tokens = Vec::with_capacity(chars.len());
    let mut index = 0;
    while index < chars.len() {
        work.parser_step();
        if starts_with(&chars, index, &['/', '*', '*', '/']) {
            tokens.push(Token::SlashDoubleStarSlash);
            index = index.saturating_add(4);
        } else if starts_with(&chars, index, &['/', '*', '*']) {
            tokens.push(Token::SlashDoubleStar);
            index = index.saturating_add(3);
        } else if starts_with(&chars, index, &['*', '*', '/']) {
            tokens.push(Token::DoubleStarSlash);
            index = index.saturating_add(3);
        } else if starts_with(&chars, index, &['*', '*']) {
            let previous_is_separator = index == 0 || chars[index.saturating_sub(1)] == '/';
            let after = index.saturating_add(2);
            let next_is_separator = after == chars.len() || chars.get(after) == Some(&'/');
            if dialect == GlobDialect::Strict && !(previous_is_separator && next_is_separator) {
                return Err(PatternError {
                    kind: PatternErrorKind::DoubleStarPlacement,
                    byte_offset: byte_offset(&chars, index),
                });
            }
            tokens.push(Token::DoubleStar);
            index = after;
        } else if chars[index] == '*' {
            tokens.push(Token::Star);
            index = index.saturating_add(1);
        } else if chars[index] == '?' {
            tokens.push(Token::Question);
            index = index.saturating_add(1);
        } else if chars[index] == '[' {
            let (negated, content, close) = class_bounds(&chars, &next_close, index);
            if close == chars.len() {
                if dialect == GlobDialect::Strict {
                    return Err(PatternError {
                        kind: PatternErrorKind::UnclosedClass,
                        byte_offset: byte_offset(&chars, index),
                    });
                }
                tokens.push(Token::Literal('['));
                index = index.saturating_add(1);
            } else {
                let ranges = compile_class(&chars[content..close], dialect, index, work)?;
                tokens.push(Token::Class { ranges, negated });
                index = close.saturating_add(1);
            }
        } else {
            tokens.push(Token::Literal(chars[index]));
            index = index.saturating_add(1);
        }
    }
    Ok(GlobPattern { tokens })
}

/// Tests a short token prefix without allocating a suffix or rescanning input.
fn starts_with(chars: &[char], offset: usize, prefix: &[char]) -> bool {
    chars.get(offset..offset.saturating_add(prefix.len())) == Some(prefix)
}

/// Resolves one class opener using the precomputed linear-time close index.
fn class_bounds(chars: &[char], next_close: &[usize], opening: usize) -> (bool, usize, usize) {
    let mut content = opening.saturating_add(1);
    let negated = matches!(chars.get(content), Some('!' | '^'));
    if negated {
        content = content.saturating_add(1);
    }
    let mut closing_search = content;
    if chars.get(closing_search) == Some(&']') {
        closing_search = closing_search.saturating_add(1);
    }
    let close = next_close
        .get(closing_search)
        .copied()
        .unwrap_or(chars.len());
    (negated, content, close)
}

/// Converts a scalar offset to the public UTF-8 byte-offset convention.
fn byte_offset(chars: &[char], scalar_offset: usize) -> usize {
    chars
        .get(..scalar_offset)
        .unwrap_or(chars)
        .iter()
        .map(|ch| ch.len_utf8())
        .sum()
}

/// Compiles the members of one class into intervals sorted for binary lookup.
fn compile_class(
    body: &[char],
    dialect: GlobDialect,
    class_offset: usize,
    work: &mut Work,
) -> Result<Vec<ScalarRange>, PatternError> {
    let mut ranges = Vec::with_capacity(body.len());
    let mut index = 0;
    while index < body.len() {
        work.parser_step();
        if index.saturating_add(2) < body.len() && body[index.saturating_add(1)] == '-' {
            let start = body[index];
            let end = body[index.saturating_add(2)];
            if start > end && dialect == GlobDialect::Strict {
                return Err(PatternError {
                    kind: PatternErrorKind::DescendingRange,
                    byte_offset: class_offset,
                });
            }
            if start <= end {
                ranges.push(ScalarRange { start, end });
            }
            index = index.saturating_add(3);
        } else {
            ranges.push(ScalarRange {
                start: body[index],
                end: body[index],
            });
            index = index.saturating_add(1);
        }
    }
    radix_sort_ranges(&mut ranges, work);
    merge_overlapping_ranges(&mut ranges, work);
    Ok(ranges)
}

/// Merges overlapping intervals so binary-search membership remains monotone.
fn merge_overlapping_ranges(ranges: &mut Vec<ScalarRange>, work: &mut Work) {
    let mut written = 0_usize;
    for read in 0..ranges.len() {
        work.parser_step();
        let range = ranges[read];
        if written > 0 && range.start <= ranges[written.saturating_sub(1)].end {
            let previous = &mut ranges[written.saturating_sub(1)];
            if range.end > previous.end {
                previous.end = range.end;
            }
        } else {
            ranges[written] = range;
            written = written.saturating_add(1);
        }
    }
    ranges.truncate(written);
}

/// Sorts scalar intervals in linear time using six fixed-width radix passes.
fn radix_sort_ranges(ranges: &mut Vec<ScalarRange>, work: &mut Work) {
    if ranges.len() < 2 {
        return;
    }
    let mut output = vec![ranges[0]; ranges.len()];
    for shift in [0_u32, 8, 16, 24, 32, 40] {
        let mut counts = [0_usize; 256];
        for range in ranges.iter() {
            work.parser_step();
            let bucket = range_bucket(*range, shift);
            counts[bucket] = counts[bucket].saturating_add(1);
        }
        let mut positions = [0_usize; 256];
        let mut start = 0;
        for bucket in 0..counts.len() {
            work.parser_step();
            positions[bucket] = start;
            start = start.saturating_add(counts[bucket]);
        }
        for range in ranges.iter() {
            work.parser_step();
            let bucket = range_bucket(*range, shift);
            let position = positions[bucket];
            output[position] = *range;
            positions[bucket] = position.saturating_add(1);
        }
        core::mem::swap(ranges, &mut output);
    }
}

/// Returns the current radix bucket for the lexicographic `(start, end)` key.
fn range_bucket(range: ScalarRange, shift: u32) -> usize {
    let start = u64::from(u32::from(range.start));
    let end = u64::from(u32::from(range.end));
    let key = (start << 21) | end;
    usize::from(u8::try_from((key >> shift) & 0xff).unwrap_or(0))
}

/// Advances one compiled token over one path using the rolling previous row.
fn step_token(token: &Token, path: &[char], previous: &[u8], next: &mut [u8], work: &mut Work) {
    match *token {
        Token::Literal(literal) => {
            for offset in 0..path.len() {
                work.transition_step();
                if previous[offset] != 0 && path[offset] == literal {
                    next[offset.saturating_add(1)] = 1;
                }
            }
        }
        Token::Question => {
            for offset in 0..path.len() {
                work.transition_step();
                if previous[offset] != 0 && path[offset] != '/' {
                    next[offset.saturating_add(1)] = 1;
                }
            }
        }
        Token::Class {
            ref ranges,
            negated,
        } => {
            for offset in 0..path.len() {
                work.transition_step();
                let member = class_contains(ranges, path[offset], work);
                if previous[offset] != 0 && path[offset] != '/' && (member != negated) {
                    next[offset.saturating_add(1)] = 1;
                }
            }
        }
        Token::Star => {
            for offset in 0..=path.len() {
                work.transition_step();
                let zero_or_more = previous[offset] != 0
                    || (offset > 0
                        && next[offset.saturating_sub(1)] != 0
                        && path[offset.saturating_sub(1)] != '/');
                next[offset] = u8::from(zero_or_more);
            }
        }
        Token::DoubleStar => {
            let mut reachable = false;
            for offset in 0..=path.len() {
                work.transition_step();
                reachable |= previous[offset] != 0;
                next[offset] = u8::from(reachable);
            }
        }
        Token::DoubleStarSlash => {
            let mut reachable = false;
            for offset in 0..=path.len() {
                work.transition_step();
                if offset > 0 && path[offset.saturating_sub(1)] == '/' {
                    reachable |= previous[offset.saturating_sub(1)] != 0;
                    next[offset] = u8::from(reachable);
                }
                next[offset] |= previous[offset];
                reachable |= previous[offset] != 0;
            }
        }
        Token::SlashDoubleStar => {
            let mut can_extend = false;
            for offset in 0..=path.len() {
                work.transition_step();
                if offset > 0 {
                    can_extend |= previous[offset.saturating_sub(1)] != 0
                        && path[offset.saturating_sub(1)] == '/';
                }
                next[offset] = u8::from(can_extend || previous[offset] != 0);
            }
        }
        Token::SlashDoubleStarSlash => {
            let mut started = false;
            for offset in 1..=path.len() {
                work.transition_step();
                let separator = path[offset.saturating_sub(1)] == '/';
                started |= previous[offset.saturating_sub(1)] != 0 && separator;
                next[offset] = u8::from(started && separator);
            }
        }
    }
}

/// Looks up one scalar in pre-sorted, disjointness-independent intervals.
fn class_contains(ranges: &[ScalarRange], value: char, work: &mut Work) -> bool {
    if ranges.len() <= 16 {
        ranges.iter().any(|range| {
            work.transition_step();
            range.start <= value && value <= range.end
        })
    } else {
        ranges
            .binary_search_by(|range| {
                work.transition_step();
                if value < range.start {
                    core::cmp::Ordering::Greater
                } else if value > range.end {
                    core::cmp::Ordering::Less
                } else {
                    core::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }
}

/// Reports whether `path` matches `pattern` using the historical permissive dialect.
///
/// This one-call convenience function retains the old behavior: `?` and
/// classes consume one Unicode scalar, `*` stops at `/`, `**` may cross `/`,
/// unmatched `[` is literal, and `**` may occur within a component. It compiles
/// O(M) pattern storage and allocates O(N) scratch. For repeated matches, use
/// [`GlobPattern`] and reuse [`GlobScratch`].
#[must_use]
pub fn matches(pattern: &str, path: &str) -> bool {
    let Ok(compiled) = GlobPattern::compile_with_dialect(pattern, GlobDialect::Legacy) else {
        return false;
    };
    compiled.is_match(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_literal_inputs_keep_exact_results() {
        assert!(matches("", ""), "empty pattern matches the empty path");
        assert!(!matches("", "a"), "empty pattern rejects non-empty path");
        assert!(matches("hello", "hello"), "literal text matches itself");
        assert!(
            !matches("hello", "world"),
            "different literal text does not match"
        );
        assert!(!matches("hello", "hello/world"), "matching is anchored");
        assert!(matches("[", "["), "literal bracket remains matchable");
        assert!(
            matches("[abc", "[abc"),
            "legacy unclosed bracket remains literal"
        );
    }

    #[test]
    fn question_and_star_preserve_separator_boundaries() {
        assert!(matches("a?c", "abc"), "question consumes one scalar");
        assert!(matches("a*c", "abbc"), "star consumes a same-segment run");
        assert!(!matches("a?c", "a/c"), "question never consumes slash");
        assert!(!matches("a*c", "a/b/c"), "single star never crosses slash");
        assert!(
            matches("*.rs", ".hidden.rs"),
            "leading dots are ordinary text"
        );
        assert!(matches("a\\b", "a\\b"), "backslash is an ordinary literal");
    }

    #[test]
    fn classes_are_scalar_ordered_and_never_match_slash() {
        assert!(matches("[é]", "é"), "a class matches one multibyte scalar");
        assert!(
            matches("[zyxwvutsrqponmlkjihgfedcba]", "a"),
            "large unordered classes are compiled for logarithmic lookup"
        );
        assert!(
            matches("[a-zb-c]", "y"),
            "overlapping ranges merge before binary-search membership"
        );
        assert!(matches("[অ-ঊ]", "ঈ"), "Bengali range follows scalar order");
        assert!(
            matches("[😀-🙏]", "😃"),
            "supplementary range follows scalar order"
        );
        assert!(
            !matches("?", "e\u{301}"),
            "question does not collapse a decomposed two-scalar sequence"
        );
        assert!(
            matches("??", "e\u{301}"),
            "two questions consume the decomposed base and combining scalar"
        );
        assert!(
            !matches("?", "éx"),
            "one question does not consume two scalars"
        );
        assert!(!matches("??", "é"), "two questions do not match one scalar");
        assert!(!matches("[!a]", "/"), "negated classes still exclude slash");
        assert!(!matches("[a-z]", "/"), "ranges exclude slash");
        assert!(!matches("?", "/"), "question excludes slash");
        assert!(!matches("*", "a/b"), "star does not cross slash");
    }

    #[test]
    fn double_star_legacy_forms_keep_exact_directory_reach() {
        assert!(
            matches("a/**/b", "a/b"),
            "double-star slash accepts zero directories"
        );
        assert!(
            matches("a/**/b", "a/x/y/b"),
            "double-star slash accepts nested directories"
        );
        assert!(
            matches("**/b", "b"),
            "leading double-star slash accepts no directory"
        );
        assert!(
            matches("**/b", "a/b"),
            "leading double-star slash accepts one directory"
        );
        assert!(
            matches("**/b", "x/y/b"),
            "leading double-star slash accepts nested directories"
        );
        assert!(
            matches("a/**", "a"),
            "trailing slash double-star accepts no suffix"
        );
        assert!(
            matches("a**b", "a/x/y/b"),
            "legacy embedded double-star may cross slash"
        );
        assert!(
            !GlobPattern::compile("a**b").is_ok(),
            "strict dialect rejects embedded double-star"
        );
    }

    #[test]
    fn checked_compilation_reports_each_malformed_pattern_class() {
        assert_eq!(
            GlobPattern::compile("[abc").map(|_| ()),
            Err(PatternError {
                kind: PatternErrorKind::UnclosedClass,
                byte_offset: 0,
            }),
            "strict compilation reports an unclosed class offset"
        );
        assert_eq!(
            GlobPattern::compile("[]").map(|_| ()),
            Err(PatternError {
                kind: PatternErrorKind::UnclosedClass,
                byte_offset: 0,
            }),
            "strict compilation reports the unclosed leading-bracket class"
        );
        assert_eq!(
            GlobPattern::compile("[z-a]").map(|_| ()),
            Err(PatternError {
                kind: PatternErrorKind::DescendingRange,
                byte_offset: 0,
            }),
            "strict compilation reports a descending range"
        );
        assert!(
            !matches("[z-a]", "a"),
            "legacy descending range remains a no-match class"
        );
        assert!(
            matches("[]]", "]"),
            "leading close bracket remains a class member"
        );
        assert!(
            matches("[!]]", "a"),
            "negated leading close bracket class remains valid"
        );
    }

    #[test]
    fn compiled_and_one_call_paths_have_identical_public_semantics() -> Result<(), PatternError> {
        let pattern = GlobPattern::compile_with_dialect("a**[b-d]?", GlobDialect::Legacy)?;
        let mut scratch = GlobScratch::new();
        for (path, expected) in [("axy/bc", true), ("abz", true), ("a/x/dé", true)] {
            assert_eq!(
                matches("a**[b-d]?", path),
                expected,
                "one-call result is stable"
            );
            assert_eq!(
                pattern.is_match_with(path, &mut scratch),
                expected,
                "compiled result matches one-call result"
            );
        }
        Ok(())
    }

    #[test]
    fn each_transition_has_linear_measured_work_for_n_2n_4n() -> Result<(), PatternError> {
        let pattern = GlobPattern::compile_with_dialect("*a*", GlobDialect::Legacy)?;
        let mut previous = 0_usize;
        for size in [256_usize, 512, 1024] {
            let path = "a".repeat(size);
            let mut scratch = GlobScratch::new();
            assert!(
                pattern.is_match_with(&path, &mut scratch),
                "repeated a path matches"
            );
            let measured = scratch.work.transition;
            assert!(
                measured <= pattern.tokens.len().saturating_mul(size.saturating_add(1)),
                "work is bounded by token_count * scalar_count: {measured}"
            );
            if previous != 0 {
                assert!(
                    measured
                        <= previous
                            .saturating_mul(2)
                            .saturating_add(pattern.tokens.len()),
                    "doubling input does not produce quadratic work: {previous} -> {measured}"
                );
            }
            previous = measured;
        }
        Ok(())
    }

    #[test]
    fn every_double_star_transition_scales_linearly() -> Result<(), PatternError> {
        for source in ["**", "**/x", "a/**", "a/**/b"] {
            let pattern = GlobPattern::compile_with_dialect(source, GlobDialect::Legacy)?;
            let mut previous = 0_usize;
            for size in [256_usize, 512, 1024] {
                let path = format!("a/{}/b", "x/".repeat(size.div_ceil(2)));
                let mut scratch = GlobScratch::new();
                let matched = pattern.is_match_with(&path, &mut scratch);
                assert_eq!(
                    matched,
                    source != "**/x",
                    "double-star transition preserves expected path result"
                );
                let measured = scratch.work.transition;
                assert!(
                    measured
                        <= pattern
                            .tokens
                            .len()
                            .saturating_mul(path.chars().count().saturating_add(1)),
                    "{source} transition work is linear: {measured}"
                );
                if previous != 0 {
                    assert!(
                        measured
                            <= previous
                                .saturating_mul(2)
                                .saturating_add(pattern.tokens.len()),
                        "{source} work growth is linear: {previous} -> {measured}"
                    );
                }
                previous = measured;
            }
        }
        Ok(())
    }

    #[test]
    fn unmatched_class_parser_work_is_linear_and_controls_are_explicit() -> Result<(), PatternError>
    {
        let mut previous = 0_usize;
        for size in [256_usize, 512, 1024] {
            let source = "[".repeat(size);
            let mut work = Work::default();
            let pattern = compile_pattern(&source, GlobDialect::Legacy, &mut work)?;
            let measured = work.parse;
            assert!(
                measured <= size.saturating_mul(3),
                "parser inspections remain linear: {measured}"
            );
            if previous != 0 {
                assert!(
                    measured <= previous.saturating_mul(2).saturating_add(3),
                    "doubling pattern does not cause suffix rescans: {previous} -> {measured}"
                );
            }
            assert_eq!(
                pattern.tokens.len(),
                size,
                "each unmatched bracket remains a literal"
            );
            previous = measured;
        }
        assert!(
            GlobPattern::compile("[abc]").is_ok(),
            "normal class control compiles"
        );
        assert!(
            matches("[abc", "[abc"),
            "literal malformed-class control uses legacy behavior"
        );
        assert_eq!(
            GlobPattern::compile("[abc").map(|_| ()),
            Err(PatternError {
                kind: PatternErrorKind::UnclosedClass,
                byte_offset: 0,
            }),
            "strict malformed-class control is explicit"
        );
        Ok(())
    }

    #[test]
    fn a_compiled_pattern_is_shareable_across_threads_by_construction() {
        // The claim on `GlobPattern` is not a comment: the assertion is
        // checked at compile time, and this is what keeps it from being
        // deleted as an uncalled constant. It also proves the pattern is
        // shareable across real OS threads, not only that the bounds hold.
        assert_shared_across_threads();
        let compiled = GlobPattern::compile_with_dialect(SHARED_PATTERN, GlobDialect::Legacy);
        let Ok(pattern) = compiled else {
            return;
        };
        let shared = std::sync::Arc::new(pattern);
        let observed: Vec<bool> = (0..8)
            .map(|index| {
                let shared = std::sync::Arc::clone(&shared);
                match std::thread::Builder::new()
                    .name(format!("glob-share-{index}"))
                    .stack_size(64 * 1024)
                    .spawn(move || {
                        let mut scratch = GlobScratch::new();
                        let path = format!("a/{}b7z", "x/".repeat(index));
                        shared.is_match_with(&path, &mut scratch)
                    }) {
                    Ok(joined) => joined.join().unwrap_or(false),
                    Err(_) => false,
                }
            })
            .collect();
        assert!(
            observed.iter().all(|matched| *matched),
            "every caller must match the shared pattern identically: {observed:?}"
        );
    }

    /// The pattern the shareability test above matches with, kept next to it so
    /// the two cannot drift apart.
    const SHARED_PATTERN: &str = "*a**/b[0-9]?";

    #[test]
    fn scratch_capacity_is_reused_without_per_token_row_allocations() -> Result<(), PatternError> {
        let pattern = GlobPattern::compile_with_dialect("*a**/b[0-9]?", GlobDialect::Legacy)?;
        let mut scratch = GlobScratch::new();
        assert!(
            pattern.is_match_with("a/x/b7é", &mut scratch),
            "first match succeeds"
        );
        let capacities = (
            scratch.scalars.capacity(),
            scratch.previous.capacity(),
            scratch.next.capacity(),
        );
        for _ in 0..8 {
            assert!(
                pattern.is_match_with("a/x/b7é", &mut scratch),
                "reused match succeeds"
            );
            assert_eq!(
                (
                    scratch.scalars.capacity(),
                    scratch.previous.capacity(),
                    scratch.next.capacity()
                ),
                capacities,
                "scalar and two-row capacities remain fixed after warm capacity"
            );
        }
        assert_eq!(
            pattern.tokens.len(),
            6,
            "compiled pattern storage is counted separately"
        );
        Ok(())
    }
}
