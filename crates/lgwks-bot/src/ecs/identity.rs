//! Domain-separated identities for actions, digests and admitted inputs.

use std::num::NonZeroU128;

use lgwks_std::hash::{Digest, Hasher};

use crate::journal::frame::SaturatingFrom;

use crate::effect::{ActionDigest, ActionId, EffectKey, FlowRevision, Id128};
use crate::journal::{EventKind, JournalError};

/// The domain separator for an [`ActionId`] derived from a bot's structure.
///
/// A separator rather than a bare concatenation, because the fields hashed into
/// an identity are variable-length strings and a run of them with no framing is
/// a value two different bots can collide on.
pub(super) const ACTION_ID_DOMAIN: &[u8] = b"lgwks.bot.action-id.v2";

/// The domain separator for an [`ActionDigest`] derived from an admitted input.
pub(super) const ACTION_DIGEST_DOMAIN: &[u8] = b"lgwks.bot.action-digest.v2";

/// The domain separator for an admitted input's content identity.
///
/// `v2` replaced the `v1` stream, which hashed the compiler-provided
/// `type_name` of the output type. `type_name` is diagnostic and unstable
/// across toolchains, so `v2` binds the [`InputIdentity::SCHEMA_ID`] instead
/// and the two streams are different identities by construction (issue #118).
pub(super) const OUTPUT_IDENTITY_DOMAIN: &[u8] = b"lgwks.bot.input-identity.v2";

/// The refusal for a journal that already holds a *different* outcome for `key`
/// than the one being settled.
///
/// One constructor for both places that compare a recorded outcome with the
/// evidence in hand, so they cannot name different orderings for one conflict.
pub(super) fn changed_outcome(key: EffectKey) -> JournalError {
    JournalError::OutOfOrder {
        key: Box::new(key),
        expected: Some(EventKind::Verified),
        attempted: EventKind::OutcomeObserved,
    }
}

/// Hash a sequence of byte fields into the estate's content-identity digest.
///
/// Each field is length-prefixed with its `u64` little-endian width before the
/// bytes themselves, so a variable-length name cannot slide into the next field
/// and produce the same digest for a different parse. A domain separator
/// identifies the format; it does not frame the fields inside it. Indices are
/// always `u64::to_le_bytes` rather than `usize`, so a 32-bit and a 64-bit
/// target derive the same identity for the same bot (issue #101).
pub(super) fn hash_parts(parts: &[&[u8]]) -> Digest {
    let mut hasher = Hasher::new();
    for part in parts {
        // Length-framed: a stream over several variable-length parts must
        // depend on the parts, not only on their concatenation (issue #118).
        hasher.write_framed(part);
    }
    hasher.finalize()
}

/// A `usize` index as a portable `u64`.
///
/// `usize::to_le_bytes` is 4 bytes on a 32-bit target and 8 on a 64-bit one,
/// so the same bot derived different identities on each. Every index that
/// enters a digest goes through here.
pub(super) fn portable_index(index: usize) -> u64 {
    u64::saturating_from(index)
}

/// The fallback identity for a binding that cannot name itself.
pub(super) fn derive_input_stamp(stamp: u64) -> [u8; 16] {
    let digest = hash_parts(&[b"lgwks.bot.input-stamp.v1", &stamp.to_le_bytes()]);
    let mut wide = [0_u8; 16];
    wide.copy_from_slice(&digest.as_bytes()[..16]);
    wide
}

/// Fold a digest into a non-zero 128-bit identifier.
///
/// The all-zero identifier is not a valid one (see [`IdError`]), and a hash has
/// no such exclusion, so a derivation that landed on zero would produce an
/// identity the parser would refuse to read back. The zero case is mapped to
/// [`NonZeroU128::MIN`] rather than refused, because a derivation that can fail
/// is a derivation every caller has to handle, and the collision it introduces
/// is one in 2^128.
///
/// [`IdError`]: crate::effect::IdError
pub(super) fn id_from_digest(digest: &Digest) -> Id128 {
    let bytes = digest.as_bytes();
    let mut wide = [0_u8; 16];
    for (slot, byte) in wide.iter_mut().zip(bytes.iter()) {
        *slot = *byte;
    }
    match NonZeroU128::new(u128::from_be_bytes(wide)) {
        Some(identity) => Id128::from_nonzero(identity),
        None => Id128::from_nonzero(NonZeroU128::MIN),
    }
}

/// The logical intent of one entry of one chain.
///
/// Derived rather than declared, because the substrate is the only thing that
/// knows a chain's position, and position is what a settlement must survive an
/// edit elsewhere in the document to. The derivation reads the bot's name, the
/// entry's position, and the action's own domain identifier, so two entries
/// with the same domain in the same chain are still two intents.
///
/// `ActionId` is not a content digest, and this is not one either: [`ActionDigest`]
/// is what binds the input, and this binds *which action* it is. Deriving it
/// from the action's own `domain_id` is the closest the erased `ExecuteAny`
/// comes to naming what it does.
pub(super) fn derive_action_id(bot: &str, chain: usize, entry: usize, domain: &str) -> ActionId {
    ActionId::new(id_from_digest(&hash_parts(&[
        ACTION_ID_DOMAIN,
        bot.as_bytes(),
        &portable_index(chain).to_le_bytes(),
        &portable_index(entry).to_le_bytes(),
        domain.as_bytes(),
    ])))
}

/// The exact admitted input one entry was dispatched from.
///
/// The ECS substrate has no payload bytes to hash: a transition's binding is an
/// erased `Box<dyn Any>` and nothing here asks it to be `Hash`. Content
/// equality is not event identity either — two distinct commands with the same
/// payload must stay distinguishable — so the digest binds the *admitted input
/// identity*: a monotonic stamp minted when the observation was admitted and
/// continued across a restart from the journal's tail, not the process-local
/// `Revision` counter, which reopens at 1 after every reconstruction (issue
/// #101).
///
/// Two attempts within one admitted input share a digest, which is what makes
/// a retry a retry. A new admission gets a new stamp even when the value is
/// equal or the revision counter has wrapped back to 1, which is what stops a
/// restarted bot from reading new work as already applied.
pub(super) fn derive_action_digest(
    flow: FlowRevision,
    chain: usize,
    entry: usize,
    input: &[u8; 16],
) -> ActionDigest {
    ActionDigest::new(hash_parts(&[
        ACTION_DIGEST_DOMAIN,
        flow.digest().as_bytes(),
        &portable_index(chain).to_le_bytes(),
        &portable_index(entry).to_le_bytes(),
        input,
    ]))
}
