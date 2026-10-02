//! Shared schema for the wire facade tests.
//!
//! The same type definitions back the retained fixture reader (`wire_fixture.rs`),
//! the seeded simulations (`sim_wire.rs`), and the feature-unification probe
//! (`wire_feature_unification.rs`), so the three cannot drift into testing
//! different schemas.
#![cfg(feature = "wire")]
#![allow(dead_code, reason = "each test target uses a subset of these items")]

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
pub const PINNED_POINTER_WIDTH_BITS: u8 = 32;
/// The fixture's archived `u32` alignment, in bytes.
pub const PINNED_U32_ALIGNMENT: u8 = 4;

/// The hex payload of the retained fixture, with `#` metadata lines removed.
#[must_use]
pub fn fixture_hex() -> String {
    let raw = include_str!("../fixtures/wire/consumer_record_v1.hex");
    raw.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect()
}
