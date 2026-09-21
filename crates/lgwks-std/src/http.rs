//! `http` owns minimal blocking HTTP exchange for tooling and probes.
//! Built on ureq with rustls-only TLS; URLs are validated as absolute
//! http(s) URIs before any socket opens.
//!
//! The API is blocking. From async code, run it on a blocking thread.
//! HTTP error statuses (4xx/5xx) are returned as [`Response`](crate::http::Response),
//! never as [`Error`](crate::http::Error): only transport failure, timeout,
//! and invalid URLs error.
//!
//! Each call builds and drops its own connection agent: there is no
//! connection pooling. Callers that issue bursts should reuse a
//! [`crate::retry::RetryPolicy`] budget and keep concurrency bounded
//! (`lgwks_bot::rt::task::join_all_bounded`), not open unbounded parallel
//! requests. Retries, backoff, and circuit-breaking are caller policy, not
//! client behavior: the client makes exactly one attempt per call.
//!
//! Response bodies are read under a ceiling the caller declares
//! ([`Options::max_body_bytes`], 8 MiB by default). A body that reaches the
//! ceiling is refused with [`Error::BodyTooLarge`] rather than truncated
//! silently, so a caller that asked for a body never receives a prefix
//! believing it is the whole thing. A caller whose use of the body is a
//! preview declares [`BodyPolicy::Preview`] instead, which keeps the prefix and
//! reports [`Truncation::Cut`]. There is no unbounded spelling: a remote server
//! does not get to decide how much memory this process commits.
//!
//! [`Options::max_body_bytes`]: crate::http::Options::max_body_bytes
//! [`Error::BodyTooLarge`]: crate::http::Error::BodyTooLarge
//! [`BodyPolicy::Preview`]: crate::http::BodyPolicy::Preview
//! [`Truncation::Cut`]: crate::http::Truncation::Cut

use std::fmt;
use std::io::Read;
use std::time::Duration;

use iri_string::types::UriAbsoluteStr;

// ── Body ceilings ───────────────────────────────────────────────────────────

/// Default ceiling on the bytes one response body may occupy: 8 MiB.
///
/// Finite on purpose. A remote server authors its own response, so an unbounded
/// read lets it author this process's memory footprint as well; there is no
/// spelling for "no ceiling" anywhere in this module. The default is generous
/// for a client built for tooling and probes, and a caller that legitimately
/// needs more raises it through [`Options::max_body_bytes`] — an explicit
/// decision, made once, at the call site that needs it.
pub const DEFAULT_MAX_BODY_BYTES: usize = 8_388_608;

/// Bytes read per `read` call in [`read_bounded`].
///
/// Bounds each read to a stack buffer rather than letting the reader size its
/// own windows: the chunk handed to a socket is whatever this constant says,
/// independent of the transport's buffering.
const READ_CHUNK_BYTES: usize = 8_192;

/// What an exchange does when a response body reaches its declared ceiling.
///
/// Declared with the ceiling rather than decided after the fact: a caller that
/// wants a whole body and a caller that wants a preview disagree about what a
/// cut-off body means, and only the caller knows which it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum BodyPolicy {
    /// Read the body whole, and refuse the exchange with
    /// [`Error::BodyTooLarge`] at the ceiling. The default: a truncated body is
    /// indistinguishable from a short one at the point of use, so a caller that
    /// was handed a prefix it believed was whole would parse half a document
    /// and call it success.
    #[default]
    Whole,
    /// Keep the first [`Options::max_body_bytes`] and mark the response
    /// [`Truncation::Cut`]. For a caller whose use of the body is a preview,
    /// where stopping at the ceiling is the declared outcome rather than a
    /// failure — and where refusing would throw away the status code, headers,
    /// and prefix the caller can actually use.
    Preview,
}

/// Whether a response body ended before the exchange's declared ceiling.
///
/// Reported rather than left implicit: under [`BodyPolicy::Preview`] the body
/// is a prefix by contract, and a consumer that must know whether it holds
/// everything has to be able to read that off the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Truncation {
    /// The body ended on its own, at or below the ceiling.
    Complete,
    /// The body reached the ceiling and reading stopped there:
    /// [`Response::body`] holds a prefix.
    Cut,
}

// ── Options ─────────────────────────────────────────────────────────────────

/// Request options. Start from [`Options::default`](crate::http::Options::default)
/// (30s timeout, 8 MiB body ceiling).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Options {
    /// Total request timeout, covering connect, TLS, send, and receive.
    pub timeout: Duration,
    /// `User-Agent` header sent with every request.
    pub user_agent: String,
    /// Extra headers sent with every request as `(name, value)` pairs, e.g.
    /// `("Authorization", "Bearer ...")`. Names and values must be valid
    /// header bytes; an invalid pair is a caller bug and the request errors
    /// rather than silently dropping the header.
    pub headers: Vec<(String, String)>,
    /// Ceiling on the response body in bytes, enforced while reading. See
    /// [`DEFAULT_MAX_BODY_BYTES`] for the default and the reason there is no
    /// unbounded value, and [`BodyPolicy`] for what happens at the ceiling.
    pub max_body_bytes: usize,
    /// What to do when the body reaches [`Options::max_body_bytes`].
    pub body_policy: BodyPolicy,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            user_agent: format!("lgwks-std/{}", env!("CARGO_PKG_VERSION")),
            headers: Vec::new(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            body_policy: BodyPolicy::Whole,
        }
    }
}

impl Options {
    /// Set the total request timeout, covering connect, TLS, send, and receive.
    ///
    /// [`Options`] is `#[non_exhaustive]`, so a caller outside this crate
    /// cannot construct it with a struct expression. Start from
    /// [`Options::default`] and set the field through this method.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set the ceiling on the response body in bytes.
    ///
    /// A body that reaches the ceiling is refused with [`Error::BodyTooLarge`],
    /// or kept as a prefix under [`BodyPolicy::Preview`]. Either way the read
    /// stops there: the ceiling bounds the memory a response can commit, so
    /// raising it is raising the amount of memory one remote server may claim.
    #[must_use]
    pub fn max_body_bytes(mut self, max_body_bytes: usize) -> Self {
        self.max_body_bytes = max_body_bytes;
        self
    }

    /// Set what happens when the body reaches [`Options::max_body_bytes`].
    #[must_use]
    pub fn body_policy(mut self, body_policy: BodyPolicy) -> Self {
        self.body_policy = body_policy;
        self
    }

    /// Attach an idempotency key (`Idempotency-Key` header) so a retried
    /// request is deduplicated by the receiver. Pair with
    /// [`crate::retry::RetryPolicy`] and a caller-generated key
    /// (`crate::id::Uuid::new_v4` under feature `random`); the client never
    /// invents the key, because a regenerated key on retry would defeat the
    /// deduplication the header exists for.
    #[must_use]
    pub fn idempotency_key(mut self, key: &str) -> Self {
        self.headers
            .push(("Idempotency-Key".to_owned(), key.to_owned()));
        self
    }
}

// ── Response ────────────────────────────────────────────────────────────────

/// A completed HTTP exchange: status, headers, and body.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Response {
    /// HTTP status code, including 4xx/5xx.
    pub status: u16,
    /// Response headers in wire order as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Response body bytes, at most [`Options::max_body_bytes`] long.
    pub body: Vec<u8>,
    /// Whether [`Response::body`] is the whole body or a prefix of it.
    pub truncation: Truncation,
}

impl Response {
    /// The body as UTF-8, or [`crate::http::Error::Transport`]
    /// when it is not valid UTF-8.
    pub fn text(&self) -> Result<&str, Error> {
        std::str::from_utf8(&self.body).map_err(|utf8_error| {
            Error::Transport(format!("response body is not valid UTF-8: {utf8_error}"))
        })
    }

    /// The body as UTF-8, replacing every invalid sequence with `U+FFFD`.
    ///
    /// Use this where the body is a preview rather than a payload: a caller
    /// that only wants a diagnostic excerpt should not have to invent a
    /// fallback for a body it is about to truncate anyway. For a strict read
    /// that reports non-UTF-8 as a transport failure, use [`Response::text`].
    #[must_use]
    pub fn text_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }
}

// ── Error ───────────────────────────────────────────────────────────────────

/// What `http` refuses to hide: bad URLs, timeouts, transport failure.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The URL is not an absolute http(s) URI. The offending URL is not
    /// carried: it can contain credentials in its userinfo or a token in its
    /// query string, and the caller already holds it.
    InvalidUrl,
    /// The request hit [`Options::timeout`](crate::http::Options::timeout).
    Timeout,
    /// The response body reached [`Options::max_body_bytes`] without ending,
    /// under [`BodyPolicy::Whole`]. The ceiling is reported, the bytes read are
    /// not: they are a prefix, and handing a prefix to a caller that asked for
    /// a body is the whole failure this variant exists to refuse.
    BodyTooLarge {
        /// The declared ceiling the body reached.
        limit: usize,
    },
    /// The exchange never completed: DNS, TCP, TLS, or protocol failure.
    Transport(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Matched through `*self` so every arm's pattern is the enum's own
        // type; `Transport` binds its payload by reference, since `Error` is
        // not `Copy` and formatting must not consume it.
        match *self {
            Self::InvalidUrl => {
                write!(f, "invalid http(s) URL (absolute http(s) URI required)")
            }
            Self::Timeout => write!(f, "request timed out"),
            Self::BodyTooLarge { limit } => write!(
                f,
                "response body reached the declared {limit}-byte ceiling; raise \
                 `max_body_bytes` to read it whole, or select `BodyPolicy::Preview` to keep a \
                 prefix"
            ),
            Self::Transport(ref cause) => write!(f, "transport failure: {cause}"),
        }
    }
}

impl std::error::Error for Error {}

// ── Exchange ────────────────────────────────────────────────────────────────

/// Reject anything that is not an absolute http(s) URI before dialing.
///
/// Four shapes are refused, and all four return the same [`Error::InvalidUrl`]:
/// a string that is not an absolute URI at all, a missing or non-http(s)
/// scheme, a URL with no authority, and a URL whose host is empty. The
/// rejection is silent (nothing is written to stderr), and the diagnostic
/// names the failure class, never the raw URL: a caller-supplied URL can carry
/// credentials or tokens in its userinfo or its query string. The caller already
/// holds the URL it passed in, so the class is the whole of what it does not
/// have.
///
/// A URL that fails here never reaches a socket.
pub fn validate_url(url: &str) -> Result<(), Error> {
    UriAbsoluteStr::new(url).map_err(|_malformed| Error::InvalidUrl)?;
    let Some(scheme) = url.split_once(':').map(|(scheme, _)| scheme) else {
        return Err(Error::InvalidUrl);
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(Error::InvalidUrl);
    }
    // An absolute URI may be authority-less (`http:user:SECRET@host` parses),
    // but it is not a valid request target. ureq refuses such a URL with a
    // message that embeds the raw text, so reject it here, class-only, rather
    // than let a lower layer echo it.
    //
    // `scheme` is a slice of `url`, so its length is already bounded by
    // `url.len()`; the offset past the `:` therefore cannot overflow, and
    // `checked_add` states that bound rather than relying on it. `get` rather
    // than an index: the offset is `url`'s only ASCII byte by construction, but
    // a `Some` here proves the bound instead of asserting it.
    let Some(rest) = scheme
        .len()
        .checked_add(1)
        .and_then(|after| url.get(after..))
    else {
        return Err(Error::InvalidUrl);
    };
    let Some(authority) = rest.strip_prefix("//") else {
        return Err(Error::InvalidUrl);
    };
    let host_end = authority.find(['/', '?', '#']).unwrap_or(authority.len());
    if authority[..host_end].is_empty() {
        return Err(Error::InvalidUrl);
    }
    Ok(())
}

/// Build the one-shot agent for `options`.
///
/// A fresh agent per call is the documented cost model: no connection pooling,
/// no shared state between requests. `http_status_as_error(false)` is what makes
/// a 4xx/5xx a [`Response`] rather than an [`Error`]; the timeout is applied
/// globally, so it covers connect, TLS, send, and receive.
fn agent(options: &Options) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(options.timeout))
        .http_status_as_error(false)
        .user_agent(&options.user_agent)
        .build();
    ureq::Agent::new_with_config(config)
}

/// Read a completed ureq response into the owned [`Response`].
///
/// Header values are decoded lossily: a non-UTF-8 header must not fail the
/// whole exchange, and the replacement character keeps the wire bytes
/// distinguishable from a header that was genuinely invalid UTF-8. The body, by
/// contrast, is read strictly: only the I/O read can fail here, and a partial
/// read is reported as [`Error::Transport`] rather than returned as a short
/// body. Non-UTF-8 *bodies* are not an error at this layer: [`Response::text`]
/// is where that is decided.
///
/// The body is read under `options.max_body_bytes`; see [`read_bounded`] for
/// how the ceiling is enforced on the way in rather than checked on the way
/// out.
fn response_of(
    mut response: ureq::http::Response<ureq::Body>,
    options: &Options,
) -> Result<Response, Error> {
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.to_string(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    let (body, truncation) = read_bounded(&mut response.body_mut().as_reader(), options)?;
    Ok(Response {
        status,
        headers,
        body,
        truncation,
    })
}

/// Read at most `options.max_body_bytes` from `reader`.
///
/// The ceiling is enforced *while* reading, not checked after: each `read` is
/// handed a window clamped to the bytes that remain, so the buffer never holds
/// more than the ceiling even momentarily, and a server that declares one
/// length and sends another cannot make this process allocate the difference.
/// The read never asks for more than [`READ_CHUNK_BYTES`], so a single
/// oversized frame is bounded too.
///
/// A body that fills the ceiling is not assumed to have ended there. One
/// further byte is read to tell a body that stopped exactly at the ceiling from
/// one that continues, and that byte is dropped rather than kept: it lies past
/// the ceiling, which is the one thing the ceiling exists to prevent. Keeping
/// it is also unnecessary, because the two cases are already distinguished by
/// the [`Truncation`] the response reports.
///
/// Under [`BodyPolicy::Whole`] a body that reached the ceiling is refused
/// outright: a prefix returned as a body is a short read the caller cannot see,
/// and the caller asked for a body. Under [`BodyPolicy::Preview`] the prefix is
/// the declared result.
fn read_bounded(reader: &mut impl Read, options: &Options) -> Result<(Vec<u8>, Truncation), Error> {
    let ceiling = options.max_body_bytes;
    let mut body = Vec::new();
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    let truncation = loop {
        let remaining = ceiling.saturating_sub(body.len());
        if remaining == 0 {
            break probe_for_more(reader)?;
        }
        // `wanted` is clamped to the chunk length, so this slice is in bounds
        // for every ceiling: the window is what remains, or one chunk,
        // whichever is smaller.
        let wanted = remaining.min(READ_CHUNK_BYTES);
        let read = reader
            .read(&mut chunk[..wanted])
            .map_err(|read_error| Error::Transport(read_error.to_string()))?;
        if read == 0 {
            break Truncation::Complete;
        }
        body.extend_from_slice(&chunk[..read]);
    };
    if options.body_policy == BodyPolicy::Whole && truncation == Truncation::Cut {
        return Err(Error::BodyTooLarge { limit: ceiling });
    }
    Ok((body, truncation))
}

/// Whether `reader` has more to give, without keeping the byte.
///
/// Reading one byte is what makes [`Truncation::Cut`] a fact rather than a
/// guess for a body that ends exactly at the ceiling; discarding it is what
/// keeps the ceiling a bound on memory. A byte that is read and dropped is
/// still a byte consumed from the socket, which is correct here: the exchange
/// ends at this point, and the transport is about to stop reading the body
/// either way.
fn probe_for_more(reader: &mut impl Read) -> Result<Truncation, Error> {
    let mut scratch = [0_u8; 1];
    let read = reader
        .read(&mut scratch)
        .map_err(|read_error| Error::Transport(read_error.to_string()))?;
    Ok(if read == 0 {
        Truncation::Complete
    } else {
        Truncation::Cut
    })
}

/// Fold a ureq failure into this crate's [`Error`].
///
/// `Timeout` is preserved as its own variant because it is the one transport
/// failure a caller can act on by widening [`Options::timeout`]. `BadUri` is
/// collapsed to [`Error::InvalidUrl`] for the same reason `validate_url` is
/// class-only: ureq's message embeds the raw URI, so the string is dropped
/// rather than carried into a caller's logs. Everything else keeps the
/// underlying detail, which names DNS, TCP, TLS, or protocol failure.
fn map_error(error: ureq::Error) -> Error {
    match error {
        ureq::Error::Timeout(_) => Error::Timeout,
        // ureq's `BadUri` message embeds the raw URI. `validate_url` rejects the
        // shapes that reach it, but collapse it to the class-only variant
        // anyway: a caller that logs the returned error must not receive the
        // userinfo or query string back.
        ureq::Error::BadUri(_) => Error::InvalidUrl,
        other => Error::Transport(other.to_string()),
    }
}

/// GET `url` with default options.
pub fn get_response(url: &str) -> Result<Response, Error> {
    get_with(url, &Options::default())
}

/// GET `url` with `options`.
pub fn get_with(url: &str, options: &Options) -> Result<Response, Error> {
    validate_url(url)?;
    let mut call = agent(options).get(url);
    // Bound whole rather than destructured: the elements are `&String` behind
    // the tuple, so a `(name, value)` pattern would not match the scrutinee
    // type, and spelling the borrow out (`&(ref name, ref value)`) is the
    // redundant-reference form clippy rejects. The fields stay as written.
    for header in &options.headers {
        call = call.header(header.0.as_str(), header.1.as_str());
    }
    call.call()
        .map_err(map_error)
        .and_then(|response| response_of(response, options))
}

/// POST `body` to `url` with `content_type`, using default options.
pub fn post(url: &str, content_type: &str, body: &[u8]) -> Result<Response, Error> {
    post_with(url, content_type, body, &Options::default())
}

/// POST `body` to `url` with `content_type` and `options`.
pub fn post_with(
    url: &str,
    content_type: &str,
    body: &[u8],
    options: &Options,
) -> Result<Response, Error> {
    validate_url(url)?;
    // Nothing about this request is written anywhere, and deliberately so: the
    // URL can carry credentials in its userinfo or a token in its query string,
    // and a library that prints on a request the caller itself authored tells
    // the caller nothing it does not already hold: it has the URL, the content
    // type, and the body length. A caller that wants a request trace owns that
    // decision, and owns redacting the URL when it does.
    let mut request = agent(options)
        .post(url)
        .header("Content-Type", content_type);
    for header in &options.headers {
        request = request.header(header.0.as_str(), header.1.as_str());
    }
    let response = request.send(body).map_err(map_error)?;
    response_of(response, options)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
// Same exception as `task.rs`, for the same reason: these tests stand up a real
// loopback server on a real thread and sleep to hold it open past the client's
// read timeout. The ban targets production code that blocks or leaks a thread;
// here the thread *is* the fixture, and this crate provides no surface that
// serves a socket from a test.
#[expect(
    clippy::disallowed_methods,
    reason = "loopback test servers need a real thread, and holding one open past a read timeout needs a real sleep"
)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    const ECHO: &str = "echo-body-123";

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
        let handle = thread::spawn(move || -> std::io::Result<()> {
            for (status, body) in replies {
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
                let header_end = head
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    // `position` reports the delimiter's first byte, so the
                    // header end is four past it; in a 4096-byte buffer that
                    // cannot approach `usize::MAX`.
                    .map(|position| position.saturating_add(4))
                    .unwrap_or(head.len());
                let text = String::from_utf8_lossy(&head[..header_end]);
                let content_length = text
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|entry| entry.0.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                // `header_end` is either the delimiter's end or the whole
                // buffer, so it never exceeds `head.len()`.
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
                stream.write_all(reply.as_bytes())?;
            }
            Ok(())
        });
        Ok((port, handle))
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
    fn serve_raw(
        reply: Vec<u8>,
    ) -> std::io::Result<(u16, thread::JoinHandle<std::io::Result<()>>)> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let handle = thread::spawn(move || -> std::io::Result<()> {
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
        });
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
        let got = get_response(&url)?;
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
            user_agent: "lgwks-std-test".into(),
            headers: Vec::new(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            body_policy: BodyPolicy::Whole,
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

    #[test]
    fn rejects_non_http_urls_before_dialing() {
        assert!(matches!(
            get_with("not a url", &quiet()),
            Err(Error::InvalidUrl)
        ));
        assert!(matches!(
            get_with("/relative/path", &quiet()),
            Err(Error::InvalidUrl)
        ));
        assert!(matches!(
            get_with("ftp://127.0.0.1/file", &quiet()),
            Err(Error::InvalidUrl)
        ));
        // Authority-less absolute URIs pass the scheme check but are not valid
        // request targets; ureq refuses them with a message that embeds the
        // raw text, so they must be rejected here instead.
        assert!(matches!(
            get_with("http:user:SECRET@host", &quiet()),
            Err(Error::InvalidUrl)
        ));
        assert!(matches!(
            get_with("https://", &quiet()),
            Err(Error::InvalidUrl)
        ));
    }

    #[test]
    fn refused_connection_is_transport_not_timeout() -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let Err(error) = get_with(&format!("http://127.0.0.1:{port}/"), &quiet()) else {
            return Err("a refused connection must not yield a response".into());
        };
        assert!(matches!(error, Error::Transport(_)));
        Ok(())
    }

    #[test]
    fn custom_headers_reach_the_server() -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        // The recorder returns the request it saw, so a socket refusal in the
        // thread reaches the test through the join below instead of panicking.
        let handle = thread::spawn(move || -> std::io::Result<String> {
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
        });
        let mut options = quiet();
        options
            .headers
            .push(("Authorization".into(), "Bearer test-token".into()));
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
        Ok(())
    }

    #[test]
    fn silent_server_hits_timeout() -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        // Held open past the client's timeout: the accept must succeed (so the
        // dial is not what fails) and the reply must never come, leaving the
        // client's read timeout as the only thing that can end the request.
        let handle = thread::spawn(move || -> std::io::Result<()> {
            let (_stream, _) = listener.accept()?;
            thread::sleep(Duration::from_secs(30));
            Ok(())
        });
        let options = quiet().timeout(Duration::from_millis(200));
        let Err(error) = get_with(&format!("http://127.0.0.1:{port}/"), &options) else {
            return Err("a silent server must hit the read timeout".into());
        };
        assert_eq!(error, Error::Timeout);
        drop(handle);
        Ok(())
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
            return Err(
                "a body past the ceiling must be refused, not handed over as a prefix".into(),
            );
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
        let Err(error) = get_with(
            &format!("http://127.0.0.1:{port}/"),
            &ceiling(SMALL_CEILING),
        ) else {
            return Err("a chunked body past the ceiling must be refused".into());
        };
        assert_eq!(
            error,
            Error::BodyTooLarge {
                limit: SMALL_CEILING
            },
            "the ceiling is enforced against the decoded body, not the framing"
        );
        join_server(server)?;
        Ok(())
    }

    /// The same ceiling holds for a close-delimited body, where the end of the
    /// body is the end of the connection and nothing declares its length.
    #[test]
    fn a_close_delimited_body_past_the_ceiling_is_refused() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut reply = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
        reply.extend_from_slice(filler(SMALL_CEILING.saturating_mul(2)).as_bytes());
        let (port, server) = serve_raw(reply)?;
        let Err(error) = get_with(
            &format!("http://127.0.0.1:{port}/"),
            &ceiling(SMALL_CEILING),
        ) else {
            return Err("a close-delimited body past the ceiling must be refused".into());
        };
        assert_eq!(
            error,
            Error::BodyTooLarge {
                limit: SMALL_CEILING
            },
            "a body with no declared length is bounded by the read, not by a header"
        );
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
    fn the_ceiling_is_enforced_before_any_utf8_decision() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut reply = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
        reply.extend_from_slice(&vec![0xFF_u8; SMALL_CEILING.saturating_mul(2)]);
        let (port, server) = serve_raw(reply)?;
        let Err(error) = get_with(
            &format!("http://127.0.0.1:{port}/"),
            &ceiling(SMALL_CEILING),
        ) else {
            return Err("a multi-byte body past the ceiling must be refused".into());
        };
        assert_eq!(
            error,
            Error::BodyTooLarge {
                limit: SMALL_CEILING
            },
            "the ceiling counts bytes, whatever they encode"
        );
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
}
