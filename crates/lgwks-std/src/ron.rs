//! `ron` owns Rusty Object Notation encoding and decoding, enforcing
//! INV-RON-SERDE: all serialization round-trips through serde so types that
//! derive `Serialize`/`Deserialize` work without a second set of impls.
//!
//! RON is the preferred notation for configuration, data files, and
//! any human-facing serialization where JSON's lack of enums, comments, and
//! trailing commas creates friction. JSON remains the external-interop default.

pub use serde::{self, Deserialize, Serialize};

/// The encoder's error, as the functions above return it.
///
/// Re-exported because a facade that returns a type has to let a caller name
/// it. Without this the type is reachable only as `ron::Error` through a crate
/// this surface does not re-export, so a caller could receive a value it could
/// not write down.
pub use ron::Error;

/// The decoder's error, with the position it was found at.
///
/// Re-exported for the same reason as [`Error`]: [`from_str`] returns it.
pub use ron::error::SpannedError;

// ── Encoding ────────────────────────────────────────────────────────────────

/// Serialize `value` to a compact RON string (no indentation).
pub fn to_string<T: Serialize + ?Sized>(value: &T) -> Result<String, ron::Error> {
    ron::to_string(value)
}

/// Serialize `value` to a pretty-printed RON string with default indentation.
pub fn to_string_pretty<T: Serialize + ?Sized>(value: &T) -> Result<String, ron::Error> {
    ron::ser::to_string_pretty(value, ron::ser::PrettyConfig::default())
}

/// Serialize `value` to a writer (compact).
///
/// The value is rendered to a string before writing. Serialization errors
/// leave the writer untouched; I/O errors can leave a prefix in the writer.
/// `write_all` does not expose how many bytes were accepted before failure, so
/// this API reports no progress count. A writer failure is preserved as
/// [`WriterError::Write`], including its I/O source.
pub fn to_writer<W: std::io::Write, T: Serialize + ?Sized>(
    mut writer: W,
    value: &T,
) -> Result<(), WriterError> {
    let serialized = ron::to_string(value).map_err(WriterError::Serialize)?;
    writer
        .write_all(serialized.as_bytes())
        .map_err(WriterError::Write)
}

/// Failure while rendering RON or writing the rendered document.
///
/// `to_writer` now distinguishes serialization and I/O failures. Callers that
/// previously matched `ron::Error` should match [`Self::Serialize`]
/// and [`Self::Write`]; the contained causes remain available through
/// [`std::error::Error::source`].
#[derive(Debug)]
#[non_exhaustive]
pub enum WriterError {
    /// RON could not serialize the value; no bytes were written.
    Serialize(ron::Error),
    /// The writer failed after possibly accepting a prefix of the document.
    Write(std::io::Error),
}

impl core::fmt::Display for WriterError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::Serialize(ref error) => write!(formatter, "RON serialization failed: {error}"),
            Self::Write(ref error) => write!(formatter, "RON writer failed: {error}"),
        }
    }
}

impl std::error::Error for WriterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Serialize(ref error) => Some(error),
            Self::Write(ref error) => Some(error),
        }
    }
}

// ── Decoding ────────────────────────────────────────────────────────────────

/// Deserialize RON while allowing unescaped strings to borrow from `text`.
///
/// Escaped strings need decoded storage and therefore cannot deserialize into
/// a borrowed `&str`; use an owned `String` field for inputs that may escape.
pub fn from_str<'de, T: serde::Deserialize<'de>>(
    text: &'de str,
) -> Result<T, ron::error::SpannedError> {
    ron::from_str(text)
}

/// Deserialize a RON byte slice into the requested type.
///
/// The bytes must be UTF-8; the distinction between "not text at all" and
/// "text that is not valid RON" is preserved in [`FromSliceError`] so a caller
/// can tell a transport problem from a content problem.
pub fn from_slice<'de, T: serde::Deserialize<'de>>(bytes: &'de [u8]) -> Result<T, FromSliceError> {
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

    #[derive(Deserialize)]
    struct Borrowed<'a> {
        value: &'a str,
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

    #[test]
    fn unescaped_string_fields_borrow_from_text_and_slice() -> Result<(), Box<dyn std::error::Error>>
    {
        let text = "(value: \"borrowed\")";
        let decoded: Borrowed<'_> = from_str(text)?;
        assert_eq!(
            decoded.value, "borrowed",
            "the parsed field retains its value"
        );
        assert!(
            text.as_ptr() <= decoded.value.as_ptr()
                && decoded.value.as_ptr() < text.as_ptr().wrapping_add(text.len()),
            "unescaped RON text borrows from the supplied input"
        );

        let bytes = text.as_bytes();
        let decoded: Borrowed<'_> = from_slice(bytes)?;
        assert_eq!(
            decoded.value, "borrowed",
            "the parsed slice field retains its value"
        );
        assert!(
            bytes.as_ptr() <= decoded.value.as_ptr()
                && decoded.value.as_ptr() < bytes.as_ptr().wrapping_add(bytes.len()),
            "unescaped RON slice text borrows from the supplied input"
        );
        Ok(())
    }

    #[test]
    fn escaped_string_cannot_be_returned_as_a_borrowed_str() {
        let result: Result<Borrowed<'_>, _> = from_str("(value: \"line\\nfeed\")");
        assert!(
            result.is_err(),
            "escaped RON text requires owned decoded storage"
        );
    }

    #[test]
    fn borrowed_serializers_accept_str_and_slices() -> Result<(), ron::Error> {
        let text: &str = "plain";
        assert_eq!(to_string(text)?, "\"plain\"", "str serializes directly");
        assert_eq!(
            to_string_pretty(text)?,
            "\"plain\"",
            "pretty serialization accepts str"
        );
        assert_eq!(
            to_string([1, 2].as_slice())?,
            "[1,2]",
            "slices serialize directly"
        );
        let mut output = Vec::new();
        let serialized = to_writer(&mut output, [1, 2].as_slice());
        assert!(matches!(serialized, Ok(())), "writer serializes slices");
        assert_eq!(output, b"[1,2]", "writer serialization accepts slices");
        Ok(())
    }

    #[test]
    fn parse_errors_keep_location_trailing_content_and_utf8_source() {
        let malformed: Result<Point, _> = from_str("(\n x: )");
        assert!(
            malformed
                .as_ref()
                .err()
                .is_some_and(|error| error.span.start.line == 2 && error.span.start.col > 0),
            "RON syntax errors retain their source location"
        );

        let trailing: Result<Point, _> = from_str("(x: 1, y: 2) trailing");
        assert!(
            trailing.is_err(),
            "non-whitespace trailing content is rejected"
        );

        let invalid_utf8: Result<Point, _> = from_slice(&[0xFF]);
        assert!(
            invalid_utf8.as_ref().err().is_some_and(|error| {
                matches!(error, FromSliceError::Utf8(_))
                    && std::error::Error::source(error)
                        .is_some_and(|source| source.is::<std::str::Utf8Error>())
            }),
            "the UTF-8 refusal preserves its original source"
        );
    }

    #[test]
    fn writer_preserves_serialization_and_io_failures() {
        let mut serialization_writer = Vec::new();
        let serialization = to_writer(&mut serialization_writer, &RefusesSerialize);
        assert!(
            matches!(serialization, Err(WriterError::Serialize(_))),
            "serialization failure remains distinct"
        );
        assert!(
            serialization_writer.is_empty(),
            "serialization fails before writing"
        );
        assert!(
            serialization.as_ref().err().is_some_and(|error| {
                std::error::Error::source(error).is_some_and(|source| source.is::<ron::Error>())
            }),
            "serialization failures preserve the original RON error"
        );

        let mut partial_writer = FailAfter::new(3);
        let write = to_writer(&mut partial_writer, &Point { x: 1, y: 2 });
        assert_eq!(
            partial_writer.accepted, b"(x:",
            "the writer records its real accepted prefix"
        );
        assert!(
            write.as_ref().err().is_some_and(|error| {
                matches!(error, WriterError::Write(_))
                    && std::error::Error::source(error)
                        .is_some_and(|source| source.is::<std::io::Error>())
            }),
            "writer errors preserve the underlying I/O source"
        );

        let zero = to_writer(ZeroWriter, &Point { x: 1, y: 2 });
        assert_eq!(
            zero.as_ref().err().and_then(|error| match *error {
                WriterError::Write(ref io_error) => Some(io_error.kind()),
                WriterError::Serialize(_) => None,
            }),
            Some(std::io::ErrorKind::WriteZero),
            "write_all reports a zero-progress writer as WriteZero"
        );
    }

    /// A serializer used to force failure before the writer is touched.
    struct RefusesSerialize;

    impl serde::Serialize for RefusesSerialize {
        fn serialize<Serializer>(
            &self,
            _serializer: Serializer,
        ) -> Result<Serializer::Ok, Serializer::Error>
        where
            Serializer: serde::Serializer,
        {
            Err(<Serializer::Error as serde::ser::Error>::custom(
                "controlled serialization refusal",
            ))
        }
    }

    /// A writer that accepts a bounded prefix and then reports an I/O failure.
    struct FailAfter {
        remaining: usize,
        accepted: Vec<u8>,
    }

    impl FailAfter {
        /// Create a writer that accepts at most `limit` bytes.
        fn new(limit: usize) -> Self {
            Self {
                remaining: limit,
                accepted: Vec::new(),
            }
        }
    }

    impl std::io::Write for FailAfter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            crate::trace::warn!(
                operation = "write",
                "operation refused its request; the typed error carries the facts"
            );
            if self.remaining == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "controlled writer failure",
                ));
            }
            let accepted = bytes.len().min(self.remaining);
            self.accepted.extend_from_slice(&bytes[..accepted]);
            self.remaining = self.remaining.saturating_sub(accepted);
            Ok(accepted)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A writer that reports no progress so `write_all` returns `WriteZero`.
    struct ZeroWriter;

    impl std::io::Write for ZeroWriter {
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
            Ok(0)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
