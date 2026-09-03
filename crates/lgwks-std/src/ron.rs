//! `ron` owns Rusty Object Notation encoding and decoding, enforcing
//! INV-RON-SERDE: all serialization round-trips through serde so types that
//! derive `Serialize`/`Deserialize` work without a second set of impls.
//!
//! RON is the preferred notation for configuration, data files, and
//! any human-facing serialization where JSON's lack of enums, comments, and
//! trailing commas creates friction. JSON remains the external-interop default.

pub use serde::{Deserialize, Serialize};

// ── Encoding ────────────────────────────────────────────────────────────────

/// Serialize `value` to a compact RON string (no indentation).
pub fn to_string<T: Serialize>(value: &T) -> Result<String, ron::Error> {
    ron::to_string(value)
}

/// Serialize `value` to a pretty-printed RON string with default indentation.
pub fn to_string_pretty<T: Serialize>(value: &T) -> Result<String, ron::Error> {
    ron::ser::to_string_pretty(value, ron::ser::PrettyConfig::default())
}

/// Serialize `value` to a writer (compact).
pub fn to_writer<W: std::io::Write, T: Serialize>(
    mut writer: W,
    value: &T,
) -> Result<(), ron::Error> {
    let s = ron::to_string(value)?;
    writer
        .write_all(s.as_bytes())
        .map_err(|e| ron::Error::Message(e.to_string()))
}

// ── Decoding ────────────────────────────────────────────────────────────────

/// Deserialize a RON string into the requested type.
pub fn from_str<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, ron::error::SpannedError> {
    ron::from_str(s)
}

/// Deserialize a RON byte slice into the requested type.
pub fn from_slice<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, FromSliceError> {
    let s = std::str::from_utf8(bytes).map_err(FromSliceError::Utf8)?;
    ron::from_str(s).map_err(FromSliceError::Ron)
}

/// Why a byte-slice RON decode failed.
#[derive(Debug)]
pub enum FromSliceError {
    /// Input was not valid UTF-8.
    Utf8(std::str::Utf8Error),
    /// RON parsing failed.
    Ron(ron::error::SpannedError),
}

impl core::fmt::Display for FromSliceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Utf8(e) => write!(f, "RON input is not valid UTF-8: {e}"),
            Self::Ron(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FromSliceError {}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
    enum Shape {
        Circle(u32),
        Rect { w: u32, h: u32 },
    }

    #[test]
    fn struct_roundtrips_through_string() {
        let p = Point { x: 1, y: 2 };
        let s = to_string(&p).unwrap();
        let q: Point = from_str(&s).unwrap();
        assert_eq!(p, q);
    }

    #[test]
    fn enum_roundtrips() {
        let shapes = vec![Shape::Circle(5), Shape::Rect { w: 10, h: 20 }];
        for shape in &shapes {
            let s = to_string(shape).unwrap();
            let back: Shape = from_str(&s).unwrap();
            assert_eq!(*shape, back);
        }
    }

    #[test]
    fn pretty_output_has_indentation() {
        let p = Point { x: 1, y: 2 };
        let s = to_string_pretty(&p).unwrap();
        assert!(s.contains('\n'));
    }

    #[test]
    fn from_str_rejects_invalid() {
        let result: Result<Point, _> = from_str("{{{");
        assert!(result.is_err());
    }

    #[test]
    fn from_slice_rejects_invalid_utf8() {
        let result: Result<Point, _> = from_slice(&[0xFF, 0xFE]);
        assert!(matches!(result, Err(FromSliceError::Utf8(_))));
    }
}
