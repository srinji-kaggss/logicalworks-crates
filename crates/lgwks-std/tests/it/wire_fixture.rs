//! Reads the retained wire fixture: a committed byte-for-byte archive that
//! pins an application schema *and* a codec format, decoded on every target
//! whose effective format matches the pinned one.
//!
//! This is the cross-target half of #167's acceptance: the bytes and the
//! schema live in the repository, not in a value reconstructed at test time,
//! so a change to either the schema or the rkyv format controls that would
//! move the encoding is caught on every OS the suite runs on.
#![forbid(unsafe_code)]

#[cfg(feature = "wire")]
use crate::wire_record;

#[cfg(feature = "wire")]
mod fixture {
    use lgwks_std::wire::{Endianness, WireError, access, from_bytes, to_bytes};

    use crate::wire_fixture::wire_record::{
        ArchivedConsumerRecord, ConsumerRecord, PINNED_POINTER_WIDTH_BITS, PINNED_U32_ALIGNMENT,
        fixture_hex, sample,
    };

    /// Decode the retained fixture hex, or report a refusal with its cause.
    fn fixture_bytes() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        Ok(lgwks_std::hex::decode(fixture_hex()).map_err(|error| {
            std::io::Error::other(format!("retained fixture is not hex: {error}"))
        })?)
    }

    /// Whether this build's effective rkyv format is the one the fixture pins.
    fn format_is_pinned() -> Result<bool, WireError> {
        let format = lgwks_std::wire::format_descriptor()?;
        Ok(format.pointer_width_bits == PINNED_POINTER_WIDTH_BITS
            && format.archived_u32_alignment == PINNED_U32_ALIGNMENT
            && format.endianness == Endianness::Little)
    }

    #[test]
    fn retained_fixture_decodes_to_the_pinned_application_value()
    -> Result<(), Box<dyn std::error::Error>> {
        // A feature-unified build that selects another width/endianness/alignment
        // deliberately cannot read this fixture; the probe in
        // `wire_feature_unification.rs` is what exercises that configuration.
        if !format_is_pinned()? {
            return Ok(());
        }
        let bytes = fixture_bytes()?;
        let archived = access::<ArchivedConsumerRecord, WireError>(&bytes)?;
        assert_eq!(
            archived.name.as_str(),
            "consumer fixture",
            "retained bytes carry the pinned schema's text field"
        );
        assert_eq!(
            u64::from(archived.indices[2].to_native()),
            8,
            "retained bytes carry pointer-sized indices under the pinned width"
        );
        assert_eq!(
            i64::from(archived.signed_index.to_native()),
            -13,
            "retained bytes carry the signed pointer-sized scalar"
        );

        let restored = from_bytes::<ConsumerRecord, WireError>(&bytes)?;
        assert_eq!(
            restored,
            sample(),
            "the retained fixture reconstructs the exact retained value"
        );
        Ok(())
    }

    #[test]
    fn the_pinned_schema_still_emits_the_retained_bytes() -> Result<(), Box<dyn std::error::Error>>
    {
        if !format_is_pinned()? {
            return Ok(());
        }
        let pinned = fixture_bytes()?;
        let emitted = to_bytes::<WireError>(&sample())?;
        assert_eq!(
            emitted.as_slice(),
            pinned.as_slice(),
            "the pinned schema and format reproduce the retained fixture byte for byte"
        );
        Ok(())
    }
}
