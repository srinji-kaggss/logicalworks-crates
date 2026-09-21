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
///
/// The value is rendered to a string first, so a serialization failure is
/// reported before the writer is touched and the writer never receives a
/// partial document. A write failure is reported as [`ron::Error::Message`]
/// carrying the number of bytes handed to `write_all` and the underlying I/O
/// error; a library returns that context rather than printing it.
pub fn to_writer<W: std::io::Write, T: Serialize>(
    mut writer: W,
    value: &T,
) -> Result<(), ron::Error> {
    let serialized = ron::to_string(value)?;
    writer.write_all(serialized.as_bytes()).map_err(|error| {
        ron::Error::Message(format!(
            "write_all failed after {} bytes: {error}",
            serialized.len()
        ))
    })
}

// ── Decoding ────────────────────────────────────────────────────────────────

/// Deserialize a RON string into the requested type.
pub fn from_str<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, ron::error::SpannedError> {
    ron::from_str(text)
}

/// Deserialize a RON byte slice into the requested type.
///
/// The bytes must be UTF-8; the distinction between "not text at all" and
/// "text that is not valid RON" is preserved in [`FromSliceError`] so a caller
/// can tell a transport problem from a content problem.
pub fn from_slice<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, FromSliceError> {
    let text = std::str::from_utf8(bytes).map_err(FromSliceError::Utf8)?;
    ron::from_str(text).map_err(FromSliceError::Ron)
}

/// Why a byte-slice RON decode failed.
///
/// Variants are stable and machine-readable, and `#[non_exhaustive]` keeps a
/// future rejection reason an additive change.
#[derive(Debug)]
#[non_exhaustive]
pub enum FromSliceError {
    /// Input was not valid UTF-8.
    Utf8(std::str::Utf8Error),
    /// RON parsing failed.
    Ron(ron::error::SpannedError),
}

impl core::fmt::Display for FromSliceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::Utf8(ref err) => write!(f, "RON input is not valid UTF-8: {err}"),
            Self::Ron(ref err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for FromSliceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Utf8(ref err) => Some(err),
            Self::Ron(ref err) => Some(err),
        }
    }
}

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
        Rect { w: u32, height: u32 },
    }

    // These tests return `Result` rather than unwrapping: a RON refusal reports
    // its own `Debug` on failure, which is the same report `.unwrap` would have
    // panicked with, without an `unwrap` in the tree.
    #[test]
    fn struct_roundtrips_through_string() -> Result<(), Box<dyn std::error::Error>> {
        let point = Point { x: 1, y: 2 };
        let text = to_string(&point)?;
        let restored: Point = from_str(&text)?;
        assert_eq!(point, restored);
        Ok(())
    }

    #[test]
    fn enum_roundtrips() -> Result<(), Box<dyn std::error::Error>> {
        let shapes = vec![Shape::Circle(5), Shape::Rect { w: 10, height: 20 }];
        for shape in &shapes {
            let text = to_string(shape)?;
            let restored: Shape = from_str(&text)?;
            assert_eq!(*shape, restored);
        }
        Ok(())
    }

    #[test]
    fn pretty_output_has_indentation() -> Result<(), Box<dyn std::error::Error>> {
        let point = Point { x: 1, y: 2 };
        let text = to_string_pretty(&point)?;
        assert!(text.contains('\n'));
        Ok(())
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
