//! Failure stages and kinds: timeouts at connect, TLS, send, body and EOF probe; truncated transport; decoding; and the bounded body reader.

use super::*;

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
    let (port, release, server) = serve_held(Some(
        b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: keep-alive\r\n\r\nx".to_vec(),
    ))?;
    let result = get_with(
        &format!("http://127.0.0.1:{port}/"),
        &quiet().timeout(Duration::from_millis(150)),
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
    let (port, release, server) = serve_held(Some(response))?;
    let result = get_with(
        &format!("http://127.0.0.1:{port}/"),
        &previewing(limit).timeout(Duration::from_millis(150)),
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
