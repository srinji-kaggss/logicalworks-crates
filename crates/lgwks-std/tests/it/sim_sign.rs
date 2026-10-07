//! Seeded deterministic replay of the `sign` sealed-record contracts.
//!
//! One seed draws every key, tag and record, a failure prints the seed that
//! produced it, and the same seed must produce the same trace hash. Every
//! probe drives the public API of the shipped crate, except the one family
//! that pins the message construction against the backend directly: the seal
//! a consumer migrates must be byte-identical to the seal it used to make
//! with the same crate, and that formula is checked here rather than assumed.
//!
//! The property here is strict sealed binding: one key, tag and record give
//! one seal, and changing any of the three — or one bit of the seal —
//! refuses. The malleability family states the sharp edge: the scalar twin a
//! lax verifier accepts is refused here, and the test proves the twin is
//! non-canonical rather than merely different.
//!
//! The module is gated `feature = "sign"` because that is what gates
//! `lgwks_std::sign` (INV-DEP-6). No family reads OS entropy: keys come from
//! seeded draws through [`SecretKey::from_seed`], so every case replays
//! exactly. Generation from entropy is covered by the live unit test beside
//! the module.

#![cfg(feature = "sign")]

use crate::seeded_bytes;
use crate::seeded_sweep;

use lgwks_std::seeded::Seeded;
use lgwks_std::sign::{PublicKey, SecretKey, Signature, sign, verify_strict};
use seeded_bytes::{below, fold_bytes, next_array, next_bytes};
use seeded_sweep::{
    SWEEP_SEEDS, assert_distinct_seeds_diverge, assert_same_seed_replays, fold, fold_usize,
    initial_trace, seeded_stream,
};

/// The domain tag the consumer seals run records under, written here from its
/// documented construction rather than taken from the crate: the compat
/// family compares against an independent spelling of the same bytes.
const CONSUMER_DOMAIN: &[u8] = b"logical-ci/run-record/1\0";

/// The record lengths every boundary family states its properties at.
const BOUNDARY_LENGTHS: [usize; 8] = [0, 1, 2, 31, 32, 33, 64, 1024];

/// Draw one sealing case: the key seed, the domain tag and the record bytes.
fn draw_case(state: &mut Seeded) -> ([u8; 32], Vec<u8>, Vec<u8>) {
    let seed = next_array::<32>(state);
    let tag_len = below(state, 40);
    let tag = next_bytes(state, tag_len);
    let record_len = below(state, 160);
    let record = next_bytes(state, record_len);
    (seed, tag, record)
}

/// Flip one bit of `bytes` at a seeded byte and bit.
fn flip_one(state: &mut Seeded, bytes: &mut [u8]) {
    if bytes.is_empty() {
        return;
    }
    let position = below(state, bytes.len());
    let bit = below(state, 8);
    if let Some(slot) = bytes.get_mut(position) {
        *slot ^= 1_u8 << bit;
    }
}

/// Runs the seeded seal-and-verify sweep and returns its trace.
///
/// The tamper arms folded are 1 for a wrong key, 2 for a wrong tag, 3 for a
/// wrong record and 4 for a damaged seal byte, so the trace tells the four
/// refusals apart.
fn sign_trace(seed: u64) -> u64 {
    let mut state = seeded_stream(seed);
    let mut trace = initial_trace();

    for _ in 0..32 {
        let (key_seed, tag, record) = draw_case(&mut state);
        let secret = SecretKey::from_seed(key_seed);
        let public = secret.public_key();
        let seal = sign(&secret, &tag, &record);

        // Determinism: sealing is a pure function of key, tag and record.
        assert_eq!(
            sign(&secret, &tag, &record),
            seal,
            "seed {seed}: resealing the same case must give the same seal"
        );
        // Binding: the seal opens under the case it was sealed for.
        assert!(
            verify_strict(&public, &tag, &record, &seal).is_ok(),
            "seed {seed}: a fresh seal must verify under its own case"
        );

        // Tamper every axis: a wrong key, tag or record refuses.
        let other = SecretKey::from_seed(next_array::<32>(&mut state));
        let refusals = [
            verify_strict(&other.public_key(), &tag, &record, &seal).is_err(),
            verify_strict(&public, b"wrong-domain", &record, &seal).is_err(),
            verify_strict(&public, &tag, b"wrong record", &seal).is_err(),
        ];
        for (position, refused) in refusals.iter().enumerate() {
            assert!(
                *refused,
                "seed {seed}: tamper arm {} verified when it must refuse",
                position.saturating_add(1)
            );
            fold_usize(&mut trace, position.saturating_add(1));
        }
        let mut damaged = seal.to_bytes();
        flip_one(&mut state, &mut damaged);
        assert!(
            verify_strict(&public, &tag, &record, &Signature::from_bytes(damaged)).is_err(),
            "seed {seed}: a seal with a flipped bit verified when it must refuse"
        );
        fold(&mut trace, 4);

        fold_bytes(&mut trace, &seal.to_bytes());
        fold_bytes(&mut trace, &public.to_bytes());
        fold_usize(&mut trace, record.len());
    }
    trace
}

#[test]
/// The replay oracle: one seed, one trace.
fn the_same_seed_replays_to_the_same_sign_trace() {
    for seed in SWEEP_SEEDS {
        assert_same_seed_replays(sign_trace, seed);
    }
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn distinct_sign_seeds_diverge_in_their_trace() {
    assert_distinct_seeds_diverge(sign_trace, SWEEP_SEEDS[0], SWEEP_SEEDS[1]);
}

#[test]
/// A seal binds at every record length, under an empty tag and an empty
/// record alike: neither half of the domain separation may be absent by
/// accident, and both may be empty on purpose.
fn sealing_binds_at_every_boundary_length() {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        let secret = SecretKey::from_seed(next_array::<32>(&mut state));
        let public = secret.public_key();
        for length in BOUNDARY_LENGTHS {
            let record = next_bytes(&mut state, length);
            for tag in [Vec::new(), b"t".to_vec(), CONSUMER_DOMAIN.to_vec()] {
                let seal = sign(&secret, &tag, &record);
                assert!(
                    verify_strict(&public, &tag, &record, &seal).is_ok(),
                    "seed {seed}: a {length}-byte record under a {}-byte tag must verify",
                    tag.len()
                );
                // The tag is load-bearing: the same record under another tag
                // is another seal, and this seal does not open there.
                let sibling = if tag == b"t".to_vec() {
                    b"u".to_vec()
                } else {
                    b"t".to_vec()
                };
                assert!(
                    verify_strict(&public, &sibling, &record, &seal).is_err(),
                    "seed {seed}: a seal must not verify under a sibling tag"
                );
            }
        }
    }
}

#[test]
/// Two tenants never cross: a seal opens only under the key that made it, so
/// a verifier holding one tenant's public key cannot be shown another
/// tenant's record as its own.
fn two_tenants_never_cross_a_seal() {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        let (first_seed, tag, record) = draw_case(&mut state);
        let (second_seed, _, _) = draw_case(&mut state);
        let first = SecretKey::from_seed(first_seed);
        let second = SecretKey::from_seed(second_seed);
        let seal = sign(&first, &tag, &record);
        assert!(
            verify_strict(&first.public_key(), &tag, &record, &seal).is_ok(),
            "seed {seed}: the issuing tenant must verify its own seal"
        );
        assert!(
            verify_strict(&second.public_key(), &tag, &record, &seal).is_err(),
            "seed {seed}: another tenant's key must not open the seal"
        );
    }
}

#[test]
/// The sealed construction is exactly `tag || BLAKE3(record)`, checked
/// against the backend primitive rather than against the module under test:
/// a record the consumer sealed with the same tag opens here, and a seal
/// made here opens there.
fn the_sealed_message_is_the_tag_then_the_record_digest() -> Result<(), Box<dyn std::error::Error>>
{
    use ed25519_dalek::Signer as _;

    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        let (key_seed, _, record) = draw_case(&mut state);
        let backend_key = ed25519_dalek::SigningKey::from_bytes(&key_seed);
        let backend_public = backend_key.verifying_key();

        // The consumer formula, spelled independently of the module.
        let digest = lgwks_std::hash::blake3(&record);
        let mut expected = Vec::with_capacity(CONSUMER_DOMAIN.len().saturating_add(32));
        expected.extend_from_slice(CONSUMER_DOMAIN);
        expected.extend_from_slice(digest.as_bytes());

        // The module's seal is the backend's signature over that message.
        let secret = SecretKey::from_seed(key_seed);
        let seal = sign(&secret, CONSUMER_DOMAIN, &record);
        assert_eq!(
            seal.to_bytes(),
            backend_key.sign(&expected).to_bytes(),
            "seed {seed}: the module must sign exactly the consumer formula"
        );
        // And the backend accepts the module's seal over that message, so a
        // verifier that never imports this module still opens it.
        let backend_accepts = backend_public
            .verify_strict(
                &expected,
                &ed25519_dalek::Signature::from_bytes(&seal.to_bytes()),
            )
            .is_ok();
        assert!(
            backend_accepts,
            "seed {seed}: the backend must accept the module's seal over the formula"
        );
        // The module opens the backend's seal in return.
        let public = PublicKey::from_bytes(backend_public.to_bytes()).map_err(|error| {
            format!("seed {seed}: the backend's public key must parse: {error}")
        })?;
        let backend_seal = Signature::from_bytes(backend_key.sign(&expected).to_bytes());
        assert!(
            verify_strict(&public, CONSUMER_DOMAIN, &record, &backend_seal).is_ok(),
            "seed {seed}: the module must open the backend's seal"
        );
    }
    Ok(())
}

/// The curve order as 32 little-endian bytes: adding it to a scalar is the
/// malleability twin, the second signature a lax verifier accepts.
const CURVE_ORDER_LE: [u8; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x26, 0x12, 0x58, 0xd6, 0x9c, 0xf2, 0xa4, 0xde, 0xf9, 0xde, 0x14,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
];

/// 256-bit addition of two little-endian values, for the malleability twin.
///
/// Exact rather than saturating: the caller adds a scalar below the order to
/// the order itself, so the sum stays below twice the order and no limb
/// carries out of the width. Eight-bit limbs keep every step infallible
/// without a narrowing cast.
fn add_256(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut carry = 0u16;
    for ((slot, first), second) in out.iter_mut().zip(left.iter()).zip(right.iter()) {
        let total = u16::from(*first)
            .saturating_add(u16::from(*second))
            .saturating_add(carry);
        *slot = total.to_le_bytes()[0];
        carry = total >> 8;
    }
    out
}

/// Whether `value` reads at or above the curve order, compared limb by limb
/// from the top.
fn at_or_above_order(value: &[u8; 32]) -> bool {
    for (ours, theirs) in value.iter().zip(CURVE_ORDER_LE.iter()).rev() {
        if ours != theirs {
            return ours > theirs;
        }
    }
    true
}

#[test]
/// Strictness refuses the malleability twin: adding the curve order to a
/// seal's scalar keeps the verification equation true for a lax verifier,
/// but the twin scalar is non-canonical and is refused here — and the test
/// proves the twin is non-canonical rather than merely different.
fn a_malleated_scalar_is_refused_as_non_canonical() {
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        let (key_seed, tag, record) = draw_case(&mut state);
        let secret = SecretKey::from_seed(key_seed);
        let public = secret.public_key();
        let seal = sign(&secret, &tag, &record);
        let bytes = seal.to_bytes();
        let (body, scalar) = bytes.split_at(32);
        let mut scalar_array = [0u8; 32];
        scalar_array.copy_from_slice(scalar);
        let twin_scalar = add_256(&scalar_array, &CURVE_ORDER_LE);
        let mut twin = [0u8; 64];
        twin[..32].copy_from_slice(body);
        twin[32..].copy_from_slice(&twin_scalar);
        assert!(
            at_or_above_order(&twin_scalar),
            "seed {seed}: the twin scalar must be at or above the order, or the case proves nothing"
        );
        assert!(
            verify_strict(&public, &tag, &record, &Signature::from_bytes(twin)).is_err(),
            "seed {seed}: the non-canonical twin must be refused by strict verification"
        );
    }
}

#[test]
/// The byte forms round-trip, and parsing agrees with the backend on every
/// input: this module adds no second opinion about which encodings are
/// points, so a stored key the backend accepts parses here and one it
/// refuses is refused as [`KeyError::InvalidPublicKey`].
fn public_key_and_signature_byte_forms_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let mut refusals = 0_usize;
    for seed in SWEEP_SEEDS {
        let mut state = seeded_stream(seed);
        for _ in 0..32 {
            let candidate = next_array::<32>(&mut state);
            let backend = ed25519_dalek::VerifyingKey::from_bytes(&candidate);
            match PublicKey::from_bytes(candidate) {
                Ok(parsed) => match backend {
                    Ok(expected) => {
                        assert_eq!(
                            parsed.to_bytes(),
                            expected.to_bytes(),
                            "seed {seed}: an accepted key must be the backend's key"
                        );
                    }
                    Err(_) => {
                        return Err(format!(
                            "seed {seed}: the module accepted a key the backend refused"
                        )
                        .into());
                    }
                },
                Err(error) => {
                    assert!(
                        backend.is_err(),
                        "seed {seed}: the module refused with {error} a key the backend accepted"
                    );
                    refusals = refusals.saturating_add(1);
                }
            }
        }
        let (key_seed, _, _) = draw_case(&mut state);
        let secret = SecretKey::from_seed(key_seed);
        let public = secret.public_key();
        let parsed = PublicKey::from_bytes(public.to_bytes())
            .map_err(|error| format!("seed {seed}: a generated public key must parse: {error}"))?;
        assert_eq!(
            parsed, public,
            "seed {seed}: the public key must parse back to itself"
        );
        let seal = sign(&secret, b"forms/v1", b"round trip");
        assert_eq!(
            Signature::from_bytes(seal.to_bytes()),
            seal,
            "seed {seed}: the seal must parse back to itself"
        );
    }
    assert!(
        refusals > 0,
        "no drawn input was refused: the refusal arm ran unexercised"
    );
    Ok(())
}

#[test]
/// The pinned seal: one fixed key, tag and record give one fixed 64-byte
/// seal, so a future edit that moves the construction fails here rather than
/// in a consumer's migration, and the consumer can replay this vector with
/// its own copy of the backend.
fn the_pinned_seal_vector_is_stable() {
    let secret = SecretKey::from_seed([0x42u8; 32]);
    let seal = sign(&secret, CONSUMER_DOMAIN, b"{\"tenant\":\"acme\",\"run\":7}");
    assert_eq!(
        lgwks_std::hex::encode(seal.to_bytes()),
        "04134f77622165f74e3970f3a79b6740a4610216fa6511f0a26e4fbce88f806cc5869ca2a9b370d85b903313dee3078314e118a96c047bbd9552d4e91485c704",
        "the pinned seal moved: the sealed construction changed"
    );
    let public = secret.public_key();
    assert!(
        verify_strict(
            &public,
            CONSUMER_DOMAIN,
            b"{\"tenant\":\"acme\",\"run\":7}",
            &seal
        )
        .is_ok(),
        "the pinned seal must verify under its own case"
    );
}
