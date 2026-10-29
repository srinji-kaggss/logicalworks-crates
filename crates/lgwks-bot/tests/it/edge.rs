//! The HTTP edge driven over a real socket (ADR 0006 R4).
//!
//! Source: the R4 falsifier, "two tenants never read each other's state", and
//! RFC 9110 §15 for the statuses the edge answers itself. The test binds a
//! loopback port, serves a world through `lgwks_bot::rt::edge::serve`, speaks
//! HTTP/1.1 to it as a client would, and stops it.

use std::future::{Future, ready};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lgwks_bot::rt::edge::{EdgeStop, MAX_BODY_BYTES, serve};
use lgwks_bot::spec::EffectScope;
use lgwks_bot::{
    Auth, Bot, BotError, Cadence, Cap, GrantSet, Observe, Request, RequestFeed, Response,
    WakeHandle,
};

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

/// A tenant's notes: `POST /notes` appends the body as a line, counts it in
/// `written` and wakes the world's `Written` source; `GET /notes` reads every
/// line the tenant wrote.
fn notes(
    request: &Request<'_>,
    lines: &mut String,
    written: &AtomicU64,
    wake: &WakeHandle,
) -> Response {
    if request.path() != "/notes" {
        return Response::new(404, Vec::new());
    }
    if request.method() == "POST" {
        lines.push_str(&String::from_utf8_lossy(request.body()));
        lines.push('\n');
        written.fetch_add(1, Ordering::AcqRel);
        wake.wake();
    }
    Response::new(200, lines.clone().into_bytes())
}

/// The world's view of every tenant's notes: a `Wake` source the handler
/// wakes, so the world reads it only when a note was written.
struct Written {
    /// Notes written across tenants, as the handler counts them.
    total: Arc<AtomicU64>,
    /// The total this source read on its latest poll.
    seen: Arc<AtomicU64>,
    /// How many times the world polled it.
    polls: Arc<AtomicU64>,
}

impl Observe for Written {
    type Output = u64;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    fn poll(&self, _call: (Auth, ())) -> impl Future<Output = Result<u64, BotError>> {
        let total = self.total.load(Ordering::Acquire);
        self.seen.store(total, Ordering::Release);
        self.polls.fetch_add(1, Ordering::AcqRel);
        ready(Ok(total))
    }

    fn cadence(&self) -> Cadence {
        Cadence::Wake
    }

    fn domain_id(&self) -> &str {
        "edge-test::written"
    }
}

#[test]
fn two_tenants_over_http_never_read_each_others_state() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    let (total, seen, polls) = (Arc::default(), Arc::default(), Arc::default());
    let mut world = Bot::builder("edge-test")
        .with_effects(EffectScope::ephemeral()?)
        .observe(Written {
            total: Arc::clone(&total),
            seen: Arc::clone(&seen),
            polls: Arc::clone(&polls),
        })
        .build(&GrantSet::empty())?;
    let wake = WakeHandle::for_source(&world, 0).ok_or("a Wake source has a handle")?;
    let written = Arc::clone(&total);
    let feed =
        RequestFeed::new(move |request, lines: &mut String| notes(request, lines, &written, &wake));
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
    // The world was driven while it served: woken by the two notes, it read
    // both, and it never polled its source without a wake (ADR 0006 R5).
    assert_eq!(seen.load(Ordering::Acquire), 2, "the world read both notes");
    let polled = polls.load(Ordering::Acquire);
    assert!(
        (2..=3).contains(&polled),
        "a baseline poll plus one per wake at most, coalesced: {polled}"
    );
    assert!(
        TcpStream::connect(addr).is_err(),
        "the port is released after serve returns"
    );
    Ok(())
}
