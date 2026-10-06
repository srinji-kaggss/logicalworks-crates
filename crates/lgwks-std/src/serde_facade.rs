//! The borrowing contract both serde-backed codec facades share.
//!
//! INV-CODEC-1 is one property with two facades: an unescaped text field is
//! borrowed from the input, and an escaped one is not. `json` and `ron` are two
//! codecs over one `serde` surface, so each codec contributes only its own
//! document and its own two entry points here, and the assertion that reads
//! them is written once. Copying the assertion per codec is how two copies stop
//! testing the same thing.
//!
//! Test-only: a codec's spelling stays in the codec, and nothing outside the
//! crate's test build needs this.

use serde::Deserialize;

/// A document with one borrowed string field.
#[derive(Deserialize)]
pub(crate) struct Borrowed<'a> {
    /// The field under test, borrowed from the document when unescaped.
    pub(crate) value: &'a str,
}

/// Whether `field`'s bytes live inside `input`'s buffer.
fn borrowed_from(input: &[u8], field: &str) -> bool {
    let start = input.as_ptr();
    let end = input.as_ptr().wrapping_add(input.len());
    let at = field.as_ptr();
    at >= start && at < end
}

/// Asserts that a codec's text and slice decoders both borrow an unescaped
/// field from the input they were handed.
///
/// A field that did not borrow points outside the input's buffer and fails here,
/// with the codec named.
fn assert_borrows_from_text_and_slice<ET, ES>(
    codec: &str,
    document: &str,
    expected: &str,
    decode_text: impl for<'a> FnOnce(&'a str) -> Result<Borrowed<'a>, ET>,
    decode_slice: impl for<'a> FnOnce(&'a [u8]) -> Result<Borrowed<'a>, ES>,
) -> Result<(), String>
where
    ET: std::fmt::Debug,
    ES: std::fmt::Debug,
{
    let from_text = decode_text(document)
        .map_err(|error| format!("{codec}: text decode refused: {error:?}"))?;
    assert_eq!(
        from_text.value, expected,
        "{codec}: the parsed text field retains its value"
    );
    assert!(
        borrowed_from(document.as_bytes(), from_text.value),
        "{codec}: an unescaped text field must borrow from the supplied text"
    );

    let from_slice = decode_slice(document.as_bytes())
        .map_err(|error| format!("{codec}: slice decode refused: {error:?}"))?;
    assert_eq!(
        from_slice.value, expected,
        "{codec}: the parsed slice field retains its value"
    );
    assert!(
        borrowed_from(document.as_bytes(), from_slice.value),
        "{codec}: an unescaped slice field must borrow from the supplied input"
    );
    Ok(())
}

/// Asserts that a codec's text decoder refuses an escaped field as a borrow.
fn assert_escaped_field_is_refused<ET>(
    codec: &str,
    escaped_document: &str,
    decode_text: impl for<'a> FnOnce(&'a str) -> Result<Borrowed<'a>, ET>,
) {
    assert!(
        decode_text(escaped_document).is_err(),
        "{codec}: an escaped field needs owned decoded storage, not a borrow"
    );
}

/// `json`'s half of INV-CODEC-1: its own document, through its own entry points.
#[cfg(feature = "json")]
pub(crate) fn json_borrow_contract() -> Result<(), String> {
    assert_borrows_from_text_and_slice(
        "json",
        r#"{"value":"borrowed"}"#,
        "borrowed",
        |text| crate::json::from_str::<Borrowed<'_>>(text),
        |bytes| crate::json::from_slice::<Borrowed<'_>>(bytes),
    )
}

/// `ron`'s half of INV-CODEC-1: its own document, through its own entry points.
#[cfg(feature = "ron")]
pub(crate) fn ron_borrow_contract() -> Result<(), String> {
    assert_borrows_from_text_and_slice(
        "ron",
        "(value: \"borrowed\")",
        "borrowed",
        |text| crate::ron::from_str::<Borrowed<'_>>(text),
        |bytes| crate::ron::from_slice::<Borrowed<'_>>(bytes),
    )
}

/// `json`'s half of the escaped-field refusal.
#[cfg(feature = "json")]
pub(crate) fn json_escaped_field_is_refused() {
    assert_escaped_field_is_refused("json", r#"{"value":"line\nfeed"}"#, |text| {
        crate::json::from_str::<Borrowed<'_>>(text)
    });
}

/// `ron`'s half of the escaped-field refusal.
#[cfg(feature = "ron")]
pub(crate) fn ron_escaped_field_is_refused() {
    assert_escaped_field_is_refused("ron", "(value: \"line\\nfeed\")", |text| {
        crate::ron::from_str::<Borrowed<'_>>(text)
    });
}
