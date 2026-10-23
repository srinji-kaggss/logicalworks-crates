//! A peer controls the bytes a digest covers and the text a digest is read from.
//!
//! A content digest is an identity, so the attacks are the ones that make two
//! contents share one (a concatenation that conflates field boundaries, the
//! ambiguity bugs behind many signed-record forgeries), that make one content
//! have two (a second spelling of the digest's own text), or that leak through
//! timing. The attacks: every split of a stream into parts, the empty part, a
//! length prefix smuggled inside a part, every position and byte value of a hex
//! digest, every single-bit fault in the input and the key, chunk sizes at every
//! BLAKE3 block and chunk boundary, and sixteen megabytes fed in awkward pieces.
//!
//! The references are the BLAKE3 specification's published vectors, the property
//! that streaming equals one-shot at every length, and a brute-force count over
//! every pair of short strings for the framing.
#![cfg(feature = "hash")]

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher as _};

use lgwks_std::hash::{Digest, DigestParseError, Hasher, blake3, keyed};
use lgwks_std::hex::DecodeError;

use crate::adversary::{
    Attacker, TestResult, as_text, decoder_campaign, diverges_between_seeds,
    every_byte_at_every_position, flip_bit, replays_under_every_seed, sweep,
};

/// A valid digest text, used where a template is needed.
const SAMPLE: &[u8; 64] = b"af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

/// Message lengths at and around every BLAKE3 block (64) and chunk (1024)
/// boundary.
const BOUNDARIES: [usize; 16] = [
    0, 1, 63, 64, 65, 127, 128, 129, 1023, 1024, 1025, 2047, 2048, 2049, 4096, 8192,
];

/// Piece sizes a caller might stream in, again at the boundaries.
const PIECES: [usize; 8] = [1, 7, 63, 64, 65, 1023, 1024, 1025];

/// What reading a digest from text came to, flattened so it can be compared.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// The text was a digest with these bytes.
    Digest([u8; 32]),
    /// The text had this many bytes instead of sixty-four.
    Length(usize),
    /// The character at `at`, with this value, is not a hex digit.
    Digit {
        /// Offset of the offending character.
        at: usize,
        /// Its value.
        byte: u8,
    },
    /// Any other refusal, as its debug text.
    Other(String),
}

/// What `Digest::from_hex` made of `text`.
fn verdict(text: &str) -> Verdict {
    match Digest::from_hex(text) {
        Ok(digest) => Verdict::Digest(*digest.as_bytes()),
        Err(DigestParseError::WrongLength { len }) => Verdict::Length(len),
        Err(DigestParseError::Hex(DecodeError::NotHexDigit { at, byte })) => {
            Verdict::Digit { at, byte }
        }
        Err(other) => Verdict::Other(format!("{other:?}")),
    }
}

/// The value of one hex digit, from the standard library's own definition.
fn digit_value(byte: u8) -> Option<u8> {
    let value = char::from(byte).to_digit(16)?;
    u8::try_from(value).ok()
}

/// The answer for `text`: sixty-four bytes, then the first non-hex byte, then
/// the thirty-two bytes the pairs spell.
fn reference_verdict(text: &str) -> Verdict {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return Verdict::Length(bytes.len());
    }
    let mut digits = Vec::with_capacity(64);
    for (at, &byte) in bytes.iter().enumerate() {
        match digit_value(byte) {
            Some(value) => digits.push(value),
            None => return Verdict::Digit { at, byte },
        }
    }
    let mut out = [0_u8; 32];
    let (pairs, _) = digits.as_chunks::<2>();
    for (slot, &[high, low]) in out.iter_mut().zip(pairs) {
        *slot = high.wrapping_shl(4) | low;
    }
    Verdict::Digest(out)
}

/// The digest of parts fed through `write_framed`.
fn framed(parts: &[&[u8]]) -> Digest {
    let mut hasher = Hasher::new();
    for part in parts {
        hasher.write_framed(part);
    }
    hasher.finalize()
}

/// The digest of parts fed through `update`, which is the concatenation's.
fn joined(parts: &[&[u8]]) -> Digest {
    let mut hasher = Hasher::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize()
}

/// The number of bits in which two digests differ.
fn distance(left: &Digest, right: &Digest) -> u32 {
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .map(|(one, other)| (one ^ other).count_ones())
        .sum()
}

/// A digest of drawn bytes, as a value to compare and print.
fn draw_digest(attacker: &mut Attacker) -> Digest {
    Digest::from_bytes(attacker.array::<32>())
}

/// `key` with bit number `bit` flipped, as the key type.
fn flipped_key(key: &[u8; 32], bit: usize) -> [u8; 32] {
    let mut out = [0_u8; 32];
    for (slot, byte) in out.iter_mut().zip(flip_bit(key, bit)) {
        *slot = byte;
    }
    out
}

/// Every string of length `0..=5` over the alphabet `{0, 1}`.
fn short_strings() -> Vec<Vec<u8>> {
    let mut all = Vec::new();
    for len in 0..=5_u32 {
        for pattern in 0..1_u32.wrapping_shl(len) {
            all.push(
                (0..len)
                    .map(|bit| u8::from((pattern.wrapping_shr(bit) & 1) == 1))
                    .collect::<Vec<u8>>(),
            );
        }
    }
    all
}

/// One campaign: hostile digest text through `from_hex`, outcomes folded.
fn campaign(seed: u64) -> u64 {
    decoder_campaign(
        seed,
        |attacker| draw_digest(attacker).to_hex().into_bytes(),
        |hostile| format!("{:?}", verdict(&as_text(hostile))),
    )
}

/// Whatever the attacker builds from a real digest's text, `from_hex` gives the
/// reference's answer: the same bytes, or the same refusal at the same offset.
#[test]
fn sim_adversary_digest_hostile_text_gets_the_reference_answer() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let hex = draw_digest(attacker).to_hex();
        let hostile = as_text(&attacker.mutate_n(hex.as_bytes(), 3));
        assert_eq!(
            verdict(&hostile),
            reference_verdict(&hostile),
            "seed {seed:#x} round {round}: {hostile:?}"
        );
        Ok(())
    })
}

/// Every ASCII byte at every one of the sixty-four positions of a digest text
/// gets the reference's answer.
#[test]
fn sim_adversary_digest_every_ascii_byte_at_every_position() {
    every_byte_at_every_position(*SAMPLE, |input, position, candidate| {
        let Ok(text) = std::str::from_utf8(&input) else {
            return;
        };
        assert_eq!(
            verdict(text),
            reference_verdict(text),
            "{candidate:#04x} at {position}"
        );
    });
}

/// Every length from nothing to a hundred and thirty bytes is a wrong length
/// that names itself unless it is sixty-four.
#[test]
fn sim_adversary_digest_every_length_is_exact_or_named() {
    for len in 0..=130_usize {
        let text = "a".repeat(len);
        assert_eq!(verdict(&text), reference_verdict(&text), "length {len}");
        if len != 64 {
            assert_eq!(verdict(&text), Verdict::Length(len), "length {len}");
        }
    }
}

/// Length is counted in bytes: thirty-two two-byte characters pass the length
/// check and are refused at their first byte, and a multi-byte character among
/// ASCII is refused wherever it falls.
#[test]
fn sim_adversary_digest_length_is_bytes_not_characters() {
    let wide = "\u{e9}".repeat(32);
    assert_eq!(
        verdict(&wide),
        Verdict::Digit { at: 0, byte: 0xc3 },
        "thirty-two two-byte characters"
    );
    for position in [0_usize, 1, 31, 62] {
        let mut text = as_text(SAMPLE);
        text.replace_range(position..position.saturating_add(2), "\u{e9}");
        assert_eq!(verdict(&text), reference_verdict(&text), "at {position}");
        assert!(
            matches!(verdict(&text), Verdict::Digit { .. } | Verdict::Length(_)),
            "a two-byte character at {position} was taken for hex"
        );
    }
}

/// Printing and parsing round-trip for any digest, in either case, and the
/// printed forms agree: `to_hex`, `Display`, and the hex inside `Debug`.
#[test]
fn sim_adversary_digest_text_forms_round_trip_in_any_case() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let digest = draw_digest(attacker);
        let hex = digest.to_hex();
        assert_eq!(
            hex,
            digest.to_string(),
            "seed {seed:#x} round {round}: Display"
        );
        assert_eq!(
            format!("{digest:?}"),
            format!("Digest({hex})"),
            "seed {seed:#x} round {round}: Debug"
        );
        assert_eq!(hex.len(), 64, "seed {seed:#x} round {round}: length");
        for variant in [hex.clone(), hex.to_ascii_uppercase()] {
            assert_eq!(
                Digest::from_hex(&variant)?,
                digest,
                "seed {seed:#x} round {round}: {variant}"
            );
        }
        Ok(())
    })
}

/// Anything accepted prints as its own lowercase form: a digest has one printed
/// identity whatever spelling was read.
#[test]
fn sim_adversary_digest_accepted_text_prints_as_its_lowercase_form() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let hex = draw_digest(attacker).to_hex();
        let hostile = as_text(&attacker.mutate_n(hex.as_bytes(), 1));
        let Ok(digest) = Digest::from_hex(&hostile) else {
            return Ok(());
        };
        assert_eq!(
            digest.to_hex(),
            hostile.to_ascii_lowercase(),
            "seed {seed:#x} round {round}"
        );
        Ok(())
    })
}

/// The BLAKE3 specification's vectors: the empty input and `abc`.
#[test]
fn sim_adversary_digest_the_published_vectors_hold() {
    assert_eq!(
        blake3(b"").to_hex(),
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
        "empty input"
    );
    assert_eq!(
        blake3(b"abc").to_hex(),
        "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85",
        "abc"
    );
}

/// Streaming equals one-shot at every length from nothing to 4200 bytes, fed
/// whole, split in two at a drawn point, and byte by byte: the block and chunk
/// boundaries are where an incremental hasher slips.
#[test]
fn sim_adversary_digest_streaming_equals_one_shot_at_every_length() -> TestResult {
    sweep(1, |seed, _, attacker| {
        let block = attacker.bytes(251);
        let data: Vec<u8> = block.iter().cycle().take(4_200).copied().collect();
        for len in 0..=4_200_usize {
            let message = &data[..len];
            let whole = blake3(message);
            assert_eq!(
                joined(&[message]),
                whole,
                "seed {seed:#x}: {len} bytes whole"
            );
            let cut = attacker.below(len.saturating_add(1));
            assert_eq!(
                joined(&[&message[..cut], &message[cut..]]),
                whole,
                "seed {seed:#x}: {len} bytes cut at {cut}"
            );
        }
        let mut hasher = Hasher::new();
        for &byte in data.iter().take(2_100) {
            hasher.update(&[byte]);
        }
        assert_eq!(hasher.finalize(), blake3(&data[..2_100]), "byte by byte");
        Ok(())
    })
}

/// Pieces of every awkward size, at every boundary length, give the one-shot
/// digest.
#[test]
fn sim_adversary_digest_awkward_pieces_give_the_one_shot_digest() -> TestResult {
    sweep(1, |seed, _, attacker| {
        for len in BOUNDARIES {
            let data = attacker.bytes(len);
            let whole = blake3(&data);
            for piece in PIECES {
                let mut hasher = Hasher::new();
                for part in data.chunks(piece) {
                    hasher.update(part);
                }
                assert_eq!(
                    hasher.finalize(),
                    whole,
                    "seed {seed:#x}: {len} bytes in pieces of {piece}"
                );
            }
        }
        Ok(())
    })
}

/// A finalised hasher is a snapshot: asking twice gives one answer, feeding more
/// changes it, and the earlier snapshot still equals the digest of what was fed.
#[test]
fn sim_adversary_digest_finalize_is_a_snapshot() -> TestResult {
    sweep(512, |seed, round, attacker| {
        let len = attacker.below(200);
        let (first, second) = (attacker.bytes(len), attacker.bytes(len.saturating_add(1)));
        let mut hasher = Hasher::new();
        hasher.update(&first);
        let early = hasher.finalize();
        assert_eq!(
            early,
            hasher.finalize(),
            "seed {seed:#x} round {round}: twice"
        );
        assert_eq!(
            early,
            blake3(&first),
            "seed {seed:#x} round {round}: snapshot"
        );
        hasher.update(&second);
        assert_ne!(
            hasher.finalize(),
            early,
            "seed {seed:#x} round {round}: feeding more must change it"
        );
        Ok(())
    })
}

/// Every split of a framed stream is a different digest, and every split of a
/// plain stream is the same one: the framing is what carries the boundary.
#[test]
fn sim_adversary_digest_framing_separates_every_split() -> TestResult {
    sweep(256, |seed, round, attacker| {
        let len = 1_usize.saturating_add(attacker.below(40));
        let data = attacker.bytes(len);
        let mut framed_digests = BTreeSet::new();
        for cut in 0..=len {
            framed_digests.insert(framed(&[&data[..cut], &data[cut..]]));
            assert_eq!(
                joined(&[&data[..cut], &data[cut..]]),
                blake3(&data),
                "seed {seed:#x} round {round}: plain split at {cut}"
            );
        }
        assert_eq!(
            framed_digests.len(),
            len.saturating_add(1),
            "seed {seed:#x} round {round}: framed splits collided"
        );
        Ok(())
    })
}

/// Over every pair of strings up to five long on a two-letter alphabet, framed
/// digests are all distinct, while plain concatenation collapses the 3,969 pairs
/// to the 2,047 distinct joined strings: the ambiguity is real and the framing
/// removes all of it.
#[test]
fn sim_adversary_digest_framing_is_injective_where_concatenation_is_not() {
    let strings = short_strings();
    assert_eq!(
        strings.len(),
        63,
        "strings of length 0 to 5 over two letters"
    );
    let mut by_frame = BTreeMap::new();
    let mut by_join = BTreeMap::new();
    for (left_index, left) in strings.iter().enumerate() {
        for (right_index, right) in strings.iter().enumerate() {
            by_frame.insert(framed(&[left, right]), (left_index, right_index));
            by_join.insert(joined(&[left, right]), (left_index, right_index));
        }
    }
    assert_eq!(
        by_frame.len(),
        3_969,
        "every framed pair has its own digest"
    );
    assert_eq!(
        by_join.len(),
        2_047,
        "joined pairs collapse to joined strings"
    );
}

/// An empty part is a part: no part, one empty part and two empty parts are
/// different digests, and moving a byte across an empty part moves the digest.
#[test]
fn sim_adversary_digest_an_empty_part_is_not_no_part() {
    let none = framed(&[]);
    let empty: &[u8] = b"";
    let all = [
        none,
        framed(&[empty]),
        framed(&[empty, empty]),
        framed(&[b"a", empty]),
        framed(&[empty, b"a"]),
        framed(&[b"a"]),
    ];
    let distinct: BTreeSet<Digest> = all.iter().copied().collect();
    assert_eq!(distinct.len(), all.len(), "an empty part changed nothing");
    assert_eq!(none, blake3(b""), "no parts is the empty input");
}

/// A framed stream never equals the same bytes fed plain: the prefix is always
/// there, so a framed digest cannot be confused with a plain one.
#[test]
fn sim_adversary_digest_framed_and_plain_never_meet() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let len = attacker.below(64);
        let data = attacker.bytes(len);
        assert_ne!(
            framed(&[&data]),
            joined(&[&data]),
            "seed {seed:#x} round {round}: {len} bytes"
        );
        Ok(())
    })
}

/// A part that contains the length prefix of a part cannot stand in for two
/// parts: the outer prefix counts the inner bytes, so the digests differ.
#[test]
fn sim_adversary_digest_a_smuggled_prefix_does_not_forge_a_boundary() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let (left_len, right_len) = (attacker.below(24), attacker.below(24));
        let (left, right) = (attacker.bytes(left_len), attacker.bytes(right_len));
        let mut smuggled = left.clone();
        smuggled.extend(right.len().to_le_bytes());
        smuggled.extend(&right);
        assert_ne!(
            framed(&[&left, &right]),
            framed(&[&smuggled]),
            "seed {seed:#x} round {round}: one part posing as two"
        );
        Ok(())
    })
}

/// Flipping any single bit of a keyed digest's key or message changes the
/// digest, for every one of the 256 key bits and 512 message bits, and a keyed
/// digest is never the plain digest of the same bytes.
#[test]
fn sim_adversary_digest_every_single_bit_fault_is_noticed() -> TestResult {
    sweep(4, |seed, round, attacker| {
        let key = attacker.array::<32>();
        let message = attacker.bytes(64);
        let base = keyed(&key, &message);
        assert_ne!(
            base,
            blake3(&message),
            "seed {seed:#x} round {round}: key ignored"
        );
        let mut seen = BTreeSet::new();
        seen.insert(base);
        for bit in 0..256 {
            assert!(
                seen.insert(keyed(&flipped_key(&key, bit), &message)),
                "seed {seed:#x} round {round}: key bit {bit} was not noticed"
            );
        }
        for bit in 0..512 {
            assert!(
                seen.insert(keyed(&key, &flip_bit(&message, bit))),
                "seed {seed:#x} round {round}: message bit {bit} was not noticed"
            );
        }
        Ok(())
    })
}

/// The avalanche: flipping one input bit changes about half of the 256 digest
/// bits, averaged over all 512 flips of a 64-byte message. A digest that
/// changes far fewer is weak; far more is correlated.
#[test]
fn sim_adversary_digest_one_flipped_bit_changes_half_the_digest() -> TestResult {
    sweep(4, |seed, round, attacker| {
        let message = attacker.bytes(64);
        let base = blake3(&message);
        let total: u32 = (0..512)
            .map(|bit| distance(&base, &blake3(&flip_bit(&message, bit))))
            .sum();
        assert!(
            (512 * 120..=512 * 136).contains(&total),
            "seed {seed:#x} round {round}: {total} bits changed over 512 flips"
        );
        Ok(())
    })
}

/// Equality sees every one of the 256 bits, agrees with `ct_eq` and with a byte
/// compare, and ordering is the bytes' lexicographic order.
#[test]
fn sim_adversary_digest_equality_sees_every_bit() -> TestResult {
    sweep(16, |seed, round, attacker| {
        let digest = draw_digest(attacker);
        assert!(
            digest.ct_eq(&digest),
            "seed {seed:#x} round {round}: reflexive"
        );
        for bit in 0..256 {
            let other = Digest::from_bytes(flipped_key(digest.as_bytes(), bit));
            assert!(
                digest != other && !digest.ct_eq(&other),
                "seed {seed:#x} round {round}: bit {bit} was not seen"
            );
            assert_eq!(
                digest.cmp(&other),
                digest.as_bytes().cmp(other.as_bytes()),
                "seed {seed:#x} round {round}: order at bit {bit}"
            );
        }
        Ok(())
    })
}

/// Equal digests hash alike under the standard hasher and sit once in a set, and
/// raw bytes survive `from_bytes` and `as_bytes`.
#[test]
fn sim_adversary_digest_equal_digests_are_one_key() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let bytes = attacker.array::<32>();
        let (left, right) = (Digest::from_bytes(bytes), Digest::from_bytes(bytes));
        assert_eq!(left.as_bytes(), &bytes, "seed {seed:#x} round {round}");
        let (mut one, mut other) = (
            std::collections::hash_map::DefaultHasher::new(),
            std::collections::hash_map::DefaultHasher::new(),
        );
        left.hash(&mut one);
        right.hash(&mut other);
        assert_eq!(one.finish(), other.finish(), "seed {seed:#x} round {round}");
        let set: BTreeSet<Digest> = [left, right].into_iter().collect();
        assert_eq!(set.len(), 1, "seed {seed:#x} round {round}: two keys");
        Ok(())
    })
}

/// Sixteen megabytes fed in pieces of 4,093 bytes (coprime with every block
/// size) give the one-shot digest, and the same stream with its last bit flipped
/// does not.
#[test]
fn sim_adversary_digest_sixteen_megabytes_in_awkward_pieces() -> TestResult {
    sweep(1, |seed, _, attacker| {
        let block = attacker.bytes(251);
        let size = 16_usize.saturating_mul(1_048_576);
        let data: Vec<u8> = block.iter().cycle().take(size).copied().collect();
        let whole = blake3(&data);
        let mut hasher = Hasher::new();
        for part in data.chunks(4_093) {
            hasher.update(part);
        }
        assert_eq!(hasher.finalize(), whole, "seed {seed:#x}: pieces of 4093");
        let mut damaged = data;
        damaged[size.saturating_sub(1)] ^= 0x01;
        assert_ne!(blake3(&damaged), whole, "seed {seed:#x}: the last bit");
        Ok(())
    })
}

/// A refusal's message is one printable line, whatever the bad byte was, so a
/// hostile string cannot forge a log line (CWE-117).
#[test]
fn sim_adversary_digest_a_refusal_message_is_one_safe_line() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let hex = draw_digest(attacker).to_hex();
        let hostile = as_text(&attacker.mutate_n(hex.as_bytes(), 3));
        if let Err(error) = Digest::from_hex(&hostile) {
            let message = error.to_string();
            assert!(
                !message.contains(['\n', '\r', '\u{1b}', '\0']),
                "seed {seed:#x} round {round}: {message:?} can forge a log line"
            );
        }
        Ok(())
    })
}

/// The same seed mounts the same attack on the parser.
#[test]
fn sim_adversary_digest_the_same_seed_replays_the_same_campaign() {
    replays_under_every_seed(campaign);
}

/// Two seeds mount two attacks.
#[test]
fn sim_adversary_digest_distinct_seeds_mount_distinct_campaigns() {
    diverges_between_seeds(campaign);
}
