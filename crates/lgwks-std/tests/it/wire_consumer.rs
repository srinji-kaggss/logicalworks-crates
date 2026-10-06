//! Exercises the wire facade from an external-crate consumer context.
#![cfg(feature = "wire")]

use crate::wire_record;

use std::mem::{align_of, size_of};

use lgwks_std::wire::{
    self, Archive, Deserialize, Endianness, Serialize, WireError, access, from_bytes, to_bytes,
};
use wire_record::{ArchivedConsumerRecord, ConsumerChild, ConsumerRecord, sample};

#[derive(Archive, Deserialize, Serialize)]
#[rkyv(crate = lgwks_std::wire::rkyv, derive(Debug))]
struct StructurallySimilarButDifferentSchema {
    name: String,
    indices: Vec<usize>,
    signed_index: isize,
    fields: std::collections::BTreeMap<String, u32>,
    child: Option<Box<ConsumerChild>>,
}

#[test]
fn external_consumer_derives_serializes_checked_accesses_and_reconstructs() -> Result<(), WireError>
{
    let value = sample();
    let bytes = to_bytes::<WireError>(&value)?;
    let archived = access::<ArchivedConsumerRecord, WireError>(&bytes)?;
    assert_eq!(
        archived.name.as_str(),
        value.name.as_str(),
        "nested text is borrowed from the checked archive"
    );
    assert_eq!(
        archived.indices.len(),
        value.indices.len(),
        "archive retains the pointer-sized collection length"
    );
    assert_eq!(
        u64::from(archived.indices[0].to_native()),
        3,
        "archive retains pointer-sized collection entries"
    );
    assert_eq!(
        i64::from(archived.signed_index.to_native()),
        -13,
        "archive retains signed pointer-sized values"
    );

    let restored = from_bytes::<ConsumerRecord, WireError>(&bytes)?;
    assert_eq!(
        restored, value,
        "reconstruction restores the complete nested consumer value"
    );
    Ok(())
}

#[test]
fn reports_effective_rkyv_primitive_format() -> Result<(), WireError> {
    let format = wire::format_descriptor()?;

    assert!(
        matches!(format.endianness, Endianness::Little | Endianness::Big),
        "format probe identifies one of rkyv's supported byte orders"
    );
    assert!(
        matches!(format.pointer_width_bits, 16 | 32 | 64),
        "format reports a supported archived pointer width"
    );
    assert!(
        matches!(format.archived_u32_alignment, 1 | 2 | 4),
        "format reports archived primitive alignment"
    );
    assert_eq!(
        format.codec_series, "rkyv 0.8",
        "format identifies the compatibility series, not a patch pin"
    );
    let archived_usize_bytes = size_of::<lgwks_std::wire::rkyv::primitive::ArchivedUsize>();
    let expected_pointer_width_bits = archived_usize_bytes.saturating_mul(8);
    assert_eq!(
        format.pointer_width_bits, expected_pointer_width_bits,
        "reported pointer width matches the effective archived alias"
    );
    assert_eq!(
        format.archived_u32_alignment,
        align_of::<lgwks_std::wire::rkyv::primitive::ArchivedU32>(),
        "reported alignment matches the effective archived primitive"
    );
    Ok(())
}

#[test]
fn equivalent_ordered_values_have_repeatable_bytes() -> Result<(), WireError> {
    let first = sample();
    let second = sample();
    let first_bytes = to_bytes::<WireError>(&first)?;
    let second_bytes = to_bytes::<WireError>(&second)?;

    assert_eq!(
        first, second,
        "the independently constructed records are equal"
    );
    assert_eq!(
        first_bytes.as_slice(),
        second_bytes.as_slice(),
        "the same schema and ordered map produce repeatable bytes in this format"
    );
    Ok(())
}

#[test]
fn different_values_produce_different_archive_bytes() -> Result<(), WireError> {
    let mut first = sample();
    let second = sample();
    first.signed_index = -14;
    let first_bytes = to_bytes::<WireError>(&first)?;
    let second_bytes = to_bytes::<WireError>(&second)?;

    assert_ne!(
        first_bytes.as_slice(),
        second_bytes.as_slice(),
        "archives preserve a changed field in this schema and format"
    );
    Ok(())
}

#[test]
fn checked_access_rejects_truncated_and_misaligned_archive_bytes() -> Result<(), WireError> {
    let bytes = to_bytes::<WireError>(&sample())?;
    let truncated = &bytes[..bytes.len().saturating_sub(1)];
    assert!(
        access::<ArchivedConsumerRecord, WireError>(truncated).is_err(),
        "checked access rejects a truncated archive"
    );

    let shifted = &bytes[1..];
    assert!(
        access::<ArchivedConsumerRecord, WireError>(shifted).is_err(),
        "checked access rejects an offset-misaligned archive"
    );
    Ok(())
}

#[test]
fn structural_validation_does_not_supply_application_schema_identity() -> Result<(), WireError> {
    let bytes = to_bytes::<WireError>(&sample())?;

    // This type has the same archived field layout but a different Rust name.
    // rkyv validates its representation; an application envelope must bind
    // the schema/version before treating it as the intended record.
    let _other_schema = access::<ArchivedStructurallySimilarButDifferentSchema, WireError>(&bytes)?;
    Ok(())
}

#[test]
fn equal_zero_values_can_have_different_archive_bytes() -> Result<(), WireError> {
    let positive_zero = 0.0_f32;
    let negative_zero = -0.0_f32;
    assert!(
        f32::eq(&positive_zero, &negative_zero),
        "IEEE signed zeros compare equal as Rust values"
    );

    let positive_bytes = to_bytes::<WireError>(&positive_zero)?;
    let negative_bytes = to_bytes::<WireError>(&negative_zero)?;
    assert_ne!(
        positive_bytes.as_slice(),
        negative_bytes.as_slice(),
        "archive bytes preserve distinct signed-zero representations"
    );
    Ok(())
}
