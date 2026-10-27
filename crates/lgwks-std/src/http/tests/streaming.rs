//! Streaming bodies, server-sent events, and cursor pagination, driven
//! through the public streaming API against loopback servers.

use super::*;
use std::io::Write;
use std::net::TcpListener;

/// Read one request head, then hand the socket to `write` and close.
fn serve_after_head(
    write: impl FnOnce(&mut std::net::TcpStream) -> std::io::Result<()> + Send + 'static,
) -> std::io::Result<(u16, thread::JoinHandle<std::io::Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let handle = fixture_thread(move || -> std::io::Result<()> {
        let (mut stream, _) = listener.accept()?;
        let head = read_request_head(&mut stream)?;
        if !head.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(());
        }
        write(&mut stream)
    })?;
    Ok((port, handle))
}

/// Read one request head, then write `reply` and close.
fn serve_once_and_close(
    reply: Vec<u8>,
) -> std::io::Result<(u16, thread::JoinHandle<std::io::Result<()>>)> {
    serve_after_head(move |stream| stream.write_all(&reply))
}

/// A chunked reply carrying `pieces` in order.
fn chunked_reply(pieces: &[&[u8]]) -> Vec<u8> {
    let mut reply = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for piece in pieces {
        reply.extend_from_slice(format!("{:X}\r\n", piece.len()).as_bytes());
        reply.extend_from_slice(piece);
        reply.extend_from_slice(b"\r\n");
    }
    reply.extend_from_slice(b"0\r\n\r\n");
    reply
}

/// Read a whole stream through `read_chunk` with a small window.
fn drain(stream: &mut StreamedBody) -> Result<(Vec<u8>, Option<Truncation>), Error> {
    let mut body = Vec::new();
    let mut window = [0_u8; 4];
    while let Some(read) = stream.read_chunk(&mut window)? {
        body.extend(window.iter().take(read));
    }
    Ok((body, stream.truncation()))
}

/// A chunked body arrives whole and in order through the typed path.
#[test]
fn chunked_body_arrives_in_order() -> Result<(), Box<dyn std::error::Error>> {
    let pieces: &[&[u8]] = &[b"hello ", b"stream", b"ed body"];
    let (port, server) = serve_once_and_close(chunked_reply(pieces))?;
    let client = Client::new(quiet())?;
    let mut response = client.get_stream(&format!("http://127.0.0.1:{port}/"))?;
    assert_eq!(response.status, 200, "the stream head answers");
    let (body, truncation) = drain(response.body())?;
    assert_eq!(body, b"hello streamed body", "every chunk arrives in order");
    assert_eq!(
        truncation,
        Some(Truncation::Complete),
        "an ended body is complete"
    );
    join_server(server)?;
    Ok(())
}

/// A streamed whole body past the ceiling is refused at the ceiling.
#[test]
fn streamed_whole_body_past_the_ceiling_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let (port, server) = serve_once_and_close(chunked_reply(&[b"0123456789abcdef"]))?;
    let options = ceiling(10);
    let client = Client::new(options)?;
    let mut response = client.get_stream(&format!("http://127.0.0.1:{port}/"))?;
    let refused = drain(response.body());
    assert!(
        matches!(refused, Err(Error::BodyTooLarge { limit: 10 })),
        "sixteen bytes past a ten-byte ceiling refuse with the ceiling, got {refused:?}"
    );
    join_server(server)?;
    Ok(())
}

/// A preview stream stops at the ceiling and reports the cut.
#[test]
fn preview_stream_stops_at_the_ceiling() -> Result<(), Box<dyn std::error::Error>> {
    let (port, server) = serve_once_and_close(chunked_reply(&[b"0123456789abcdef"]))?;
    let options = previewing(10);
    let client = Client::new(options)?;
    let mut response = client.get_stream(&format!("http://127.0.0.1:{port}/"))?;
    let (body, truncation) = drain(response.body())?;
    assert_eq!(body, b"0123456789", "a preview keeps the ceiling's worth");
    assert_eq!(truncation, Some(Truncation::Cut), "and reports the cut");
    join_server(server)?;
    Ok(())
}

/// A torn stream is a transport failure at the body stage, never a prefix.
#[test]
fn torn_stream_is_never_a_complete_body() -> Result<(), Box<dyn std::error::Error>> {
    let (port, server) = serve_after_head(|stream| {
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")?;
        stream.write_all(&[b'x'; 40])
    })?;
    let client = Client::new(quiet())?;
    let mut response = client.get_stream(&format!("http://127.0.0.1:{port}/"))?;
    let mut window = [0_u8; 64];
    let outcome: Result<(), Error> = (|| {
        loop {
            match response.body().read_chunk(&mut window)? {
                None => return Ok(()),
                Some(_) => continue,
            }
        }
    })();
    assert!(
        matches!(
            outcome,
            Err(Error::Failure {
                stage: FailureStage::Body,
                kind: FailureKind::Transport,
                ..
            })
        ),
        "forty of one hundred bytes is a torn transport, got {outcome:?}"
    );
    join_server(server)?;
    Ok(())
}

/// A scripted event stream: comments, ids, retry, unknown fields, CRLF, and
/// multi-line data, split into odd pieces on the wire.
#[test]
fn sse_dispatches_events_comments_ids_and_retry() -> Result<(), Box<dyn std::error::Error>> {
    let script = b": a comment\r\nevent: greeting\ndata: hello\ndata: world\nid: 1\nbogus: ignored\n\nevent: tick\r\ndata: {\"n\": 1}\r\nretry: 3000\r\n\r\n: trailing comment\ndata: lonely\n";
    let pieces = 7_usize;
    let (port, server) = serve_after_head(move |stream| {
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n",
            script.len()
        );
        stream.write_all(header.as_bytes())?;
        for piece in script.chunks(pieces) {
            stream.write_all(piece)?;
        }
        Ok(())
    })?;
    let client = Client::new(quiet())?;
    let mut events = client
        .get_stream(&format!("http://127.0.0.1:{port}/"))?
        .into_sse();
    let first = events.next_event()?.ok_or("the greeting must dispatch")?;
    assert_eq!(first.event(), "greeting", "the event type is kept");
    assert_eq!(
        first.data(),
        "hello\nworld",
        "data lines join with newlines"
    );
    assert_eq!(first.id(), Some("1"), "the id is carried");
    assert_eq!(first.retry(), None, "no retry was named yet");
    let second = events.next_event()?.ok_or("the tick must dispatch")?;
    assert_eq!(second.event(), "tick", "CRLF lines parse the same");
    assert_eq!(second.data(), "{\"n\": 1}", "payload bytes are exact");
    assert_eq!(second.id(), Some("1"), "the id persists across events");
    assert_eq!(second.retry(), Some(3000), "the retry is carried");
    let third = events
        .next_event()?
        .ok_or("the trailing event must dispatch")?;
    assert_eq!(
        third.event(),
        "message",
        "unnamed events default to message"
    );
    assert_eq!(third.data(), "lonely", "a comment never becomes data");
    assert!(
        events.next_event()?.is_none(),
        "a closed stream ends after its last event"
    );
    join_server(server)?;
    Ok(())
}

/// A stream that ends with a pending event dispatches it rather than losing it.
#[test]
fn sse_dispatches_a_pending_event_at_eof() -> Result<(), Box<dyn std::error::Error>> {
    let (port, server) = serve_once_and_close(
        b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\ndata: last\n".to_vec(),
    )?;
    let client = Client::new(quiet())?;
    let mut events = client
        .get_stream(&format!("http://127.0.0.1:{port}/"))?
        .into_sse();
    let only = events
        .next_event()?
        .ok_or("the pending event must dispatch")?;
    assert_eq!(
        only.data(),
        "last",
        "no trailing blank line is still an event"
    );
    assert!(
        events.next_event()?.is_none(),
        "the stream ends after the dispatched event"
    );
    join_server(server)?;
    Ok(())
}

/// A line longer than the ceiling is refused: a line is only deliverable whole.
#[test]
fn sse_refuses_a_line_past_the_ceiling() -> Result<(), Box<dyn std::error::Error>> {
    let reply = b"HTTP/1.1 200 OK\r\nContent-Length: 106\r\n\r\ndata: 012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789\n\n".to_vec();
    let (port, server) = serve_once_and_close(reply)?;
    let options = ceiling(16);
    let client = Client::new(options)?;
    let mut events = client
        .get_stream(&format!("http://127.0.0.1:{port}/"))?
        .into_sse();
    let refused = events.next_event();
    assert!(
        matches!(refused, Err(Error::BodyTooLarge { limit: 16 })),
        "a hundred-byte line past a sixteen-byte ceiling refuses, got {refused:?}"
    );
    join_server(server)?;
    Ok(())
}

/// A streamed redirect is followed before the body starts.
#[test]
fn streamed_redirect_lands_and_streams() -> Result<(), Box<dyn std::error::Error>> {
    let (landed_port, landed_server) =
        serve_once_and_close(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nstream".to_vec())?;
    let (origin_port, origin_server) = serve_once_and_close(
        format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{landed_port}/landed\r\nContent-Length: 0\r\n\r\n"
        )
        .into_bytes(),
    )?;
    let client = Client::new(quiet())?;
    let mut response = client.get_stream(&format!("http://127.0.0.1:{origin_port}/start"))?;
    assert_eq!(
        response.final_target(),
        format!("http://127.0.0.1:{landed_port}/landed"),
        "the stream follows the redirect first"
    );
    let (body, truncation) = drain(response.body())?;
    assert_eq!(body, b"stream", "the landed body streams");
    assert_eq!(truncation, Some(Truncation::Complete), "and ends whole");
    origin_server
        .join()
        .map_err(|_| "redirect origin panicked")??;
    landed_server
        .join()
        .map_err(|_| "redirect destination panicked")??;
    Ok(())
}

/// A fetch refusal the pagination tests carry: typed, so `?` keeps working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FetchRefusal(&'static str);

impl std::fmt::Display for FetchRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl std::error::Error for FetchRefusal {}

/// Pagination walks cursors to their end and collects every item in order.
#[test]
fn paginate_collects_pages_in_order() -> Result<(), Box<dyn std::error::Error>> {
    let pages = [
        Page::new(vec!["a", "b"], Some("second".to_owned())),
        Page::new(vec!["c"], Some("third".to_owned())),
        Page::new(vec!["d"], None),
    ];
    let seen: std::sync::Mutex<Vec<Option<String>>> = std::sync::Mutex::new(Vec::new());
    let collected = paginate(None, PaginationLimits::default(), |cursor| {
        match seen.lock() {
            Ok(mut seen) => seen.push(cursor.map(str::to_owned)),
            Err(_) => return Err(FetchRefusal("the cursor log is poisoned")),
        }
        let index = match cursor {
            None => 0,
            Some("second") => 1,
            Some("third") => 2,
            Some(_unexpected) => {
                return Err(FetchRefusal("unknown cursor"));
            }
        };
        Ok::<Page<&str>, FetchRefusal>(pages[index].clone())
    })?;
    assert_eq!(collected, vec!["a", "b", "c", "d"], "every item in order");
    let seen = match seen.lock() {
        Ok(seen) => seen.to_vec(),
        Err(_) => return Err("the cursor log is poisoned".into()),
    };
    assert_eq!(
        seen,
        vec![None, Some("second".to_owned()), Some("third".to_owned())],
        "cursors pass through byte-identical"
    );
    Ok(())
}

/// A walk that never runs out of pages stops at the page bound.
#[test]
fn paginate_stops_a_never_ending_walk() {
    let limits = PaginationLimits::default().max_pages(2);
    let refused = paginate::<&str, FetchRefusal>(None, limits, |cursor| {
        Ok(Page::new(
            vec!["x"],
            cursor.map(str::to_owned).or(Some("again".to_owned())),
        ))
    });
    assert!(
        matches!(
            refused,
            Err(Paginate::LimitExceeded {
                pages_fetched: 2,
                items_collected: 2
            })
        ),
        "two pages then the bound, got {refused:?}"
    );
}

/// A walk that overflows the item bound refuses the whole walk.
#[test]
fn paginate_refuses_an_overflowing_walk() {
    let limits = PaginationLimits::default().max_items(2);
    let refused = paginate::<&str, FetchRefusal>(None, limits, |cursor| {
        if cursor.is_some() {
            return Ok(Page::new(Vec::new(), None));
        }
        Ok(Page::new(vec!["a", "b", "c"], None))
    });
    assert!(
        matches!(
            refused,
            Err(Paginate::LimitExceeded {
                pages_fetched: 1,
                items_collected: 0
            })
        ),
        "three items past a two-item bound refuse whole, got {refused:?}"
    );
}

/// A fetch refusal is carried, never reinterpreted.
#[test]
fn paginate_carries_a_fetch_refusal() {
    let refused = paginate::<&str, FetchRefusal>(None, PaginationLimits::default(), |_| {
        Err(FetchRefusal("the server said no"))
    });
    assert!(
        matches!(refused, Err(Paginate::Fetch(ref refusal)) if *refusal == FetchRefusal("the server said no")),
        "the caller's refusal passes through, got {refused:?}"
    );
}

/// Zero pages refuses before the first fetch.
#[test]
fn paginate_with_zero_pages_fetches_nothing() {
    let fetches = std::sync::atomic::AtomicUsize::new(0);
    let limits = PaginationLimits::default().max_pages(0);
    let refused = paginate::<&str, FetchRefusal>(None, limits, |_| {
        fetches.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Page::new(vec!["x"], None))
    });
    assert!(
        matches!(
            refused,
            Err(Paginate::LimitExceeded {
                pages_fetched: 0,
                items_collected: 0
            })
        ),
        "zero pages refuses at once, got {refused:?}"
    );
    assert_eq!(
        fetches.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no fetch runs when no page is allowed"
    );
}
