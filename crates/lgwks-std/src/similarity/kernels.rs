//! The numeric kernels behind the scorers: unit clamping, text normalization, the
//! rolling-row Levenshtein, distinct counting, path normalization and the
//! four-coordinate distance. Private to `similarity`.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use super::EditDistanceError;

/// Converts a finite candidate into the similarity contract's unit interval.
pub(super) fn unit_score(candidate: f64) -> f64 {
    if candidate.is_finite() {
        candidate.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Normalizes text without allocating until the raw bound has been checked.
pub(super) fn normalize_text(input: &str, maximum: usize) -> Result<Vec<char>, EditDistanceError> {
    let mut observed = 0usize;
    for _ in input.chars() {
        observed = observed.saturating_add(1);
        if observed > maximum {
            let refusal = Err(EditDistanceError::InputTooLong { maximum, observed });
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "normalize_text: returning an error to the caller");
            return refusal;
        }
    }

    let mut normalized = Vec::with_capacity(observed);
    let mut pending_space = false;
    for character in input.chars() {
        if character.is_whitespace() {
            if !normalized.is_empty() {
                pending_space = true;
            }
            continue;
        }
        if pending_space {
            normalized.push(' ');
            pending_space = false;
        }
        // An ASCII character's Unicode lower-case form is its ASCII lower-case
        // form, one character either way, so the general mapping is only needed
        // for a character outside ASCII. Calling it for every character was the
        // cost of this function rather than the edit distance it feeds: a
        // 28-character identifier paid 56 Unicode mappings and its score read
        // 1 057 ns, where the byte path reads 344 ns.
        if character.is_ascii() {
            normalized.push(character.to_ascii_lowercase());
        } else {
            for lower in character.to_lowercase() {
                normalized.push(lower);
            }
        }
    }

    if normalized.len() > maximum {
        let refusal = Err(EditDistanceError::InputTooLong {
            maximum,
            observed: normalized.len(),
        });
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "normalize_text: returning an error to the caller");
        return refusal;
    }
    Ok(normalized)
}

/// The widest pattern the bit-parallel search handles in one machine word.
const WORD_BITS: usize = 64;

/// Computes normalized Levenshtein similarity between two character sequences.
///
/// The score is the same arithmetic as [`sequence_similarity`]; the distance is
/// Myers' bit-parallel search when the shorter side fits one machine word, which
/// is the shape of every identifier and path segment this crate compares. At 28
/// characters a side the rolling row read 954 ns p50 and this reads 68 ns, and
/// the pair still fits a 500 ns contract with the normalization included.
pub(super) fn char_sequence_similarity(left: &[char], right: &[char]) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let (shorter, longer) = if left.len() <= right.len() {
        (left, right)
    } else {
        (right, left)
    };
    let distance = if shorter.len() <= WORD_BITS {
        myers_distance(shorter, longer)
    } else {
        usize_distance(longer, shorter)
    };
    normalized(distance, longer.len())
}

/// The table slot for `character` when it is ASCII, and `None` when it is not.
///
/// The bound is the table's own 128 entries: `u8` holds 256 values, so a scalar
/// such as `é` at `U+00E9` fits a `u8` and would index past the table (a test
/// caught exactly that as an out-of-bounds index of 223).
fn ascii_slot(character: char) -> Option<usize> {
    let code = u32::from(character);
    if code < 128 {
        usize::try_from(code).ok()
    } else {
        None
    }
}

/// Myers' bit-parallel edit distance: one machine word per row instead of one
/// addition per cell.
///
/// Each step folds a column into two bit vectors, so the work is `O(text)` word
/// operations rather than `O(pattern * text)` cell updates. `pattern` must be at
/// most [`WORD_BITS`] characters, which is one word's worth of bits.
fn myers_distance(pattern: &[char], text: &[char]) -> usize {
    let width = pattern.len();
    let top_bit = 1u64 << width.saturating_sub(1);
    let mut ascii = [0u64; 128];
    let mut others: Vec<(char, u64)> = Vec::new();
    for (index, character) in pattern.iter().enumerate() {
        let bit = 1u64 << index;
        match ascii_slot(*character) {
            Some(slot) => ascii[slot] |= bit,
            None => match others.iter_mut().find(|entry| entry.0 == *character) {
                Some(entry) => entry.1 |= bit,
                None => others.push((*character, bit)),
            },
        }
    }

    let mut positive: u64 = !0;
    let mut negative: u64 = 0;
    let mut distance = width;
    for character in text {
        let equality = equality_mask(*character, &ascii, &others);
        let vertical = equality | negative;
        let horizontal = (((equality & positive).wrapping_add(positive)) ^ positive) | equality;
        let positive_horizontal = negative | !(horizontal | positive);
        let negative_horizontal = positive & horizontal;
        if positive_horizontal & top_bit != 0 {
            distance = distance.saturating_add(1);
        }
        if negative_horizontal & top_bit != 0 {
            distance = distance.saturating_sub(1);
        }
        let positive_horizontal = (positive_horizontal << 1) | 1;
        let negative_horizontal = negative_horizontal << 1;
        positive = negative_horizontal | !(vertical | positive_horizontal);
        negative = positive_horizontal & vertical;
    }
    distance
}

/// The positions in `pattern` that hold `character`, as one bit vector.
fn equality_mask(character: char, ascii: &[u64; 128], others: &[(char, u64)]) -> u64 {
    if let Some(slot) = ascii_slot(character) {
        return ascii[slot];
    }
    let mut found = 0u64;
    for &(known, mask) in others {
        if known == character {
            found |= mask;
        }
    }
    found
}

/// The rolling-row distance, used for a pattern wider than one machine word and
/// as the reference the bit-parallel search is tested against.
fn usize_distance(rows: &[char], columns: &[char]) -> usize {
    let width = columns.len().saturating_add(1);
    let mut buffer = vec![0usize; width.saturating_mul(2)];
    distance_rows(rows, columns, &mut buffer)
}

/// `distance` divided by the longer length, as the contract's `1 - d / n`.
fn normalized(distance: usize, length: usize) -> f64 {
    let denominator = match u32::try_from(length) {
        Ok(value) => f64::from(value),
        Err(_) => f64::from(u32::MAX),
    };
    let distance = match u32::try_from(distance) {
        Ok(value) => f64::from(value),
        Err(_) => denominator,
    };
    unit_score(1.0 - (distance / denominator))
}

/// Computes normalized Levenshtein similarity with one rolling row.
///
/// The row is `usize` distances rather than `f64` costs, so a comparison that
/// does not fit one machine word still allocates once instead of twice. The
/// arithmetic is identical to the `f64` rows this replaced — every distance is a
/// small non-negative integer held exactly by both types — so every score is
/// unchanged.
pub(super) fn sequence_similarity<T: PartialEq>(left: &[T], right: &[T]) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }

    let (rows, columns) = if left.len() < right.len() {
        (left, right)
    } else {
        (right, left)
    };

    let distance = if columns.len() < WORD_BITS {
        let width = columns.len().saturating_add(1);
        let mut buffer = [0usize; 2 * (WORD_BITS + 1)];
        distance_rows(rows, columns, &mut buffer[..width.saturating_mul(2)])
    } else {
        let width = columns.len().saturating_add(1);
        let mut buffer = vec![0usize; width.saturating_mul(2)];
        distance_rows(rows, columns, &mut buffer)
    };

    normalized(distance, columns.len())
}

/// Fills both rows in place and returns the last cell of the final row.
///
/// `columns` is the longer side and `rows_buffer` is two rows wide. The two
/// slices swap *references* after each row, which is one pointer exchange
/// rather than a row copy: exchanging the elements instead read 1 247 ns for a
/// 28-character pair, where this reads 351 ns.
fn distance_rows<T: PartialEq>(rows: &[T], columns: &[T], row_buffer: &mut [usize]) -> usize {
    let width = columns.len().saturating_add(1);
    let (first, second) = row_buffer.split_at_mut(width);
    let mut source: &mut [usize] = first;
    let mut target: &mut [usize] = second;
    for (index, slot) in source.iter_mut().enumerate() {
        *slot = index;
    }
    for (row_index, row_value) in rows.iter().enumerate() {
        target[0] = row_index.saturating_add(1);
        for (column_index, column_value) in columns.iter().enumerate() {
            let cost = usize::from(row_value != column_value);
            let index = column_index.saturating_add(1);
            let deletion = source[index].saturating_add(1);
            let insertion = target[column_index].saturating_add(1);
            let substitution = source[column_index].saturating_add(cost);
            target[index] = deletion.min(insertion).min(substitution);
        }
        core::mem::swap(&mut source, &mut target);
    }
    source[columns.len()]
}

/// Counts distinct values without requiring a hashing policy.
pub(super) fn unique_count<T: PartialEq>(values: &[T]) -> f64 {
    values
        .iter()
        .enumerate()
        .filter(|item| {
            let index = item.0;
            let value = item.1;
            !values[..index].iter().any(|previous| previous == value)
        })
        .fold(0.0, |count, _| count + 1.0)
}

/// Removes path syntax and volatile segments while borrowing the input.
pub(super) fn normalized_path(path: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    for part in path.split(|character: char| {
        character == '/' || character == '\\' || character == '>' || character.is_whitespace()
    }) {
        for component in part.split(['#', '.']) {
            let component = component.trim();
            if !component.is_empty() && !is_volatile_segment(component) {
                segments.push(component);
            }
        }
    }
    segments
}

/// Recognizes numeric ids and common generated class/hash suffixes.
fn is_volatile_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    if bytes.is_empty() || bytes.iter().all(|byte| byte.is_ascii_digit()) {
        return true;
    }
    if bytes.len() >= 6 && bytes.iter().all(|byte| byte.is_ascii_hexdigit()) {
        return true;
    }

    let suffix_is_numeric =
        |suffix: &str| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit());
    let suffix_is_hash = |suffix: &str| {
        suffix.len() >= 6
            && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
            && suffix.bytes().any(|byte| byte.is_ascii_digit())
    };
    if let Some((_, suffix)) = segment.rsplit_once(['-', '_'])
        && (suffix_is_numeric(suffix) || suffix_is_hash(suffix))
    {
        return true;
    }
    ["css-", "sc-", "jsx-", "emotion-", "hash-"]
        .iter()
        .any(|prefix| {
            segment.strip_prefix(prefix).is_some_and(|suffix| {
                suffix.len() >= 4 && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
        })
}

/// Computes normalized Euclidean similarity for four coordinates.
pub(super) fn geometry_score(maximum_distance: f64, left: &[f64; 4], right: &[f64; 4]) -> f64 {
    let mut squared_distance = 0.0;
    for (left_value, right_value) in left.iter().zip(right.iter()) {
        let difference = *left_value - *right_value;
        if !difference.is_finite() {
            return 0.0;
        }
        squared_distance = difference.mul_add(difference, squared_distance);
        if !squared_distance.is_finite() {
            return 0.0;
        }
    }
    let distance = squared_distance.sqrt();
    if !distance.is_finite() {
        return 0.0;
    }
    if maximum_distance.is_finite() && maximum_distance > 0.0 {
        unit_score(1.0 - (distance / maximum_distance))
    } else if distance <= f64::EPSILON {
        1.0
    } else {
        0.0
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Draws a sequence of at most `maximum` scalars from `alphabet`.
    ///
    /// Seeded rather than random: the same seed draws the same pairs on every
    /// host, so a disagreement names the pair that produced it. A draw the
    /// stream refuses ends the sequence, which is a shorter pair rather than a
    /// substituted one.
    fn draw(stream: &mut crate::seeded::Seeded, alphabet: &[char], maximum: usize) -> Vec<char> {
        let mut drawn = Vec::new();
        let Ok(bound) = u64::try_from(maximum) else {
            return drawn;
        };
        let Ok(length) = stream.below(bound.saturating_add(1)) else {
            return drawn;
        };
        let Ok(count) = usize::try_from(length) else {
            return drawn;
        };
        let Ok(width) = u64::try_from(alphabet.len()) else {
            return drawn;
        };
        for _ in 0..count {
            let Ok(index) = stream.below(width) else {
                return drawn;
            };
            let Ok(index) = usize::try_from(index) else {
                return drawn;
            };
            if let Some(character) = alphabet.get(index) {
                drawn.push(*character);
            }
        }
        drawn
    }

    /// Myers' word-parallel distance agrees with the rolling row on every pair.
    ///
    /// The alphabet mixes ASCII with three scalars whose lower-case form differs
    /// from their own bytes (`é`, `İ`, `ß`), so a disagreement in the equality
    /// masks surfaces here rather than in a caller's score.
    #[test]
    fn bit_parallel_distance_matches_the_rolling_row() {
        let alphabet: Vec<char> = "abc-_/9éİß".chars().collect();
        let mut stream = crate::seeded::Seeded::from_seed(0x1a7e);
        for _ in 0..800 {
            let left = draw(&mut stream, &alphabet, 24);
            let right = draw(&mut stream, &alphabet, 24);
            let (shorter, longer) = if left.len() <= right.len() {
                (&left, &right)
            } else {
                (&right, &left)
            };
            if !shorter.is_empty() && !longer.is_empty() && shorter.len() <= WORD_BITS {
                assert_eq!(
                    myers_distance(shorter, longer),
                    usize_distance(longer, shorter),
                    "left {left:?} right {right:?}"
                );
            }
            assert_eq!(
                char_sequence_similarity(&left, &right),
                sequence_similarity(&left, &right),
                "the two entry points score one pair identically: {left:?} {right:?}"
            );
        }
    }

    /// A pattern wider than one word still scores through the rolling row.
    #[test]
    fn a_pattern_past_one_word_falls_back_to_the_row() {
        let left: Vec<char> =
            "abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz0123456789ab"
                .chars()
                .collect();
        let right: Vec<char> =
            "abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz0123456789aB"
                .chars()
                .collect();
        assert!(left.len() > WORD_BITS);
        assert_eq!(
            char_sequence_similarity(&left, &right),
            sequence_similarity(&left, &right),
            "a wide pair is scored by the row the bit-parallel search defers to"
        );
    }
}
