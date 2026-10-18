//! The markdown container guard the checked parse runs before the grammar sees a source.

use super::ParseError;

/// The name the markdown guard is keyed on.
///
/// A caller registering its own grammar chooses this name, so a custom grammar
/// called `markdown` is measured by the same rule. That is the intended reading:
/// the name is the stable identity a caller matches findings on, and a caller who
/// claims to be markdown gets markdown's bound.
pub(super) const MARKDOWN: &str = "markdown";

/// The containers declared on the first line of `source` that exceeds `limit`,
/// or `None` when no line does.
///
/// The count is an **upper bound** on the scanner's open containers, for two
/// reasons, and both push the same way:
///
/// - Indentation and markers can both carry nesting, so their depths are added
///   rather than the larger taken. A list nested by indentation opens one
///   container per line and adds depth through its indent; a line carrying both
///   is charged for both.
/// - A marker is counted wherever it appears in a line, not only where it could
///   legally open a container. `a - b - c` counts two. That is prose, not
///   nesting, and refusing it costs nothing because the bound is 64; the point
///   is that a count which is *too high* is a refusal and a count which is too
///   low is a crash.
///
/// One pass over the bytes, and four `usize`s of state: no line is buffered, no
/// allocation is made, and the walk stops at the first line over the bound, so a
/// hostile source costs `O(bytes)` and nothing else. Returning `None` for a
/// source inside the bound is the whole contract — the deepest count *within* the
/// bound is not interesting to a caller, who has already been told it passed.
#[must_use]
pub fn markdown_containers(source: &str, limit: usize) -> Option<usize> {
    for line in source.split('\n') {
        let (indent_columns, rest) = split_indent(line);
        // Two spaces per level of indentation-nested container: the narrowest
        // indent a real nested list carries. A tab is four columns, which is the
        // width every markdown tool agrees on for one. The divisor is a
        // non-zero constant, so the division saturates rather than standing in a
        // column count for a divisor that does not exist.
        let from_indent = indent_columns.saturating_div(2);
        let counted = from_indent.saturating_add(count_markers(rest));
        if counted > limit {
            return Some(counted);
        }
    }
    None
}

/// The leading whitespace of `line` in columns, and the line without it.
fn split_indent(line: &str) -> (usize, &str) {
    let mut columns = 0_usize;
    for (at, character) in line.char_indices() {
        match character {
            ' ' => columns = columns.saturating_add(1),
            '\t' => columns = columns.saturating_add(4),
            _ => return (columns, &line[at..]),
        }
    }
    (columns, "")
}

/// The block containers `line` declares, after its indentation.
///
/// Counts, per markdown's block grammar: every `>` (a run of them is that many
/// levels of blockquote), every `-`, `*` or `+` immediately followed by a space
/// or the end of the line (a list marker needs that space; `---` is a thematic
/// break and `*emphasis*` is not a list), every digit run followed by `.` or `)`
/// and then that same space, and one for a line that opens a code fence.
///
/// Markers are counted **anywhere** in the line, not only where one could
/// legally open a container. `1. 1. 1. x` is three nested ordered lists and
/// `a - b - c` is prose that happens to contain two dashes; the second is
/// counted and the bound is 64, so the price of the over-count is a refusal on
/// a line nobody writes, while the price of an under-count is a process that is
/// no longer there.
fn count_markers(line: &str) -> usize {
    if opens_code_fence(line) {
        return 1;
    }
    let mut counted = 0_usize;
    let mut rest = line;
    while let Some(character) = rest.chars().next() {
        let tail = &rest[character.len_utf8()..];
        if character == '>' {
            counted = counted.saturating_add(1);
        } else if matches!(character, '-' | '*' | '+') {
            if is_marker_end(tail) {
                counted = counted.saturating_add(1);
            }
        } else if character.is_ascii_digit() {
            counted = counted.saturating_add(ordered_marker_len(rest));
        }
        rest = tail;
    }
    counted
}

/// Whether `tail` begins the space or end of line a list marker needs.
fn is_marker_end(tail: &str) -> bool {
    tail.is_empty() || tail.starts_with([' ', '\t'])
}

/// How many containers one ordered-list marker at the head of `rest` declares.
///
/// `1.` and `1)` open a list; `1.5` is a number, `v1.2` is prose, and `1234567.`
/// is a list whose item number is seven digits long. The marker is one
/// container however long its number is, which is why this counts a container
/// and not the digits it consumed.
fn ordered_marker_len(rest: &str) -> usize {
    let digits = match rest.char_indices().find(|entry| !entry.1.is_ascii_digit()) {
        Some((offset, _)) => offset,
        None => rest.len(),
    };
    let after_digits = &rest[digits..];
    if after_digits
        .strip_prefix(['.', ')'])
        .is_some_and(is_marker_end)
    {
        1
    } else {
        0
    }
}

/// Whether `line` opens a fenced code block.
fn opens_code_fence(line: &str) -> bool {
    let fence = line.trim_start();
    let run = fence
        .chars()
        .take_while(|marker| *marker == '`' || *marker == '~')
        .count();
    run >= 3
}

/// Refuse a markdown source whose container nesting the scanner could not
/// serialize, before the scanner is handed it.
///
/// The refusal is [`ParseError::ContainerNestingTooDeep`]; `None` means the
/// source is inside [`MAX_MARKDOWN_CONTAINERS_PER_LINE`] and the grammar may see
/// it. The check is `O(bytes)` with `O(1)` state, and it runs after the byte
/// bound so an oversized source is still refused as oversized.
pub(super) fn validate_markdown_containers(
    source: &str,
    name: &'static str,
    limit: usize,
) -> Result<(), ParseError> {
    let Some(observed) = markdown_containers(source, limit) else {
        return Ok(());
    };
    let refusal = Err(ParseError::ContainerNestingTooDeep {
        language: name,
        observed,
        limit,
    });
    tracing::warn!(
        language = name,
        observed,
        limit,
        "validate_markdown_containers: refusing before the grammar's external scanner, whose \
         serialization buffer would overflow and abort the process"
    );
    refusal
}
