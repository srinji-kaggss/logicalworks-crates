//! Tests of the HTTP client against real loopback servers: exchange, headers, redirects, deadlines, failure stages and body ceilings.

use super::*;
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

/// Hold `stream` for up to `pause`, the way a slow upstream does, and say
/// whether the client hung up first.
///
/// The wait is a read on the client's own socket, so the client hanging up
/// ends it at once. A client that is still waiting for an answer sends
/// nothing, so the read times out at `pause`. The pause is the slow upstream
/// under test, and it is cut short by the one event that makes it moot.
fn client_hung_up_within(stream: &mut TcpStream, pause: Duration) -> std::io::Result<bool> {
    stream.set_read_timeout(Some(pause))?;
    let mut probe = [0_u8; 1];
    match stream.read(&mut probe) {
        Ok(0) => Ok(true),
        Ok(_) => Ok(false),
        Err(error) => match error.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => Ok(false),
            std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe => Ok(true),
            _ => Err(error),
        },
    }
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
    let mut request = vec![0u8; 4096];
    let mut head = Vec::new();
    loop {
        let n = stream.read(&mut request)?;
        head.extend_from_slice(&request[..n]);
        if head.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
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
        head.extend_from_slice(&request[..n]);
        received = received.saturating_add(n);
    }
    let full = String::from_utf8_lossy(&head);
    let echoed = full.contains(ECHO);
    let payload = if echoed { ECHO.to_owned() } else { body };
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(reply.as_bytes())
}

/// Joins the canned server, surfacing its refusal or a panic inside it.
fn join_server(
    server: thread::JoinHandle<std::io::Result<()>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let served = server
        .join()
        .map_err(|_| "the canned server thread panicked before replying")?;
    served?;
    Ok(())
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
        let mut request = vec![0u8; 4096];
        let mut head = Vec::new();
        loop {
            let n = stream.read(&mut request)?;
            head.extend_from_slice(&request[..n]);
            if head.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let text = String::from_utf8_lossy(&head).into_owned();
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

mod ceilings;

mod failures;

mod redirects;
