//! Public-consumer regression checks for codec coordinate contracts.
#![forbid(unsafe_code)]

#[cfg(all(feature = "hash", feature = "random"))]
use lgwks_std::hash::Digest;
#[cfg(all(feature = "hash", feature = "random"))]
use lgwks_std::id::Uuid;
#[cfg(all(feature = "random", feature = "hash"))]
use lgwks_std::leb128;
use lgwks_std::{encoding::percent, hex};

#[test]
fn consumer_sees_source_and_decoded_coordinate_spaces() {
    assert_eq!(
        percent::decode("ok%2G"),
        Err(percent::DecodeError::NotHexDigit { at: 4 }),
        "malformed percent escapes identify the original offending byte"
    );
    assert_eq!(
        percent::decode("é%G0"),
        Err(percent::DecodeError::NotHexDigit { at: 3 }),
        "source offsets count UTF-8 input bytes"
    );
    assert_eq!(
        percent::decode("a%20%FF"),
        Err(percent::DecodeError::NotUtf8 { decoded_at: 2 }),
        "UTF-8 failures identify their decoded-byte coordinate"
    );
}

#[test]
fn consumer_uses_atomic_exact_length_hex_decode() {
    let mut output = [0xa5; 2];
    assert_eq!(
        hex::decode_into("aabz", &mut output),
        Err(hex::DecodeError::NotHexDigit { at: 3, byte: b'z' }),
        "invalid data identifies the offending character"
    );
    assert_eq!(output, [0xa5; 2], "refused input leaves output unchanged");
    assert_eq!(
        hex::decode("Af09").as_deref(),
        Ok(&[0xaf, 0x09][..]),
        "the owned convenience API shares uppercase decoding behavior"
    );
}

#[test]
#[cfg(all(feature = "random", feature = "hash"))]
fn consumer_preserves_uuid_digest_and_minimal_leb_profiles()
-> Result<(), Box<dyn std::error::Error>> {
    let uuid = Uuid::parse("00000000-0000-0000-0000-000000000000")?;
    assert_eq!(
        uuid.to_string(),
        "00000000-0000-0000-0000-000000000000",
        "UUID parse/display preserve arbitrary UUID bits"
    );

    let digest =
        Digest::from_hex("af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262")?;
    assert_eq!(
        digest.to_string(),
        digest.to_hex(),
        "digest formatting writes canonical lowercase hex"
    );

    assert_eq!(
        leb128::decode_u64(&[0x00, 0xff]),
        Ok((0, 1)),
        "LEB decoding consumes only the minimal value prefix"
    );
    Ok(())
}
