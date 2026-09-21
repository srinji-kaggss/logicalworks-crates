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

use std::fmt;
use std::io::Read;
use std::time::Duration;

use iri_string::types::UriAbsoluteStr;

// ── Options ─────────────────────────────────────────────────────────────────

/// Request options. Start from [`Options::default`](crate::http::Options::default) (30s timeout).
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
}

impl Default for Options {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            user_agent: format!("lgwks-std/{}", env!("CARGO_PKG_VERSION")),
            headers: Vec::new(),
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

/// A completed HTTP exchange: status, headers, and full body.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Response {
    /// HTTP status code, including 4xx/5xx.
    pub status: u16,
    /// Response headers in wire order as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Full response body bytes.
    pub body: Vec<u8>,
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
fn response_of(mut response: ureq::http::Response<ureq::Body>) -> Result<Response, Error> {
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
    let mut body = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut body)
        .map_err(|read_error| Error::Transport(read_error.to_string()))?;
    Ok(Response {
        status,
        headers,
        body,
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
    call.call().map_err(map_error).and_then(response_of)
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
    response_of(response)
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
        replies: Vec<(&'static str, &'static str)>,
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
                let payload = if echoed { ECHO } else { body };
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

    // These tests return `Result` rather than unwrapping: a refusal reports its
    // own `Debug` on failure, which is the same report `.unwrap` would have
    // panicked with, without an `unwrap` in the tree.
    #[test]
    fn default_option_wrappers_reach_the_same_path() -> Result<(), Box<dyn std::error::Error>> {
        let (port, server) = serve(vec![("200 OK", "hello"), ("200 OK", "ok")])?;
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
        }
    }

    #[test]
    fn gets_status_headers_and_body() -> Result<(), Box<dyn std::error::Error>> {
        let (port, server) = serve(vec![("200 OK", "hello")])?;
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
        let (port, server) = serve(vec![("404 Not Found", "missing")])?;
        let response = get_with(&format!("http://127.0.0.1:{port}/"), &quiet())?;
        assert_eq!(response.status, 404);
        assert_eq!(response.text()?, "missing");
        join_server(server)?;
        Ok(())
    }

    #[test]
    fn posts_body_with_content_type() -> Result<(), Box<dyn std::error::Error>> {
        let (port, server) = serve(vec![("200 OK", "")])?;
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
        let options = Options {
            timeout: Duration::from_secs(5),
            user_agent: "lgwks-std-test".into(),
            headers: vec![("Authorization".into(), "Bearer test-token".into())],
        };
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
        let options = Options {
            timeout: Duration::from_millis(200),
            user_agent: "lgwks-std-test".into(),
            headers: Vec::new(),
        };
        let Err(error) = get_with(&format!("http://127.0.0.1:{port}/"), &options) else {
            return Err("a silent server must hit the read timeout".into());
        };
        assert_eq!(error, Error::Timeout);
        drop(handle);
        Ok(())
    }
}
