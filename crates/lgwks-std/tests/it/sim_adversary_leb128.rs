//! A peer controls every byte of the varints a reader decodes.
//!
//! The attacks are mounted on all four widths (`u64`, `u32`, `i64`, `i32`) from
//! one definition each in `support/leb128_attacks.rs`, so a width cannot be left
//! out: a second spelling of a value, a padded final group, a run of
//! continuation bytes with no end, a last group with bits above the width, a
//! prefix cut mid-value, trailing bytes that would extend a value, a stream
//! whose cursor might not advance, and a wide value read by a narrow reader.
//! The reference for every length is written from the definition of the
//! encoding; the reference for every canonical form is the encoder itself, which
//! the minimal-length family pins against the reference.
//!
//! Sources: WebAssembly binary format 5.2.2, CWE-190, CWE-835, CWE-20.

use lgwks_std::leb128::{DecodeError, decode_i32, decode_i64, decode_u32, decode_u64};

use crate::adversary::{Attacker, TestResult, sweep};
use crate::leb128_attacks::{
    Width, a_narrower_reader_takes_exactly_what_fits,
    a_run_of_continuations_overflows_at_the_last_group, a_stream_read_always_advances_and_ends,
    boundaries_around_every_power_of_two_round_trip, canonical_on_random_bytes,
    dangling_continuations_end_exactly, encoded, encodings_have_exactly_the_minimal_length,
    every_proper_prefix_is_an_unexpected_end, mutated_encodings_stay_canonical, padding_is_refused,
    padding_to_any_depth_is_refused, trailing_bytes_are_left_alone,
};
use crate::seeded_bytes::fold_bytes;
use crate::seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, initial_trace,
};

/// `count` copies of `fill` followed by `last`: the shape of a final-group attack.
fn run_then(fill: u8, count: usize, last: u8) -> Vec<u8> {
    let mut bytes = vec![fill; count];
    bytes.push(last);
    bytes
}

/// The refusal text of a decode, when it refused.
fn refusal_text<W: Width>(bytes: &[u8]) -> Option<String> {
    W::decode(bytes).err().map(|error| error.to_string())
}

/// One campaign: hostile bytes through all four readers, outcomes folded.
fn campaign(seed: u64) -> u64 {
    let mut attacker = Attacker::new(seed);
    let mut trace = initial_trace();
    for _ in 0..256 {
        let value = attacker.word();
        let hostile = attacker.mutate_n(&encoded(value), 2);
        for outcome in [
            format!("{:?}", u64::decode(&hostile)),
            format!("{:?}", u32::decode(&hostile)),
            format!("{:?}", i64::decode(&hostile)),
            format!("{:?}", i32::decode(&hostile)),
        ] {
            fold_bytes(&mut trace, outcome.as_bytes());
        }
    }
    trace
}

/// Random bytes: what `u64` accepts is the one canonical spelling of its value.
#[test]
fn sim_adversary_leb128_u64_random_bytes_are_accepted_only_canonically() -> TestResult {
    canonical_on_random_bytes::<u64>()
}

/// Random bytes: what `u32` accepts is the one canonical spelling of its value.
#[test]
fn sim_adversary_leb128_u32_random_bytes_are_accepted_only_canonically() -> TestResult {
    canonical_on_random_bytes::<u32>()
}

/// Random bytes: what `i64` accepts is the one canonical spelling of its value.
#[test]
fn sim_adversary_leb128_i64_random_bytes_are_accepted_only_canonically() -> TestResult {
    canonical_on_random_bytes::<i64>()
}

/// Random bytes: what `i32` accepts is the one canonical spelling of its value.
#[test]
fn sim_adversary_leb128_i32_random_bytes_are_accepted_only_canonically() -> TestResult {
    canonical_on_random_bytes::<i32>()
}

/// Nudged valid `u64` encodings are accepted only canonically.
#[test]
fn sim_adversary_leb128_u64_mutated_encodings_stay_canonical() -> TestResult {
    mutated_encodings_stay_canonical::<u64>()
}

/// Nudged valid `u32` encodings are accepted only canonically.
#[test]
fn sim_adversary_leb128_u32_mutated_encodings_stay_canonical() -> TestResult {
    mutated_encodings_stay_canonical::<u32>()
}

/// Nudged valid `i64` encodings are accepted only canonically.
#[test]
fn sim_adversary_leb128_i64_mutated_encodings_stay_canonical() -> TestResult {
    mutated_encodings_stay_canonical::<i64>()
}

/// Nudged valid `i32` encodings are accepted only canonically.
#[test]
fn sim_adversary_leb128_i32_mutated_encodings_stay_canonical() -> TestResult {
    mutated_encodings_stay_canonical::<i32>()
}

/// A padded final group of a `u64` is refused where the padding sits.
#[test]
fn sim_adversary_leb128_u64_a_padded_final_group_is_refused() -> TestResult {
    padding_is_refused::<u64>()
}

/// A padded final group of a `u32` is refused where the padding sits.
#[test]
fn sim_adversary_leb128_u32_a_padded_final_group_is_refused() -> TestResult {
    padding_is_refused::<u32>()
}

/// A sign-padded final group of an `i64` is refused where the padding sits.
#[test]
fn sim_adversary_leb128_i64_a_padded_final_group_is_refused() -> TestResult {
    padding_is_refused::<i64>()
}

/// A sign-padded final group of an `i32` is refused where the padding sits.
#[test]
fn sim_adversary_leb128_i32_a_padded_final_group_is_refused() -> TestResult {
    padding_is_refused::<i32>()
}

/// Every cut of a `u64` encoding is an unexpected end at the cut.
#[test]
fn sim_adversary_leb128_u64_every_cut_is_an_unexpected_end() -> TestResult {
    every_proper_prefix_is_an_unexpected_end::<u64>()
}

/// Every cut of a `u32` encoding is an unexpected end at the cut.
#[test]
fn sim_adversary_leb128_u32_every_cut_is_an_unexpected_end() -> TestResult {
    every_proper_prefix_is_an_unexpected_end::<u32>()
}

/// Every cut of an `i64` encoding is an unexpected end at the cut.
#[test]
fn sim_adversary_leb128_i64_every_cut_is_an_unexpected_end() -> TestResult {
    every_proper_prefix_is_an_unexpected_end::<i64>()
}

/// Every cut of an `i32` encoding is an unexpected end at the cut.
#[test]
fn sim_adversary_leb128_i32_every_cut_is_an_unexpected_end() -> TestResult {
    every_proper_prefix_is_an_unexpected_end::<i32>()
}

/// Bytes after a `u64` are left for the caller.
#[test]
fn sim_adversary_leb128_u64_trailing_bytes_are_left_alone() -> TestResult {
    trailing_bytes_are_left_alone::<u64>()
}

/// Bytes after a `u32` are left for the caller.
#[test]
fn sim_adversary_leb128_u32_trailing_bytes_are_left_alone() -> TestResult {
    trailing_bytes_are_left_alone::<u32>()
}

/// Bytes after an `i64` are left for the caller.
#[test]
fn sim_adversary_leb128_i64_trailing_bytes_are_left_alone() -> TestResult {
    trailing_bytes_are_left_alone::<i64>()
}

/// Bytes after an `i32` are left for the caller.
#[test]
fn sim_adversary_leb128_i32_trailing_bytes_are_left_alone() -> TestResult {
    trailing_bytes_are_left_alone::<i32>()
}

/// Zero padded to every depth is refused as a `u64`.
#[test]
fn sim_adversary_leb128_u64_zero_padded_to_any_depth_is_refused() {
    padding_to_any_depth_is_refused::<u64>();
}

/// Zero padded to every depth is refused as a `u32`.
#[test]
fn sim_adversary_leb128_u32_zero_padded_to_any_depth_is_refused() {
    padding_to_any_depth_is_refused::<u32>();
}

/// Zero and minus one padded to every depth are refused as an `i64`.
#[test]
fn sim_adversary_leb128_i64_sign_padded_to_any_depth_is_refused() {
    padding_to_any_depth_is_refused::<i64>();
}

/// Zero and minus one padded to every depth are refused as an `i32`.
#[test]
fn sim_adversary_leb128_i32_sign_padded_to_any_depth_is_refused() {
    padding_to_any_depth_is_refused::<i32>();
}

/// A run of continuations overflows a `u64` at its last group.
#[test]
fn sim_adversary_leb128_u64_a_continuation_run_overflows_at_the_last_group() {
    a_run_of_continuations_overflows_at_the_last_group::<u64>();
}

/// A run of continuations overflows a `u32` at its last group.
#[test]
fn sim_adversary_leb128_u32_a_continuation_run_overflows_at_the_last_group() {
    a_run_of_continuations_overflows_at_the_last_group::<u32>();
}

/// A run of continuations overflows an `i64` at its last group.
#[test]
fn sim_adversary_leb128_i64_a_continuation_run_overflows_at_the_last_group() {
    a_run_of_continuations_overflows_at_the_last_group::<i64>();
}

/// A run of continuations overflows an `i32` at its last group.
#[test]
fn sim_adversary_leb128_i32_a_continuation_run_overflows_at_the_last_group() {
    a_run_of_continuations_overflows_at_the_last_group::<i32>();
}

/// All-continuation input ends or overflows a `u64` exactly where it must.
#[test]
fn sim_adversary_leb128_u64_dangling_continuations_end_exactly() {
    dangling_continuations_end_exactly::<u64>();
}

/// All-continuation input ends or overflows a `u32` exactly where it must.
#[test]
fn sim_adversary_leb128_u32_dangling_continuations_end_exactly() {
    dangling_continuations_end_exactly::<u32>();
}

/// All-continuation input ends or overflows an `i64` exactly where it must.
#[test]
fn sim_adversary_leb128_i64_dangling_continuations_end_exactly() {
    dangling_continuations_end_exactly::<i64>();
}

/// All-continuation input ends or overflows an `i32` exactly where it must.
#[test]
fn sim_adversary_leb128_i32_dangling_continuations_end_exactly() {
    dangling_continuations_end_exactly::<i32>();
}

/// The `u64` encoder emits the fewest groups.
#[test]
fn sim_adversary_leb128_u64_encodings_have_the_minimal_length() -> TestResult {
    encodings_have_exactly_the_minimal_length::<u64>()
}

/// The `u32` encoder emits the fewest groups.
#[test]
fn sim_adversary_leb128_u32_encodings_have_the_minimal_length() -> TestResult {
    encodings_have_exactly_the_minimal_length::<u32>()
}

/// The `i64` encoder emits the fewest groups.
#[test]
fn sim_adversary_leb128_i64_encodings_have_the_minimal_length() -> TestResult {
    encodings_have_exactly_the_minimal_length::<i64>()
}

/// The `i32` encoder emits the fewest groups.
#[test]
fn sim_adversary_leb128_i32_encodings_have_the_minimal_length() -> TestResult {
    encodings_have_exactly_the_minimal_length::<i32>()
}

/// Values around every power of two round-trip as `u64`.
#[test]
fn sim_adversary_leb128_u64_power_of_two_boundaries_round_trip() {
    boundaries_around_every_power_of_two_round_trip::<u64>();
}

/// Values around every power of two round-trip as `u32`.
#[test]
fn sim_adversary_leb128_u32_power_of_two_boundaries_round_trip() {
    boundaries_around_every_power_of_two_round_trip::<u32>();
}

/// Values around every power of two round-trip as `i64`.
#[test]
fn sim_adversary_leb128_i64_power_of_two_boundaries_round_trip() {
    boundaries_around_every_power_of_two_round_trip::<i64>();
}

/// Values around every power of two round-trip as `i32`.
#[test]
fn sim_adversary_leb128_i32_power_of_two_boundaries_round_trip() {
    boundaries_around_every_power_of_two_round_trip::<i32>();
}

/// A hostile `u64` stream is read by a cursor that always advances and ends.
#[test]
fn sim_adversary_leb128_u64_a_stream_read_advances_and_ends() -> TestResult {
    a_stream_read_always_advances_and_ends::<u64>()
}

/// A hostile `u32` stream is read by a cursor that always advances and ends.
#[test]
fn sim_adversary_leb128_u32_a_stream_read_advances_and_ends() -> TestResult {
    a_stream_read_always_advances_and_ends::<u32>()
}

/// A hostile `i64` stream is read by a cursor that always advances and ends.
#[test]
fn sim_adversary_leb128_i64_a_stream_read_advances_and_ends() -> TestResult {
    a_stream_read_always_advances_and_ends::<i64>()
}

/// A hostile `i32` stream is read by a cursor that always advances and ends.
#[test]
fn sim_adversary_leb128_i32_a_stream_read_advances_and_ends() -> TestResult {
    a_stream_read_always_advances_and_ends::<i32>()
}

/// The tenth `u64` group admits one bit and must end the sequence: `2^63` is
/// accepted, any more is an overflow at that group.
#[test]
fn sim_adversary_leb128_u64_the_last_group_admits_one_bit() {
    assert_eq!(
        decode_u64(&run_then(0x80, 9, 0x01)),
        Ok((9_223_372_036_854_775_808, 10)),
        "2^63 is the largest single-bit top group"
    );
    for last in [0x02_u8, 0x03, 0x7f] {
        assert_eq!(
            decode_u64(&run_then(0x80, 9, last)),
            Err(DecodeError::Overflow { at: 9 }),
            "a top group of {last:#04x} carries bits above 64"
        );
    }
}

/// The fifth `u32` group admits four bits and must end the sequence.
#[test]
fn sim_adversary_leb128_u32_the_last_group_admits_four_bits() {
    assert_eq!(
        decode_u32(&run_then(0x80, 4, 0x0f)),
        Ok((0xf000_0000, 5)),
        "the largest top group is accepted"
    );
    for last in [0x10_u8, 0x1f, 0x7f] {
        assert_eq!(
            decode_u32(&run_then(0x80, 4, last)),
            Err(DecodeError::Overflow { at: 4 }),
            "a top group of {last:#04x} carries bits above 32"
        );
    }
}

/// The tenth `i64` group is the sign extension of the nine below it: the two
/// extremes are accepted and anything between is an overflow at that group.
#[test]
fn sim_adversary_leb128_i64_the_last_group_is_a_sign_extension() {
    assert_eq!(
        decode_i64(&run_then(0x80, 9, 0x7f)),
        Ok((i64::MIN, 10)),
        "i64::MIN ends in a sign group of 0x7f"
    );
    assert_eq!(
        decode_i64(&run_then(0xff, 9, 0x00)),
        Ok((i64::MAX, 10)),
        "i64::MAX ends in a sign group of 0x00"
    );
    for last in [0x01_u8, 0x40, 0x7e] {
        assert_eq!(
            decode_i64(&run_then(0x80, 9, last)),
            Err(DecodeError::Overflow { at: 9 }),
            "a top group of {last:#04x} is neither sign extension"
        );
    }
}

/// The fifth `i32` group is the sign extension of the four below it.
#[test]
fn sim_adversary_leb128_i32_the_last_group_is_a_sign_extension() {
    assert_eq!(
        decode_i32(&run_then(0x80, 4, 0x78)),
        Ok((i32::MIN, 5)),
        "i32::MIN ends in a sign group of 0x78"
    );
    assert_eq!(
        decode_i32(&run_then(0xff, 4, 0x07)),
        Ok((i32::MAX, 5)),
        "i32::MAX ends in a sign group of 0x07"
    );
    for last in [0x08_u8, 0x10, 0x40, 0x77] {
        assert_eq!(
            decode_i32(&run_then(0x80, 4, last)),
            Err(DecodeError::Overflow { at: 4 }),
            "a top group of {last:#04x} is neither sign extension"
        );
    }
}

/// A `u64` read by a `u32` reader is taken exactly when it fits.
#[test]
fn sim_adversary_leb128_a_u32_reader_takes_exactly_the_u64_values_that_fit() -> TestResult {
    a_narrower_reader_takes_exactly_what_fits::<u64, u32>()
}

/// An `i64` read by an `i32` reader is taken exactly when it fits.
#[test]
fn sim_adversary_leb128_an_i32_reader_takes_exactly_the_i64_values_that_fit() -> TestResult {
    a_narrower_reader_takes_exactly_what_fits::<i64, i32>()
}

/// Nothing is not a value: every width refuses empty input at offset zero.
#[test]
fn sim_adversary_leb128_empty_input_is_an_unexpected_end_at_zero() {
    assert_eq!(
        decode_u64(&[]),
        Err(DecodeError::UnexpectedEnd { at: 0 }),
        "u64"
    );
    assert_eq!(
        decode_u32(&[]),
        Err(DecodeError::UnexpectedEnd { at: 0 }),
        "u32"
    );
    assert_eq!(
        decode_i64(&[]),
        Err(DecodeError::UnexpectedEnd { at: 0 }),
        "i64"
    );
    assert_eq!(
        decode_i32(&[]),
        Err(DecodeError::UnexpectedEnd { at: 0 }),
        "i32"
    );
}

/// A refusal's message is one printable line with its offset, whatever bytes
/// caused it, so a hostile stream cannot forge a log line (CWE-117).
#[test]
fn sim_adversary_leb128_a_refusal_message_is_one_safe_line() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let len = 1_usize.saturating_add(attacker.below(14));
        let bytes = attacker.bytes(len);
        for message in [
            refusal_text::<u64>(&bytes),
            refusal_text::<u32>(&bytes),
            refusal_text::<i64>(&bytes),
            refusal_text::<i32>(&bytes),
        ]
        .into_iter()
        .flatten()
        {
            assert!(
                !message.contains(['\n', '\r', '\u{1b}', '\0'])
                    && message.chars().any(|glyph| glyph.is_ascii_digit()),
                "seed {seed:#x} round {round}: {message:?} is not one printable line with an offset"
            );
        }
        Ok(())
    })
}

/// The same seed mounts the same attack on all four readers.
#[test]
fn sim_adversary_leb128_the_same_seed_replays_the_same_campaign() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(campaign, seed);
    }
}

/// Two seeds mount two attacks.
#[test]
fn sim_adversary_leb128_distinct_seeds_mount_distinct_campaigns() {
    assert_distinct_seeds_diverge(campaign, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
    assert_distinct_seeds_diverge(campaign, SWEEP_SEEDS[2], SWEEP_SEEDS[3]);
}
