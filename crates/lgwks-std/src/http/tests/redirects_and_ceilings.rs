//! `http`'s redirect, method-rewrite and body-ceiling tests, beside the
//! fixtures in the parent module.

use super::*;

/// 307 keeps the method, so following it would replay the POST body at a
/// target the caller never named; it is refused without a second request.
#[test]
fn a_method_keeping_redirect_of_a_post_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let (port, server) = serve_recording(vec![redirect_reply("307 Temporary Redirect", "/again")])?;
    let result = post_with(
        &format!("http://127.0.0.1:{port}/submit"),
        "application/json",
        b"",
        &quiet(),
    );
    assert!(
        matches!(
            result,
            Err(Error::Failure {
                stage: FailureStage::Redirect,
                kind: FailureKind::Transport,
                ..
            })
        ),
        "{result:?}"
    );
    let heads = server.join().map_err(|_| "server panicked")??;
    assert_eq!(heads.len(), 1, "the body was not replayed");
    Ok(())
}

/// Origins compare lowercased scheme and host and the effective port.
#[test]
fn origins_compare_scheme_host_and_effective_port() {
    assert!(origin_of("HTTP://Example.COM/a").is_some());
    assert_eq!(
        origin_of("HTTP://Example.COM/a"),
        origin_of("http://example.com:80/b")
    );
    assert_eq!(
        origin_of("https://example.com/"),
        origin_of("https://example.com:443/")
    );
    assert_ne!(
        origin_of("https://example.com/"),
        origin_of("http://example.com/"),
        "a downgrade is another origin"
    );
    assert_ne!(
        origin_of("http://example.com:8080/"),
        origin_of("http://example.com/")
    );
    assert_eq!(
        origin_of("http://[::1]:8080/"),
        origin_of("http://[::1]:8080/x")
    );
    assert!(
        origin_of("http://example.com:99999/").is_none(),
        "an unreadable port is never the same origin"
    );
}

/// A `Location` resolves to an absolute http(s) target or is refused
/// without echoing it.
#[test]
fn a_location_resolves_to_an_absolute_http_target_or_is_refused() -> Result<(), Error> {
    assert_eq!(
        resolve_location("http://a.test/dir/page", "next?x=1#top")?,
        "http://a.test/dir/next?x=1"
    );
    assert_eq!(
        resolve_location("http://a.test/dir/page", "//b.test/p")?,
        "http://b.test/p"
    );
    for hostile in [
        "ftp://a.test/token-secret",
        "javascript:token-secret",
        "http://",
        "not a uri token-secret",
    ] {
        let refusal = resolve_location("http://a.test/", hostile);
        assert!(
            matches!(
                refusal,
                Err(Error::Failure {
                    stage: FailureStage::Redirect,
                    ..
                })
            ),
            "{hostile} must be refused"
        );
        assert!(
            !format!("{refusal:?}").contains("token-secret"),
            "the Location is not echoed"
        );
    }
    Ok(())
}

#[test]
fn posts_body_with_content_type() -> Result<(), Box<dyn std::error::Error>> {
    let (port, server) = serve(vec![("200 OK", String::new())])?;
    let response = post_with(
        &format!("http://127.0.0.1:{port}/"),
        "text/plain",
        ECHO.as_bytes(),
        &quiet(),
    )?;
    assert_eq!(response.status, 200);
    assert_eq!(response.text()?, ECHO);
    join_server(server)?;
    Ok(())
}

/// Asserts `url` is refused as an invalid URL, before any dial.
fn assert_refused_before_dialing(url: &str) {
    assert!(
        matches!(get_with(url, &quiet()), Err(Error::InvalidUrl)),
        "{url:?} must be refused as an invalid URL before any dial"
    );
}

#[test]
fn rejects_non_http_urls_before_dialing() {
    assert_refused_before_dialing("not a url");
    assert_refused_before_dialing("/relative/path");
    assert_refused_before_dialing("ftp://127.0.0.1/file");
    // Authority-less absolute URIs pass the scheme check but are not valid
    // request targets; ureq refuses them with a message that embeds the
    // raw text, so they must be rejected here instead.
    assert_refused_before_dialing("http:user:SECRET@host");
    assert_refused_before_dialing("https://");
}

#[test]
fn refused_connection_is_transport_not_timeout() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let Err(error) = get_with(&format!("http://127.0.0.1:{port}/"), &quiet()) else {
        return Err("a refused connection must not yield a response".into());
    };
    assert!(matches!(
        error,
        Error::Failure {
            stage: FailureStage::Connect,
            kind: FailureKind::Transport,
            ..
        }
    ));
    Ok(())
}

#[test]
fn custom_headers_reach_the_server() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    // The recorder returns the request it saw, so a socket refusal in the
    // thread reaches the test through the join below instead of panicking.
    let handle = fixture_thread(move || -> std::io::Result<String> {
        let (mut stream, _) = listener.accept()?;
        // The whole request, body included, before the reply: replying and
        // closing with the body unread reset the connection (#433 CI).
        let text = String::from_utf8_lossy(&read_full_request(&mut stream)?).into_owned();
        let reply = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
        stream.write_all(reply.as_bytes())?;
        Ok(text)
    })?;
    let options = quiet()
        .header("Authorization", "Bearer test-token")
        .idempotency_key("old-operation")
        .idempotency_key("current-operation");
    let response = post_with(
        &format!("http://127.0.0.1:{port}/"),
        "text/plain",
        b"hi",
        &options,
    )?;
    assert_eq!(response.status, 200);
    let seen = handle
        .join()
        .map_err(|_| "the recording server thread panicked before replying")??;
    assert!(
        seen.to_ascii_lowercase()
            .contains("authorization: bearer test-token"),
        "server never saw the Authorization header:\n{seen}"
    );
    assert_eq!(
        seen.to_ascii_lowercase()
            .matches("idempotency-key:")
            .count(),
        1,
        "the receiver sees exactly one idempotency key"
    );
    assert!(
        seen.to_ascii_lowercase()
            .contains("idempotency-key: current-operation")
    );
    Ok(())
}

#[test]
fn silent_server_hits_timeout() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    // Held open past the client's timeout: the accept must succeed (so the
    // dial is not what fails) and the reply must never come, leaving the
    // client's read timeout as the only thing that can end the request.
    let (release, released) = std::sync::mpsc::sync_channel(1);
    let handle = fixture_thread(move || -> std::io::Result<()> {
        let (_stream, _) = listener.accept()?;
        let _released = released.recv();
        Ok(())
    })?;
    let options = quiet().timeout(Duration::from_millis(200));
    let Err(error) = get_with(&format!("http://127.0.0.1:{port}/"), &options) else {
        return Err("a silent server must hit the read timeout".into());
    };
    // A signal can interrupt the read on the way to the timeout, and the
    // kernel may deliver it before the deadline. That is a real and
    // different outcome — see `an_interrupted_request_is_not_a_transport
    // failure` — so a silent server yields either the header-stage
    // timeout or the interruption, and never a transport failure.
    assert!(
        matches!(
            error,
            Error::Failure {
                stage: FailureStage::Headers,
                kind: FailureKind::Timeout,
                ..
            } | Error::Failure {
                kind: FailureKind::Interrupted,
                ..
            }
        ),
        "a silent server yields a header timeout or an interruption, not {error:?}"
    );
    release
        .send(())
        .map_err(|_| "timeout fixture receiver ended")?;
    handle
        .join()
        .map_err(|_| "timeout fixture thread panicked")??;
    Ok(())
}

/// A stalled TLS handshake is reported at the connect stage with timeout class.
#[test]
fn tls_handshake_timeout_preserves_connect_stage() -> Result<(), Box<dyn std::error::Error>> {
    let (port, release, server) = serve_held(None)?;
    let result = get_with(
        &format!("https://127.0.0.1:{port}/"),
        &quiet().timeout(Duration::from_millis(150)),
    );
    assert!(matches!(
        result,
        Err(Error::Failure {
            stage: FailureStage::Connect,
            kind: FailureKind::Timeout,
            ..
        })
    ));
    release.send(()).map_err(|_| "TLS fixture receiver ended")?;
    server.join().map_err(|_| "TLS fixture thread panicked")??;
    Ok(())
}

/// Malformed TLS bytes remain a TLS-class transport failure.
#[test]
fn malformed_tls_handshake_preserves_tls_stage() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let server = fixture_thread(move || -> std::io::Result<()> {
        let (mut stream, _) = listener.accept()?;
        // Read the ClientHello before answering and drain until the client
        // hangs up: closing a socket with unread bytes sends RST, and a
        // reset that overtakes the reply is a connection failure, not the
        // malformed record this test is about.
        let mut hello = [0_u8; 1024];
        let _hello_len = stream.read(&mut hello)?;
        stream.write_all(b"not-a-tls-record")?;
        // How the client hangs up (FIN or reset) is not under test.
        let _hung_up = std::io::copy(&mut stream, &mut std::io::sink());
        Ok(())
    })?;
    let result = get_with(
        &format!("https://127.0.0.1:{port}/"),
        &quiet().timeout(Duration::from_secs(2)),
    );
    assert!(matches!(
        result,
        Err(Error::Failure {
            stage: FailureStage::Tls,
            kind: FailureKind::Transport,
            ..
        })
    ));
    server
        .join()
        .map_err(|_| "malformed TLS fixture panicked")??;
    Ok(())
}

/// A stalled request-body write reports the send stage and timeout class.
#[test]
fn request_body_timeout_preserves_send_stage() -> Result<(), Box<dyn std::error::Error>> {
    let (port, release, server) = serve_held(None)?;
    let body = vec![b'x'; 32 * 1024 * 1024];
    let result = post_with(
        &format!("http://127.0.0.1:{port}/"),
        "application/octet-stream",
        &body,
        &quiet().timeout(Duration::from_millis(150)),
    );
    assert!(matches!(
        result,
        Err(Error::Failure {
            stage: FailureStage::Send,
            kind: FailureKind::Timeout,
            ..
        })
    ));
    release
        .send(())
        .map_err(|_| "send fixture receiver ended")?;
    server
        .join()
        .map_err(|_| "send fixture thread panicked")??;
    Ok(())
}

/// A body timeout keeps its body phase and timeout class after headers land.
#[test]
fn body_timeout_preserves_stage_and_class() -> Result<(), Box<dyn std::error::Error>> {
    // The head must land before the timeout for the stall to be a body stall (#272).
    let timeout = host_budget(Duration::from_millis(150), Duration::from_secs(5))?;
    let (port, release, server) = serve_held(Some(
        b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: keep-alive\r\n\r\nx".to_vec(),
    ))?;
    let result = get_with(
        &format!("http://127.0.0.1:{port}/"),
        &quiet().timeout(timeout),
    );
    assert!(matches!(
        result,
        Err(Error::Failure {
            stage: FailureStage::Body,
            kind: FailureKind::Timeout,
            ..
        })
    ));
    release
        .send(())
        .map_err(|_| "body fixture receiver ended")?;
    server
        .join()
        .map_err(|_| "body fixture thread panicked")??;
    Ok(())
}

/// The exact-cap EOF probe reports its own timeout stage.
#[test]
fn eof_probe_timeout_preserves_stage_and_class() -> Result<(), Box<dyn std::error::Error>> {
    let limit = SMALL_CEILING;
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
        limit.saturating_add(1)
    )
    .into_bytes();
    response.extend(std::iter::repeat_n(b'x', limit));
    // The head and the body must land before the timeout for the stall to be the probe's (#272).
    let timeout = host_budget(Duration::from_millis(150), Duration::from_secs(5))?;
    let (port, release, server) = serve_held(Some(response))?;
    let result = get_with(
        &format!("http://127.0.0.1:{port}/"),
        &previewing(limit).timeout(timeout),
    );
    assert!(matches!(
        result,
        Err(Error::Failure {
            stage: FailureStage::EofProbe,
            kind: FailureKind::Timeout,
            ..
        })
    ));
    release.send(()).map_err(|_| "EOF fixture receiver ended")?;
    server.join().map_err(|_| "EOF fixture thread panicked")??;
    Ok(())
}

/// A short content-length response is transport failure, never ordinary EOF.
#[test]
fn truncated_transport_is_not_reported_as_complete_eof() -> Result<(), Box<dyn std::error::Error>> {
    let (port, server) = serve_raw(
        b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nabc".to_vec(),
    )?;
    let result = get_with(&format!("http://127.0.0.1:{port}/"), &quiet());
    assert!(matches!(
        result,
        Err(Error::Failure {
            stage: FailureStage::Body,
            kind: FailureKind::Transport,
            ..
        })
    ));
    join_server(server)?;
    Ok(())
}

/// Invalid UTF-8 belongs to payload decoding after the HTTP response exists.
#[test]
fn invalid_utf8_is_a_payload_decoding_failure() -> Result<(), Box<dyn std::error::Error>> {
    let (port, server) = serve_raw(
        b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n\xff".to_vec(),
    )?;
    let response = get_with(&format!("http://127.0.0.1:{port}/"), &quiet())?;
    assert_eq!(response.status, 200, "the HTTP exchange completed");
    assert!(matches!(
        response.text(),
        Err(Error::Failure {
            stage: FailureStage::TextDecode,
            kind: FailureKind::InvalidUtf8,
            ..
        })
    ));
    join_server(server)?;
    Ok(())
}

/// Logical Vec capacity stays inside a non-power-of-two body ceiling.
#[test]
fn non_power_of_two_ceiling_bounds_retained_capacity() -> Result<(), Box<dyn std::error::Error>> {
    let limit = 73;
    let mut reader = std::io::Cursor::new(filler(limit).into_bytes());
    let (body, truncation) = read_bounded(&mut reader, &ceiling(limit))?;
    assert_eq!(body.len(), limit, "the exact body is retained");
    assert!(
        body.capacity() <= limit,
        "body capacity stays within the declared ceiling"
    );
    assert_eq!(truncation, Truncation::Complete);
    Ok(())
}

/// The body reader is handed one fixed-size window per call, never the
/// declared body length, and the retained capacity is clamped to a
/// non-power-of-two ceiling.
///
/// This is the *scratch* measurement the ceiling's contract names: the
/// window handed to the transport is the scratch the read path commits,
/// and it is `min(remaining, READ_CHUNK_BYTES)` — independent of how large
/// the body claims to be. A mutant that passed `remaining` straight through
/// would hand the reader `limit` bytes here and fail the `READ_CHUNK_BYTES`
/// bound for the 10 000-byte ceiling, which is why that ceiling is one of
/// the measured cases: it is the smallest that forces more than one chunk.
#[test]
fn the_read_window_is_a_fixed_chunk_and_capacity_is_clamped_to_the_ceiling()
-> Result<(), Box<dyn std::error::Error>> {
    /// A reader that yields one byte per call and records the largest
    /// buffer it was ever handed.
    struct WindowRecorder {
        /// The bytes still to yield.
        remaining: usize,
        /// The largest `read` window observed.
        largest_window: usize,
        /// Number of `read` calls, including the EOF probe.
        reads: usize,
    }

    impl Read for WindowRecorder {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.largest_window = self.largest_window.max(buffer.len());
            self.reads = self.reads.saturating_add(1);
            if buffer.is_empty() || self.remaining == 0 {
                return Ok(0);
            }
            buffer[0] = b'x';
            self.remaining = self.remaining.saturating_sub(1);
            Ok(1)
        }
    }

    for limit in [73_usize, 1000, 3003, 10_000] {
        let mut reader = WindowRecorder {
            remaining: limit,
            largest_window: 0,
            reads: 0,
        };
        let (body, truncation) = read_bounded(&mut reader, &ceiling(limit))?;
        assert_eq!(
            body.len(),
            limit,
            "the exact body is retained at a {limit}-byte ceiling"
        );
        assert_eq!(
            body.capacity(),
            limit,
            "capacity is clamped to the non-power-of-two ceiling at {limit}: {}",
            body.capacity()
        );
        assert_eq!(
            truncation,
            Truncation::Complete,
            "an exactly-at-ceiling body ends on its own at {limit}"
        );
        assert!(
            reader.largest_window <= READ_CHUNK_BYTES,
            "the reader window is one fixed chunk at {limit}, not the body: {}",
            reader.largest_window
        );
        if limit > READ_CHUNK_BYTES {
            assert_eq!(
                reader.largest_window, READ_CHUNK_BYTES,
                "the first window of a body larger than one chunk is exactly one chunk"
            );
        }
        assert_eq!(
            reader.reads,
            limit.saturating_add(1),
            "one read per byte plus the single EOF probe at {limit}"
        );
    }
    Ok(())
}

/// An interruption is its own class at every stage, never a transport failure.
///
/// ureq's TCP transport maps `TimedOut` and `WouldBlock` to its timeout
/// variant and lets `EINTR` through as an I/O error, which arrived here as
/// a transport failure. Signals land on whichever thread they land on, so
/// under load this was frequent enough to break CI. The request path and
/// the body path classify it the same way, and each keeps its stage.
#[test]
fn an_interruption_is_classified_the_same_way_at_every_stage() {
    let interrupted = || std::io::Error::from(std::io::ErrorKind::Interrupted);
    assert!(
        matches!(
            map_error(ureq::Error::Io(interrupted()), false),
            Error::Failure {
                kind: FailureKind::Interrupted,
                ..
            }
        ),
        "EINTR on the request path must not be reported as a transport failure"
    );
    for stage in [FailureStage::Body, FailureStage::EofProbe] {
        assert!(
            matches!(
                map_read_error(interrupted(), stage),
                Error::Failure {
                    kind: FailureKind::Interrupted,
                    stage: observed,
                    ..
                } if observed == stage
            ),
            "EINTR while reading at {stage:?} keeps its stage and its class"
        );
    }
    // A genuine transport error still is one, so the split is meaningful
    // rather than every I/O failure collapsing to an interruption.
    assert!(matches!(
        map_error(
            ureq::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionReset)),
            false,
        ),
        Error::Failure {
            kind: FailureKind::Transport,
            ..
        }
    ));
}

/// The scheme check behind TLS stage attribution ignores case, as
/// [`validate_url`] does.
#[test]
fn the_https_scheme_is_recognised_in_any_case() {
    assert!(is_https("https://example.com/"));
    assert!(is_https("HTTPS://example.com/"));
    assert!(is_https("HtTpS://example.com/"));
    assert!(!is_https("http://example.com/"));
    assert!(!is_https("https:"));
}

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
