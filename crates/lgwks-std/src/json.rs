//! `json` owns JSON encoding and decoding for external interfaces.
//! Built on serde_json for correctness and interoperability. For
//! internal binary wire format, use the `wire` module (feature `wire`).
//!
//! INV-JSON-UTF8: all output is valid UTF-8. Input is validated as
//! UTF-8 during deserialization.

// ── Re-exports ──────────────────────────────────────────────────────────────

pub use serde::{self, Deserialize, Serialize};
pub use serde_json::{Deserializer, Error, Map, Number, Value, json};

// ── Encoding ────────────────────────────────────────────────────────────────

/// Serialize `value` to a compact JSON string (no whitespace, no newlines).
pub fn to_string<T: Serialize>(value: &T) -> Result<String, Error> {
    serde_json::to_string(value)
}

/// Serialize `value` to a pretty-printed JSON string with indentation.
pub fn to_string_pretty<T: Serialize>(value: &T) -> Result<String, Error> {
    serde_json::to_string_pretty(value)
}

/// Serialize `value` to a byte vector (compact JSON).
pub fn to_vec<T: Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(value)
}

/// Serialize `value` to a writer.
pub fn to_writer<W: std::io::Write, T: Serialize>(writer: W, value: &T) -> Result<(), Error> {
    serde_json::to_writer(writer, value)
}

// ── Decoding ────────────────────────────────────────────────────────────────

/// Deserialize a JSON string into the requested type.
pub fn from_str<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, Error> {
    serde_json::from_str(text)
}

/// Deserialize a JSON byte slice into the requested type.
pub fn from_slice<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(bytes)
}

/// Deserialize the entire `reader` as JSON into `T`. The reader is consumed to
/// EOF; an I/O failure and a syntax failure both surface as [`Error`].
pub fn from_reader<R: std::io::Read, T: serde::de::DeserializeOwned>(
    reader: R,
) -> Result<T, Error> {
    serde_json::from_reader(reader)
}

/// Deserialize a [`Value`] into the requested type.
pub fn from_value<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value)
}

/// Build a [`Value`] tree from `value`. Fails when `value` contains a map whose
/// keys are not strings, or when its `Serialize` impl reports an error.
pub fn to_value<T: Serialize>(value: &T) -> Result<Value, Error> {
    serde_json::to_value(value)
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

    // These tests return `Result` rather than unwrapping: a JSON refusal
    // reports its own `Debug` on failure, which is the same report `.unwrap`
    // would have panicked with, without an `unwrap` in the tree.
    #[test]
    fn roundtrips_through_string() -> Result<(), Error> {
        let point = Point { x: 1, y: 2 };
        let text = to_string(&point)?;
        let restored: Point = from_str(&text)?;
        assert_eq!(point, restored);
        Ok(())
    }

    #[test]
    fn compact_output_has_no_whitespace() -> Result<(), Error> {
        let point = Point { x: 1, y: 2 };
        let text = to_string(&point)?;
        assert!(!text.contains('\n'));
        assert!(!text.contains("  "));
        Ok(())
    }

    #[test]
    fn pretty_output_has_indentation() -> Result<(), Error> {
        let point = Point { x: 1, y: 2 };
        let text = to_string_pretty(&point)?;
        assert!(text.contains('\n'));
        Ok(())
    }

    #[test]
    fn from_str_rejects_invalid() {
        let result: Result<Point, _> = from_str("{");
        assert!(result.is_err());
    }
}
