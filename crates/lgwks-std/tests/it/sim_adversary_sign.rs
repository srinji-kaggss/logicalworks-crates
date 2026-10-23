//! A forger controls the record, the tag, the public key and the signature bytes.
//!
//! A seal is only as good as the checks behind it, so the attacks are the ones
//! that have forged real signatures: a second valid encoding of the same
//! signature (the Ed25519 scalar malleability RFC 8032 section 5.1.7 closes and
//! CVE-2020-36843 and its relatives reopened), a small-order public key with an
//! all-zero signature that verifies every message in a lax verifier, a tag that
//! can be slid across the record boundary, and a single flipped bit anywhere. The
//! attacks: every bit of the signature, record, tag and key flipped in turn, the
//! group order added to the scalar up to seven times, the identity and order-two
//! points as keys, forged all-zero and all-one signatures, every split of a
//! string into tag and record, a tag extended with the digest's own bytes, and
//! megabytes of tag and record.
//!
//! The signed message is `tag || BLAKE3(record)`; the group order below is the
//! constant of RFC 8032 section 5.1.
#![cfg(feature = "sign")]

use std::collections::BTreeSet;

use lgwks_std::sign::{PublicKey, SecretKey, Signature, VerifyError, sign, verify_strict};

use crate::adversary::{
    Attacker, TestResult, diverges_between_seeds, flip_bit, replays_under_every_seed, sweep,
};
use crate::seeded_bytes::fold_bytes;
use crate::seeded_sweep::{SWEEP_SEEDS, initial_trace};

/// The order `L` of the Ed25519 base point, little-endian (RFC 8032 section 5.1):
/// `2^252 + 27742317777372353535851937790883648493`.
const GROUP_ORDER: [u8; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
];

/// The encoding of the identity point, which has order one.
const IDENTITY: [u8; 32] = [
    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// The encoding of the point of order two, `(0, -1)`.
const ORDER_TWO: [u8; 32] = [
    0xec, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f,
];

/// A tag, as a record kind a caller might seal.
const TAG: &[u8] = b"lgwks.test.record.v1";

/// `left + right` as 256-bit little-endian integers, or `None` past 256 bits.
fn add_le(left: &[u8; 32], right: &[u8; 32]) -> Option<[u8; 32]> {
    let mut out = [0_u8; 32];
    let mut carry = 0_u16;
    for ((slot, &one), &other) in out.iter_mut().zip(left).zip(right) {
        let sum = u16::from(one)
            .saturating_add(u16::from(other))
            .saturating_add(carry);
        let [low, high] = sum.to_le_bytes();
        *slot = low;
        carry = u16::from(high);
    }
    (carry == 0).then_some(out)
}

/// A signature's bytes with the scalar half replaced.
fn with_scalar(signature: &Signature, scalar: &[u8; 32]) -> Signature {
    let mut bytes = signature.to_bytes();
    bytes[32..].copy_from_slice(scalar);
    Signature::from_bytes(bytes)
}

/// The scalar half of a signature.
fn scalar_of(signature: &Signature) -> [u8; 32] {
    let mut scalar = [0_u8; 32];
    scalar.copy_from_slice(&signature.to_bytes()[32..]);
    scalar
}

/// A signature with these bytes, taken from a slice of exactly sixty-four.
fn signature_of(bytes: &[u8]) -> Signature {
    let mut raw = [0_u8; 64];
    for (slot, &byte) in raw.iter_mut().zip(bytes) {
        *slot = byte;
    }
    Signature::from_bytes(raw)
}

/// A drawn key, with its public half.
fn draw_key(attacker: &mut Attacker) -> (SecretKey, PublicKey) {
    let secret = SecretKey::from_seed(attacker.array::<32>());
    let public = secret.public_key();
    (secret, public)
}

/// A drawn record of `below(max)` bytes.
fn draw_record(attacker: &mut Attacker, max: usize) -> Vec<u8> {
    let len = attacker.below(max);
    attacker.bytes(len)
}

/// One campaign: seals over drawn records, signature bytes folded into a trace.
fn campaign(seed: u64) -> u64 {
    let mut attacker = Attacker::new(seed);
    let mut trace = initial_trace();
    for _ in 0..64 {
        let (secret, _) = draw_key(&mut attacker);
        let record = draw_record(&mut attacker, 128);
        fold_bytes(&mut trace, &sign(&secret, TAG, &record).to_bytes());
    }
    trace
}

/// Every seal verifies under its own key, tag and record, for drawn keys,
/// drawn tags and records of every size from empty to a few hundred bytes.
#[test]
fn sim_adversary_sign_a_seal_opens_under_its_own_key() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let (secret, public) = draw_key(attacker);
        let tag = draw_record(attacker, 40);
        let record = draw_record(attacker, 400);
        assert_eq!(
            verify_strict(&public, &tag, &record, &sign(&secret, &tag, &record)),
            Ok(()),
            "seed {seed:#x} round {round}"
        );
        Ok(())
    })
}

/// Every one of the 512 bits of a signature, flipped in turn, breaks the seal.
#[test]
fn sim_adversary_sign_every_signature_bit_is_checked() -> TestResult {
    sweep(4, |seed, round, attacker| {
        let (secret, public) = draw_key(attacker);
        let record = draw_record(attacker, 64);
        let seal = sign(&secret, TAG, &record).to_bytes();
        for bit in 0..512 {
            let damaged = signature_of(&flip_bit(&seal, bit));
            assert_eq!(
                verify_strict(&public, TAG, &record, &damaged),
                Err(VerifyError::Mismatch),
                "seed {seed:#x} round {round}: signature bit {bit}"
            );
        }
        Ok(())
    })
}

/// Every bit of a 64-byte record, flipped in turn, breaks the seal.
#[test]
fn sim_adversary_sign_every_record_bit_is_bound() -> TestResult {
    sweep(4, |seed, round, attacker| {
        let (secret, public) = draw_key(attacker);
        let record = attacker.bytes(64);
        let seal = sign(&secret, TAG, &record);
        for bit in 0..512 {
            assert_eq!(
                verify_strict(&public, TAG, &flip_bit(&record, bit), &seal),
                Err(VerifyError::Mismatch),
                "seed {seed:#x} round {round}: record bit {bit}"
            );
        }
        Ok(())
    })
}

/// Every bit of the tag, flipped in turn, breaks the seal: a seal over one kind
/// of record never verifies as another.
#[test]
fn sim_adversary_sign_every_tag_bit_is_bound() -> TestResult {
    sweep(4, |seed, round, attacker| {
        let (secret, public) = draw_key(attacker);
        let record = draw_record(attacker, 64);
        let seal = sign(&secret, TAG, &record);
        for bit in 0..TAG.len().saturating_mul(8) {
            assert_eq!(
                verify_strict(&public, &flip_bit(TAG, bit), &record, &seal),
                Err(VerifyError::Mismatch),
                "seed {seed:#x} round {round}: tag bit {bit}"
            );
        }
        Ok(())
    })
}

/// A seal never verifies under another key, however the keys were drawn.
#[test]
fn sim_adversary_sign_a_seal_does_not_open_under_another_key() -> TestResult {
    sweep(1_024, |seed, round, attacker| {
        let (secret, _) = draw_key(attacker);
        let (_, stranger) = draw_key(attacker);
        let record = draw_record(attacker, 128);
        assert_eq!(
            verify_strict(&stranger, TAG, &record, &sign(&secret, TAG, &record)),
            Err(VerifyError::Mismatch),
            "seed {seed:#x} round {round}"
        );
        Ok(())
    })
}

/// A public key with any one bit flipped is refused as no point or fails to open
/// the seal: a damaged key never silently accepts.
#[test]
fn sim_adversary_sign_every_public_key_bit_is_checked() -> TestResult {
    sweep(4, |seed, round, attacker| {
        let (secret, public) = draw_key(attacker);
        let record = draw_record(attacker, 64);
        let seal = sign(&secret, TAG, &record);
        for bit in 0..256 {
            let damaged = flip_bit(&public.to_bytes(), bit);
            let mut raw = [0_u8; 32];
            raw.copy_from_slice(&damaged);
            if let Ok(key) = PublicKey::from_bytes(raw) {
                assert_eq!(
                    verify_strict(&key, TAG, &record, &seal),
                    Err(VerifyError::Mismatch),
                    "seed {seed:#x} round {round}: key bit {bit}"
                );
            }
        }
        Ok(())
    })
}

/// The scalar malleability: adding the group order to the signature's scalar,
/// one to seven times, gives a second encoding a lax verifier accepts. The
/// strict verifier refuses each, so there is exactly one valid seal per
/// key-tag-record (RFC 8032 section 5.1.7).
#[test]
fn sim_adversary_sign_the_scalar_plus_the_group_order_is_refused() -> TestResult {
    sweep(256, |seed, round, attacker| {
        let (secret, public) = draw_key(attacker);
        let record = draw_record(attacker, 96);
        let seal = sign(&secret, TAG, &record);
        assert_eq!(
            verify_strict(&public, TAG, &record, &seal),
            Ok(()),
            "seed {seed:#x} round {round}: the control seal must open"
        );
        let mut scalar = scalar_of(&seal);
        for multiple in 1..=7 {
            let Some(next) = add_le(&scalar, &GROUP_ORDER) else {
                break;
            };
            scalar = next;
            assert_eq!(
                verify_strict(&public, TAG, &record, &with_scalar(&seal, &scalar)),
                Err(VerifyError::Mismatch),
                "seed {seed:#x} round {round}: scalar plus {multiple} group orders"
            );
        }
        Ok(())
    })
}

/// A small-order public key, the identity or the point of order two, with
/// forged signatures of the shapes that verify any message in a lax verifier,
/// opens nothing: the key is refused or the seal is.
#[test]
fn sim_adversary_sign_small_order_keys_open_nothing() -> TestResult {
    let mut forged = vec![[0_u8; 64], [0xff_u8; 64]];
    for point in [IDENTITY, ORDER_TWO] {
        let mut raw = [0_u8; 64];
        raw[..32].copy_from_slice(&point);
        forged.push(raw);
    }
    sweep(64, |seed, round, attacker| {
        let record = draw_record(attacker, 96);
        for key_bytes in [IDENTITY, ORDER_TWO] {
            let Ok(key) = PublicKey::from_bytes(key_bytes) else {
                continue;
            };
            for raw in &forged {
                assert_eq!(
                    verify_strict(&key, TAG, &record, &Signature::from_bytes(*raw)),
                    Err(VerifyError::Mismatch),
                    "seed {seed:#x} round {round}: small-order key {key_bytes:02x?}"
                );
            }
        }
        Ok(())
    })
}

/// All-zero, all-one and random sixty-four byte strings are signatures that
/// parse and never verify under a real key.
#[test]
fn sim_adversary_sign_forged_signatures_never_open() -> TestResult {
    sweep(2_048, |seed, round, attacker| {
        let (_, public) = draw_key(attacker);
        let record = draw_record(attacker, 64);
        for raw in [[0_u8; 64], [0xff_u8; 64], attacker.array::<64>()] {
            let forged = Signature::from_bytes(raw);
            assert_eq!(
                forged.to_bytes(),
                raw,
                "seed {seed:#x} round {round}: bytes"
            );
            assert_eq!(
                verify_strict(&public, TAG, &record, &forged),
                Err(VerifyError::Mismatch),
                "seed {seed:#x} round {round}"
            );
        }
        Ok(())
    })
}

/// A seal is deterministic and sensitive: the same inputs give the same sixty-four
/// bytes, and changing the key, tag or record gives different ones.
#[test]
fn sim_adversary_sign_a_seal_is_deterministic_and_sensitive() -> TestResult {
    sweep(512, |seed, round, attacker| {
        let (secret, _) = draw_key(attacker);
        let (other, _) = draw_key(attacker);
        let record = draw_record(attacker, 64);
        let base = sign(&secret, TAG, &record);
        assert_eq!(
            base,
            sign(&secret, TAG, &record),
            "seed {seed:#x} round {round}"
        );
        assert_ne!(
            base,
            sign(&other, TAG, &record),
            "seed {seed:#x} round {round}: key"
        );
        assert_ne!(
            base,
            sign(&secret, b"other", &record),
            "seed {seed:#x} round {round}: tag"
        );
        let mut longer = record;
        longer.push(0);
        assert_ne!(
            base,
            sign(&secret, TAG, &longer),
            "seed {seed:#x} round {round}: record"
        );
        Ok(())
    })
}

/// A string split into tag and record at every point gives a different seal at
/// each, and each opens only at its own split: the boundary cannot be slid.
#[test]
fn sim_adversary_sign_a_boundary_cannot_be_slid() -> TestResult {
    sweep(32, |seed, round, attacker| {
        let (secret, public) = draw_key(attacker);
        let text = attacker.bytes(24);
        let seals: Vec<Signature> = (0..=text.len())
            .map(|cut| sign(&secret, &text[..cut], &text[cut..]))
            .collect();
        let distinct: BTreeSet<[u8; 64]> = seals.iter().map(|seal| seal.to_bytes()).collect();
        assert_eq!(
            distinct.len(),
            seals.len(),
            "seed {seed:#x} round {round}: two splits share a seal"
        );
        for (cut, seal) in seals.iter().enumerate() {
            for other in 0..=text.len() {
                let opens = verify_strict(&public, &text[..other], &text[other..], seal).is_ok();
                assert_eq!(
                    opens,
                    cut == other,
                    "seed {seed:#x} round {round}: seal for split {cut} read at split {other}"
                );
            }
        }
        Ok(())
    })
}

/// A tag extended with any prefix of the record's own digest, over the same or a
/// different record, never opens the original seal: the digest's fixed width is
/// what keeps `tag || digest` unambiguous.
#[test]
fn sim_adversary_sign_a_tag_extended_with_digest_bytes_does_not_open() -> TestResult {
    sweep(64, |seed, round, attacker| {
        let (secret, public) = draw_key(attacker);
        let record = draw_record(attacker, 96);
        let seal = sign(&secret, TAG, &record);
        let digest = lgwks_std::hash::blake3(&record);
        for take in 0..=32_usize {
            let mut extended = TAG.to_vec();
            extended.extend_from_slice(&digest.as_bytes()[..take]);
            for candidate in [record.clone(), Vec::new(), draw_record(attacker, 32)] {
                let opens = verify_strict(&public, &extended, &candidate, &seal).is_ok();
                assert!(
                    !opens || (take == 0 && candidate == record),
                    "seed {seed:#x} round {round}: tag plus {take} digest bytes opened a seal"
                );
            }
        }
        Ok(())
    })
}

/// An empty tag, an empty record and a megabyte of each all seal and open, and a
/// bit flipped in the megabyte record breaks the seal.
#[test]
fn sim_adversary_sign_empty_and_huge_inputs_seal_and_open() -> TestResult {
    let (secret, public) = draw_key(&mut Attacker::new(SWEEP_SEEDS[0]));
    let big = vec![0x5a_u8; 1_048_576];
    for (tag, record) in [
        (&[][..], &[][..]),
        (TAG, &[][..]),
        (&[][..], &big[..]),
        (&big[..], TAG),
        (&big[..], &big[..]),
    ] {
        let seal = sign(&secret, tag, record);
        assert_eq!(
            verify_strict(&public, tag, record, &seal),
            Ok(()),
            "tag of {} bytes, record of {}",
            tag.len(),
            record.len()
        );
    }
    let seal = sign(&secret, TAG, &big);
    assert_eq!(
        verify_strict(&public, TAG, &flip_bit(&big, 8 * 524_288), &seal),
        Err(VerifyError::Mismatch),
        "one bit in the middle of a megabyte"
    );
    Ok(())
}

/// The secret key never prints its seed: debug is the same redacted text for
/// every key, and no key's seed appears in it (CWE-532).
#[test]
fn sim_adversary_sign_the_secret_key_prints_nothing() -> TestResult {
    sweep(256, |seed, round, attacker| {
        let bytes = attacker.array::<32>();
        let printed = format!("{:?}", SecretKey::from_seed(bytes));
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(printed, "SecretKey(..)", "seed {seed:#x} round {round}");
        assert!(
            !printed.contains(&hex),
            "seed {seed:#x} round {round}: the seed leaked"
        );
        Ok(())
    })
}

/// The public key prints as its sixty-four lowercase hex digits and round-trips
/// through its bytes; distinct seeds give distinct public keys.
#[test]
fn sim_adversary_sign_public_keys_print_and_round_trip() -> TestResult {
    sweep(1, |seed, _, attacker| {
        let mut seen = BTreeSet::new();
        for _ in 0..1_024 {
            let (_, public) = draw_key(attacker);
            let bytes = public.to_bytes();
            let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            assert_eq!(
                format!("{public:?}"),
                format!("PublicKey({hex})"),
                "seed {seed:#x}"
            );
            assert_eq!(
                PublicKey::from_bytes(bytes)?,
                public,
                "seed {seed:#x}: round trip"
            );
            seen.insert(bytes);
        }
        assert_eq!(seen.len(), 1_024, "seed {seed:#x}: two seeds shared a key");
        Ok(())
    })
}

/// A seed is the key: the same seed gives the same public key and the same
/// seals, in any order of construction.
#[test]
fn sim_adversary_sign_a_seed_is_the_key() -> TestResult {
    sweep(512, |seed, round, attacker| {
        let bytes = attacker.array::<32>();
        let (first, second) = (SecretKey::from_seed(bytes), SecretKey::from_seed(bytes));
        assert_eq!(
            first.public_key(),
            second.public_key(),
            "seed {seed:#x} round {round}"
        );
        let record = draw_record(attacker, 64);
        assert_eq!(
            sign(&first, TAG, &record),
            sign(&second, TAG, &record),
            "seed {seed:#x} round {round}"
        );
        Ok(())
    })
}

/// Any thirty-two bytes either parse as a public key that returns the same bytes
/// or are refused as no point, and the refusal is the one typed error.
#[test]
fn sim_adversary_sign_a_public_key_parses_or_is_refused_typed() -> TestResult {
    let (mut accepted, mut refused) = (0_usize, 0_usize);
    sweep(2_048, |seed, round, attacker| {
        let bytes = attacker.array::<32>();
        match PublicKey::from_bytes(bytes) {
            Ok(key) => {
                accepted = accepted.saturating_add(1);
                assert_eq!(key.to_bytes(), bytes, "seed {seed:#x} round {round}");
            }
            Err(error) => {
                refused = refused.saturating_add(1);
                assert!(
                    error.to_string().contains("not a valid ed25519 point"),
                    "seed {seed:#x} round {round}: {error}"
                );
            }
        }
        Ok(())
    })?;
    assert!(
        accepted > 1_000 && refused > 1_000,
        "{accepted} accepted and {refused} refused: both outcomes must occur"
    );
    Ok(())
}

/// A refusal's message is one printable line that names no secret.
#[test]
fn sim_adversary_sign_refusal_messages_are_one_safe_line() {
    let verify = VerifyError::Mismatch.to_string();
    assert!(
        !verify.contains(['\n', '\r', '\u{1b}', '\0']),
        "{verify:?} can forge a log line"
    );
    let key = PublicKey::from_bytes([0xff; 32]).map(|_| ()).err();
    if let Some(error) = key {
        let message = error.to_string();
        assert!(
            !message.contains(['\n', '\r', '\u{1b}', '\0']),
            "{message:?}"
        );
    }
}

/// The same seed mounts the same attack on the signer.
#[test]
fn sim_adversary_sign_the_same_seed_replays_the_same_campaign() {
    replays_under_every_seed(campaign);
}

/// Two seeds mount two attacks.
#[test]
fn sim_adversary_sign_distinct_seeds_mount_distinct_campaigns() {
    diverges_between_seeds(campaign);
}
