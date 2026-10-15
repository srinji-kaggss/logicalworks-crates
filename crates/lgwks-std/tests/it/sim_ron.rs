//! Seeded deterministic replay of the `ron` codec facade.
//!
//! One seed draws records — signed and unsigned extremes, finite floats, text
//! carrying quotes, backslashes, controls and multi-byte letters, optional and
//! repeated fields, every enum shape — and every family asserts what a caller
//! storing configuration in RON sees through the shipped public API:
//!
//! - every record round-trips through the compact, the pretty and the writer
//!   encodings, and the writer's bytes are the compact text exactly;
//! - compact text is one line, and pretty text parses to the same record;
//! - the slice decoder agrees with the text decoder on valid UTF-8 and
//!   refuses invalid UTF-8 as a transport fault, never as a content fault;
//! - every strict prefix of a document is refused, trailing whitespace is
//!   accepted and trailing content is refused;
//! - a writer that fails after `n` bytes holds exactly the first `n` bytes of
//!   the document and the failure is reported as a write, not a serialization;
//! - the same seed replays the same documents and distinct seeds diverge.
//!
//! Sources: the RON specification (ron-rs/ron `docs/grammar.md`), RFC 3629 for
//! what a valid UTF-8 byte sequence is, and INV-RON-SERDE, which makes serde
//! the one path every value takes.
#![cfg(feature = "ron")]

use std::error::Error;
use std::io::Write;

use crate::seeded_bytes::{below, fold_bytes, next_byte};
use crate::seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, initial_trace, next_seed,
    seeded_stream,
};

use lgwks_std::ron::{self, Deserialize, FromSliceError, Serialize, WriterError};
use lgwks_std::seeded::Seeded;

/// How many records each family draws per seed.
const DRAWS: usize = 64;

/// The glyphs drawn text is made of: plain letters, the characters RON must
/// escape inside a string, and multi-byte letters it must carry whole.
const GLYPHS: [char; 14] = [
    'a', 'Z', '7', ' ', '"', '\\', '\n', '\t', '\u{0}', 'é', '✓', '𝄞', '(', ',',
];

/// A family's result.
type Outcome = Result<(), Box<dyn Error>>;

/// Every enum shape RON spells differently.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::ron::serde")]
enum Shape {
    /// A unit variant.
    Empty,
    /// A newtype variant.
    Circle(u32),
    /// A tuple variant.
    Pair(i16, bool),
    /// A struct variant.
    Rect {
        /// Width.
        width: u32,
        /// Height.
        height: u32,
    },
}

/// A configuration-shaped record carrying every field kind drawn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::ron::serde")]
struct Record {
    /// A signed integer, drawn across its whole range.
    signed: i64,
    /// An unsigned integer, drawn across its whole range.
    unsigned: u64,
    /// A finite float, drawn from its bit pattern.
    ratio: f64,
    /// Text carrying quotes, escapes and multi-byte letters.
    label: String,
    /// A flag.
    enabled: bool,
    /// An optional field.
    limit: Option<u32>,
    /// A repeated field.
    bytes: Vec<u8>,
    /// An enum field.
    shape: Shape,
}

/// Up to 24 drawn glyphs.
fn drawn_text(state: &mut Seeded) -> String {
    // `below` keeps every draw inside the table, so the `None` arm names the
    // case rather than taking it.
    (0..below(state, 25))
        .map(|_| match GLYPHS.get(below(state, GLYPHS.len())) {
            Some(glyph) => *glyph,
            None => 'a',
        })
        .collect()
}

/// A finite float drawn from its bit pattern.
fn drawn_ratio(state: &mut Seeded) -> f64 {
    let bits = next_seed(state);
    let drawn = f64::from_bits(bits);
    if drawn.is_finite() {
        drawn
    } else {
        f64::from(next_byte(state))
    }
}

/// One drawn record, extremes included.
fn drawn_record(state: &mut Seeded) -> Record {
    let signed = match below(state, 4) {
        0 => i64::MIN,
        1 => i64::MAX,
        _ => i64::from_le_bytes(next_seed(state).to_le_bytes()),
    };
    let unsigned = match below(state, 3) {
        0 => u64::MAX,
        _ => next_seed(state),
    };
    let shape = match below(state, 4) {
        0 => Shape::Empty,
        1 => Shape::Circle(u32::from(next_byte(state))),
        2 => Shape::Pair(i16::from(next_byte(state)), next_byte(state) & 1 == 1),
        _ => Shape::Rect {
            width: u32::from(next_byte(state)),
            height: u32::from(next_byte(state)),
        },
    };
    Record {
        signed,
        unsigned,
        ratio: drawn_ratio(state),
        label: drawn_text(state),
        enabled: next_byte(state) & 1 == 1,
        limit: (next_byte(state) & 1 == 1).then(|| u32::from(next_byte(state))),
        bytes: (0..below(state, 6)).map(|_| next_byte(state)).collect(),
        shape,
    }
}

/// Every drawn record under each seed.
fn sweep(mut check: impl FnMut(u64, &mut Seeded, &Record) -> Outcome) -> Outcome {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..DRAWS {
            let record = drawn_record(&mut state);
            check(seed, &mut state, &record)?;
        }
    }
    Ok(())
}

#[test]
/// Every record round-trips through the compact text, bit-exactly.
fn sim_a_record_round_trips_through_compact_text() -> Outcome {
    sweep(|seed, _, record| {
        let text = ron::to_string(record)?;
        let restored: Record = ron::from_str(&text)?;
        assert_eq!(&restored, record, "seed {seed:#018x}: {text}");
        assert_eq!(
            restored.ratio.to_bits(),
            record.ratio.to_bits(),
            "seed {seed:#018x}"
        );
        Ok(())
    })
}

#[test]
/// Compact text is one line, and pretty text parses to the same record.
fn sim_pretty_text_parses_to_the_same_record() -> Outcome {
    sweep(|seed, _, record| {
        let compact = ron::to_string(record)?;
        assert!(
            !compact.contains('\n'),
            "seed {seed:#018x}: compact is one line"
        );
        let pretty = ron::to_string_pretty(record)?;
        let restored: Record = ron::from_str(&pretty)?;
        assert_eq!(&restored, record, "seed {seed:#018x}: {pretty}");
        assert_eq!(ron::to_string(&restored)?, compact, "seed {seed:#018x}");
        Ok(())
    })
}

#[test]
/// The writer's bytes are the compact text exactly.
fn sim_the_writer_writes_the_compact_text() -> Outcome {
    sweep(|seed, _, record| {
        let mut written = Vec::new();
        ron::to_writer(&mut written, record)?;
        assert_eq!(
            written,
            ron::to_string(record)?.into_bytes(),
            "seed {seed:#018x}"
        );
        Ok(())
    })
}

#[test]
/// The slice decoder agrees with the text decoder on valid UTF-8.
fn sim_the_slice_decoder_agrees_with_the_text_decoder() -> Outcome {
    sweep(|seed, _, record| {
        let text = ron::to_string(record)?;
        let from_slice: Record = ron::from_slice(text.as_bytes())?;
        let from_text: Record = ron::from_str(&text)?;
        assert_eq!(from_slice, from_text, "seed {seed:#018x}");
        Ok(())
    })
}

#[test]
/// Invalid UTF-8 anywhere is refused as a transport fault.
fn sim_invalid_utf8_is_a_transport_fault() -> Outcome {
    sweep(|seed, state, record| {
        let mut bytes = ron::to_string(record)?.into_bytes();
        let at = below(state, bytes.len().saturating_add(1));
        let poison = [0xFF_u8, 0xC0, 0x80, 0xFE];
        bytes.insert(
            at,
            poison
                .get(below(state, poison.len()))
                .copied()
                .ok_or("a poison byte")?,
        );
        let decoded: Result<Record, FromSliceError> = ron::from_slice(&bytes);
        assert!(
            matches!(decoded, Err(FromSliceError::Utf8(_))),
            "seed {seed:#018x}: a stray byte at {at} gave {decoded:?}"
        );
        Ok(())
    })
}

#[test]
/// Every strict prefix of a document is refused.
fn sim_every_strict_prefix_is_refused() -> Outcome {
    sweep(|seed, state, record| {
        let text = ron::to_string(record)?;
        let boundaries: Vec<usize> = (0..text.len())
            .filter(|cut| text.is_char_boundary(*cut))
            .collect();
        let cut = boundaries
            .get(below(state, boundaries.len()))
            .copied()
            .ok_or("a boundary")?;
        let prefix = text.get(..cut).ok_or("a char boundary")?;
        assert!(
            ron::from_str::<Record>(prefix).is_err(),
            "seed {seed:#018x}: the prefix {prefix:?} parsed"
        );
        Ok(())
    })
}

#[test]
/// Trailing whitespace is accepted and trailing content is refused.
fn sim_trailing_content_is_refused_and_whitespace_is_not() -> Outcome {
    sweep(|seed, state, record| {
        let text = ron::to_string(record)?;
        let padding = [" ", "\n", "\t\n  "];
        let padded = format!(
            "{text}{}",
            padding.get(below(state, padding.len())).ok_or("pad")?
        );
        assert_eq!(
            &ron::from_str::<Record>(&padded)?,
            record,
            "seed {seed:#018x}"
        );
        let junk = ["x", "()", "1", ",", "\"s\""];
        let trailing = format!(
            "{text} {}",
            junk.get(below(state, junk.len())).ok_or("junk")?
        );
        assert!(
            ron::from_str::<Record>(&trailing).is_err(),
            "seed {seed:#018x}: trailing content parsed: {trailing:?}"
        );
        Ok(())
    })
}

/// A writer that accepts at most `remaining` bytes and then fails.
struct FailAfter {
    /// Bytes still accepted.
    remaining: usize,
    /// What was accepted.
    accepted: Vec<u8>,
}

impl Write for FailAfter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        match self.remaining {
            0 => Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "sim pipe closed",
            )),
            remaining => {
                let taken = bytes.len().min(remaining);
                let prefix = bytes
                    .get(..taken)
                    .ok_or_else(|| std::io::Error::other("a prefix longer than the buffer"))?;
                self.accepted.extend_from_slice(prefix);
                self.remaining = remaining.saturating_sub(taken);
                Ok(taken)
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
/// A writer that fails after `n` bytes holds exactly the document's first
/// `n` bytes, and the failure is a write.
fn sim_a_failing_writer_holds_exactly_the_documents_prefix() -> Outcome {
    sweep(|seed, state, record| {
        let document = ron::to_string(record)?.into_bytes();
        let limit = below(state, document.len().saturating_add(2));
        let mut writer = FailAfter {
            remaining: limit,
            accepted: Vec::new(),
        };
        let written = ron::to_writer(&mut writer, record);
        let expected = document
            .get(..limit.min(document.len()))
            .ok_or("a prefix")?;
        assert_eq!(
            writer.accepted, expected,
            "seed {seed:#018x}: limit {limit}"
        );
        if limit >= document.len() {
            assert!(
                written.is_ok(),
                "seed {seed:#018x}: room for the whole document"
            );
        } else {
            assert!(
                matches!(written, Err(WriterError::Write(_))),
                "seed {seed:#018x}: {written:?}"
            );
        }
        Ok(())
    })
}

/// The replay trace of every document drawn under `seed`.
fn document_trace(seed: u64) -> u64 {
    let mut state = seeded_stream(seed);
    let mut trace = initial_trace();
    for _ in 0..DRAWS {
        let record = drawn_record(&mut state);
        match ron::to_string(&record) {
            Ok(text) => fold_bytes(&mut trace, text.as_bytes()),
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
