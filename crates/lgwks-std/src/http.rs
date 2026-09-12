//! `http` owns minimal blocking HTTP exchange for tooling and probes.
//! Built on ureq with rustls-only TLS; URLs are validated as absolute
//! http(s) URIs before any socket opens.
//!
//! The API is blocking. From async code, run it on a blocking thread.
//! HTTP error statuses (4xx/5xx) are returned as [`Response`], never as
//! [`Error`] — only transport failure, timeout, and invalid URLs error.

use std::fmt;
use std::io::Read;
use std::time::Duration;

use iri_string::types::UriAbsoluteStr;

// ── Options ─────────────────────────────────────────────────────────────────

/// Request options. Start from [`Options::default`] (30s timeout).
#[derive(Debug, Clone)]
pub struct Options {
    /// Total request timeout, covering connect, TLS, send, and receive.
    pub timeout: Duration,
    /// `User-Agent` header sent with every request.
    pub user_agent: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            user_agent: format!("lgwks-std/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

// ── Response ────────────────────────────────────────────────────────────────

/// A completed HTTP exchange: status, headers, and full body.
#[derive(Debug, Clone)]
pub struct Response {
    /// HTTP status code, including 4xx/5xx.
    pub status: u16,
    /// Response headers in wire order as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Full response body bytes.
    pub body: Vec<u8>,
}

impl Response {
    /// The body as UTF-8, or [`Error::Transport`] when it is not valid UTF-8.
    pub fn text(&self) -> Result<&str, Error> {
        std::str::from_utf8(&self.body).map_err(|utf8_error| {
            Error::Transport(format!("response body is not valid UTF-8: {utf8_error}"))
        })
    }
}

// ── Error ───────────────────────────────────────────────────────────────────

/// What `http` refuses to hide: bad URLs, timeouts, transport failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The URL is not an absolute http(s) URI.
    InvalidUrl(String),
    /// The request hit [`Options::timeout`].
    Timeout,
    /// The exchange never completed: DNS, TCP, TLS, or protocol failure.
    Transport(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(url) => write!(f, "invalid http(s) URL: {url}"),
            Self::Timeout => write!(f, "request timed out"),
            Self::Transport(cause) => write!(f, "transport failure: {cause}"),
        }
    }
}

impl std::error::Error for Error {}

// ── Exchange ────────────────────────────────────────────────────────────────

/// Reject anything that is not an absolute http(s) URI before dialing. The
/// diagnostics name the failure class, never the raw URL: a caller-supplied URL
/// can carry credentials or tokens in its userinfo or query string.
pub fn validate_url(url: &str) -> Result<(), Error> {
    UriAbsoluteStr::new(url).map_err(|cause| {
        eprintln!("lgwks_std::http: rejecting malformed URL: {cause}");
        Error::InvalidUrl(url.into())
    })?;
    let Some(scheme) = url.split_once(':').map(|(scheme, _)| scheme) else {
        eprintln!("lgwks_std::http: rejecting URL with no scheme");
        return Err(Error::InvalidUrl(url.into()));
    };
    if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") {
        Ok(())
    } else {
        eprintln!("lgwks_std::http: rejecting non-http(s) scheme {scheme:?}");
        Err(Error::InvalidUrl(url.into()))
    }
}

fn agent(options: &Options) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(options.timeout))
        .http_status_as_error(false)
        .user_agent(&options.user_agent)
        .build();
    ureq::Agent::new_with_config(config)
}

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
        .map_err(|e| Error::Transport(e.to_string()))?;
    Ok(Response {
        status,
        headers,
        body,
    })
}

fn map_error(error: ureq::Error) -> Error {
    match error {
        ureq::Error::Timeout(_) => Error::Timeout,
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
    agent(options)
        .get(url)
        .call()
        .map_err(map_error)
        .and_then(response_of)
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
    // The URL is intentionally not logged: it can carry credentials in its
    // userinfo or query string. Method, content type, and size are enough to
    // correlate the request.
    eprintln!(
        "lgwks_std::http: POST ({content_type}, {} bytes)",
        body.len()
    );
    let request = agent(options)
        .post(url)
        .header("Content-Type", content_type);
    let response = request.send(body).map_err(map_error)?;
    response_of(response)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    const ECHO: &str = "echo-body-123";

    /// Serve `replies` canned responses, then exit. Returns the bound port.
    /// Reads full requests (headers plus any `Content-Length` body) before
    /// replying, so POST bodies are never mistaken for missing.
    fn serve(replies: Vec<(&'static str, &'static str)>) -> (u16, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            for (status, body) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = vec![0u8; 4096];
                let mut head = Vec::new();
                loop {
                    let n = stream.read(&mut request).unwrap();
                    head.extend_from_slice(&request[..n]);
                    if head.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let header_end = head
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|p| p + 4)
                    .unwrap_or(head.len());
                let text = String::from_utf8_lossy(&head[..header_end]);
                let content_length = text
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                let mut received = head.len() - header_end;
                while received < content_length {
                    let n = stream.read(&mut request).unwrap();
                    head.extend_from_slice(&request[..n]);
                    received += n;
                }
                let full = String::from_utf8_lossy(&head);
                let echoed = full.contains(ECHO);
                let payload = if echoed { ECHO } else { body };
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                stream.write_all(reply.as_bytes()).unwrap();
            }
        });
        (port, handle)
    }

    fn quiet() -> Options {
        Options {
            timeout: Duration::from_secs(5),
            user_agent: "lgwks-std-test".into(),
        }
    }

    #[test]
    fn gets_status_headers_and_body() {
        let (port, server) = serve(vec![("200 OK", "hello")]);
        let response = get_with(&format!("http://127.0.0.1:{port}/"), &quiet()).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"hello");
        assert_eq!(response.text().unwrap(), "hello");
        assert!(
            response
                .headers
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        );
        server.join().unwrap();
    }

    #[test]
    fn error_statuses_are_responses_not_errors() {
        let (port, server) = serve(vec![("404 Not Found", "missing")]);
        let response = get_with(&format!("http://127.0.0.1:{port}/"), &quiet()).unwrap();
        assert_eq!(response.status, 404);
        assert_eq!(response.text().unwrap(), "missing");
        server.join().unwrap();
    }

    #[test]
    fn posts_body_with_content_type() {
        let (port, server) = serve(vec![("200 OK", "")]);
        let response = post_with(
            &format!("http://127.0.0.1:{port}/"),
            "text/plain",
            ECHO.as_bytes(),
            &quiet(),
        )
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.text().unwrap(), ECHO);
        server.join().unwrap();
    }

    #[test]
    fn rejects_non_http_urls_before_dialing() {
        assert!(matches!(
            get_with("not a url", &quiet()),
            Err(Error::InvalidUrl(_))
        ));
        assert!(matches!(
            get_with("/relative/path", &quiet()),
            Err(Error::InvalidUrl(_))
        ));
        assert!(matches!(
            get_with("ftp://127.0.0.1/file", &quiet()),
            Err(Error::InvalidUrl(_))
        ));
    }

    #[test]
    fn refused_connection_is_transport_not_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let error = get_with(&format!("http://127.0.0.1:{port}/"), &quiet()).unwrap_err();
        assert!(matches!(error, Error::Transport(_)));
    }

    #[test]
    fn silent_server_hits_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_secs(30));
        });
        let options = Options {
            timeout: Duration::from_millis(200),
            user_agent: "lgwks-std-test".into(),
        };
        let error = get_with(&format!("http://127.0.0.1:{port}/"), &options).unwrap_err();
        assert_eq!(error, Error::Timeout);
        drop(handle);
    }
}
