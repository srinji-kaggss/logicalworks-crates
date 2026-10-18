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
        for lower in character.to_lowercase() {
            normalized.push(lower);
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

/// Computes normalized Levenshtein similarity with one rolling row.
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

    let mut previous: Vec<f64> = Vec::with_capacity(columns.len().saturating_add(1));
    let mut denominator = 0.0f64;
    previous.push(0.0);
    for _ in columns {
        denominator += 1.0;
        previous.push(denominator);
    }
    let mut current: Vec<f64> = vec![0.0; columns.len().saturating_add(1)];
    let mut row_distance = 1.0;
    for row_value in rows {
        current[0] = row_distance;
        for (column_index, column_value) in columns.iter().enumerate() {
            let cost = if row_value == column_value { 0.0 } else { 1.0 };
            let index = column_index.saturating_add(1);
            let deletion = previous[index] + 1.0;
            let insertion = current[column_index] + 1.0;
            let substitution = previous[column_index] + cost;
            current[index] = deletion.min(insertion).min(substitution);
        }
        core::mem::swap(&mut previous, &mut current);
        row_distance += 1.0;
    }

    unit_score(1.0 - (previous[columns.len()] / denominator))
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
