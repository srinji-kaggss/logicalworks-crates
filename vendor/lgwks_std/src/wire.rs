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
        h: u32,
    }

    #[derive(Archive, Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
    #[rkyv(compare(PartialEq), derive(Debug))]
    struct Nested {
        name: String,
        bounds: Rect,
    }

    #[test]
    fn roundtrips_through_bytes() {
        let v = Nested {
            name: "test".into(),
            bounds: Rect {
                x: 1,
                y: 2,
                w: 100,
                h: 200,
            },
        };
        let bytes = to_bytes::<WireError>(&v).unwrap();
        let archived = access::<ArchivedNested, WireError>(&bytes).unwrap();
        assert_eq!(&v, archived);
        let restored = from_bytes::<Nested, WireError>(&bytes).unwrap();
        assert_eq!(v, restored);
    }

    #[test]
    fn encoding_is_deterministic() {
        let v = Rect {
            x: 5,
            y: 10,
            w: 50,
            h: 100,
        };
        let a = to_bytes::<WireError>(&v).unwrap();
        let b = to_bytes::<WireError>(&v).unwrap();
        assert_eq!(a.as_slice(), b.as_slice());
    }

    #[test]
    fn different_values_produce_different_bytes() {
        let a = to_bytes::<WireError>(&Rect {
            x: 1,
            y: 1,
            w: 1,
            h: 1,
        })
        .unwrap();
        let b = to_bytes::<WireError>(&Rect {
            x: 2,
            y: 1,
            w: 1,
            h: 1,
        })
        .unwrap();
        assert_ne!(a.as_slice(), b.as_slice());
    }
}
