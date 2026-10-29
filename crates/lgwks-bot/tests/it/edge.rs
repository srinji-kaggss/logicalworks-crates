//! The HTTP edge driven over a real socket (ADR 0006 R4).
//!
//! Source: the R4 falsifier, "two tenants never read each other's state", and
//! RFC 9110 §15 for the statuses the edge answers itself. The test binds a
//! loopback port, serves a world through `lgwks_bot::rt::edge::serve`, speaks
//! HTTP/1.1 to it as a client would, and stops it.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};

use lgwks_bot::rt::edge::{EdgeStop, MAX_BODY_BYTES, serve};
use lgwks_bot::spec::EffectScope;
use lgwks_bot::{Bot, GrantSet, Request, RequestFeed, Response};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Send one request on a fresh connection and read the whole answer; its
/// status and body.
fn call(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<(u16, String), Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(addr)?;
    let head = format!(
        "{method} {path} HTTP/1.1\r\nhost: {addr}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer)?;
    let status = answer
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or("no status line")?;
    let body = answer
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .ok_or("no body")?;
    Ok((status, body))
}

/// A tenant's notes: `POST /notes` appends the body as a line, `GET /notes`
/// reads every line the tenant wrote.
fn notes(request: &Request<'_>, lines: &mut String) -> Response {
    if request.path() != "/notes" {
        return Response::new(404, Vec::new());
    }
    if request.method() == "POST" {
        lines.push_str(&String::from_utf8_lossy(request.body()));
        lines.push('\n');
    }
    Response::new(200, lines.clone().into_bytes())
}

#[test]
fn two_tenants_over_http_never_read_each_others_state() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let mut world = Bot::builder("edge-test")
        .with_effects(EffectScope::ephemeral()?)
        .build(&GrantSet::empty())?;
    let feed = RequestFeed::new(notes);
    let stop = EdgeStop::new();
    // The world stays on the thread that built it; the client runs beside it.
    std::thread::scope(|scope| -> TestResult {
        let client = scope.spawn(|| {
            let journey = (|| -> Result<(), Box<dyn std::error::Error>> {
                assert_eq!(
                    call(addr, "POST", "/t/acme/notes", b"acme secret")?.1,
                    "acme secret\n"
                );
                assert_eq!(
                    call(addr, "GET", "/t/globex/notes", b"")?,
                    (200, String::new()),
                    "globex reads its own notes, not acme's"
                );
                assert_eq!(
                    call(addr, "POST", "/t/globex/notes", b"globex")?.1,
                    "globex\n"
                );
                assert_eq!(call(addr, "GET", "/t/acme/notes", b"")?.1, "acme secret\n");
                assert_eq!(
                    call(addr, "GET", "/t/acme/other", b"")?.0,
                    404,
                    "the world's own 404"
                );
                assert_eq!(call(addr, "GET", "/notes", b"")?.0, 400, "no tenant named");
                assert_eq!(
                    call(addr, "GET", "/t/a.b/notes", b"")?.0,
                    400,
                    "a tenant no name can be"
                );
                let over = vec![b'x'; MAX_BODY_BYTES.saturating_add(1)];
                assert_eq!(
                    call(addr, "POST", "/t/acme/notes", &over)?.0,
                    413,
                    "over the body ceiling"
                );
                Ok(())
            })();
            stop.stop();
            journey.map_err(|error| error.to_string())
        });
        serve(&mut world, &feed, listener, &stop)?;
        client.join().map_err(|_| "the client thread panicked")??;
        Ok(())
    })?;
    let counts = feed.counts();
    assert_eq!(
        (counts.settled, counts.refused, counts.tenants),
        (5, 0, 2),
        "five requests reached the handler; the edge answered the rest itself"
    );
    assert!(
        TcpStream::connect(addr).is_err(),
        "the port is released after serve returns"
    );
    Ok(())
}
