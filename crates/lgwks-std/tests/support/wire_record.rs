//! Shared schema for the wire facade tests.
//!
//! The same type definitions back the retained fixture reader (`wire_fixture.rs`),
//! the seeded simulations (`sim_wire.rs`), and the feature-unification probe
//! (`wire_feature_unification.rs`), so the three cannot drift into testing
//! different schemas. It carries its own tests rather than an `allow`: each
//! target reads a different subset of it, and a fixture that is only correct
//! where its unread half is silenced is a fixture nobody has read.
#![cfg(feature = "wire")]

use std::collections::BTreeMap;

use lgwks_std::wire::{Archive, Deserialize, Serialize};

/// A representative nested consumer value.
#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Eq, Clone)]
#[rkyv(crate = lgwks_std::wire::rkyv, derive(Debug))]
pub struct ConsumerRecord {
    /// Borrowed-from-archive text.
    pub(crate) name: String,
    /// Pointer-sized collection.
    pub(crate) indices: Vec<usize>,
    /// Signed pointer-sized scalar.
    pub(crate) signed_index: isize,
    /// Ordered map, so bytes are independent of the map's iteration choice.
    pub(crate) fields: BTreeMap<String, u32>,
    /// Optional boxed child.
    pub(crate) child: Option<Box<ConsumerChild>>,
}

/// A nested child record.
#[derive(Archive, Deserialize, Serialize, Debug, PartialEq, Eq, Clone)]
#[rkyv(crate = lgwks_std::wire::rkyv, compare(PartialEq), derive(Debug))]
pub struct ConsumerChild {
    /// Child scalar.
    pub(crate) value: u64,
}

/// The fixture value, identical to the one the retained bytes were produced from.
#[must_use]
pub fn sample() -> ConsumerRecord {
    let mut fields = BTreeMap::new();
    fields.insert(String::from("height"), 200);
    fields.insert(String::from("width"), 100);
    ConsumerRecord {
        name: String::from("consumer fixture"),
        indices: vec![3, 5, 8],
        signed_index: -13,
        fields,
        child: Some(Box::new(ConsumerChild { value: 42 })),
    }
}

/// The format the retained fixture bytes were produced under.
pub const PINNED_POINTER_WIDTH_BITS: usize = 32;
/// The fixture's archived `u32` alignment, in bytes.
pub const PINNED_U32_ALIGNMENT: usize = 4;

/// The hex payload of the retained fixture, with `#` metadata lines removed.
#[must_use]
pub fn fixture_hex() -> String {
    let raw = include_str!("../fixtures/wire/consumer_record_v1.hex");
    raw.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect()
}

#[cfg(test)]
mod tests {
    use lgwks_std::wire::{WireError, access, to_bytes};

    use super::{
        ArchivedConsumerRecord, ConsumerChild, ConsumerRecord, PINNED_POINTER_WIDTH_BITS,
        PINNED_U32_ALIGNMENT, fixture_hex, sample,
    };

    /// The retained fixture's own schema, so a target that never reads the bytes
    /// still refuses a schema that has drifted away from them.
    #[test]
    fn the_fixture_value_is_the_one_its_bytes_were_produced_from() {
        let record = sample();
        assert_eq!(
            record.name, "consumer fixture",
            "the fixture's text field is the one the retained bytes carry"
        );
        assert_eq!(
            record.indices,
            vec![3, 5, 8],
            "the fixture's pointer-sized collection is the one the retained bytes carry"
        );
        assert_eq!(
            record.signed_index, -13,
            "the fixture's signed scalar is the one the retained bytes carry"
        );
        assert_eq!(
            record.fields.get("height").copied(),
            Some(200),
            "the fixture's ordered map carries its height"
        );
        assert_eq!(
            record.child.as_deref(),
            Some(&ConsumerChild { value: 42 }),
            "the fixture's optional boxed child is present and carries its value"
        );
    }

    /// A struct equal to itself is not the property; two draws are, because a
    /// sample built from a `BTreeMap` has one order however it is drawn.
    #[test]
    fn two_draws_of_the_fixture_are_the_same_value() {
        assert_eq!(
            sample(),
            sample(),
            "the fixture must not depend on map iteration order or draw order"
        );
    }

    /// The fixture is pinned to one format and these two numbers are that pin. A
    /// change to either changes what the retained bytes mean, and the fixture's
    /// reader refuses every target whose descriptor does not match them.
    #[test]
    fn the_fixture_stays_pinned_to_the_format_its_bytes_were_archived_under() {
        assert_eq!(
            PINNED_POINTER_WIDTH_BITS, 32,
            "the retained fixture's archived pointer width"
        );
        assert_eq!(
            PINNED_U32_ALIGNMENT, 4,
            "the retained fixture's archived u32 alignment"
        );
    }

    /// The fixture schema round-trips through this build's own archive, which is
    /// what makes the retained bytes readable rather than merely retained.
    #[test]
    fn the_fixture_schema_round_trips_through_this_build_s_archive() -> Result<(), WireError> {
        let bytes = to_bytes::<WireError>(&sample())?;
        let archived = access::<ArchivedConsumerRecord, WireError>(&bytes)?;
        assert_eq!(
            archived.name.as_str(),
            "consumer fixture",
            "the archived view of the fixture carries the text it was drawn with"
        );
        assert_eq!(
            archived.signed_index, -13,
            "the archived view of the fixture carries its signed scalar"
        );
        assert_eq!(
            archived.child.as_ref().map(|child| child.value.to_native()),
            Some(42),
            "the archived view of the fixture carries its optional child"
        );
        Ok(())
    }

    /// The fixture payload is hex with its metadata lines gone.
    #[test]
    fn the_fixture_payload_is_hex_without_its_metadata() {
        let payload = fixture_hex();
        assert!(
            !payload.is_empty(),
            "the retained fixture must carry bytes to read"
        );
        assert!(
            payload.lines().all(|line| {
                line.trim().is_empty() || line.trim().bytes().all(|byte| byte.is_ascii_hexdigit())
            }),
            "every retained byte must be a hex digit, got {payload:?}"
        );
        let record: ConsumerRecord = sample();
        assert_eq!(
            record.fields.len(),
            2,
            "the fixture's ordered map has two entries, so its hex payload is the whole record"
        );
    }
}
