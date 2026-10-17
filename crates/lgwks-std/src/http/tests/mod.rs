//! `http`'s tests: real loopback servers on named, joined fixture threads.

use super::exchange::{
    host_is_literal, is_https, map_error, map_read_error, origin_of, read_bounded, reserve_for,
    resolve_location,
};
use super::*;

mod deadlines;
use deadlines::host_budget;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

const ECHO: &str = "echo-body-123";

/// Read through the end of one HTTP request header block.
fn read_request_head(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut request = [0_u8; 1024];
    let mut head = Vec::new();
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut request)?;
        if read == 0 {
            break;
        }
        head.extend_from_slice(&request[..read]);
    }
    Ok(head)
}

/// Starts one fixture thread and hands back its handle.
///
/// The named builder rather than a bare `thread::spawn`, which this
/// workspace bans because a dropped handle hides a thread whose work failed:
/// every fixture below is joined by the test that started it, and a refusal
/// to start one is reported to that test rather than ignored.
fn fixture_thread<T: Send + 'static>(
    body: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<thread::JoinHandle<T>> {
    thread::Builder::new()
        .name("lgwks-http-fixture".to_owned())
        .spawn(body)
}

/// Accept one socket, optionally send `response`, and hold the socket open
/// until the test releases it.
///
/// Returns the port, the sender that releases it, and the server thread's
/// handle: the thread is the fixture and the test that started it joins it.
/// A silent server and a server that answers a prefix are the same fixture
/// with and without the reply, so they are built once here.
fn serve_held(
    response: Option<Vec<u8>>,
) -> std::io::Result<(
    u16,
    std::sync::mpsc::SyncSender<()>,
    thread::JoinHandle<std::io::Result<()>>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let (release, wait_release) = std::sync::mpsc::sync_channel(1);
    let handle = fixture_thread(move || -> std::io::Result<()> {
        let (mut stream, _) = listener.accept()?;
        if let Some(response) = response {
            let _ = read_request_head(&mut stream)?;
            stream.write_all(&response)?;
        }
        let _released = wait_release.recv();
        Ok(())
    })?;
    Ok((port, release, handle))
}

/// Asserts that the reply served at `port` is refused at the small ceiling.
///
/// The families that use it differ in how the body is framed and in what
/// bytes it carries; what the ceiling answers does not depend on either, so
/// the refusal and its limit are asserted once here and each family states
/// only what its own framing adds.
fn assert_refused_at_small_ceiling(port: u16, what: &str) {
    // Asserts rather than returns: the only way this can fail is that the
    // answer was not the refusal, and the assertion is what carries that to
    // the test that called this. A fallible signature here would hand the
    // caller an error it has to propagate before the test could say what it
    // saw.
    let answer = get_with(
        &format!("http://127.0.0.1:{port}/"),
        &ceiling(SMALL_CEILING),
    );
    assert!(
        matches!(
            answer,
            Err(Error::BodyTooLarge { limit }) if limit == SMALL_CEILING
        ),
        "{what} must be refused at the ceiling for its size, not its framing or its encoding; got {answer:?}"
    );
}

/// Serve `replies` canned responses, then exit. Returns the bound port.
/// Reads full requests (headers plus any `Content-Length` body) before
/// replying, so POST bodies are never mistaken for missing.
///
/// Fallible rather than unwrapping, so a bind or socket refusal is reported
/// to the test that asked for the server instead of panicking in a helper.
fn serve(
    replies: Vec<(&'static str, String)>,
) -> std::io::Result<(u16, thread::JoinHandle<std::io::Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let handle = fixture_thread(move || -> std::io::Result<()> {
        for (status, body) in replies {
            let (mut stream, _) = listener.accept()?;
            serve_one(&mut stream, status, body)?;
        }
        Ok(())
    })?;
    Ok((port, handle))
}

/// Reads one request off `stream` and writes the canned reply.
///
/// A helper rather than the accept loop's body: as one block the loop carried
/// five fallible operations across the header read, the body read and the
/// reply write, and the header/body split is the part a reader of this fixture
/// actually needs to understand. It is now named, and the reply decision -- an
/// `ECHO` marker in the request wins over the canned body -- is one `if`.
#[cfg(test)]
fn serve_one(
    stream: &mut std::net::TcpStream,
    status: &'static str,
    body: String,
) -> std::io::Result<()> {
    let head = read_full_request(stream)?;
    let full = String::from_utf8_lossy(&head);
    let echoed = full.contains(ECHO);
    let payload = if echoed { ECHO.to_owned() } else { body };
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(reply.as_bytes())
}

/// Reads one whole request off `stream`: the header block, then as many body
/// bytes as its `Content-Length` declares.
///
/// Whole, because a server that closes with request bytes still unread resets
/// the connection (RFC 9112 §9.6), and on macOS the client's next socket call
/// on the reset socket fails with EINVAL before it reads a reply that was
/// already sent (#433 CI, os error 22). A peer that closes early is
/// `UnexpectedEof`, never a loop on a zero-byte read.
fn read_full_request(stream: &mut std::net::TcpStream) -> std::io::Result<Vec<u8>> {
    let mut request = vec![0u8; 4096];
    let mut head = Vec::new();
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut request)?;
        if n == 0 {
            #[cfg(feature = "trace")]
            crate::trace::warn!(
                header_bytes = head.len(),
                "canned server: the peer closed before the header block ended"
            );
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        head.extend_from_slice(&request[..n]);
    }
    let header_end = match head.windows(4).position(|w| w == b"\r\n\r\n") {
        // `position` reports the delimiter's first byte, so the header end is
        // four past it; in a 4096-byte buffer that cannot approach
        // `usize::MAX`.
        Some(position) => position.saturating_add(4),
        // No delimiter in what has been read: the header is everything read.
        None => head.len(),
    };
    let text = String::from_utf8_lossy(&head[..header_end]);
    // `Content-Length` is the only length this fixture honours. A request
    // without a parsable one declares no body — which is what a GET carries
    // — so the accumulator starts at zero and a header replaces it.
    let mut content_length = 0_usize;
    for (name, value) in text.lines().filter_map(|line| line.split_once(':')) {
        if name.eq_ignore_ascii_case("content-length") {
            match value.trim().parse::<usize>() {
                Ok(declared) => content_length = declared,
                Err(_) => content_length = 0,
            }
        }
    }
    // `header_end` is either the delimiter's end or the whole buffer, so it
    // never exceeds `head.len()`.
    let mut received = head.len().saturating_sub(header_end);
    while received < content_length {
        let n = stream.read(&mut request)?;
        if n == 0 {
            #[cfg(feature = "trace")]
            crate::trace::warn!(
                received,
                content_length,
                "canned server: the peer closed before the declared body arrived"
            );
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        head.extend_from_slice(&request[..n]);
        received = received.saturating_add(n);
    }
    Ok(head)
}

/// Joins a fixture server and hands back what it returned, surfacing its
/// refusal or a panic inside it.
fn join_server<T>(
    server: thread::JoinHandle<std::io::Result<T>>,
) -> Result<T, Box<dyn std::error::Error>> {
    let served = match server.join() {
        Ok(served) => served,
        Err(payload) => {
            // The payload is the server's own account of what went wrong, so it
            // is carried into the failure rather than replaced by a generic one.
            let message = match payload.downcast_ref::<&str>() {
                Some(text) => (*text).to_owned(),
                None => match payload.downcast_ref::<String>() {
                    Some(text) => text.clone(),
                    None => "a payload that is not text".to_owned(),
                },
            };
            #[cfg(feature = "trace")]
            crate::trace::error!(%message, "the canned server thread panicked before replying");
            return Err(
                format!("the canned server thread panicked before replying: {message}").into(),
            );
        }
    };
    Ok(served?)
}

/// Serve one raw response byte-for-byte.
///
/// The canned `serve` helper always writes a `Content-Length`, so it cannot
/// express the two framings where a body's length is discovered while
/// reading it: chunked, and close-delimited. Those are the frames a ceiling
/// has to hold under, since a declared length can be read before a single
/// body byte arrives.
fn serve_raw(reply: Vec<u8>) -> std::io::Result<(u16, thread::JoinHandle<std::io::Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let handle = fixture_thread(move || -> std::io::Result<()> {
        let (mut stream, _) = listener.accept()?;
        let mut request = vec![0u8; 4096];
        let mut head = Vec::new();
        loop {
            let n = stream.read(&mut request)?;
            // EOF before the terminator: the client gave up mid-head (a
            // reset-path test drops it on purpose). EOF is not an error, so
            // `?` cannot end this loop; without this return a zero-byte read
            // extends nothing and the fixture spins until the CI timeout.
            if n == 0 {
                return Ok(());
            }
            head.extend_from_slice(&request[..n]);
            if head.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        stream.write_all(&reply)
    })?;
    Ok((port, handle))
}

// These tests return `Result` rather than unwrapping: a refusal reports its
// own `Debug` on failure, which is the same report `.unwrap` would have
// panicked with, without an `unwrap` in the tree.
#[test]
fn default_option_wrappers_reach_the_same_path() -> Result<(), Box<dyn std::error::Error>> {
    let (port, server) = serve(vec![
        ("200 OK", "hello".to_owned()),
        ("200 OK", "ok".to_owned()),
    ])?;
    let url = format!("http://127.0.0.1:{port}/");
    let got = get(&url)?;
    assert_eq!(got.status, 200);
    assert_eq!(got.body, b"hello");
    let posted = post(&url, "text/plain", ECHO.as_bytes())?;
    assert_eq!(posted.status, 200);
    assert_eq!(posted.text()?, ECHO);
    join_server(server)?;
    Ok(())
}

fn quiet() -> Options {
    Options {
        timeout: Duration::from_secs(5),
        deadline: None,
        user_agent: "lgwks-std-test".into(),
        headers: Vec::new(),
        max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        body_policy: BodyPolicy::Whole,
        redirect_policy: RedirectPolicy::default(),
    }
}

/// The ceiling the bound tests use.
///
/// Small because the enforcement path does not depend on size: the same
/// clamped window, the same one-byte probe, and the same refusal decide a
/// 64-byte ceiling and an 8 MiB one. Keeping the bodies small keeps `serve`
/// from blocking on a socket the refusing client has stopped reading.
const SMALL_CEILING: usize = 64;

/// `quiet()` with a declared ceiling.
fn ceiling(max_body_bytes: usize) -> Options {
    quiet().max_body_bytes(max_body_bytes)
}

/// `quiet()` with a declared ceiling and the preview policy.
fn previewing(max_body_bytes: usize) -> Options {
    ceiling(max_body_bytes).body_policy(BodyPolicy::Preview)
}

/// A body of exactly `bytes` bytes.
fn filler(bytes: usize) -> String {
    "x".repeat(bytes)
}

#[test]
fn gets_status_headers_and_body() -> Result<(), Box<dyn std::error::Error>> {
    let (port, server) = serve(vec![("200 OK", "hello".to_owned())])?;
    let response = get_with(&format!("http://127.0.0.1:{port}/"), &quiet())?;
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"hello");
    assert_eq!(response.text()?, "hello");
    assert!(
        response
            .headers
            .iter()
            .any(|header| header.0.eq_ignore_ascii_case("content-length"))
    );
    join_server(server)?;
    Ok(())
}

#[test]
fn error_statuses_are_responses_not_errors() -> Result<(), Box<dyn std::error::Error>> {
    let (port, server) = serve(vec![("404 Not Found", "missing".to_owned())])?;
    let response = get_with(&format!("http://127.0.0.1:{port}/"), &quiet())?;
    assert_eq!(response.status, 404);
    assert_eq!(response.text()?, "missing");
    join_server(server)?;
    Ok(())
}

/// Raw header bytes remain distinct even when the String view replaces both.
#[test]
fn legal_header_bytes_and_repeated_values_are_preserved() -> Result<(), Box<dyn std::error::Error>>
{
    let (port, server) = serve_raw(
        b"HTTP/1.1 200 OK\r\nX-Value: \x80A\r\nX-Value: \x81A\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec(),
    )?;
    let response = get_with(&format!("http://127.0.0.1:{port}/"), &quiet())?;
    let values: Vec<_> = response
        .header_values()
        .filter(|header| header.0.eq_ignore_ascii_case("x-value"))
        .map(|header| header.1.to_vec())
        .collect();
    assert_eq!(values.len(), 2, "both repeated values are retained");
    assert!(
        values.contains(&vec![0x80, b'A']),
        "first raw value is exact"
    );
    assert!(
        values.contains(&vec![0x81, b'A']),
        "second raw value is exact"
    );
    let display_values: Vec<_> = response
        .headers()
        .iter()
        .filter(|header| header.0.eq_ignore_ascii_case("x-value"))
        .map(|header| header.1.clone())
        .collect();
    assert_eq!(
        display_values.len(),
        2,
        "the compatibility view keeps multiplicity"
    );
    assert_eq!(
        display_values[0], display_values[1],
        "lossy display is explicitly lossy"
    );
    join_server(server)?;
    Ok(())
}

/// No-follow returns a redirect response without making a second exchange.
#[test]
fn no_follow_returns_the_redirect_without_contacting_its_target()
-> Result<(), Box<dyn std::error::Error>> {
    let reply = b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/next?token=secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let (port, server) = serve_raw(reply.to_vec())?;
    let response = get_with(
        &format!("http://127.0.0.1:{port}/start?credential=hidden"),
        &quiet().redirect_policy(RedirectPolicy::NoFollow),
    )?;
    assert_eq!(
        response.status, 302,
        "the redirect remains an HTTP response"
    );
    assert_eq!(
        response.final_target(),
        format!("http://127.0.0.1:{port}/start")
    );
    assert_eq!(response.redirect_chain().len(), 1);
    assert!(!response.final_target().contains("hidden"));
    join_server(server)?;
    Ok(())
}

/// Following records sanitized provenance and preserves ureq's no-auth redirect rule.
#[test]
fn followed_redirect_records_target_and_strips_sensitive_headers()
-> Result<(), Box<dyn std::error::Error>> {
    let destination = TcpListener::bind("127.0.0.1:0")?;
    let destination_port = destination.local_addr()?.port();
    let destination_server = fixture_thread(move || -> std::io::Result<String> {
        let (mut stream, _) = destination.accept()?;
        let request = read_request_head(&mut stream)?;
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")?;
        Ok(String::from_utf8_lossy(&request).into_owned())
    })?;
    let origin = TcpListener::bind("127.0.0.1:0")?;
    let origin_port = origin.local_addr()?.port();
    let origin_server = fixture_thread(move || -> std::io::Result<()> {
        let (mut stream, _) = origin.accept()?;
        let _ = read_request_head(&mut stream)?;
        let reply = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{destination_port}/final?token=hidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(reply.as_bytes())
    })?;
    let options = quiet()
        .header("Authorization", "Bearer secret")
        .redirect_policy(RedirectPolicy::FollowAtMost(1));
    let response = get_with(
        &format!("http://127.0.0.1:{origin_port}/start?token=hidden"),
        &options,
    )?;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.final_target(),
        format!("http://127.0.0.1:{destination_port}/final")
    );
    assert_eq!(response.redirect_chain().len(), 2);
    assert!(
        response
            .redirect_chain()
            .iter()
            .all(|target| !target.contains("hidden") && !target.contains("secret")),
        "redirect provenance strips query secrets"
    );
    let received = destination_server
        .join()
        .map_err(|_| "redirect destination thread panicked")??;
    assert!(
        !received.to_ascii_lowercase().contains("authorization:"),
        "sensitive authorization is not forwarded"
    );
    origin_server
        .join()
        .map_err(|_| "redirect origin thread panicked")??;
    Ok(())
}

/// Redirect cycles stop at the declared hop limit and report a typed refusal.
#[test]
fn redirect_loop_refuses_at_the_configured_limit() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let server = fixture_thread(move || -> std::io::Result<usize> {
        let mut count = 0;
        for _ in 0..=2 {
            let (mut stream, _) = listener.accept()?;
            let _ = read_request_head(&mut stream)?;
            let reply = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/loop\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(reply.as_bytes())?;
            count += 1;
        }
        Ok(count)
    })?;
    let result = get_with(
        &format!("http://127.0.0.1:{port}/loop"),
        &quiet().redirect_policy(RedirectPolicy::FollowAtMost(2)),
    );
    assert!(matches!(
        result,
        Err(Error::Failure {
            stage: FailureStage::Redirect,
            kind: FailureKind::RedirectLimit,
            ..
        })
    ));
    assert_eq!(
        server
            .join()
            .map_err(|_| "redirect loop server panicked")??,
        3,
        "the receiver observed the initial request plus two followed hops"
    );
    Ok(())
}

/// A body that trickles in under the per-phase timeout is still stopped by
/// the whole-call deadline, and the failure names the deadline.
#[test]
fn a_deadline_bounds_a_trickled_body() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let (done, client_done) = std::sync::mpsc::sync_channel::<()>(1);
    let server = fixture_thread(move || -> std::io::Result<()> {
        let (mut stream, _) = listener.accept()?;
        let _ = read_request_head(&mut stream)?;
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\n\r\n")?;
        // One byte per 50 ms: each gap is inside the per-phase timeout, so
        // only the whole-call deadline can stop the read.
        for _ in 0..64 {
            let gap = client_done.recv_timeout(Duration::from_millis(50));
            if !matches!(gap, Err(std::sync::mpsc::RecvTimeoutError::Timeout)) {
                break;
            }
            if stream.write_all(b"x").is_err() {
                break;
            }
        }
        Ok(())
    })?;
    let started = std::time::Instant::now();
    let result = get_with(
        &format!("http://127.0.0.1:{port}/"),
        &quiet()
            .timeout(Duration::from_secs(2))
            .deadline(Duration::from_millis(400)),
    );
    let elapsed = started.elapsed();
    drop(done);
    assert!(
        matches!(
            result,
            Err(Error::Failure {
                stage: FailureStage::Deadline,
                kind: FailureKind::Timeout,
                ..
            })
        ),
        "a trickled body is stopped at the call deadline, got {result:?}"
    );
    assert!(
        elapsed < Duration::from_millis(1_500),
        "the body was not read to its 3.2 s end: {elapsed:?}"
    );
    server.join().map_err(|_| "trickle server panicked")??;
    Ok(())
}

/// Body capacity grows geometrically and never past the ceiling: a 1 MiB
/// body in 8 KiB reads reallocates about log2(128) times, not 128 (#191).
#[test]
fn body_capacity_grows_geometrically_within_the_ceiling() -> Result<(), Box<dyn std::error::Error>>
{
    let body_len = 1 << 20;
    let ceiling = body_len + 3;
    let mut body = Vec::new();
    let mut growths = 0;
    let mut last_capacity = body.capacity();
    while body.len() < body_len {
        let incoming = READ_CHUNK_BYTES.min(body_len - body.len());
        reserve_for(&mut body, incoming, ceiling)?;
        body.extend(std::iter::repeat_n(b'x', incoming));
        if body.capacity() != last_capacity {
            growths += 1;
            last_capacity = body.capacity();
        }
        assert!(
            body.capacity() <= ceiling,
            "capacity stays within the ceiling"
        );
    }
    assert!(growths <= 9, "{growths} reallocations for 128 reads");

    // Near the ceiling the growth is clamped to it, not doubled past it:
    // doubling 10 would ask for 20, and the ceiling is 15.
    let mut near_limit = Vec::with_capacity(10);
    near_limit.extend([0_u8; 10]);
    reserve_for(&mut near_limit, 5, 15)?;
    assert!(near_limit.capacity() >= 15, "room for the incoming read");
    assert!(
        near_limit.capacity() < 20,
        "the reservation was clamped to the ceiling: {}",
        near_limit.capacity()
    );
    Ok(())
}

/// Redirect limits above the supported ceiling fail before any socket opens.
#[test]
fn oversized_redirect_limit_is_refused_before_dialing() {
    assert!(matches!(
        get_with(
            "http://127.0.0.1:9/",
            &quiet().redirect_policy(RedirectPolicy::FollowAtMost(
                MAX_REDIRECT_HOPS.saturating_add(1)
            )),
        ),
        Err(Error::RedirectLimitTooLarge { .. })
    ));
}

const OK_REPLY: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

/// A bodiless redirect reply with `status` pointing at `location`.
fn redirect_reply(status: &str, location: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

/// Answer one connection per raw reply, in order, and return every
/// request head received, lowercased, in arrival order.
fn serve_recording(
    replies: Vec<String>,
) -> std::io::Result<(u16, thread::JoinHandle<std::io::Result<Vec<String>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let handle = fixture_thread(move || -> std::io::Result<Vec<String>> {
        let mut heads = Vec::new();
        for reply in replies {
            let (mut stream, _) = listener.accept()?;
            let head = read_request_head(&mut stream)?;
            heads.push(String::from_utf8_lossy(&head).to_ascii_lowercase());
            stream.write_all(reply.as_bytes())?;
        }
        Ok(heads)
    })?;
    Ok((port, handle))
}

/// Only a host that is an IP address drops the resolve bound. A name, a
/// name that begins like an address, a zoned or future IPv6 form and an
/// unreadable target all keep it, because only a real literal's lookup
/// cannot block.
#[test]
fn only_an_ip_literal_host_drops_the_resolve_bound() {
    for literal in [
        "http://127.0.0.1/",
        "http://127.0.0.1:8080/path?query",
        "https://[::1]:443/",
        "http://[2001:db8::1]/",
    ] {
        assert!(host_is_literal(literal), "{literal} names an address");
    }
    for name in [
        "http://localhost/",
        "https://example.com/",
        "http://127.0.0.1.example/",
        "http://[fe80::1%25en0]/",
        "http://[v1.fe]/",
        "not a url",
    ] {
        assert!(!host_is_literal(name), "{name} keeps the resolve bound");
    }
}

/// A hop to another origin carries none of the caller's headers: not the
/// credentials ureq stripped, and not the custom ones it forwarded.
#[test]
fn a_cross_origin_redirect_carries_no_caller_header() -> Result<(), Box<dyn std::error::Error>> {
    let (destination_port, destination) = serve_recording(vec![OK_REPLY.to_owned()])?;
    let (origin_port, origin) = serve_recording(vec![redirect_reply(
        "302 Found",
        &format!("http://127.0.0.1:{destination_port}/landed"),
    )])?;
    let options = quiet()
        .header("X-Api-Key", "key-secret")
        .header("Proxy-Authorization", "Basic proxy-secret")
        .header("Authorization", "Bearer bearer-secret");
    let response = get_with(&format!("http://127.0.0.1:{origin_port}/start"), &options)?;
    assert_eq!(response.status, 200);
    let sent = origin
        .join()
        .map_err(|_| "origin server panicked")??
        .concat();
    assert!(
        sent.contains("x-api-key: key-secret"),
        "the first hop carries the caller's headers:\n{sent}"
    );
    let forwarded = destination
        .join()
        .map_err(|_| "destination server panicked")??
        .concat();
    assert!(
        !forwarded.contains("secret"),
        "no caller header reaches another origin:\n{forwarded}"
    );
    Ok(())
}

/// A hop back to the same origin keeps ordinary caller headers and drops
/// credentials; a relative `Location` resolves and loses its fragment.
#[test]
fn a_same_origin_redirect_keeps_ordinary_headers_but_not_credentials()
-> Result<(), Box<dyn std::error::Error>> {
    let (port, server) = serve_recording(vec![
        redirect_reply("302 Found", "next?page=2#section"),
        OK_REPLY.to_owned(),
    ])?;
    let options = quiet()
        .header("X-Api-Key", "key-value")
        .header("Authorization", "Bearer bearer-secret");
    let response = get_with(&format!("http://127.0.0.1:{port}/dir/start"), &options)?;
    assert_eq!(
        response.final_target(),
        format!("http://127.0.0.1:{port}/dir/next")
    );
    let heads = server.join().map_err(|_| "server panicked")??;
    let (Some(first), Some(second), 2) = (heads.first(), heads.get(1), heads.len()) else {
        return Err(format!("expected two requests, saw {}", heads.len()).into());
    };
    assert!(first.contains("authorization: bearer bearer-secret"));
    assert!(
        second.starts_with("get /dir/next?page=2 "),
        "the relative Location resolves against the target, without its fragment:\n{second}"
    );
    assert!(
        second.contains("x-api-key: key-value"),
        "same origin keeps ordinary headers:\n{second}"
    );
    assert!(
        !second.contains("authorization:"),
        "credentials are not re-sent on a server's say-so:\n{second}"
    );
    Ok(())
}

/// 303 turns a POST into a bodiless GET with no content type.
#[test]
fn a_see_other_redirect_turns_a_post_into_a_bodiless_get() -> Result<(), Box<dyn std::error::Error>>
{
    let (port, server) = serve_recording(vec![
        redirect_reply("303 See Other", "/after"),
        OK_REPLY.to_owned(),
    ])?;
    let response = post_with(
        &format!("http://127.0.0.1:{port}/submit"),
        "application/json",
        b"",
        &quiet(),
    )?;
    assert_eq!(response.status, 200);
    let heads = server.join().map_err(|_| "server panicked")??;
    let (Some(first), Some(second), 2) = (heads.first(), heads.get(1), heads.len()) else {
        return Err(format!("expected two requests, saw {}", heads.len()).into());
    };
    assert!(first.starts_with("post /submit "), "{first}");
    assert!(second.starts_with("get /after "), "{second}");
    assert!(
        !second.contains("content-type:"),
        "the GET carries no body, so no content type:\n{second}"
    );
    Ok(())
}

mod redirects_and_ceilings;
