//! What the JSON adversarial families share: the text an attacker draws, the tree
//! built from it, and the two questions every family asks of the facade.
//!
//! The generators draw from the one [`Attacker`] stream (INV-STD-SEEDED-1), so a
//! failure names the seed that reproduces it, and from a table of characters that
//! break a naive encoder rather than from the alphabet a test author would think
//! of first.
#![cfg(feature = "json")]

use lgwks_std::json::{Value, from_str};

use crate::adversary::Attacker;

/// Characters that break a naive encoder: controls, both quote forms, the line
/// separators JavaScript treats as newlines, a byte-order mark, the largest BMP
/// scalar, an astral scalar and plain text.
const GLYPHS: [char; 16] = [
    '\0',
    '\u{1}',
    '\u{1f}',
    '"',
    '\\',
    '/',
    '\u{7f}',
    '\u{80}',
    '\u{2028}',
    '\u{2029}',
    '\u{feff}',
    '\u{ffff}',
    '\u{10000}',
    '\u{1f600}',
    'a',
    '\u{e9}',
];

/// A string drawn from the glyph table, `below(max)` characters long.
pub fn draw_text(attacker: &mut Attacker, max: usize) -> String {
    let len = attacker.below(max);
    (0..len).map(|_| *attacker.pick(&GLYPHS)).collect()
}

/// A tree of every value kind, at most `depth` levels deep.
pub fn draw_value(attacker: &mut Attacker, depth: usize) -> Value {
    let kinds = if depth == 0 { 6 } else { 8 };
    match attacker.below(kinds) {
        0 => Value::Null,
        1 => Value::Bool(attacker.word().rem_euclid(2) == 1),
        2 => Value::from(i64::from_le_bytes(attacker.word().to_le_bytes())),
        3 => Value::from(f64::from_bits(attacker.word())),
        4 | 5 => Value::String(draw_text(attacker, 12)),
        6 => Value::Array(
            (0..attacker.below(4))
                .map(|_| draw_value(attacker, depth.saturating_sub(1)))
                .collect(),
        ),
        _ => Value::Object(
            (0..attacker.below(4))
                .map(|_| {
                    (
                        draw_text(attacker, 6),
                        draw_value(attacker, depth.saturating_sub(1)),
                    )
                })
                .collect(),
        ),
    }
}

/// The compact text of `value`, which a tree cannot fail to have.
pub fn encode(value: &Value) -> Vec<u8> {
    value.to_string().into_bytes()
}

/// Whether the facade reads `text` as a document of one value.
pub fn accepts(text: &str) -> bool {
    from_str::<Value>(text).is_ok()
}
