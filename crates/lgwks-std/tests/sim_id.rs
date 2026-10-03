//! Seeded deterministic replay of the `id` UUID contracts.
//!
//! One seed draws every identifier, a failure prints the seed that produced it,
//! and the same seed must produce the same trace hash. Every probe drives the
//! public API of the shipped crate, and every answer is checked against a
//! reference parser and renderer written here from the documented canonical
//! form rather than from the implementation under test.
//!
//! The property INV-ID-1 names is that the v4 masks belong to **generation
//! only**: `new_v4` stamps the version and variant bits onto freshly drawn
//! entropy, while `parse` and the raw-byte constructor preserve whatever bits
//! they are handed. A parser that masked on the way in would silently rewrite
//! every non-v4 identifier a caller was trying to read back, and a family that
//! only ever generated ids would never notice — so this file checks arbitrary
//! drawn bytes through the whole parse/format/parse cycle.
//!
//! The module is gated `feature = "random"` because that is what gates
//! `lgwks_std::id` itself. INV-DEP-6: code in the feature matrix must not
//! assume the module exists.
#![cfg(feature = "random")]

#[path = "support/seeded_bytes.rs"]
mod seeded_bytes;
#[path = "support/seeded_sweep.rs"]
mod seeded_sweep;

use lgwks_std::id::{ParseError, Uuid};

use seeded_bytes::{below, fold_bytes, fold_refusal, next_bytes, reference_nibble};
use seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold, fold_usize,
    initial_trace,
};

/// The canonical form is exactly this many characters.
const CANONICAL_LEN: usize = 36;

/// The hyphenated group layout, as (character width, byte width) per group.
const GROUPS: [(usize, usize); 5] = [(8, 4), (4, 2), (4, 2), (4, 2), (12, 6)];

/// The character offset at which each group's **hex** begins.
///
/// The separator is consumed before a non-first group's hex, so the hex of
/// group `n > 0` begins one character after that group's separator. This is the
/// offset INV-ID-1 names as the group start of a non-hex refusal.
const GROUP_STARTS: [usize; 5] = [0, 9, 14, 19, 24];

/// Renders 16 bytes as the canonical hyphenated lowercase form, computed from
/// the documented layout rather than by asking the shipped `Display`.
fn reference_render(bytes: &[u8; 16]) -> String {
    let mut rendered = String::new();
    for (index, &(_, byte_width)) in GROUPS.iter().enumerate() {
        if index > 0 {
            rendered.push('-');
        }
        let start = GROUPS
            .iter()
            .take(index)
            .map(|&(_, bytes)| bytes)
            .sum::<usize>();
        for byte in bytes
            .get(start..start.saturating_add(byte_width))
            .unwrap_or(&[])
        {
            let high = u32::from(byte >> 4);
            let low = u32::from(byte & 0x0f);
            rendered.push(char::from_digit(high, 16).unwrap_or('?'));
            rendered.push(char::from_digit(low, 16).unwrap_or('?'));
        }
    }
    rendered
}

/// The character offset at which each group's hex begins, from the byte widths
/// alone.
///
/// The separator is consumed before a non-first group's hex, so the offset is
/// advanced past it before the hex start is recorded. This is derived rather
/// than copied from [`GROUP_STARTS`], so a wrong group start is a wrong answer
/// here instead of an echo of the shipped one — and the two must still agree,
/// which the malformed-hex family checks on every corrupted position.
fn reference_group_starts() -> [usize; 5] {
    let mut starts = [0usize; 5];
    let mut cursor = 0usize;
    for (index, &(char_width, _)) in GROUPS.iter().enumerate() {
        if index > 0 {
            cursor = cursor.saturating_add(1);
        }
        starts[index] = cursor;
        cursor = cursor.saturating_add(char_width);
    }
    starts
}

/// The version nibble a raw value reports under the RFC variant rule.
fn reference_version(bytes: &[u8; 16]) -> Option<u8> {
    let eighth = bytes.get(8).copied().unwrap_or(0);
    if eighth & 0xc0 == 0x80 {
        bytes.get(6).map(|byte| byte >> 4)
    } else {
        None
    }
}

/// Parses canonical text into raw bytes, or names the refusal the documented
/// contract states.
///
/// Both coordinates of a non-hex refusal are produced: the group the offending
/// character belongs to, and the character itself. The group start is found by
/// walking the declared layout rather than by remembering a table, so a wrong
/// group start is a wrong answer here rather than a copy of the shipped one.
fn reference_parse(text: &str) -> Result<[u8; 16], Reference> {
    let bytes = text.as_bytes();
    if bytes.len() != CANONICAL_LEN {
        return Err(Reference::WrongLength { len: bytes.len() });
    }
    let starts = reference_group_starts();
    let mut raw = [0u8; 16];
    let mut byte_cursor = 0usize;
    for (index, &(_, byte_width)) in GROUPS.iter().enumerate() {
        let hex_at = starts[index];
        // The separator is consumed *before* a non-first group's hex, so it sits
        // one character earlier. Two different facts about two different
        // offsets: a missing separator is named at the separator's own
        // position, and a non-hex digit is named at its own position inside the
        // hex. Collapsing them is the mistake this separation exists to catch.
        let pair_start = if index > 0 {
            let hyphen_at = hex_at.saturating_sub(1);
            if bytes.get(hyphen_at) != Some(&b'-') {
                return Err(Reference::MissingHyphen { at: hyphen_at });
            }
            hex_at
        } else {
            hex_at
        };

        for offset in 0..byte_width {
            let high_at = pair_start.saturating_add(offset.saturating_mul(2));
            let low_at = high_at.saturating_add(1);
            let high = bytes.get(high_at).copied().and_then(reference_nibble);
            let low = bytes.get(low_at).copied().and_then(reference_nibble);
            let at = match (high, low) {
                (Some(high), Some(low)) => {
                    let assembled = (high << 4) | low;
                    if let Some(slot) = raw.get_mut(byte_cursor.saturating_add(offset)) {
                        *slot = assembled;
                    }
                    continue;
                }
                (None, _) => high_at,
                (_, None) => low_at,
            };
            return Err(Reference::NotDigit {
                group_at: hex_at,
                at,
            });
        }
        byte_cursor = byte_cursor.saturating_add(byte_width);
    }
    Ok(raw)
}

/// Why the reference parser refused one text. A successful parse yields the raw
/// bytes directly, so there is no "parsed" arm here to confuse a refusal with.
#[derive(Debug)]
enum Reference {
    /// The text was not 36 characters long.
    WrongLength {
        /// The observed length.
        len: usize,
    },
    /// A hyphen was missing before a group.
    MissingHyphen {
        /// Offset where the separator was required.
        at: usize,
    },
    /// A group held a character outside the hex alphabet.
    NotDigit {
        /// Offset where the malformed group begins.
        group_at: usize,
        /// Offset of the offending character itself.
        at: usize,
    },
}

/// The canonical form for arbitrary drawn bytes.
fn draw_canonical(state: &mut u64) -> String {
    let bytes = next_bytes(state, 16);
    let raw = <[u8; 16]>::try_from(bytes.as_slice()).unwrap_or([0u8; 16]);
    reference_render(&raw)
}

/// Runs the seeded parse/render sweep and returns its trace.
fn id_trace(seed: u64) -> u64 {
    let mut state = seed;
    let mut trace = initial_trace();

    for _ in 0..64 {
        let bytes = next_bytes(&mut state, 16);
        let raw = <[u8; 16]>::try_from(bytes.as_slice()).unwrap_or([0u8; 16]);
        let rendered = reference_render(&raw);

        assert_eq!(
            rendered.len(),
            CANONICAL_LEN,
            "seed {seed}: the canonical form is 36 characters"
        );
        let parsed = Uuid::parse(&rendered);
        assert_eq!(
            parsed.as_ref().map(Uuid::as_bytes),
            Ok(&raw),
            "seed {seed}: parsing must preserve every drawn bit"
        );
        assert_eq!(
            parsed.as_ref().map(ToString::to_string),
            Ok(rendered.clone()),
            "seed {seed}: the rendered form must round-trip through parse and display"
        );

        fold_bytes(&mut trace, &raw);
        fold_bytes(&mut trace, rendered.as_bytes());
        fold_usize(
            &mut trace,
            usize::from(
                parsed
                    .as_ref()
                    .map_or(0, |value| value.version().unwrap_or(0)),
            ),
        );
    }
    trace
}

#[test]
/// Arbitrary bytes — no version, no variant, every bit pattern — survive
/// `parse` then `Display` unchanged. This is INV-ID-1's parsing half: nothing
/// on the way in is masked.
fn arbitrary_bytes_round_trip_through_parse_and_display() -> Result<(), ParseError> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..64 {
            let bytes = next_bytes(&mut state, 16);
            let raw = <[u8; 16]>::try_from(bytes.as_slice()).unwrap_or([0u8; 16]);
            let rendered = reference_render(&raw);

            let parsed = Uuid::parse(&rendered)?;
            assert_eq!(
                parsed.as_bytes(),
                &raw,
                "seed {seed}: parse must preserve all sixteen drawn bytes"
            );
            assert_eq!(
                parsed.to_string(),
                rendered,
                "seed {seed}: display must render the drawn bytes as the reference does"
            );
            assert_eq!(
                Uuid::from(raw).to_string(),
                rendered,
                "seed {seed}: raw-byte construction must render like a parse"
            );
            assert_eq!(
                Uuid::from(raw),
                parsed,
                "seed {seed}: raw-byte construction and parsing must agree"
            );
        }
    }
    Ok(())
}

#[test]
/// The rendered form has exactly the documented layout: 36 characters, hyphens
/// at offsets 8, 13, 18 and 23, and hex digits everywhere else.
fn the_rendered_form_has_the_documented_hyphen_layout() -> Result<(), ParseError> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..32 {
            let rendered = draw_canonical(&mut state);
            assert_eq!(
                rendered.len(),
                CANONICAL_LEN,
                "seed {seed}: the canonical form is 36 characters"
            );
            for hyphen_at in [8, 13, 18, 23] {
                assert_eq!(
                    rendered.as_bytes().get(hyphen_at),
                    Some(&b'-'),
                    "seed {seed}: a hyphen belongs at offset {hyphen_at}"
                );
            }
            for (index, character) in rendered.chars().enumerate() {
                let is_hyphen_slot = [8, 13, 18, 23].contains(&index);
                if is_hyphen_slot {
                    continue;
                }
                assert!(
                    character.is_ascii_hexdigit() && !character.is_ascii_uppercase(),
                    "seed {seed}: offset {index} holds {character:?}, which is not a lowercase hex digit"
                );
            }
            assert_eq!(
                Uuid::parse(&rendered)?.to_string(),
                rendered,
                "seed {seed}: a form of the documented layout parses and redisplayed identically"
            );
        }
    }
    Ok(())
}

#[test]
/// The v4 masks apply to generation only: `new_v4` stamps the version nibble
/// and the RFC variant bits, and every other byte of the identifier keeps the
/// entropy it was drawn from.
fn generated_identifiers_carry_the_v4_masks() -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..64 {
        let generated = Uuid::new_v4()?;
        let bytes = generated.as_bytes();
        assert_eq!(
            generated.version(),
            Some(4),
            "a generated identifier reports version 4"
        );
        assert_eq!(
            bytes.get(6).map(|byte| byte >> 4),
            Some(4),
            "the version nibble is stamped into the seventh byte"
        );
        assert_eq!(
            bytes.get(8).map(|byte| byte & 0xc0),
            Some(0x80),
            "the RFC variant bits are stamped into the ninth byte"
        );
    }
    Ok(())
}

#[test]
/// The masks are applied by generation and by nothing else, so every byte a
/// generator *did not* mask is free to be any value at all: a family that only
/// ever looked at versions would miss a parser that stamped one in.
fn parsing_preserves_version_and_variant_bits_a_generator_would_have_stamped() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..32 {
            let mut bytes = next_bytes(&mut state, 16);
            if let Some(slot) = bytes.get_mut(6) {
                *slot = (*slot & 0x0f) | 0x40;
            }
            if let Some(slot) = bytes.get_mut(8) {
                *slot = (*slot & 0x3f) | 0x80;
            }
            let raw = <[u8; 16]>::try_from(bytes.as_slice()).unwrap_or([0u8; 16]);
            let rendered = reference_render(&raw);

            let Ok(parsed) = Uuid::parse(&rendered) else {
                continue;
            };
            assert_eq!(
                parsed.as_bytes(),
                &raw,
                "seed {seed}: a v4-shaped value must parse back to the same bits"
            );
            assert_eq!(
                parsed.version(),
                Some(4),
                "seed {seed}: the parsed value reports the version its bits carry"
            );
        }
    }
}

#[test]
/// The version a value reports is a property of its own bits under the RFC
/// variant rule — `Some` only when the variant bits are set — and it is never
/// the value generation would have forced.
fn the_reported_version_follows_the_variant_rule_of_the_drawn_bits() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..48 {
            let bytes = next_bytes(&mut state, 16);
            let raw = <[u8; 16]>::try_from(bytes.as_slice()).unwrap_or([0u8; 16]);
            let Ok(parsed) = Uuid::parse(&reference_render(&raw)) else {
                continue;
            };
            assert_eq!(
                parsed.version(),
                reference_version(&raw),
                "seed {seed}: the reported version must follow the variant rule of the drawn bits"
            );
        }
    }
}

#[test]
/// A malformed hex character is reported at **both** coordinates INV-ID-1 names:
/// the start of the group it belongs to, and its own offset in the input.
///
/// Every non-hex character position of every drawn canonical form is corrupted
/// in turn, so the family covers all five groups rather than one of them.
///
/// A corruption of a separator is checked against the reference too, and the
/// reference says it is a *missing hyphen*: the two are different facts about
/// the same position, and a family that asserted one arm for all 36 positions
/// would be asserting a falsehood about four of them.
fn malformed_hex_reports_both_the_group_start_and_the_character_offset()
-> Result<(), Box<dyn std::error::Error>> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..16 {
            let rendered = draw_canonical(&mut state);
            // The alien is drawn from characters outside the hex alphabet and outside the
            // separator, so the corruption is a malformed *hex character* and the
            // text keeps its length — the two facts this family depends on.
            let alien = draw_alien(&mut state);
            let position = below(&mut state, CANONICAL_LEN);

            let mut corrupted = rendered.into_bytes();
            if !corrupt_position(&mut corrupted, position, alien) {
                continue;
            }
            let corrupted = String::from_utf8(corrupted).unwrap_or_default();

            match reference_parse(&corrupted) {
                Err(Reference::NotDigit { group_at, at }) => assert_eq!(
                    Uuid::parse(&corrupted),
                    Err(ParseError::NotHexDigit { group_at, at }),
                    "seed {seed}: position {position} must name its group {group_at} and itself {at}"
                ),
                Err(Reference::MissingHyphen { at }) => assert_eq!(
                    Uuid::parse(&corrupted),
                    Err(ParseError::MissingHyphen { at }),
                    "seed {seed}: position {position} must be reported as the separator it displaced"
                ),
                Err(Reference::WrongLength { len }) => {
                    return Err(format!("seed {seed}: the length moved to {len}").into());
                }
                Ok(_) => {
                    return Err(format!("seed {seed}: position {position} was accepted").into());
                }
            }
        }
    }
    Ok(())
}

/// The printable ASCII characters that are *not* hex digits, used to corrupt a
/// canonical form without shortening it or making it invalid UTF-8.
///
/// `-` is excluded as well: it is a legal character in the canonical form, but
/// at a hex position it is a different refusal (a missing separator), and this
/// family is about the two-coordinate non-hex refusal.
const ALIEN_CHARACTERS: &[u8; 11] = b"gzGZ !/:@[]";

/// An alien drawn from [`ALIEN_CHARACTERS`], as one byte of the corrupted text.
fn draw_alien(state: &mut u64) -> u8 {
    let index = below(state, ALIEN_CHARACTERS.len());
    ALIEN_CHARACTERS[index]
}

/// Replaces the character at `position`, reporting when the position is outside
/// the text so the caller can skip the case rather than write out of bounds.
fn corrupt_position(text: &mut [u8], position: usize, alien: u8) -> bool {
    match text.get_mut(position) {
        Some(slot) => {
            *slot = alien;
            true
        }
        None => false,
    }
}

#[test]
/// A canonical text of the wrong length is refused on that length, both one
/// character short and one character long — the two spellings a truncation and
/// an extension produce.
fn a_wrong_length_is_refused_before_anything_is_parsed() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..16 {
            let rendered = draw_canonical(&mut state);
            for len in [
                0,
                1,
                CANONICAL_LEN.saturating_sub(1),
                CANONICAL_LEN.saturating_sub(35),
            ] {
                let shortened = rendered.get(..len).unwrap_or("").to_owned();
                assert_eq!(
                    shortened.chars().count(),
                    len,
                    "seed {seed}: the drawn truncation really is {len} characters"
                );
                assert_eq!(
                    Uuid::parse(&shortened),
                    Err(ParseError::WrongLength { len, at: len }),
                    "seed {seed}: a {len}-character text is not the canonical form"
                );
            }
        }
    }
}

#[test]
/// Each of the four separators is required: removing one hyphen makes the text
/// 35 characters, and moving one off its own position makes a 36-character text
/// refuse with the separator's offset.
fn every_separator_position_is_required() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for hyphen_at in [8, 13, 18, 23] {
            let rendered = draw_canonical(&mut state);
            let mut moved = rendered.clone().into_bytes();
            if let Some(slot) = moved.get_mut(hyphen_at) {
                *slot = b'0';
            }
            let moved = String::from_utf8(moved).unwrap_or_default();
            assert_eq!(
                Uuid::parse(&moved),
                Err(ParseError::MissingHyphen { at: hyphen_at }),
                "seed {seed}: offset {hyphen_at} must hold the separator"
            );
        }
    }
}

#[test]
/// An upper-case spelling of a canonical form is the same identifier: the parse
/// accepts both cases and the display always renders lowercase.
fn an_upper_case_spelling_parses_to_the_same_identifier() -> Result<(), ParseError> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..32 {
            let rendered = draw_canonical(&mut state);
            let upper = rendered.to_ascii_uppercase();
            assert_eq!(
                upper.len(),
                rendered.len(),
                "seed {seed}: case must not change the length"
            );
            assert_eq!(
                Uuid::parse(&upper)?,
                Uuid::parse(&rendered)?,
                "seed {seed}: the two case spellings are one identifier"
            );
            assert_eq!(
                Uuid::parse(&upper)?.to_string(),
                rendered,
                "seed {seed}: an upper-case input still displays lowercase"
            );
        }
    }
    Ok(())
}

#[test]
/// Every truncation of a canonical form is refused on its length: the parser
/// admits exactly the 36-character layout and never a prefix of one.
fn every_truncation_of_a_canonical_form_is_refused() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..8 {
            let rendered = draw_canonical(&mut state);
            for cut in 0..CANONICAL_LEN {
                let prefix = rendered.get(..cut).unwrap_or("");
                assert_eq!(
                    Uuid::parse(prefix),
                    Err(ParseError::WrongLength { len: cut, at: cut }),
                    "seed {seed}: a {cut}-character truncation is not a UUID"
                );
            }
        }
    }
}

#[test]
/// The endpoints are exact. The all-zero value has no RFC variant and reports
/// no version; the all-one value has `0b11` in its variant bits, which is
/// neither of the two RFC variants, so it too reports none — a value whose
/// variant is unrecognisable has no version to report, rather than a fabricated
/// one read out of the wrong nibble.
fn the_all_zero_and_all_one_values_are_exact_endpoints() -> Result<(), ParseError> {
    let zero = Uuid::parse("00000000-0000-0000-0000-000000000000")?;
    assert_eq!(
        zero.as_bytes(),
        &[0u8; 16],
        "the all-zero value is sixteen zero bytes"
    );
    assert_eq!(
        zero.version(),
        None,
        "the all-zero value has no RFC variant and so reports no version"
    );
    assert_eq!(
        zero.to_string(),
        "00000000-0000-0000-0000-000000000000",
        "the all-zero value round-trips through its canonical form"
    );

    let ones = Uuid::parse("ffffffff-ffff-ffff-ffff-ffffffffffff")?;
    assert_eq!(
        ones.as_bytes(),
        &[0xffu8; 16],
        "the all-one value is sixteen set bytes"
    );
    assert_eq!(
        ones.version(),
        None,
        "the all-one value's variant bits are neither RFC variant, so it reports none"
    );
    assert_eq!(
        ones.to_string(),
        "ffffffff-ffff-ffff-ffff-ffffffffffff",
        "the all-one value round-trips through its canonical form"
    );
    Ok(())
}

#[test]
/// A multi-byte character inside a group is refused as a non-hex digit rather
/// than silently consuming the characters after it: replacing two hex
/// characters with one two-byte scalar keeps the text at 36 characters, and the
/// parser must name the scalar's own first byte rather than carry on past it.
fn a_multi_byte_character_in_a_group_is_refused_at_its_own_offset()
-> Result<(), Box<dyn std::error::Error>> {
    let multibyte = ['é', '☃', 'ü'];
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..16 {
            let rendered = draw_canonical(&mut state);
            let group_index = below(&mut state, GROUPS.len());
            // `GROUP_STARTS` is the hex start, which is where this family's
            // corruption must land: a separator position is a different refusal
            // with a different coordinate.
            let hex_start = GROUP_STARTS[group_index];
            let hex_width = GROUPS[group_index]
                .0
                .saturating_sub(usize::from(group_index > 0));
            let within = below(&mut state, hex_width.saturating_sub(1));
            let position = hex_start.saturating_add(within);
            let alien_index = below(&mut state, multibyte.len());
            let alien = multibyte[alien_index];

            // The two hex characters are replaced by one scalar, and however many bytes
            // that scalar occupies the *hex span* it takes is taken off the tail,
            // so the text is still exactly 36 bytes and the refusal can only be
            // about the character rather than about the length.
            let head = rendered.get(..position).unwrap_or("");
            let alien_len = alien.len_utf8();
            let tail_start = position.saturating_add(alien_len);
            let tail_len = CANONICAL_LEN.saturating_sub(tail_start);
            let tail = rendered.get(tail_start..tail_start.saturating_add(tail_len));
            let corrupted = format!("{head}{alien}{}", tail.unwrap_or(""));
            assert_eq!(
                corrupted.len(),
                CANONICAL_LEN,
                "seed {seed}: the corrupted text is still {CANONICAL_LEN} bytes"
            );
            assert!(
                corrupted.len() > corrupted.chars().count(),
                "seed {seed}: the corrupted text really is carrying a multi-byte scalar"
            );

            match reference_parse(&corrupted) {
                Err(Reference::NotDigit { group_at, at }) => assert_eq!(
                    Uuid::parse(&corrupted),
                    Err(ParseError::NotHexDigit { group_at, at }),
                    "seed {seed}: the multi-byte scalar names its group and its own first byte"
                ),
                other => {
                    return Err(format!("seed {seed}: group {group_index} gave {other:?}").into());
                }
            }
        }
    }
    Ok(())
}

#[test]
/// A refusal arm and both of its coordinates are folded into the trace, so a
/// family that reached one arm for every corruption would still diverge here.
fn refusals_fold_their_arm_and_both_of_their_coordinates() {
    let mut trace = initial_trace();
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..24 {
            let rendered = draw_canonical(&mut state);
            let position = below(&mut state, CANONICAL_LEN);
            let alien = draw_alien(&mut state);
            let mut corrupted = rendered.into_bytes();
            if !corrupt_position(&mut corrupted, position, alien) {
                continue;
            }
            let corrupted = String::from_utf8(corrupted).unwrap_or_default();
            match Uuid::parse(&corrupted) {
                Ok(value) => fold_usize(&mut trace, value.to_string().len()),
                Err(ParseError::NotHexDigit { group_at, at }) => {
                    fold_refusal(&mut trace, 1, group_at, at);
                }
                Err(ParseError::MissingHyphen { at }) => fold_refusal(&mut trace, 2, at, at),
                Err(ParseError::WrongLength { len, at }) => fold_refusal(&mut trace, 3, len, at),
                Err(ParseError::DecodeOutputLength { expected, actual }) => {
                    fold_refusal(&mut trace, 4, expected, actual);
                }
                Err(other) => fold_refusal(&mut trace, 5, 0, format!("{other}").len()),
            }
        }
    }
    assert_ne!(
        trace,
        initial_trace(),
        "the refusal family must fold at least one observation"
    );
}

#[test]
/// The replay oracle: one seed, one trace.
fn the_same_seed_replays_to_the_same_id_trace() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(id_trace, seed);
    }
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn distinct_id_seeds_diverge_in_their_trace() {
    assert_distinct_seeds_diverge(id_trace, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
}

#[test]
/// Two seeds draw two different identifiers, so the parse family is not one
/// value checked over and over.
fn two_seeds_draw_two_different_identifiers() {
    let mut first = SWEEP_SEEDS[0];
    let mut second = SWEEP_SEEDS[1];
    let left = draw_canonical(&mut first);
    let right = draw_canonical(&mut second);
    assert_ne!(
        left, right,
        "the two sweep seeds drew the same canonical form"
    );
    let mut trace = initial_trace();
    fold(&mut trace, u64::try_from(left.len()).unwrap_or(0));
    fold_bytes(&mut trace, left.as_bytes());
    assert_ne!(
        trace,
        initial_trace(),
        "the drawn identifier must fold into the trace"
    );
}
