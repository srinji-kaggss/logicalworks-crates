//! `wire` owns binary serialization and enforces INV-WIRE-DETERMINISTIC:
//! the same value always produces the same byte sequence on the same
//! architecture (little-endian, pointer-width-64). Built on rkyv for
//! zero-copy deserialization: archived data is accessed directly from the
//! byte buffer without allocation.
//!
//! This is the internal binary wire format. For external JSON APIs, use the
//! `json` module. Callers use the rkyv derive macros (`Archive`,
//! `Serialize`, `Deserialize`) on their types, then call the functions
//! here with the error type already pinned.

// ── Re-exports ──────────────────────────────────────────────────────────────

pub use rkyv::rancor::Error as WireError;
pub use rkyv::util::AlignedVec;
pub use rkyv::{Archive, Deserialize, Serialize};
pub use rkyv::{access, from_bytes, to_bytes};

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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Archive, Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
    #[rkyv(compare(PartialEq), derive(Debug))]
    struct Rect {
        x: u32,
        y: u32,
        w: u32,
        height: u32,
    }

    #[derive(Archive, Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
    #[rkyv(compare(PartialEq), derive(Debug))]
    struct Nested {
        name: String,
        bounds: Rect,
    }

    // These tests return `Result` rather than unwrapping: a rkyv refusal
    // reports its own `Debug` on failure, which is the same report `.unwrap`
    // would have panicked with, without an `unwrap` in the tree.
    #[test]
    fn roundtrips_through_bytes() -> Result<(), WireError> {
        let value = Nested {
            name: "test".into(),
            bounds: Rect {
                x: 1,
                y: 2,
                w: 100,
                height: 200,
            },
        };
        let bytes = to_bytes::<WireError>(&value)?;
        let archived = access::<ArchivedNested, WireError>(&bytes)?;
        assert_eq!(&value, archived);
        let restored = from_bytes::<Nested, WireError>(&bytes)?;
        assert_eq!(value, restored);
        Ok(())
    }

    #[test]
    fn encoding_is_deterministic() -> Result<(), WireError> {
        let value = Rect {
            x: 5,
            y: 10,
            w: 50,
            height: 100,
        };
        let first = to_bytes::<WireError>(&value)?;
        let second = to_bytes::<WireError>(&value)?;
        assert_eq!(first.as_slice(), second.as_slice());
        Ok(())
    }

    #[test]
    fn different_values_produce_different_bytes() -> Result<(), WireError> {
        let first = to_bytes::<WireError>(&Rect {
            x: 1,
            y: 1,
            w: 1,
            height: 1,
        })?;
        let second = to_bytes::<WireError>(&Rect {
            x: 2,
            y: 1,
            w: 1,
            height: 1,
        })?;
        assert_ne!(first.as_slice(), second.as_slice());
        Ok(())
    }
}
