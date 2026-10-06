//! Seeded deterministic replay of the `hash` content-identity contracts.
//!
//! One seed draws every message, a failure prints the seed that produced it,
//! and the same seed must produce the same trace hash. Every probe drives the
//! public API of the shipped crate, and every answer is checked against a
//! reference written here from the documented contract rather than from the
//! implementation under test.
//!
//! The property here is determinism with one sharp edge: a digest depends on the
//! *bytes*, never on how they were split, so an incremental `Hasher` fed the
//! same bytes in any chunking must equal the one-shot `blake3`. The families
//! below sweep chunkings drawn from the seed rather than testing one split, and
//! the framing family checks the boundary the plain `update` leaves open.
//!
//! The module is gated `feature = "hash"` because that is what gates
//! `lgwks_std::hash` (INV-DEP-6).

#![cfg(feature = "hash")]

use crate::seeded_bytes;
use crate::seeded_sweep;

use lgwks_std::hash::{Digest, DigestParseError, Hasher, blake3, keyed};

use seeded_bytes::{below, fold_bytes, next_byte, next_bytes, repeated};
use seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold_usize, initial_trace,
};

/// The eight-byte little-endian length prefix a framed part is fed.
///
/// Written here from the documented frame rather than taken from the crate, so
/// the framing families compare against an independent construction: the
/// length's own bytes, eight of them, then the part.
fn frame_prefix(length: usize) -> [u8; 8] {
    let mut prefix = [0u8; 8];
    for (slot, byte) in prefix.iter_mut().zip(length.to_le_bytes()) {
        *slot = byte;
    }
    prefix
}

/// The widest message any family in this file hashes, so a boundary case is a
/// wide payload and not a one-byte one.
const WIDE_MESSAGE_BYTES: usize = 8_192;

/// The message lengths every family in this file states its properties at.
const BOUNDARY_LENGTHS: [usize; 6] = [0, 1, 2, 63, 64, WIDE_MESSAGE_BYTES];

/// Splits `message` into chunks of the drawn sizes, always advancing at least
/// one byte so the split always terminates.
fn seeded_chunks(state: &mut u64, message: &[u8]) -> Vec<Vec<u8>> {
    if message.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut cursor = 0usize;
    while cursor < message.len() {
        let width = below(state, 9).saturating_add(1);
        let end = cursor.saturating_add(width).min(message.len());
        // `cursor` is below the length and `end` is at most it, so the chunk
        // exists; the loop advances `cursor` to `end`.
        chunks.push(message[cursor..end].to_vec());
        cursor = end;
    }
    chunks
}

/// The concatenation of `chunks`, computed here so the round-trip family can
/// assert the split preserved every byte.
fn joined(chunks: &[Vec<u8>]) -> Vec<u8> {
    chunks.concat()
}

/// Runs the seeded determinism and chunking sweep and returns its trace.
fn hash_trace(seed: u64) -> u64 {
    let mut state = seed;
    let mut trace = initial_trace();

    for _ in 0..48 {
        let length = below(&mut state, 96);
        let message = next_bytes(&mut state, length);
        let one_shot = blake3(&message);

        // Determinism: the same bytes hash the same way every time.
        assert_eq!(
            blake3(&message),
            one_shot,
            "seed {seed}: hashing the same bytes twice must give the same digest"
        );

        // Chunking independence: the split is drawn from the seed, so every
        // seeding exercises a different boundary.
        let chunks = seeded_chunks(&mut state, &message);
        assert_eq!(
            joined(&chunks),
            message,
            "seed {seed}: the seeded split must preserve every byte"
        );
        let mut hasher = Hasher::new();
        for chunk in &chunks {
            hasher.update(chunk);
        }
        assert_eq!(
            hasher.finalize(),
            one_shot,
            "seed {seed}: a {}-way split must hash as the one-shot digest",
            chunks.len()
        );

        fold_bytes(&mut trace, &message);
        fold_bytes(&mut trace, one_shot.as_bytes());
        fold_usize(&mut trace, chunks.len());
    }
    trace
}

#[test]
/// The same bytes always produce the same digest, over seeded messages of every
/// boundary length — determinism is the whole of INV-STD-HASH-1.
fn the_same_bytes_always_produce_the_same_digest() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for length in BOUNDARY_LENGTHS {
            let message = next_bytes(&mut state, length);
            let first = blake3(&message);
            let second = blake3(&message);
            assert_eq!(
                first, second,
                "seed {seed}: a {length}-byte message must hash identically twice"
            );
            assert_eq!(
                first,
                blake3(&message),
                "seed {seed}: hashing the message a third time must agree"
            );
        }
    }
}

#[test]
/// An incremental `Hasher` equals the one-shot digest at **every** seeded
/// chunking, not at one convenient split: the seed drives the chunk widths, so
/// a single-byte-at-a-time feed and a feed of the whole message are both among
/// the cases.
fn incremental_hashing_equals_one_shot_at_every_seeded_chunking() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..64 {
            let length = below(&mut state, 80);
            let message = next_bytes(&mut state, length);
            let chunks = seeded_chunks(&mut state, &message);

            let mut hasher = Hasher::new();
            for chunk in &chunks {
                hasher.update(chunk);
            }
            assert_eq!(
                hasher.finalize(),
                blake3(&message),
                "seed {seed}: {} chunks of a {length}-byte message must hash as one shot",
                chunks.len()
            );

            // The byte-at-a-time extreme, stated rather than left to the draw.
            let mut byte_wise = Hasher::new();
            for byte in &message {
                byte_wise.update(std::slice::from_ref(byte));
            }
            assert_eq!(
                byte_wise.finalize(),
                blake3(&message),
                "seed {seed}: a {length}-byte message fed one byte at a time must hash alike"
            );
        }
    }
}

#[test]
/// A message is not padded to a boundary: a message and the same message with a
/// trailing zero byte are different messages with different digests, at every
/// length where a trailing byte fits.
fn a_trailing_byte_changes_the_digest() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..48 {
            let length = below(&mut state, 64);
            let mut message = next_bytes(&mut state, length);
            let before = blake3(&message);
            message.push(next_byte(&mut state));
            assert_ne!(
                before,
                blake3(&message),
                "seed {seed}: appending a byte must change the digest"
            );
        }
    }
}

#[test]
/// Distinct messages are distinct digests: a one-bit change anywhere in a seeded
/// message changes the digest, so the identity is content and not a length or a
/// prefix.
fn a_single_bit_change_changes_the_digest() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..32 {
            let length = below(&mut state, 32) + 1;
            let mut message = next_bytes(&mut state, length);
            let original = blake3(&message);
            let position = below(&mut state, length);
            let flip = 1_u8 << (below(&mut state, 8));

            if let Some(byte) = message.get_mut(position) {
                *byte ^= flip;
            }
            assert_ne!(
                original,
                blake3(&message),
                "seed {seed}: flipping bit {} at {position} must change the digest",
                u32::from(flip)
            );
        }
    }
}

#[test]
/// A concatenation is ambiguous and `update` preserves that ambiguity, while
/// `write_framed` removes it: this is the one place where two different feedings
/// are *supposed* to agree, and the boundary is stated rather than assumed.
fn unframed_splits_conflate_where_framed_splits_do_not() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..24 {
            let head_len = below(&mut state, 6).saturating_add(1);
            let head = next_bytes(&mut state, head_len);
            let tail_len = below(&mut state, 6).saturating_add(1);
            let tail = next_bytes(&mut state, tail_len);

            // The two histories are drawn from an explicit three-part partition
            // of one byte string: history A feeds the three parts separately,
            // history B feeds the first separately and the last two merged.
            // Every byte appears exactly once in each, so the two feeds are
            // indistinguishable without framing, and framing tells them apart.
            let mut stream_bytes = head.clone();
            stream_bytes.extend_from_slice(&tail);
            let first_len = below(&mut state, stream_bytes.len().saturating_sub(2)) + 1;
            let middle_len = below(
                &mut state,
                stream_bytes
                    .len()
                    .saturating_sub(first_len)
                    .saturating_sub(1),
            ) + 1;
            let (first, rest) = stream_bytes.split_at(first_len);
            let (middle_part, last) = rest.split_at(middle_len);
            assert_eq!(
                first.len() + middle_part.len() + last.len(),
                stream_bytes.len(),
                "the three parts tile the whole message"
            );

            let mut unframed_left = Hasher::new();
            unframed_left.update(first).update(middle_part);
            unframed_left.update(last);
            let mut unframed_right = Hasher::new();
            unframed_right.update(first);
            unframed_right.update(middle_part).update(last);
            assert_eq!(
                unframed_left.finalize(),
                unframed_right.finalize(),
                "seed {seed}: unframed, both splits feed the same bytes"
            );
            assert_eq!(
                unframed_left.finalize(),
                blake3(&stream_bytes),
                "seed {seed}: the unframed digest is the digest of the concatenation"
            );

            let mut framed_left = Hasher::new();
            framed_left
                .write_framed(first)
                .write_framed(middle_part)
                .write_framed(last);
            let merged = [middle_part, last].concat();
            let mut framed_right = Hasher::new();
            framed_right.write_framed(first).write_framed(&merged);
            assert_ne!(
                framed_left.finalize(),
                framed_right.finalize(),
                "seed {seed}: framed, a three-part feed differs from a two-part one"
            );

            let mut framed_same = Hasher::new();
            framed_same
                .write_framed(first)
                .write_framed(middle_part)
                .write_framed(last);
            assert_eq!(
                framed_left.finalize(),
                framed_same.finalize(),
                "seed {seed}: a framed feed is deterministic"
            );
        }
    }
}

#[test]
/// A framed feed equals the digest of the length-prefixed bytes it wrote, so
/// framing is exactly the documented `u64` little-endian prefix followed by the
/// part — no more and no less.
fn a_framed_feed_equals_the_digest_of_its_documented_prefix() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..48 {
            let first_len = below(&mut state, 12);
            let first = next_bytes(&mut state, first_len);
            let second_len = below(&mut state, 12);
            let second = next_bytes(&mut state, second_len);

            let mut framed = Hasher::new();
            framed.write_framed(&first).write_framed(&second);

            let mut expected = Vec::new();
            for part in [&first, &second] {
                // The frame is the documented eight-byte little-endian length
                // followed by the part, built from the length's own bytes so the
                // expectation and the implementation agree by construction.
                expected.extend_from_slice(&frame_prefix(part.len()));
                expected.extend_from_slice(part);
            }
            assert_eq!(
                framed.finalize(),
                blake3(&expected),
                "seed {seed}: a framed feed must hash the prefixed parts"
            );

            // And framing is order-sensitive, which is the point of it.
            let mut swapped = Hasher::new();
            swapped.write_framed(&second).write_framed(&first);
            if first != second {
                assert_ne!(
                    framed.finalize(),
                    swapped.finalize(),
                    "seed {seed}: swapping two framed parts must change the digest"
                );
            }
        }
    }
}

#[test]
/// An empty part is still a part: framing it writes a zero length prefix, so it
/// is distinguishable from writing nothing at all. A stream that treats an
/// empty string as absent loses the boundary the framing exists to keep.
fn an_empty_framed_part_is_distinct_from_no_part() {
    let mut empty_part = Hasher::new();
    empty_part.write_framed(b"").write_framed(b"x");
    let mut no_part = Hasher::new();
    no_part.update(b"x");
    assert_ne!(
        empty_part.finalize(),
        no_part.finalize(),
        "a framed empty part is not the same stream as no part at all"
    );

    let mut two_empty = Hasher::new();
    two_empty.write_framed(b"").write_framed(b"");
    let mut one_empty = Hasher::new();
    one_empty.write_framed(b"");
    assert_ne!(
        two_empty.finalize(),
        one_empty.finalize(),
        "two framed empty parts are not one framed empty part"
    );
}

#[test]
/// A keyed digest depends on the key and on the message, and is not the unkeyed
/// digest of either: a keyed hash is the message-authentication case, so a key
/// that does not change the answer would prove nothing.
fn a_keyed_digest_depends_on_the_key_and_the_message() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..48 {
            let message_len = below(&mut state, 40);
            let message = next_bytes(&mut state, message_len);
            let mut key = [0u8; 32];
            for slot in key.iter_mut().take(4) {
                *slot = next_byte(&mut state);
            }
            let mut other_key = key;
            let key_position = below(&mut state, 32);
            if let Some(byte) = other_key.get_mut(key_position) {
                *byte ^= 0xff;
            }

            assert_eq!(
                keyed(&key, &message),
                keyed(&key, &message),
                "seed {seed}: a keyed digest must be deterministic"
            );
            assert_ne!(
                keyed(&key, &message),
                keyed(&other_key, &message),
                "seed {seed}: changing key byte {key_position} must change the tag"
            );
            if !message.is_empty() {
                let mut other_message = message.clone();
                let position = below(&mut state, message.len());
                if let Some(byte) = other_message.get_mut(position) {
                    *byte ^= 0x01;
                }
                assert_ne!(
                    keyed(&key, &message),
                    keyed(&key, &other_message),
                    "seed {seed}: changing message byte {position} must change the tag"
                );
            }
        }
    }
}

#[test]
/// A digest's hexadecimal form is 64 lowercase characters and parses back to the
/// same digest from either case — the representation is not where a caller can
/// lose the value.
fn the_hex_form_round_trips_at_both_cases() -> Result<(), DigestParseError> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..48 {
            let message_len = below(&mut state, 40);
            let digest = blake3(&next_bytes(&mut state, message_len));
            let hex = digest.to_hex();
            assert_eq!(
                hex.len(),
                64,
                "seed {seed}: a digest is 32 bytes and so 64 hex characters"
            );
            assert!(
                hex.chars()
                    .all(|character| character.is_ascii_hexdigit()
                        && !character.is_ascii_uppercase()),
                "seed {seed}: the hex form is lowercase"
            );
            assert_eq!(
                Digest::from_hex(&hex)?,
                digest,
                "seed {seed}: the hex form parses back to the digest it was made from"
            );
            assert_eq!(
                Digest::from_hex(&hex.to_ascii_uppercase())?,
                digest,
                "seed {seed}: the upper-case spelling parses to the same digest"
            );
            assert_eq!(
                Digest::from_bytes(*digest.as_bytes()),
                digest,
                "seed {seed}: raw-byte construction round-trips the digest"
            );
            assert_eq!(
                format!("{digest}"),
                hex,
                "seed {seed}: the displayed form is the hex form"
            );
        }
    }
    Ok(())
}

/// `DigestParseError` carries no `PartialEq`, so a refusal is matched on its
/// variant rather than compared: a caller that only wants to know *which* arm
/// fired should not need to build the whole error to compare it.
fn parse_arm(error: &DigestParseError) -> u8 {
    match *error {
        DigestParseError::WrongLength { .. } => 1,
        DigestParseError::Hex(_) => 2,
        _ => 3,
    }
}

#[test]
/// A digest hex string of the wrong length is refused on that length, whether
/// one character short or one character long, and the refusal never reaches the
/// decoder — so a malformed hex is not reported as a bad character.
fn a_digest_hex_of_the_wrong_length_is_refused_on_its_length()
-> Result<(), Box<dyn std::error::Error>> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..16 {
            let hex = blake3(&next_bytes(&mut state, 8)).to_hex();
            // A prefix can only be shorter, so the over-long case is built by
            // appending rather than by slicing: a 64-character digest hex is the
            // only length that decodes, and both directions away from it must
            // be refused.
            let longer = format!("{hex}0");
            for candidate in [
                String::new(),
                "0".to_owned(),
                hex[..63].to_owned(),
                longer,
                format!("{hex}0000"),
            ] {
                let observed = candidate.len();
                match Digest::from_hex(&candidate) {
                    Err(ref error @ DigestParseError::WrongLength { len: reported }) => {
                        assert_eq!(
                            reported, observed,
                            "seed {seed}: the refusal names the length it was given"
                        );
                        assert_eq!(
                            parse_arm(error),
                            1,
                            "seed {seed}: a {observed}-character hex is refused as a length"
                        );
                    }
                    other => {
                        return Err(format!(
                            "seed {seed}: a {observed}-character hex gave {other:?}"
                        )
                        .into());
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
/// A digest hex string of the right length holding a non-hex character is
/// refused as a hex refusal, and the length check did not swallow it — the two
/// are separate facts about separate inputs, and a decoder that reported a bad
/// length for a 64-character string would be naming a fault it never found.
fn a_digest_hex_with_a_bad_character_is_refused_as_a_hex_refusal()
-> Result<(), Box<dyn std::error::Error>> {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..32 {
            let hex = blake3(&next_bytes(&mut state, 8)).to_hex();
            let position = below(&mut state, 64);
            let alien = b"gz !"[below(&mut state, 4)];
            let mut corrupted = hex.into_bytes();
            if let Some(slot) = corrupted.get_mut(position) {
                *slot = alien;
            }
            // The digest hex is ASCII and `alien` is ASCII, so replacing one
            // byte leaves the string valid UTF-8; the arm below names that
            // invariant if it ever stops holding.
            let corrupted = match String::from_utf8(corrupted) {
                Ok(corrupted) => corrupted,
                Err(failure) => {
                    return Err(format!(
                        "seed {seed}: the alien at {position} was not ASCII, so the corrupted digest is not text: {}",
                        if failure.as_bytes().is_ascii() {
                            "an ASCII failure the codec should not have refused"
                        } else {
                            "the corrupted bytes are not text"
                        }
                    )
                    .into());
                }
            };
            match Digest::from_hex(&corrupted) {
                Err(ref error) => assert_eq!(
                    parse_arm(error),
                    2,
                    "seed {seed}: the alien at {position} must be refused as a hex fault"
                ),
                Ok(_) => {
                    return Err(format!("seed {seed}: the alien at {position} was accepted").into());
                }
            }
        }
    }
    Ok(())
}

#[test]
/// The endpoints are exact: an empty message is a message, and it does not hash
/// to the same digest as a one-byte message of any value.
fn the_empty_message_is_hashed_as_a_message() {
    let empty = blake3(b"");
    assert_eq!(
        empty.to_hex(),
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
        "the empty message hashes to the BLAKE3 empty-input digest"
    );
    let hasher = Hasher::new();
    assert_eq!(
        hasher.finalize(),
        empty,
        "a hasher that was fed nothing is the empty-message digest"
    );
    for byte in 0..=255_u8 {
        let message = [byte];
        assert_ne!(
            blake3(&message),
            empty,
            "the one-byte message {byte:#04x} must not hash as the empty message"
        );
    }
}

#[test]
/// The wide boundary is a real message: 8 KiB hashes, and a one-byte change at
/// its far end still changes the digest.
fn the_wide_boundary_is_hashed_at_its_declared_length() {
    let mut state = SWEEP_SEEDS[0];
    let message = next_bytes(&mut state, WIDE_MESSAGE_BYTES);
    let original = blake3(&message);
    assert_eq!(
        original,
        blake3(&message),
        "the wide message hashes identically twice"
    );

    let mut at_the_end = message.clone();
    if let Some(byte) = at_the_end.get_mut(WIDE_MESSAGE_BYTES - 1) {
        *byte ^= 0x80;
    }
    assert_ne!(
        original,
        blake3(&at_the_end),
        "changing the last byte of the wide message must change the digest"
    );

    let mut hasher = Hasher::new();
    hasher.update(&message);
    assert_eq!(
        hasher.finalize(),
        original,
        "the wide message hashes the same incrementally as one-shot"
    );
}

#[test]
/// A constant message at every boundary length is a distinct digest from every
/// other non-empty one: a zero-filled message is not an absence and a set-bit
/// message is not a page of zeroes. At length zero the fill byte does not exist,
/// so the three zero-length cases are the *same* message and are expected to
/// collide — the family states that rather than asserting a distinctness that is
/// not a property of anything.
fn constant_messages_at_distinct_lengths_are_distinct_digests() {
    let empty = blake3(&[]);
    let mut seen: Vec<(String, Digest)> = Vec::new();
    for length in [0_usize, 1, 2, 63, 64, 65, 1024] {
        for fill in [0x00_u8, 0xff, 0x5a] {
            let message = repeated(fill, length);
            let digest = blake3(&message);
            let label = format!("{length}:{fill:#04x}");
            if length == 0 {
                assert_eq!(
                    digest, empty,
                    "a zero-length message is the empty message whatever the fill byte was"
                );
            } else {
                assert!(
                    !seen.iter().any(|entry| entry.1 == digest),
                    "the constant message {label} collides with another"
                );
                seen.push((label, digest));
            }
        }
    }
    assert_eq!(
        seen.len(),
        18,
        "six non-empty lengths over three fills are 18 messages"
    );
    fold_usize(&mut initial_trace(), seen.len());
}

#[test]
/// The replay oracle: one seed, one trace.
fn the_same_seed_replays_to_the_same_hash_trace() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(hash_trace, seed);
    }
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn distinct_hash_seeds_diverge_in_their_trace() {
    assert_distinct_seeds_diverge(hash_trace, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
}

#[test]
/// Two seeds draw two different messages, so the determinism family is not one
/// message hashed repeatedly.
fn two_seeds_draw_two_different_messages() {
    let mut first = SWEEP_SEEDS[0];
    let mut second = SWEEP_SEEDS[1];
    let left = next_bytes(&mut first, 48);
    let right = next_bytes(&mut second, 48);
    assert_ne!(left, right, "the two sweep seeds drew the same message");
    let mut trace = initial_trace();
    fold_bytes(&mut trace, blake3(&left).as_bytes());
    fold_bytes(&mut trace, blake3(&right).as_bytes());
    fold_usize(&mut trace, left.len());
    assert_ne!(
        trace,
        initial_trace(),
        "the two messages must fold into distinct traces"
    );
}

/// Seeded digests every `ct_eq` family compares: one per boundary length and
/// 32 drawn messages, each paired with a byte-equal copy built from its bytes.
fn ct_eq_pairs(seed: u64) -> Vec<(Digest, Digest)> {
    let mut state = seed;
    let mut pairs = Vec::new();
    for length in BOUNDARY_LENGTHS {
        let digest = blake3(&next_bytes(&mut state, length));
        pairs.push((digest, Digest::from_bytes(*digest.as_bytes())));
    }
    for _ in 0..32 {
        let length = below(&mut state, 96);
        let digest = blake3(&next_bytes(&mut state, length));
        pairs.push((digest, Digest::from_bytes(*digest.as_bytes())));
    }
    pairs
}

#[test]
/// `ct_eq` answers exactly what `==` answers, on equal copies and on every
/// unequal pairing of seeded digests (#275): the constant-time path is the
/// same comparison, not a second definition of equality.
fn ct_eq_agrees_with_equality_on_seeded_pairs() {
    for seed in SWEEP_SEEDS {
        let pairs = ct_eq_pairs(seed);
        for &(left, copy) in &pairs {
            assert!(left.ct_eq(&copy), "seed {seed}: a byte-equal copy is ct_eq");
            for &(other, _) in &pairs {
                assert_eq!(
                    left.ct_eq(&other),
                    left == other,
                    "seed {seed}: ct_eq and == must agree on every pairing"
                );
            }
        }
    }
}

#[test]
/// `ct_eq` is reflexive and symmetric over seeded digests.
fn ct_eq_is_reflexive_and_symmetric() {
    for seed in SWEEP_SEEDS {
        let pairs = ct_eq_pairs(seed);
        for &(left, _) in &pairs {
            assert!(
                left.ct_eq(&left),
                "seed {seed}: a digest is ct_eq to itself"
            );
            for &(right, _) in &pairs {
                assert_eq!(
                    left.ct_eq(&right),
                    right.ct_eq(&left),
                    "seed {seed}: ct_eq must not depend on argument order"
                );
            }
        }
    }
}

#[test]
/// A single flipped bit at a seeded byte and bit is seen by `ct_eq`, so no
/// position of the 32 bytes is skipped by the constant-time comparison.
fn ct_eq_sees_a_flip_at_every_seeded_position() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for (digest, _) in ct_eq_pairs(seed) {
            for _ in 0..64 {
                let byte = below(&mut state, 32);
                // `below` draws in `0..8`, so the bit index is a shift of a byte
                // by at most seven and is never a width the shift cannot take.
                let bit = below(&mut state, 8);
                let mut flipped = *digest.as_bytes();
                if let Some(slot) = flipped.get_mut(byte) {
                    *slot ^= 1_u8 << bit;
                }
                let changed = Digest::from_bytes(flipped);
                assert!(
                    !digest.ct_eq(&changed) && !changed.ct_eq(&digest),
                    "seed {seed}: a flip of bit {bit} in byte {byte} must be seen"
                );
            }
        }
    }
}

#[test]
/// `Ord` reports `Equal` exactly when `ct_eq` holds, including for digests that
/// share every byte but the last.
fn ordering_is_equal_exactly_when_ct_eq_holds() {
    for seed in SWEEP_SEEDS {
        let pairs = ct_eq_pairs(seed);
        for &(left, copy) in &pairs {
            let mut tail = *left.as_bytes();
            if let Some(last) = tail.last_mut() {
                *last ^= 0x80;
            }
            let neighbour = Digest::from_bytes(tail);
            for right in [copy, neighbour] {
                assert_eq!(
                    left.cmp(&right) == std::cmp::Ordering::Equal,
                    left.ct_eq(&right),
                    "seed {seed}: Ord's Equal and ct_eq must agree"
                );
            }
        }
    }
}

#[test]
/// A digest parsed back from its hex form, in either case, is `ct_eq` to the
/// original, so a verifier reading a recorded head compares like with like.
fn a_digest_parsed_from_its_hex_is_ct_eq() -> Result<(), DigestParseError> {
    for seed in SWEEP_SEEDS {
        for (digest, _) in ct_eq_pairs(seed) {
            let hex = digest.to_hex();
            assert!(
                Digest::from_hex(&hex)?.ct_eq(&digest),
                "seed {seed}: the lower-case hex parses to a ct_eq digest"
            );
            assert!(
                Digest::from_hex(&hex.to_ascii_uppercase())?.ct_eq(&digest),
                "seed {seed}: the upper-case hex parses to a ct_eq digest"
            );
        }
    }
    Ok(())
}

#[test]
/// `ct_eq` separates a keyed digest from the unkeyed digest of the same message
/// and from the same message under another key: equality never collapses
/// across the key that a tenant's records are bound to.
fn ct_eq_separates_keys_over_one_message() {
    for seed in SWEEP_SEEDS {
        let mut state = seed;
        for _ in 0..32 {
            let length = below(&mut state, 96);
            let message = next_bytes(&mut state, length);
            let mut first = [0_u8; 32];
            first
                .iter_mut()
                .for_each(|byte| *byte = next_byte(&mut state));
            let mut second = first;
            if let Some(byte) = second.first_mut() {
                *byte ^= 1;
            }
            let plain = blake3(&message);
            let under_first = keyed(&first, &message);
            let under_second = keyed(&second, &message);
            assert!(
                !under_first.ct_eq(&plain),
                "seed {seed}: a keyed digest is not the unkeyed one"
            );
            assert!(
                !under_first.ct_eq(&under_second),
                "seed {seed}: two keys give two digests of one message"
            );
            assert!(
                under_first.ct_eq(&keyed(&first, &message)),
                "seed {seed}: one key and one message give one digest"
            );
        }
    }
}
