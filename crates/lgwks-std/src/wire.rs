//! `wire` is a thin archive facade over rkyv 0.8. It does not define a
//! versioned envelope or promise canonical bytes for semantic values.
//!
//! The archive format is selected by rkyv's Cargo feature-unified format
//! controls, not by the host architecture. With the current rkyv defaults,
//! archives are little-endian, aligned, and encode `usize`/`isize` as 32-bit
//! values. A downstream feature unifier can select a different endianness,
//! alignment, or pointer width. Use [`format_descriptor`](crate::wire::format_descriptor) to observe the
//! effective primitive format in this build and bind it, together with an
//! application schema/version, in any durable or exchanged envelope.
//!
//! rkyv 0.8.18 is the version pinned by this workspace's `Cargo.lock`; the
//! published dependency accepts the compatible `0.8` series. Compatibility
//! still requires the same archived schema and effective format, and a
//! semver-compatible rkyv release. No format migration or old-reader
//! guarantee is provided by this facade.
//!
//! Checked access validates the archive structure supported by the selected
//! rkyv configuration. It does not validate application invariants, schema
//! identity, authorization, freshness, or resource limits. `access` borrows
//! from the aligned archive bytes; `from_bytes` reconstructs an owned value.
//! The re-export also includes rkyv's unchecked APIs, whose callers must meet
//! their unsafe preconditions. These boundaries are part of the consumer
//! contract.
//!
//! This general facade is chosen over a fixed internal profile because
//! enabling a format-control feature here would unify it into downstream
//! builds and could change existing archive layouts. `bincode` offers
//! explicit encoder configuration, but introducing a second codec would not
//! repair this facade's contract and is outside this module's dependency
//! surface. The rkyv format controls and compatibility rules are documented
//! in the [rkyv 0.8 format reference](https://docs.rs/rkyv/0.8.18/rkyv/).
//!
//! This is an archive format, not a semantic canonicalization format. The
//! same value serialized by the same schema and format can be tested for
//! repeatable bytes, but arbitrary `Serialize` implementations, unordered
//! collections, floating-point representations, and shared-pointer topology
//! do not acquire a universal byte-identity guarantee. Do not use these bytes
//! as semantic content identity for hashing or signing without a separate,
//! explicitly constrained canonical profile.
//!
//! Callers derive `Archive`, `Serialize`, and `Deserialize` and choose the
//! rkyv error type at each call, as in `to_bytes::<WireError>(&value)`.

use core::mem::{align_of, size_of};

// ── Re-exports ──────────────────────────────────────────────────────────────

pub use rkyv::rancor::Error as WireError;
pub use rkyv::util::AlignedVec;
pub use rkyv::{Archive, Deserialize, Serialize};
pub use rkyv::{access, from_bytes, to_bytes};

/// Endianness observed in the effective archived primitive format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Endianness {
    /// Multi-byte archived primitives use little-endian bytes.
    Little,
    /// Multi-byte archived primitives use big-endian bytes.
    Big,
    /// The runtime probe did not match either supported byte order.
    Unknown,
}

/// Effective rkyv primitive-format properties for this compiled dependency.
///
/// This is not a schema identifier or a complete archive version. Persist or
/// exchange it alongside an application-owned schema/version identifier.
///
/// The two measured properties are reported in `usize`, the domain
/// `size_of` and `align_of` answer in. A width or an alignment narrowed into a
/// `u8` would need a value to report when it did not fit, and that value would
/// be indistinguishable from a real measurement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct FormatDescriptor {
    /// Effective byte order observed by serializing a `u32` sentinel.
    pub endianness: Endianness,
    /// Width used for archived `usize` and `isize`, in bits.
    pub pointer_width_bits: usize,
    /// Alignment of rkyv's archived `u32` primitive, in bytes.
    pub archived_u32_alignment: usize,
    /// rkyv codec compatibility series used by this facade.
    pub codec_series: &'static str,
}

/// Observe format controls selected for the linked rkyv instance.
///
/// rkyv feature unification means the manifest of this crate alone cannot
/// identify the effective format. This probe reports byte order, archived
/// pointer width, and primitive alignment. It deliberately does not invent a
/// schema id or promise that archives of different application types are
/// compatible.
///
/// ```rust
/// use lgwks_std::wire::{Endianness, WireError, format_descriptor};
///
/// let format = format_descriptor()?;
/// assert!(matches!(format.endianness, Endianness::Little | Endianness::Big));
/// # Ok::<(), WireError>(())
/// ```
pub fn format_descriptor() -> Result<FormatDescriptor, WireError> {
    let sentinel = to_bytes::<WireError>(&0x0102_0304_u32)?;
    let endianness = if sentinel
        .windows(4)
        .any(|bytes| bytes == [0x04, 0x03, 0x02, 0x01])
    {
        Endianness::Little
    } else if sentinel
        .windows(4)
        .any(|bytes| bytes == [0x01, 0x02, 0x03, 0x04])
    {
        Endianness::Big
    } else {
        Endianness::Unknown
    };

    Ok(FormatDescriptor {
        endianness,
        // The archived pointer is two, four or eight bytes, so the bit width is
        // that size times eight; saturating states the bound rather than
        // wrapping, and no value the probe can observe comes near it.
        pointer_width_bits: size_of::<rkyv::primitive::ArchivedUsize>().saturating_mul(8),
        archived_u32_alignment: align_of::<rkyv::primitive::ArchivedU32>(),
        codec_series: "rkyv 0.8",
    })
}

/// The archive implementation itself, re-exported so a *consumer* crate can
/// derive against it.
///
/// The derives above re-export the macros but not the crate they expand
/// against: the generated code names `::rkyv::…` absolutely, and `rkyv` is
/// `lgwks_std`'s dependency, not the consumer's — so a type in another crate
/// that derives `Archive` through this module fails with "cannot find `rkyv`
/// in the crate root" before it ever reaches a layout question.
///
/// This is the repair. A consumer writes
/// `#[rkyv(crate = lgwks_std::wire::rkyv)]` on its type, which points every
/// generated path at this re-export, and derives the macros from here as
/// normal. It is a re-export of the crate rather than one more hand-picked
/// list of items, because the list is the derive's business and it grows with
/// the derive: naming the items individually is how this module came to be
/// usable only by its own tests.
pub use rkyv;
