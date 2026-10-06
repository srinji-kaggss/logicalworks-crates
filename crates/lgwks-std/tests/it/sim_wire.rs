//! Seeded simulations of the wire facade's byte-level contract.
//!
//! One seed drives the generated nested value, its serialized bytes, and every
//! mutation applied to them. Each failing assertion names its seed, and the
//! trace hash of the emitted bytes is asserted stable for a seed, so a
//! regression replays identically.
#![forbid(unsafe_code)]

#[cfg(feature = "wire")]
use crate::rng;
#[cfg(feature = "wire")]
use crate::wire_record;

#[cfg(feature = "wire")]
mod sim {
    use std::collections::BTreeMap;

    use lgwks_std::wire::{WireError, access, from_bytes, to_bytes};

    use crate::sim_wire::rng::Rng;
    use crate::sim_wire::wire_record::{ArchivedConsumerRecord, ConsumerChild, ConsumerRecord};

    /// The width of the signed field's drawn range.
    const SIGNED_SPAN: isize = 4_096;

    /// Where that range sits around zero: half of it below zero and half above,
    /// so a drawn value exercises both signs.
    const SIGNED_MIDPOINT: isize = 2_048;

    /// The lowercase alphabet the drawn names and keys are made of.
    const LETTERS: [u8; 26] = *b"abcdefghijklmnopqrstuvwxyz";

    /// A lowercase letter drawn from the generator.
    fn letter(rng: &mut Rng) -> char {
        // The alphabet is the table, so a letter is a position in it rather than
        // an offset added to `'a'`, and no conversion sits between the draw and
        // the character it names.
        let position = rng.below(LETTERS.len());
        char::from(LETTERS[position])
    }

    /// A word of `1..=max` letters drawn from the generator.
    fn word(rng: &mut Rng, max: usize) -> String {
        let length = rng.below(max).saturating_add(1);
        (0..length).map(|_| letter(rng)).collect()
    }

    /// Build a value from `seed` alone, so the same seed is the same value.
    fn value_for(seed: u64) -> ConsumerRecord {
        let mut rng = Rng::new(seed);
        let name = word(&mut rng, 16);
        let index_count = rng.below(6);
        let indices: Vec<usize> = (0..index_count).map(|_| rng.below(1024)).collect();
        // The signed field is drawn from the low bytes of the stream's own word,
        // which is where a signed value's own bits are: the draw is read as its
        // two's complement and then offset around the middle of the field's
        // range, so no narrowing decides what a negative draw becomes.
        let raw = rng.next().to_le_bytes();
        let signed_index = isize::from(i16::from_le_bytes([raw[0], raw[1]]))
            .rem_euclid(SIGNED_SPAN)
            .saturating_sub(SIGNED_MIDPOINT);
        let mut fields = BTreeMap::new();
        for _ in 0..rng.below(4) {
            let key = word(&mut rng, 6);
            // The map's values are the low half of the stream's own word, which
            // is the width the archived value is: a value is one the map holds
            // rather than a narrowed index.
            let word = rng.next().to_le_bytes();
            let value = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
            fields.insert(key, value);
        }
        let child = (rng.below(2) == 1).then(|| Box::new(ConsumerChild { value: rng.next() }));
        ConsumerRecord {
            name,
            indices,
            signed_index,
            fields,
            child,
        }
    }

    /// A stable hash of an archive, reported so a failure prints its trace.
    fn trace_hash(bytes: &[u8]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    const SEEDS: u64 = 64;

    #[test]
    fn a_seed_replays_to_the_same_bytes_and_roundtrip() -> Result<(), WireError> {
        for seed in 0..SEEDS {
            let value = value_for(seed);
            let first = to_bytes::<WireError>(&value)?;
            let second = to_bytes::<WireError>(&value_for(seed))?;
            assert_eq!(
                first.as_slice(),
                second.as_slice(),
                "seed {seed}: the same value must serialize to the same bytes"
            );
            assert_eq!(
                trace_hash(first.as_slice()),
                trace_hash(second.as_slice()),
                "seed {seed}: trace hash is stable"
            );
            let archived = access::<ArchivedConsumerRecord, WireError>(&first)?;
            assert_eq!(
                archived.name.as_str(),
                value.name.as_str(),
                "seed {seed}: text is borrowed from the checked archive"
            );
            let restored = from_bytes::<ConsumerRecord, WireError>(&first)?;
            assert_eq!(
                restored, value,
                "seed {seed}: reconstruction restores the generated value"
            );
        }
        Ok(())
    }

    #[test]
    fn a_truncated_prefix_is_never_a_valid_archive() -> Result<(), WireError> {
        for seed in 0..SEEDS {
            let bytes = to_bytes::<WireError>(&value_for(seed))?;
            let cut = {
                let mut rng = Rng::new(seed ^ 0xdead_beef);
                rng.below(bytes.len())
            };
            let truncated = &bytes[..cut];
            assert!(
                access::<ArchivedConsumerRecord, WireError>(truncated).is_err(),
                "seed {seed}: access accepted a {cut}-byte prefix of a {}-byte archive",
                bytes.len()
            );
        }
        Ok(())
    }

    #[test]
    fn distinct_seeds_explore_distinct_values() {
        let mut seen = std::collections::BTreeSet::new();
        for seed in 0..SEEDS {
            seen.insert(format!("{:?}", value_for(seed)));
        }
        assert!(
            seen.len() > 1,
            "the generator must not collapse every seed onto one value"
        );
    }

    #[test]
    fn a_shifted_archive_is_never_validated() -> Result<(), WireError> {
        for seed in 0..SEEDS {
            let bytes = to_bytes::<WireError>(&value_for(seed))?;
            // Drop the first byte: the archive is now offset-misaligned against
            // the type's primitive alignment, which checked access must refuse.
            let shifted = &bytes[1..];
            assert!(
                access::<ArchivedConsumerRecord, WireError>(shifted).is_err(),
                "seed {seed}: access accepted a misaligned {} -byte archive",
                shifted.len()
            );
        }
        Ok(())
    }
}
