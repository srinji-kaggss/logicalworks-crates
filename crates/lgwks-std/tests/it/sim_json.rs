//! Seeded deterministic replay of the `json` facade over drawn value trees.
//!
//! One seed draws nested JSON documents — objects and arrays to a bounded
//! depth, signed, unsigned and fractional numbers, strings carrying quotes,
//! backslashes, controls and astral letters — and every family asserts what an
//! external interface exchanging JSON sees through the shipped public API:
//!
//! - a tree round-trips through text, bytes, a writer, a reader and the value
//!   bridge, and every encoder emits the same bytes;
//! - compact text holds no raw line break or control, whatever the strings
//!   carried, and pretty text parses to the same tree;
//! - every strict prefix of an object document is refused, trailing
//!   whitespace is accepted and trailing content is refused;
//! - a stray invalid UTF-8 byte anywhere is refused (INV-JSON-UTF8);
//! - a non-finite float encodes as `null`, never as text JSON cannot carry;
//! - the same seed replays the same documents and distinct seeds diverge.
//!
//! Sources: RFC 8259 (§2 whitespace, §6 numbers with no NaN or Infinity, §7
//! string escapes, §8.1 UTF-8), and INV-JSON-UTF8.
#![cfg(feature = "json")]

use std::error::Error;

use crate::seeded_bytes::{below, fold_bytes, next_byte};
use crate::seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, initial_trace, next_seed,
    seeded_stream,
};

use lgwks_std::json::{self, Map, Number, Value};
use lgwks_std::seeded::Seeded;

/// How many documents each family draws per seed.
const DRAWS: usize = 64;

/// How deep a drawn tree may nest.
const DEPTH: usize = 4;

/// The glyphs drawn strings are made of.
const GLYPHS: [char; 12] = ['a', 'Z', '9', ' ', '"', '\\', '\n', '\u{1}', '\u{7f}', 'ß', '€', '😀'];

/// A family's result.
type Outcome = Result<(), Box<dyn Error>>;

/// Up to 12 drawn glyphs.
fn drawn_string(state: &mut Seeded) -> String {
    (0..below(state, 13))
        .map(|_| match GLYPHS.get(below(state, GLYPHS.len())) {
            Some(glyph) => *glyph,
            None => 'a',
        })
        .collect()
}

/// A drawn number: signed, unsigned, or a finite fraction.
fn drawn_number(state: &mut Seeded) -> Value {
    match below(state, 3) {
        0 => Value::from(i64::from_le_bytes(next_seed(state).to_le_bytes())),
        1 => Value::from(next_seed(state)),
        _ => {
            let fractions = [-16.0, -0.125, 0.0, 0.5, 1.0e-300, 2.5e300, 31.875];
            let fraction = match fractions.get(below(state, fractions.len())) {
                Some(fraction) => *fraction,
                None => 0.0,
            };
            match Number::from_f64(fraction) {
                Some(number) => Value::Number(number),
                None => Value::Null,
            }
        }
    }
}

/// A drawn tree no deeper than `depth`.
fn drawn_value(state: &mut Seeded, depth: usize) -> Value {
    let kinds = if depth == 0 { 4 } else { 6 };
    match below(state, kinds) {
        0 => Value::Null,
        1 => Value::Bool(next_byte(state) & 1 == 1),
        2 => drawn_number(state),
        3 => Value::String(drawn_string(state)),
        4 => Value::Array(
            (0..below(state, 4))
                .map(|_| drawn_value(state, depth.saturating_sub(1)))
                .collect(),
        ),
        _ => Value::Object(drawn_object(state, depth.saturating_sub(1))),
    }
}

/// A drawn object whose members are no deeper than `depth`.
fn drawn_object(state: &mut Seeded, depth: usize) -> Map<String, Value> {
    (0..below(state, 4))
        .map(|_| (drawn_string(state), drawn_value(state, depth)))
        .collect()
}

/// Every drawn object document under each seed.
fn sweep(mut check: impl FnMut(u64, &mut Seeded, &Value) -> Outcome) -> Outcome {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..DRAWS {
            let document = Value::Object(drawn_object(&mut state, DEPTH));
            check(seed, &mut state, &document)?;
        }
    }
    Ok(())
}

#[test]
/// A tree round-trips through text and through bytes.
fn sim_a_tree_round_trips_through_text_and_bytes() -> Outcome {
    sweep(|seed, _, document| {
        let text = json::to_string(document)?;
        assert_eq!(&json::from_str::<Value>(&text)?, document, "seed {seed:#018x}: {text}");
        let bytes = json::to_vec(document)?;
        assert_eq!(&json::from_slice::<Value>(&bytes)?, document, "seed {seed:#018x}");
        Ok(())
    })
}

#[test]
/// Every encoder emits the same bytes: text, vector and writer.
fn sim_every_encoder_emits_the_same_bytes() -> Outcome {
    sweep(|seed, _, document| {
        let text = json::to_string(document)?;
        let mut written = Vec::new();
        json::to_writer(&mut written, document)?;
        assert_eq!(json::to_vec(document)?, text.as_bytes(), "seed {seed:#018x}");
        assert_eq!(written, text.as_bytes(), "seed {seed:#018x}");
        Ok(())
    })
}

#[test]
/// A reader and the value bridge decode the tree the text decoder does.
fn sim_the_reader_and_the_value_bridge_agree() -> Outcome {
    sweep(|seed, _, document| {
        let bytes = json::to_vec(document)?;
        let from_reader: Value = json::from_reader(bytes.as_slice())?;
        assert_eq!(&from_reader, document, "seed {seed:#018x}");
        let bridged: Value = json::from_value(json::to_value(document)?)?;
        assert_eq!(&bridged, document, "seed {seed:#018x}");
        Ok(())
    })
}

#[test]
/// Compact text holds no raw control, and pretty text parses to the same tree.
fn sim_compact_text_holds_no_raw_control() -> Outcome {
    sweep(|seed, _, document| {
        let compact = json::to_string(document)?;
        // RFC 8259 §7 requires escaping U+0000 through U+001F; DEL is the one
        // control a string may carry raw.
        assert!(
            compact.chars().all(|glyph| glyph >= ' '),
            "seed {seed:#018x}: a raw C0 control in {compact:?}"
        );
        let pretty = json::to_string_pretty(document)?;
        assert_eq!(&json::from_str::<Value>(&pretty)?, document, "seed {seed:#018x}");
        Ok(())
    })
}

#[test]
/// Every strict prefix of an object document is refused.
fn sim_every_strict_prefix_is_refused() -> Outcome {
    sweep(|seed, state, document| {
        let text = json::to_string(document)?;
        let boundaries: Vec<usize> = (0..text.len()).filter(|cut| text.is_char_boundary(*cut)).collect();
        let cut = boundaries.get(below(state, boundaries.len())).copied().ok_or("a boundary")?;
        let prefix = text.get(..cut).ok_or("a char boundary")?;
        assert!(
            json::from_str::<Value>(prefix).is_err(),
            "seed {seed:#018x}: the prefix {prefix:?} parsed"
        );
        Ok(())
    })
}

#[test]
/// Trailing whitespace is accepted and trailing content is refused.
fn sim_trailing_content_is_refused_and_whitespace_is_not() -> Outcome {
    sweep(|seed, state, document| {
        let text = json::to_string(document)?;
        let padding = [" ", "\n", "\r\n\t "];
        let padded = format!("{text}{}", padding.get(below(state, padding.len())).ok_or("pad")?);
        assert_eq!(&json::from_str::<Value>(&padded)?, document, "seed {seed:#018x}");
        let junk = ["x", "{}", "1", ",", "null"];
        let trailing = format!("{text} {}", junk.get(below(state, junk.len())).ok_or("junk")?);
        assert!(
            json::from_str::<Value>(&trailing).is_err(),
            "seed {seed:#018x}: trailing content parsed: {trailing:?}"
        );
        Ok(())
    })
}

#[test]
/// A stray invalid UTF-8 byte anywhere is refused.
fn sim_a_stray_invalid_byte_is_refused() -> Outcome {
    sweep(|seed, state, document| {
        let mut bytes = json::to_vec(document)?;
        let at = below(state, bytes.len().saturating_add(1));
        let poison = [0xFF_u8, 0xC0, 0x80, 0xF8];
        bytes.insert(at, poison.get(below(state, poison.len())).copied().ok_or("a poison byte")?);
        assert!(
            json::from_slice::<Value>(&bytes).is_err(),
            "seed {seed:#018x}: a stray byte at {at} decoded"
        );
        Ok(())
    })
}

#[test]
/// A non-finite float encodes as `null`, never as text JSON cannot carry.
fn sim_a_non_finite_float_encodes_as_null() -> Outcome {
    sweep(|seed, state, _| {
        let floats = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY];
        let float = floats.get(below(state, floats.len())).copied().ok_or("a float")?;
        assert_eq!(json::to_string(&float)?, "null", "seed {seed:#018x}: {float}");
        assert_eq!(json::to_value(&float)?, Value::Null, "seed {seed:#018x}: {float}");
        assert!(Number::from_f64(float).is_none(), "seed {seed:#018x}: {float}");
        Ok(())
    })
}

/// The replay trace of every document drawn under `seed`.
fn document_trace(seed: u64) -> u64 {
    let mut state = seeded_stream(seed);
    let mut trace = initial_trace();
    for _ in 0..DRAWS {
        let document = Value::Object(drawn_object(&mut state, DEPTH));
        match json::to_vec(&document) {
            Ok(bytes) => fold_bytes(&mut trace, &bytes),
            Err(_) => fold_bytes(&mut trace, b"refused"),
        }
    }
    trace
}

#[test]
/// The replay oracle: one seed, one document trace.
fn sim_the_same_seed_replays_the_same_documents() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(document_trace, seed);
    }
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn sim_distinct_seeds_draw_different_documents() {
    assert_distinct_seeds_diverge(document_trace, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
}
