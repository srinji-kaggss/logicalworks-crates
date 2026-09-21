//! `wire` owns binary serialization and enforces INV-WIRE-DETERMINISTIC:
//! the same value always produces the same byte sequence on the same
//! architecture (little-endian, pointer-width-64). Built on rkyv for
//! zero-copy deserialization — archived data is accessed directly from the
//! byte buffer without allocation.
//!
//! This is the internal binary wire format. For external JSON APIs, use
//! [`crate::json`]. Callers use the rkyv derive macros (`Archive`,
//! `Serialize`, `Deserialize`) on their types, then call the functions
//! here with the error type already pinned.

// ── Re-exports ──────────────────────────────────────────────────────────────

pub use rkyv::rancor::Error as WireError;
pub use rkyv::util::AlignedVec;
pub use rkyv::{Archive, Deserialize, Serialize};
pub use rkyv::{access, from_bytes, to_bytes};

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
