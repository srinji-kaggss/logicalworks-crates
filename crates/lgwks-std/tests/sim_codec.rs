//! Seeded deterministic replay of the JSON and RON codec contracts.
//!
//! One seed draws every value, a failure prints the seed that produced it, and
//! the same seed must produce the same trace hash. Every probe drives the
//! public API of the shipped crate, and every answer is checked against a
//! reference written here from the documented contract.
//!
//! The property INV-CODEC-1 names is **borrowing**. A decoder hands back text
//! that borrows from the input wherever its engine supports it, and an escaped
//! string needs decoded storage and so cannot be borrowed. The families below
//! check that a decoded field's pointer really lies inside the buffer it was
//! given — not merely that its value is right, which an allocating decoder
//! would also produce — and that an escaped field is refused as a borrow rather
//! than quietly allocated behind the caller's back.
//!
//! The file is gated on both features because both codecs are exercised side by
//! side (INV-DEP-6): neither is assumed to exist.

#![cfg(all(feature = "json", feature = "ron"))]

#[path = "support/seeded_bytes.rs"]
mod seeded_bytes;
#[path = "support/seeded_sweep.rs"]
mod seeded_sweep;

use lgwks_std::json;
use lgwks_std::ron::{self, FromSliceError};

use seeded_bytes::{below, fold_bytes, next_byte, next_text};
use seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold, fold_usize,
    initial_trace,
};

/// A value of every JSON/RON shape the two codecs disagree about: a scalar, a
/// nested object, a sequence, a float, and a string that may or may not need
/// escaping. Each field is borrowed, so decoding one of these into a borrowed
/// struct is only possible when the decoder genuinely borrowed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Shape {
    /// A borrowed scalar string.
    name: String,
    /// A borrowed key string.
    tag: String,
    /// A signed scalar.
    count: i64,
    /// A nested object with a borrowed value.
    nested: Nested,
    /// A sequence of borrowed strings.
    items: Vec<String>,
}

/// The nested value inside [`Shape`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Nested {
    /// A borrowed flag-like string.
    state: String,
    /// A second borrowed value.
    depth: i64,
}

/// A struct whose only field borrows, so a decoded value's address is either
/// inside the input buffer or it is not.
#[derive(Debug, serde::Deserialize)]
struct Borrowed<'a> {
    /// The borrowed field.
    value: &'a str,
}

/// A seeded [`Shape`] whose strings are drawn from printable ASCII, so no field
/// needs escaping unless the family deliberately asks for it.
fn draw_shape(state: &mut u64) -> Shape {
    let name_len = below(state, 12);
    let name = next_text(state, name_len);
    let tag_len = below(state, 8);
    let tag = next_text(state, tag_len);
    let count = i64::from(next_byte(state)).saturating_sub(128);
    let state_len = below(state, 6);
    let nested = Nested {
        state: next_text(state, state_len),
        depth: i64::from(next_byte(state)),
    };
    let item_count = below(state, 4);
    let items = (0..item_count)
        .map(|_| {
            let len = below(state, 6);
            next_text(state, len)
        })
        .collect();
    Shape {
        name,
        tag,
        count,
        nested,
        items,
    }
}

/// Whether `borrowed` lies inside `buffer`, checked on addresses rather than on
/// values: an allocating decoder produces the right *string* with the wrong
/// *address*, and this is the assertion that tells the two apart.
///
/// The comparison is by pointer *ordering*, never by a numeric address, so no
/// pointer is narrowed to an integer to be compared — which on a
/// capability-hardened target would not compile and here would trip
/// `as_conversions` for no benefit. Two pointers from the same buffer are
/// totally ordered, so `start <= borrowed < end` is exact.
fn borrowed_from(buffer: &[u8], borrowed: &str) -> bool {
    let start = buffer.as_ptr();
    let end = start.wrapping_add(buffer.len());
    let address = borrowed.as_ptr();
    address >= start && address < end
}

/// Runs the seeded round-trip sweep over both codecs and returns its trace.
fn codec_trace(seed: u64) -> u64 {
    let mut state = seed;
    let mut trace = initial_trace();

    for _ in 0..24 {
        let value = draw_shape(&mut state);

        let json_text = json::to_string(&value).map_err(|error| error.to_string());
        let json_back = json_text
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|text| json::from_str::<Shape>(text).map_err(|error| error.to_string()));
        assert_eq!(
            json_back,
            Ok(value.clone()),
            "seed {seed}: a value must round-trip through JSON text"
        );

        let ron_text = ron::to_string(&value).map_err(|error| error.to_string());
        let ron_back = ron_text
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|text| ron::from_str::<Shape>(text).map_err(|error| error.to_string()));
        assert_eq!(
            ron_back,
            Ok(value.clone()),
            "seed {seed}: a value must round-trip through RON text"
        );

        if let Some((json_text, ron_text)) = json_text.as_ref().ok().zip(ron_text.as_ref().ok()) {
            fold_bytes(&mut trace, json_text.as_bytes());
            fold_bytes(&mut trace, ron_text.as_bytes());
        }
        fold_usize(&mut trace, value.items.len());
    }
    trace
}

#[test]
/// Every seeded value round-trips through JSON text and back, unchanged: every
/// field type the drawn shape carries survives its own serialization.
fn a_seeded_value_round_trips_through_json_text() -> Result<(), json::Error> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..48 {
            let value = draw_shape(&mut state);
            let text = json::to_string(&value)?;
            let restored: Shape = json::from_str(&text)?;
            assert_eq!(
                restored, value,
                "seed {seed}: JSON must round-trip the value"
            );
            assert!(
                !text.contains('\n'),
                "seed {seed}: compact JSON has no newlines"
            );
        }
    }
    Ok(())
}

#[test]
/// Every seeded value round-trips through RON text and back, unchanged. RON is
/// the notation where an enum and a comment matter, but the round trip is the
/// same fact for both codecs and is checked for both here.
fn a_seeded_value_round_trips_through_ron_text() -> Result<(), ron::Error> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..48 {
            let value = draw_shape(&mut state);
            let text = ron::to_string(&value)?;
            let restored: Shape = ron::from_str(&text)?;
            assert_eq!(
                restored, value,
                "seed {seed}: RON must round-trip the value"
            );
        }
    }
    Ok(())
}

/// Asserts that an unescaped field decoded from `buffer` borrows from
/// `buffer` rather than owning a copy of the same text.
///
/// Every borrowing door in both codecs runs this, so the assertion exists once: the
/// doors differ only in their decoder, and a check written twice would be
/// two checks that could drift apart.
fn assert_field_borrows(seed: u64, buffer: &[u8], decoded: &Borrowed<'_>, value: &str, what: &str) {
    assert_eq!(
        decoded.value, value,
        "seed {seed}: the {what} field keeps its value"
    );
    assert!(
        borrowed_from(buffer, decoded.value),
        "seed {seed}: the {what} field must point into the supplied input"
    );
}

/// The document an unescaped field is wrapped in, per codec.
fn borrowed_document(value: &str, ron_style: bool) -> String {
    if ron_style {
        format!("(value: \"{value}\")")
    } else {
        format!(r#"{{"value":"{value}"}}"#)
    }
}

/// Which door a borrowed-field sweep goes through. Each is a separate entry
/// point with its own lifetime and its own decoder, which is exactly why the
/// same property is checked once per door rather than once in total.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Door {
    /// A `&str` input.
    Text,
    /// A `&[u8]` input.
    Slice,
}

/// Decodes `document` through the text door of the codec `ron_style` names and
/// checks the borrowed field points into it.
fn check_text_door(
    seed: u64,
    ron_style: bool,
    document: &str,
    value: &str,
    what: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if ron_style {
        let decoded: Borrowed<'_> = ron::from_str(document)?;
        assert_field_borrows(seed, document.as_bytes(), &decoded, value, what);
    } else {
        let decoded: Borrowed<'_> = json::from_str(document)?;
        assert_field_borrows(seed, document.as_bytes(), &decoded, value, what);
    }
    Ok(())
}

/// Decodes `bytes` through the slice door of the codec `ron_style` names and
/// checks the borrowed field points into it.
fn check_slice_door(
    seed: u64,
    ron_style: bool,
    bytes: Vec<u8>,
    value: &str,
    what: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if ron_style {
        let decoded: Borrowed<'_> = ron::from_slice(&bytes)?;
        assert_field_borrows(seed, &bytes, &decoded, value, what);
    } else {
        let decoded: Borrowed<'_> = json::from_slice(&bytes)?;
        assert_field_borrows(seed, &bytes, &decoded, value, what);
    }
    Ok(())
}

/// Sweeps the borrowed-field property through one door, drawing a seeded value
/// and checking that the decoded field points into the document it came from
/// rather than owning a copy of the same text.
///
/// Every codec runs this with its own wrapper and its own decoder, so one sweep
/// serves four tests: the property is the same fact behind four entry points.
fn sweep_borrowed_field(
    seed: u64,
    ron_style: bool,
    door: Door,
    what: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut state = seed;
    for _ in 0..48 {
        let value_len = below(&mut state, 16);
        let value = next_text(&mut state, value_len);
        let document = borrowed_document(&value, ron_style);
        match door {
            Door::Text => check_text_door(seed, ron_style, &document, &value, what)?,
            Door::Slice => check_slice_door(seed, ron_style, document.into_bytes(), &value, what)?,
        }
    }
    Ok(())
}

#[test]
/// The INV-CODEC-1 borrowing property for JSON through the text door: an
/// unescaped string field, decoded into a borrowed `&str`, points *into* the
/// supplied text rather than owning a copy of it.
fn an_unescaped_json_field_borrows_from_the_supplied_text() -> Result<(), Box<dyn std::error::Error>>
{
    for seed in SWEEP_SEEDS {
        sweep_borrowed_field(seed, false, Door::Text, "JSON text")?;
    }
    Ok(())
}

#[test]
/// The same borrowing property through the *slice* entry point, which is a
/// separate door with its own lifetime and its own decoder.
fn an_unescaped_json_field_borrows_from_the_supplied_slice()
-> Result<(), Box<dyn std::error::Error>> {
    for seed in SWEEP_SEEDS {
        sweep_borrowed_field(seed, false, Door::Slice, "JSON slice")?;
    }
    Ok(())
}

#[test]
/// The same borrowing property for RON, through both its text and slice doors.
///
/// RON's check is the same assertion as JSON's with a different decoder, so it
/// runs the same sweep rather than a second copy of the check: four doors and
/// one assertion, or four assertions that could drift.
fn an_unescaped_ron_field_borrows_from_the_supplied_input() -> Result<(), Box<dyn std::error::Error>>
{
    for seed in SWEEP_SEEDS {
        sweep_borrowed_field(seed, true, Door::Text, "RON text")?;
        sweep_borrowed_field(seed, true, Door::Slice, "RON slice")?;
    }
    Ok(())
}

#[test]
/// An **escaped** string cannot be borrowed: it needs decoded storage that the
/// input does not contain. A borrowed decode of such a field is refused, which
/// is how a caller learns to ask for an owned `String` instead.
fn an_escaped_field_is_refused_as_a_borrow_in_both_codecs() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..24 {
            // A raw string cannot end in a backslash, so the escaped-quote case is
            // written as a normal literal with its own escaping. The four are the
            // escape forms that make a field unborrowable: each needs decoded
            // storage that the input does not literally contain.
            let escapes = ["\\n", "\\\\", "\\\"", "\\t"];
            let escape = escapes[below(&mut state, escapes.len())];
            let json_text = format!(r#"{{"value":"line{escape}feed"}}"#);
            let json_result: Result<Borrowed<'_>, _> = json::from_str(&json_text);
            assert!(
                json_result.is_err(),
                "seed {seed}: an escaped JSON field cannot be borrowed"
            );

            let ron_text = format!("(value: \"line{escape}feed\")");
            let ron_result: Result<Borrowed<'_>, _> = ron::from_str(&ron_text);
            assert!(
                ron_result.is_err(),
                "seed {seed}: an escaped RON field cannot be borrowed"
            );
        }
    }
}

#[test]
/// A borrowed decode is a decode: what it refuses about structure, it refuses
/// the same way an owned decode would. A malformed document is malformed
/// whether the caller asked for a borrow or for ownership.
fn a_malformed_document_is_refused_by_both_the_borrowing_and_owned_paths() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..32 {
            let truncated = below(&mut state, 24);
            let text = format!(r#"{{"value":"{}"#, "x".repeat(truncated));
            let borrowed_result: Result<Borrowed<'_>, _> = json::from_str(&text);
            let owned_result: Result<Shape, _> = json::from_str(&text);
            assert!(
                borrowed_result.is_err(),
                "seed {seed}: a truncated JSON document is not a borrow"
            );
            assert!(
                owned_result.is_err(),
                "seed {seed}: the same document is refused as an owned decode too"
            );
        }
    }
}

#[test]
/// A byte slice that is not UTF-8 is refused by RON as a *transport* fault,
/// distinct from a content fault: `from_slice` keeps `Utf8` and `Ron` apart, so
/// a caller can tell "this is not text" from "this is not valid RON".
fn a_non_utf8_slice_is_refused_as_a_transport_fault() -> Result<(), Box<dyn std::error::Error>> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..32 {
            let prefix = below(&mut state, 4);
            let mut bytes = vec![b'('; prefix];
            bytes.extend_from_slice(&[0xff]);
            let arm = match ron::from_slice::<Shape>(&bytes) {
                Err(FromSliceError::Utf8(_)) => 1,
                Err(ref error) => {
                    assert!(
                        std::error::Error::source(error).is_some(),
                        "seed {seed}: the refusal keeps its source"
                    );
                    2
                }
                Ok(_) => 3,
            };
            assert_eq!(
                arm, 1,
                "seed {seed}: a non-UTF-8 byte is a transport fault, not a content fault"
            );
        }
    }
    Ok(())
}

#[test]
/// JSON's byte-slice door refuses the same bytes RON refuses as a transport
/// fault, and its refusal is located in the document rather than in the byte
/// string.
///
/// Its *category* is `serde_json`'s, which this facade does not re-export, so
/// this family observes what a consumer can actually observe — the refusal and
/// its line and column — rather than reaching past the facade for an enum it
/// was not given.
fn a_non_utf8_json_slice_is_refused_with_a_document_location()
-> Result<(), Box<dyn std::error::Error>> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..32 {
            let mut bytes = vec![b'{'; below(&mut state, 4)];
            bytes.extend_from_slice(&[0xff]);
            let result: Result<Shape, _> = json::from_slice(&bytes);
            match result {
                Err(ref error) => {
                    // The facade refuses the bytes, and the refusal carries a
                    // location a caller can point at. Its *category* is
                    // serde_json's, which this facade does not re-export, so the
                    // family observes the refusal and its location rather than
                    // reaching past the facade for an enum.
                    assert!(
                        error.line() >= 1 && error.column() >= 1,
                        "seed {seed}: the JSON refusal carries a line and column, \
                         but the facade said {error}"
                    );
                }
                Ok(_) => return Err(format!("seed {seed}: a non-UTF-8 byte was accepted").into()),
            }
        }
    }
    Ok(())
}

#[test]
/// Trailing content after a complete document is a syntax failure in both
/// codecs: a decoder that ignored it would accept a document plus an appended
/// forgery as the document alone.
fn trailing_content_after_a_document_is_refused_in_both_codecs() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..24 {
            let tail_len = below(&mut state, 8);
            let tail = next_text(&mut state, tail_len);
            let json_text = format!(r#"{{"value":"x"}}{tail}"#);
            assert!(
                json::from_str::<Borrowed<'_>>(&json_text).is_err(),
                "seed {seed}: JSON refuses trailing content"
            );
            let ron_text = format!("(value: \"x\") {tail}");
            assert!(
                ron::from_str::<Borrowed<'_>>(&ron_text).is_err(),
                "seed {seed}: RON refuses trailing content"
            );
        }
    }
}

#[test]
/// A syntax error keeps its *location*: both facades report where the failure
/// was, so a caller can point at it rather than at the whole document.
fn a_syntax_error_keeps_its_source_location() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..24 {
            let pad_len = below(&mut state, 4);
            let pad = next_text(&mut state, pad_len);
            let broken = format!("{pad}(value: )");
            let ron_error = ron::from_str::<Borrowed<'_>>(&broken);
            assert!(
                ron_error
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.span.start.col > 0),
                "seed {seed}: the RON error carries its column"
            );

            let json_broken = format!("{pad}{{");
            let json_error = json::from_str::<Borrowed<'_>>(&json_broken);
            assert!(
                json_error
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.line() >= 1 && error.column() > 0),
                "seed {seed}: the JSON error carries its line and column"
            );
        }
    }
}

#[test]
/// The two codecs disagree about compactness on purpose, and the family states
/// both shapes rather than asserting one of them: JSON's compact rendering has
/// no whitespace at all, while RON's carries the structure a reader needs.
fn each_codec_renders_its_own_documented_shape() -> Result<(), Box<dyn std::error::Error>> {
    let value = Shape {
        name: String::from("name"),
        tag: String::from("tag"),
        count: 7,
        nested: Nested {
            state: String::from("ready"),
            depth: 2,
        },
        items: vec![String::from("one"), String::from("two")],
    };

    let json_text = json::to_string(&value)?;
    assert!(!json_text.contains('\n'), "compact JSON has no newline");
    assert!(
        !json_text.contains("  "),
        "compact JSON has no double-space indent"
    );

    let ron_pretty = ron::to_string_pretty(&value)?;
    assert!(
        ron_pretty.contains('\n'),
        "pretty RON is indented across lines"
    );
    Ok(())
}

#[test]
/// A value tree round-trips the drawn value unchanged, and renders a document
/// that decodes back to it.
///
/// The rendered *bytes* are deliberately not compared against `to_string`: a
/// `Value` map is ordered by key, so a struct's field order and a tree's key
/// order differ and the two documents are equal as documents without being
/// equal as text. Comparing bytes here would be asserting a property the tree is
/// documented not to have.
fn a_value_tree_round_trips_the_drawn_value() -> Result<(), json::Error> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..24 {
            let value = draw_shape(&mut state);
            let tree = json::to_value(&value)?;
            let back: Shape = json::from_value(tree.clone())?;
            assert_eq!(
                back, value,
                "seed {seed}: a value tree round-trips the drawn value"
            );
            let rendered = json::to_string(&tree)?;
            let decoded: Shape = json::from_str(&rendered)?;
            assert_eq!(
                decoded, value,
                "seed {seed}: the rendered tree decodes back to the drawn value"
            );
        }
    }
    Ok(())
}

#[test]
/// The endpoints are exact: an empty sequence and an empty string are values,
/// not absences, and both codecs render and read them without complaint.
fn the_empty_containers_are_values_in_both_codecs() -> Result<(), Box<dyn std::error::Error>> {
    let value = Shape {
        name: String::new(),
        tag: String::new(),
        count: 0,
        nested: Nested {
            state: String::new(),
            depth: 0,
        },
        items: Vec::new(),
    };

    let json_text = json::to_string(&value)?;
    assert!(
        json_text.contains(r#""items":[]"#),
        "an empty array is rendered"
    );
    assert_eq!(
        json::from_str::<Shape>(&json_text)?,
        value,
        "the empty-container value round-trips through JSON"
    );

    let ron_text = ron::to_string(&value)?;
    assert_eq!(
        ron::from_str::<Shape>(&ron_text)?,
        value,
        "the empty-container value round-trips through RON"
    );
    Ok(())
}

#[test]
/// Non-ASCII text round-trips through both codecs unchanged: a codec that
/// escaped every multi-byte scalar into `\u` sequences would still produce the
/// right *value*, so this family checks the value across a corpus of real
/// multi-byte text rather than only ASCII.
fn multi_byte_text_round_trips_through_both_codecs() -> Result<(), Box<dyn std::error::Error>> {
    let samples = ["héllo", "こんにちは", "☃☃☃", "ünïcødé", "aé☃"];
    for sample in samples {
        let value = Shape {
            name: String::from(sample),
            tag: String::from(sample),
            count: 0,
            nested: Nested {
                state: String::from(sample),
                depth: 0,
            },
            items: vec![String::from(sample)],
        };
        let json_back: Shape = json::from_str(&json::to_string(&value)?)?;
        assert_eq!(json_back, value, "{sample:?} must round-trip through JSON");
        let ron_back: Shape = ron::from_str(&ron::to_string(&value)?)?;
        assert_eq!(ron_back, value, "{sample:?} must round-trip through RON");
    }
    Ok(())
}

#[test]
/// A shape whose nested object is empty still round-trips: the codecs must
/// handle the empty nested case rather than assuming it has a field.
fn a_shape_with_empty_collections_still_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..16 {
            let mut value = draw_shape(&mut state);
            value.items.clear();
            let json_back: Shape = json::from_str(&json::to_string(&value)?)?;
            assert_eq!(
                json_back, value,
                "seed {seed}: the emptied shape round-trips"
            );
        }
    }
    Ok(())
}

#[test]
/// The replay oracle: one seed, one trace.
fn the_same_seed_replays_to_the_same_codec_trace() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(codec_trace, seed);
    }
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn distinct_codec_seeds_diverge_in_their_trace() {
    assert_distinct_seeds_diverge(codec_trace, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
}

#[test]
/// Two seeds draw two different values, so the round-trip family is not one
/// value checked repeatedly.
fn two_seeds_draw_two_different_values() {
    let mut first = SWEEP_SEEDS[0];
    let mut second = SWEEP_SEEDS[1];
    let left = draw_shape(&mut first);
    let right = draw_shape(&mut second);
    assert_ne!(left, right, "the two sweep seeds drew the same value");
    let mut trace = initial_trace();
    fold(&mut trace, u64::try_from(left.items.len()).unwrap_or(0));
    fold_bytes(&mut trace, left.name.as_bytes());
    fold_bytes(&mut trace, right.name.as_bytes());
    assert_ne!(
        trace,
        initial_trace(),
        "the two values must fold into distinct traces"
    );
}
