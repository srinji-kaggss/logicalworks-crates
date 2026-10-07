//! Row 6 of issue #278 against the real public surface: a retrying proxy in
//! front of a keyed effect.
//!
//! The simulation (`sim_retry_proxy`) sweeps the delivery schedules; this
//! file stands a real duplicating proxy — a loopback TCP server that
//! forwards every request to the upstream three times — in front of the
//! shipped [`IdempotentPost`](lgwks_bot::idempotent::IdempotentPost)
//! [`Execute`](lgwks_bot::verb::Execute) adapter, against a real upstream
//! that deduplicates on the `Idempotency-Key` and counts the effects it
//! applies.
//!
//! The families:
//!
//! - two operations through the proxy apply exactly two upstream effects,
//!   however many times each was delivered;
//! - the client observes the upstream's answer, not the proxy's duplication;
//! - an upstream credential refusal on this path is the typed repair, not a
//!   generic failure.

use std::collections::HashMap;
use std::error::Error;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;
use std::time::Duration;

use lgwks_bot::cap::Cap;
use lgwks_bot::error::BotError;
use lgwks_bot::gate::GrantSet;
use lgwks_bot::idempotent::{IdempotentPost, PostInput};
use lgwks_bot::verb::Execute;

use crate::sweep_fixtures::refuse;

type TestResult = Result<(), Box<dyn Error>>;

/// How many times the proxy forwards each request it receives.
const DUPLICATES: usize = 3;

/// Operations the client sends through the proxy.
const OPERATIONS: usize = 2;

/// Milliseconds one socket read may wait: a bug fails loudly instead of
/// hanging the suite forever.
const READ_TIMEOUT_MS: u64 = 15_000;

/// One parsed HTTP request: its request line, its headers, and its body.
///
/// The alias the fixture signatures share, so the tuple's shape is stated
/// once rather than repeated at every site that reads or forwards one.
type RawRequest = (String, Vec<(String, String)>, Vec<u8>);

/// Read one HTTP request head (request line plus headers) and its body.
fn read_request(stream: &mut TcpStream) -> Result<RawRequest, String> {
    stream
        .set_read_timeout(Some(Duration::from_millis(READ_TIMEOUT_MS)))
        .map_err(|error| format!("setting the read timeout: {error}"))?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        let read = stream
            .read(&mut byte)
            .map_err(|error| format!("reading the request head: {error}"))?;
        if read == 0 {
            return Err("the peer closed the connection mid-request".to_owned());
        }
        head.extend_from_slice(&byte);
        if head.len() > 64 * 1024 {
            return Err("the request head exceeds 64 KiB".to_owned());
        }
    }
    let text = String::from_utf8(head).map_err(|_| "the request head is not UTF-8")?;
    let mut lines = text.lines();
    let request_line = lines.next().ok_or("an empty request")?.to_owned();
    // A 100-continue is answered before the body is read: the client waits
    // for it rather than sending into a refusal.
    let mut headers = Vec::new();
    let mut length = 0usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':').ok_or("a malformed header")?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            length = value
                .trim()
                .parse()
                .map_err(|_| "a non-numeric content length")?;
        }
        if name.trim().eq_ignore_ascii_case("expect")
            && value.trim().eq_ignore_ascii_case("100-continue")
        {
            stream
                .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .map_err(|error| format!("answering 100-continue: {error}"))?;
        }
        headers.push((name.trim().to_owned(), value.trim().to_owned()));
    }
    let mut body = vec![0u8; length];
    stream
        .read_exact(&mut body)
        .map_err(|error| format!("reading the request body: {error}"))?;
    Ok((request_line, headers, body))
}

/// Read one HTTP response to the peer's close.
fn read_response(stream: &mut TcpStream) -> Result<(u16, Vec<u8>), String> {
    let mut bytes = Vec::new();
    stream
        .read_to_end(&mut bytes)
        .map_err(|error| format!("reading the response: {error}"))?;
    let text = String::from_utf8_lossy(&bytes);
    let status: u16 = text
        .lines()
        .next()
        .ok_or("an empty response")?
        .split_whitespace()
        .nth(1)
        .ok_or("a status line without a status")?
        .parse()
        .map_err(|_| "a non-numeric status")?;
    Ok((status, bytes))
}

/// The request's idempotency key, when it carries one.
fn idempotency_key(headers: &[(String, String)]) -> Option<String> {
    headers
        .iter()
        .find(|pair| pair.0.eq_ignore_ascii_case("Idempotency-Key"))
        .map(|pair| pair.1.clone())
}

/// Serve exactly `connections` upstream deliveries, deduplicating on the
/// idempotency key, then hand back the applied effects per key.
fn serve_upstream(
    listener: TcpListener,
    connections: usize,
) -> std::io::Result<JoinHandle<Result<HashMap<String, usize>, String>>> {
    std::thread::Builder::new()
        .name("retry-proxy-upstream".to_owned())
        .spawn(move || {
            let mut effects: HashMap<String, usize> = HashMap::new();
            for _ in 0..connections {
                let (mut stream, _) = listener
                    .accept()
                    .map_err(|error| format!("upstream accept: {error}"))?;
                let (_, headers, _) = read_request(&mut stream)?;
                let key = idempotency_key(&headers).ok_or("an effect without a key")?;
                // First sight applies; a resight replays the stored answer
                // without applying again.
                let applied = effects.len().saturating_add(1);
                let count = *effects.entry(key).or_insert(applied);
                let body = format!("effect-{count}");
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(reply.as_bytes())
                    .map_err(|error| format!("upstream reply: {error}"))?;
            }
            Ok(effects)
        })
}

/// Serve exactly `connections` proxy receptions, forwarding each to
/// `upstream_port` [`DUPLICATES`] times and returning the first answer.
fn serve_proxy(
    listener: TcpListener,
    upstream_port: u16,
    connections: usize,
) -> std::io::Result<JoinHandle<Result<(), String>>> {
    std::thread::Builder::new()
        .name("retry-proxy-duplicator".to_owned())
        .spawn(move || {
            for _ in 0..connections {
                let (mut client, _) = listener
                    .accept()
                    .map_err(|error| format!("proxy accept: {error}"))?;
                let (request_line, headers, body) = read_request(&mut client)?;
                let mut first: Option<Vec<u8>> = None;
                for _ in 0..DUPLICATES {
                    let mut upstream = TcpStream::connect(format!("127.0.0.1:{upstream_port}"))
                        .map_err(|error| format!("proxy dial: {error}"))?;
                    let mut forwarded = format!(
                        "{request_line}\r\nHost: 127.0.0.1:{upstream_port}\r\nConnection: close\r\n"
                    );
                    for pair in &headers {
                        let (name, value) = (&pair.0, &pair.1);
                        if name.eq_ignore_ascii_case("host")
                            || name.eq_ignore_ascii_case("connection")
                            || name.eq_ignore_ascii_case("expect")
                        {
                            continue;
                        }
                        forwarded.push_str(name);
                        forwarded.push_str(": ");
                        forwarded.push_str(value);
                        forwarded.push_str("\r\n");
                    }
                    forwarded.push_str("\r\n");
                    upstream
                        .write_all(forwarded.as_bytes())
                        .map_err(|error| format!("proxy forward head: {error}"))?;
                    upstream
                        .write_all(&body)
                        .map_err(|error| format!("proxy forward body: {error}"))?;
                    let (_, bytes) = read_response(&mut upstream)?;
                    if first.is_none() {
                        first = Some(bytes);
                    }
                }
                match first {
                    Some(answer) => client
                        .write_all(&answer)
                        .map_err(|error| format!("proxy answer: {error}"))?,
                    None => return Err("the proxy forwarded nothing".to_owned()),
                }
            }
            Ok(())
        })
}

/// Join a fixture thread: a panic is a test failure naming the thread, and a
/// `String` refusal becomes the test's error without rewording it.
fn join<T>(handle: JoinHandle<Result<T, String>>, name: &str) -> Result<T, Box<dyn Error>> {
    match handle.join() {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(refusal)) => refuse(refusal),
        Err(_) => refuse(format!("the {name} thread panicked")),
    }
}

/// Two keyed operations through a proxy that delivers everything three
/// times: the upstream applies exactly two effects.
#[test]
fn a_proxy_that_duplicates_everything_applies_each_key_once() -> TestResult {
    let upstream_listener = TcpListener::bind("127.0.0.1:0")?;
    let upstream_port = upstream_listener.local_addr()?.port();
    let proxy_listener = TcpListener::bind("127.0.0.1:0")?;
    let proxy_port = proxy_listener.local_addr()?.port();
    // Every client delivery is forwarded DUPLICATES times: the upstream
    // must see OPERATIONS * DUPLICATES deliveries and apply OPERATIONS
    // effects.
    let upstream = serve_upstream(upstream_listener, OPERATIONS.saturating_mul(DUPLICATES))?;
    let proxy = serve_proxy(proxy_listener, upstream_port, OPERATIONS)?;
    let auth: lgwks_bot::cap::Auth = GrantSet::empty().grant(Cap::net()).issue(&[Cap::net()])?;
    let adapter = IdempotentPost::new();
    for operation in 0..OPERATIONS {
        let input = PostInput::new(
            format!("http://127.0.0.1:{proxy_port}/effect"),
            format!("op-{operation}"),
            "application/json",
            format!(r#"{{"charge":{operation}}}"#).into_bytes(),
        )?;
        let output = lgwks_std::task::block_on(adapter.execute_action((auth.clone(), &input)))?;
        assert_eq!(
            output.status(),
            200,
            "the client observes the upstream's answer"
        );
        assert!(
            output.preview().contains("effect-"),
            "the preview carries the effect's receipt, got {:?}",
            output.preview()
        );
    }
    join(proxy, "proxy")?;
    let effects = join(upstream, "upstream")?;
    assert_eq!(
        effects.len(),
        OPERATIONS,
        "two keys are two effects, however often each was delivered"
    );
    for operation in 0..OPERATIONS {
        let key = format!("op-{operation}");
        if !effects.contains_key(&key) {
            return refuse(format!("the upstream never applied {key}"));
        }
    }
    Ok(())
}

/// An upstream credential refusal on the proxy path is the typed repair, not
/// a generic failure: 401 carries its `NeedSet` rather than reading as a
/// transport fault.
#[test]
fn an_upstream_401_on_the_proxy_path_is_a_typed_repair() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let server = std::thread::Builder::new()
        .name("retry-proxy-401".to_owned())
        .spawn(move || {
            let (mut stream, _) = listener.accept().map_err(|error| error.to_string())?;
            drop(read_request(&mut stream));
            stream
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        })?;
    let auth: lgwks_bot::cap::Auth = GrantSet::empty().grant(Cap::net()).issue(&[Cap::net()])?;
    let adapter = IdempotentPost::new();
    let input = PostInput::new(
        format!("http://127.0.0.1:{port}/effect"),
        "op-repair",
        "application/json",
        b"{}".to_vec(),
    )?;
    let Err(BotError::CredentialRejected { status, needs, .. }) =
        lgwks_std::task::block_on(adapter.execute_action((auth, &input)))
    else {
        return refuse("an upstream 401 must be a typed credential refusal");
    };
    assert_eq!(status, 401, "the refusal names the upstream status");
    assert!(!needs.is_empty(), "the refusal carries what to re-grant");
    join(server, "401 fixture")?;
    Ok(())
}
