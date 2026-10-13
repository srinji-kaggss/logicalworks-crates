//! Body ceilings: whole bodies, refusals one byte past, chunked and close-delimited bodies, and previews.

use super::*;

// ── Body ceilings ───────────────────────────────────────────────────────

/// The ceiling bounds a read; it does not clip a body that fits under it.
#[test]
fn a_body_under_the_ceiling_is_whole() -> Result<(), Box<dyn std::error::Error>> {
    let (port, server) = serve(vec![("200 OK", "hello".to_owned())])?;
    let response = get_with(
        &format!("http://127.0.0.1:{port}/"),
        &ceiling(SMALL_CEILING),
    )?;
    assert_eq!(response.body, b"hello");
    assert_eq!(
        response.truncation,
        Truncation::Complete,
        "a body below the ceiling ended on its own"
    );
    join_server(server)?;
    Ok(())
}

/// A body that ends exactly at the ceiling has not overflowed it, and the
/// exchange must not refuse it: the boundary is inclusive, and getting it
/// wrong here would make the ceiling one byte smaller than declared.
#[test]
fn a_body_exactly_at_the_ceiling_is_whole() -> Result<(), Box<dyn std::error::Error>> {
    let payload = filler(SMALL_CEILING);
    let (port, server) = serve(vec![("200 OK", payload)])?;
    let response = get_with(
        &format!("http://127.0.0.1:{port}/"),
        &ceiling(SMALL_CEILING),
    )?;
    assert_eq!(
        response.body.len(),
        SMALL_CEILING,
        "the whole body is kept at the ceiling"
    );
    assert_eq!(
        response.truncation,
        Truncation::Complete,
        "a body that ends at the ceiling has not overflowed it"
    );
    join_server(server)?;
    Ok(())
}

/// A zero-byte ceiling accepts an empty body after its EOF probe.
#[test]
fn zero_ceiling_accepts_an_empty_body() -> Result<(), Box<dyn std::error::Error>> {
    let (port, server) =
        serve_raw(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec())?;
    let response = get_with(&format!("http://127.0.0.1:{port}/"), &ceiling(0))?;
    assert!(
        response.body.is_empty(),
        "zero ceiling retains no body bytes"
    );
    assert_eq!(response.truncation, Truncation::Complete);
    join_server(server)?;
    Ok(())
}

/// A zero-byte ceiling distinguishes an empty response from one body byte.
#[test]
fn zero_ceiling_refuses_a_nonempty_whole_body() -> Result<(), Box<dyn std::error::Error>> {
    let (port, server) =
        serve_raw(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx".to_vec())?;
    let result = get_with(&format!("http://127.0.0.1:{port}/"), &ceiling(0));
    assert!(matches!(result, Err(Error::BodyTooLarge { limit: 0 })));
    join_server(server)?;
    Ok(())
}

/// One byte past the ceiling is a refusal, not a body that happens to be
/// short: the caller asked for a body and cannot see that it got a prefix.
#[test]
fn a_body_one_byte_past_the_ceiling_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let payload = filler(SMALL_CEILING.saturating_add(1));
    let (port, server) = serve(vec![("200 OK", payload)])?;
    let Err(error) = get_with(
        &format!("http://127.0.0.1:{port}/"),
        &ceiling(SMALL_CEILING),
    ) else {
        return Err("a body past the ceiling must be refused, not handed over as a prefix".into());
    };
    assert_eq!(
        error,
        Error::BodyTooLarge {
            limit: SMALL_CEILING
        },
        "the refusal names the ceiling that was reached"
    );
    join_server(server)?;
    Ok(())
}

/// The ceiling holds for a body whose length is discovered while reading.
///
/// A `Content-Length` can be compared against the ceiling before a single
/// body byte arrives; a chunked body has no such number, so this is the
/// frame where the bound has to be enforced by how much is read.
#[test]
fn a_chunked_body_past_the_ceiling_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    // Two 64-byte chunks: 128 bytes of body under a 64-byte ceiling.
    const CHUNKED: &[u8] = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n40\r\nxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\r\n40\r\nyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy\r\n0\r\n\r\n";
    let (port, server) = serve_raw(CHUNKED.to_vec())?;
    assert_refused_at_small_ceiling(port, "a chunked body past the ceiling");
    join_server(server)?;
    Ok(())
}

/// The same ceiling holds for a close-delimited body, where the end of the
/// body is the end of the connection and nothing declares its length.
#[test]
fn a_close_delimited_body_past_the_ceiling_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let mut reply = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
    reply.extend_from_slice(filler(SMALL_CEILING.saturating_mul(2)).as_bytes());
    let (port, server) = serve_raw(reply)?;
    assert_refused_at_small_ceiling(port, "a close-delimited body past the ceiling");
    join_server(server)?;
    Ok(())
}

/// A preview is a declared truncation: the prefix is the result, and the
/// response says so.
#[test]
fn a_preview_keeps_the_ceiling_and_reports_the_cut() -> Result<(), Box<dyn std::error::Error>> {
    let payload = filler(SMALL_CEILING.saturating_mul(2));
    let (port, server) = serve(vec![("200 OK", payload)])?;
    let response = get_with(
        &format!("http://127.0.0.1:{port}/"),
        &previewing(SMALL_CEILING),
    )?;
    assert_eq!(
        response.body.len(),
        SMALL_CEILING,
        "the preview stops at the ceiling and keeps no more"
    );
    assert!(
        response.body.iter().all(|byte| *byte == b'x'),
        "the preview is the body's own prefix"
    );
    assert_eq!(
        response.truncation,
        Truncation::Cut,
        "a body that continues past the ceiling is reported as cut"
    );
    join_server(server)?;
    Ok(())
}

/// The preview policy reports a body that fits as whole: `Cut` is a fact
/// about the body, not a restatement of the policy that was selected.
#[test]
fn a_preview_of_a_body_that_ends_under_the_ceiling_is_whole()
-> Result<(), Box<dyn std::error::Error>> {
    let (port, server) = serve(vec![("200 OK", "alive".to_owned())])?;
    let response = get_with(
        &format!("http://127.0.0.1:{port}/"),
        &previewing(SMALL_CEILING),
    )?;
    assert_eq!(response.body, b"alive");
    assert_eq!(
        response.truncation,
        Truncation::Complete,
        "a preview of a body that ended on its own is not a cut"
    );
    join_server(server)?;
    Ok(())
}

/// A non-UTF-8 body reaching the ceiling is still refused, and the refusal
/// is about the size, not the encoding: deciding UTF-8 is
/// [`Response::text`]'s job and it never runs on a prefix.
#[test]
fn the_ceiling_is_enforced_before_any_utf8_decision() -> Result<(), Box<dyn std::error::Error>> {
    let mut reply = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
    reply.extend_from_slice(&vec![0xFF_u8; SMALL_CEILING.saturating_mul(2)]);
    let (port, server) = serve_raw(reply)?;
    assert_refused_at_small_ceiling(port, "a multi-byte body past the ceiling");
    join_server(server)?;
    Ok(())
}

/// A preview stops reading at its ceiling, and the stop is observable.
///
/// The reply served here is larger than a socket buffer can hold, so the
/// fixture can only finish writing it to a client that keeps draining it. A
/// reader that stops at the ceiling leaves the server's write refused, and
/// that refusal is the only evidence the bound exists: a bounded preview and
/// an unbounded read trimmed afterwards produce the same bytes, and differ
/// in what the server was allowed to make the client hold while producing
/// them.
#[test]
fn a_preview_stops_reading_at_the_ceiling() -> Result<(), Box<dyn std::error::Error>> {
    let mut reply = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
    reply.extend_from_slice(&vec![b'x'; SMALL_CEILING.saturating_mul(65_536)]);
    let (port, server) = serve_raw(reply)?;
    let response = get_with(
        &format!("http://127.0.0.1:{port}/"),
        &previewing(SMALL_CEILING),
    )?;
    assert_eq!(
        response.body.len(),
        SMALL_CEILING,
        "the preview stops at the ceiling"
    );
    assert_eq!(
        response.truncation,
        Truncation::Cut,
        "the reply continues past the ceiling"
    );
    let served = server
        .join()
        .map_err(|_| "the raw server thread panicked before replying")?;
    assert!(
        served.is_err(),
        "a 4 MiB reply can only be written in full to a reader that keeps reading, so a \
             completed write means the client drained it: a preview must stop at its ceiling"
    );
    Ok(())
}
