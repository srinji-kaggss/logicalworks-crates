//! `http` owns minimal blocking HTTP exchange for tooling and probes.
//! Built on ureq with rustls-only TLS; URLs are validated as absolute
//! http(s) URIs before any socket opens.
//!
//! The API is blocking. From async code, run it on a blocking thread.
//! HTTP error statuses (4xx/5xx) are returned as [`crate::http::Response`],
//! never as [`crate::http::Error`]: only transport failure, timeout,
//! and invalid URLs error.
//!
//! Each call builds and drops its own connection agent: there is no
//! connection pooling. Callers that issue bursts should reuse a
//! [`crate::retry::RetryPolicy`] budget and keep concurrency bounded
//! (`lgwks_bot::rt::task::join_all_bounded`), not open unbounded parallel
//! requests. Retries, backoff, and circuit-breaking are caller policy, not
//! client behavior. Redirects are the separately bounded exception, with an
//! explicit no-follow choice and the previous ten-hop default. A redirect to
//! another origin carries none of the caller's headers.
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
//!
//! The implementation keeps the existing ureq engine. A hand-written
//! `TcpStream` HTTP/TLS stack would duplicate protocol and TLS ownership;
//! reqwest would add a second HTTP stack rather than repair this one.

use std::fmt;
use std::time::Duration;

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

/// Maximum redirects an HTTP call may follow.
///
/// This preserves ureq's former default while bounding caller-selected policy.
pub const MAX_REDIRECT_HOPS: u8 = 10;

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

/// Redirect behavior for one HTTP call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum RedirectPolicy {
    /// Return the first redirect response without contacting its target.
    NoFollow,
    /// Follow at most ten redirects, matching the previous ureq default.
    #[default]
    Follow,
    /// Follow at most `max_hops` redirects. Values above
    /// [`MAX_REDIRECT_HOPS`] are refused.
    FollowAtMost(u8),
}

/// Stage at which an HTTP exchange failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureStage {
    /// DNS resolution.
    Resolve,
    /// Socket connection; ureq includes the TLS handshake in this phase.
    Connect,
    /// TLS protocol negotiation or certificate validation failure.
    Tls,
    /// Request headers or body transmission.
    Send,
    /// Response header reception.
    Headers,
    /// Response body reception.
    Body,
    /// The one-byte read distinguishing exact-cap EOF from overflow.
    EofProbe,
    /// Redirect validation or hop-limit refusal.
    Redirect,
    /// Response body conversion to UTF-8 text.
    TextDecode,
    /// The whole-call deadline ([`Options::deadline`]) or ureq's own global
    /// bound expired. It can expire in any phase, before the request was sent
    /// or after part of the response was read, and on any redirect hop, so a
    /// failure at this stage is not proof that nothing was sent.
    Deadline,
    /// Request configuration before the first exchange, or an I/O failure
    /// ureq reports without naming the phase it happened in. A failure at this
    /// stage is not proof that nothing was sent.
    Request,
}

/// Machine-readable reason for an HTTP failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureKind {
    /// A configured timeout elapsed.
    Timeout,
    /// A transport failed without timing out.
    Transport,
    /// A signal interrupted a blocking socket read or write (`EINTR`).
    ///
    /// Distinct from [`FailureKind::Transport`] because the connection itself
    /// was not refused, reset or severed; ureq's TCP transport maps only
    /// `TimedOut` and `WouldBlock` to its timeout variant and lets `EINTR`
    /// through as an I/O error, which used to read as an outage. It is not
    /// proof that the receiver took no action: once the request has been sent,
    /// an interruption while awaiting headers or reading the body leaves the
    /// effect as unknown as any other failure at that stage. Retry only a
    /// request that is idempotent or carries an idempotency key. A signal
    /// handler installed with `SA_RESTART` resumes the call and this kind
    /// never appears.
    Interrupted,
    /// A response body was not valid UTF-8.
    InvalidUtf8,
    /// The redirect count exceeded the declared limit.
    RedirectLimit,
    /// Request configuration is invalid or ambiguous.
    InvalidRequest,
    /// A bounded response allocation could not be satisfied.
    Resource,
}

/// Sanitized source detail carried by a structured HTTP failure.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FailureCause(FailureCauseText);

/// The inner shape of a [`FailureCause`].
///
/// Two shapes rather than one: a detail this crate authored is public-facing
/// text and is rendered as it was written, while an opaque detail arrived from
/// an untrusted peer and is rendered only by [`OpaqueHeader`], whose `Display`
/// escapes every non-printable byte. [`crate::http`]'s contract is that a
/// `Location` or URL is never echoed into an error as live text, and the opaque
/// shape is what makes that promise checkable instead of a convention.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
enum FailureCauseText {
    /// Text this crate authored; safe to render verbatim.
    Sanitized(String),
    /// Source detail that passed through untrusted input; rendered only
    /// through its own type's escaping `Display`.
    Opaque(OpaqueHeader),
}

impl FailureCause {
    /// Return the sanitized source detail.
    ///
    /// An opaque detail is escaped by construction, so this stays safe to
    /// return and to log for every cause this crate constructs.
    #[must_use]
    pub fn message(&self) -> &str {
        match self.0 {
            FailureCauseText::Sanitized(ref text) => text,
            FailureCauseText::Opaque(ref header) => header.escaped(),
        }
    }

    /// Record source detail this crate authored, which may be rendered
    /// verbatim.
    pub(crate) fn sanitized(text: String) -> Self {
        Self(FailureCauseText::Sanitized(text))
    }

    /// Record source detail that arrived from an untrusted peer.
    ///
    /// The value is escaped the moment it enters, so the detail stays readable
    /// to whoever is debugging the exchange while remaining incapable of
    /// placing a live control character into a log.
    fn opaque(header: OpaqueHeader) -> Self {
        Self(FailureCauseText::Opaque(header))
    }
}

impl fmt::Display for FailureCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            FailureCauseText::Sanitized(ref text) => f.write_str(text),
            FailureCauseText::Opaque(ref header) => f.write_str(header.escaped()),
        }
    }
}

impl std::error::Error for FailureCause {}

// ── Options ─────────────────────────────────────────────────────────────────

/// Request options. Start from [`Options::default`]
/// (30s timeout, 8 MiB body ceiling).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Options {
    /// Maximum time allowed for each transport phase. Body reads and redirect
    /// hops each receive this bound independently, so it is not a bound on the
    /// whole call; [`Options::deadline`] is.
    pub timeout: Duration,
    /// Bound on the whole call, from the first hop's DNS lookup to the last
    /// byte of the final body, carried across every redirect hop. `None` (the
    /// default) leaves only the per-phase [`Options::timeout`]. A call that
    /// exhausts it fails at [`FailureStage::Deadline`] with
    /// [`FailureKind::Timeout`].
    pub deadline: Option<Duration>,
    /// `User-Agent` header sent with every request.
    user_agent: String,
    /// Extra headers sent with every request as `(name, value)` pairs, e.g.
    /// `("Authorization", "Bearer ...")`. Names and values must be valid
    /// header bytes; an invalid pair is a caller bug and the request errors
    /// rather than silently dropping the header. A followed redirect to the
    /// same origin re-sends them all but `Authorization`, `Cookie` and
    /// `Proxy-Authorization`; a redirect to any other origin sends none.
    headers: Vec<(String, String)>,
    /// Ceiling on the response body in bytes, enforced while reading. See
    /// [`DEFAULT_MAX_BODY_BYTES`] for the default and the reason there is no
    /// unbounded value, and [`BodyPolicy`] for what happens at the ceiling.
    pub max_body_bytes: usize,
    /// What to do when the body reaches [`Options::max_body_bytes`].
    pub body_policy: BodyPolicy,
    /// Redirect policy. The default follows at most ten hops.
    pub redirect_policy: RedirectPolicy,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            deadline: None,
            user_agent: format!("lgwks-std/{}", env!("CARGO_PKG_VERSION")),
            headers: Vec::new(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            body_policy: BodyPolicy::Whole,
            redirect_policy: RedirectPolicy::default(),
        }
    }
}

impl Options {
    /// Set the `User-Agent` value sent with each request.
    #[must_use]
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    /// Add a request header. Repeated calls preserve ordinary header
    /// multiplicity; `Idempotency-Key` is kept singular by replacement.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let name = name.into();
        if name.eq_ignore_ascii_case("Idempotency-Key") {
            self.headers
                .retain(|existing| !existing.0.eq_ignore_ascii_case("Idempotency-Key"));
        }
        self.headers.push((name, value.into()));
        self
    }

    /// Set the maximum time allowed for each transport phase and body read.
    ///
    /// [`Options`] is `#[non_exhaustive]`, so a caller outside this crate
    /// cannot construct it with a struct expression. Start from
    /// [`Options::default`] and set the field through this method.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Bound the whole call, redirects and body included.
    ///
    /// [`Options::timeout`] restarts for every phase of every hop, so a
    /// ten-hop chain may take many times that value. This one is measured from
    /// the start of the call and each hop is given only what remains of it.
    #[must_use]
    pub fn deadline(mut self, deadline: Duration) -> Self {
        self.deadline = Some(deadline);
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

    /// The extra headers sent with this request, in the order they were added.
    #[must_use]
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// How many redirects this request may follow.
    ///
    /// Takes `self` and returns it, so it chains with the rest of the builder.
    /// A limit of `0` is not "unlimited": it returns whatever came back without
    /// reading it, which is the only way to observe a redirect without following
    /// it. The default is whatever [`get`] and its siblings set.
    #[must_use]
    pub fn redirect_policy(mut self, redirect_policy: RedirectPolicy) -> Self {
        self.redirect_policy = redirect_policy;
        self
    }

    /// Attach one idempotency key (`Idempotency-Key` header) for a logical
    /// operation. Repeated calls replace the configured key. Pair with
    /// [`crate::retry::RetryPolicy`] and a caller-generated key
    /// (`crate::id::Uuid::new_v4` under feature `random`); the client never
    /// invents the key. The receiver must define deduplication behavior;
    /// retries for one operation must reuse the key and semantic payload.
    /// Reuse with a different payload is receiver-defined and is not made safe
    /// by this helper.
    #[must_use]
    pub fn idempotency_key(self, key: &str) -> Self {
        self.header("Idempotency-Key", key)
    }
}

// ── Response ────────────────────────────────────────────────────────────────

/// A completed HTTP exchange: status, headers, and body.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Response {
    /// HTTP status code, including 4xx/5xx.
    pub status: u16,
    /// HTTP response headers.
    headers: Vec<(String, String)>,
    /// Final request target without userinfo, query or fragment.
    final_target: String,
    /// Redirect targets, including original and final, with userinfo, query
    /// and fragment removed.
    redirect_chain: Vec<String>,
    /// Exact legal header value bytes with multiplicity, in upstream map order.
    header_bytes: Vec<(String, Vec<u8>)>,
    /// Response body bytes, at most [`Options::max_body_bytes`] long.
    body: Vec<u8>,
    /// Whether [`Response::body`] is the whole body or a prefix of it.
    pub truncation: Truncation,
}

impl Response {
    /// Compatibility projection of headers. Values are lossy UTF-8 text and
    /// order follows the upstream `HeaderMap`, not wire arrival order.
    #[must_use]
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// Iterate header names and exact legal value bytes, preserving repeated
    /// values. Order is upstream `HeaderMap` iteration order. Each item is a
    /// header name and its exact legal value bytes, so a repeated header yields
    /// one item per value.
    pub fn header_values(&self) -> impl ExactSizeIterator<Item = (&str, &[u8])> {
        self.header_bytes
            .iter()
            .map(|header| (header.0.as_str(), header.1.as_slice()))
    }

    /// The sanitized final request target after any followed redirects.
    #[must_use]
    pub fn final_target(&self) -> &str {
        &self.final_target
    }

    /// Sanitized redirect history, including original and final targets.
    #[must_use]
    pub fn redirect_chain(&self) -> &[String] {
        &self.redirect_chain
    }

    /// The body bytes, whole or previewed according to [`Response::truncation`].
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The body as UTF-8, or a `TextDecode` / `InvalidUtf8` failure when it is
    /// not valid UTF-8. Decoding failure does not imply exchange failure.
    pub fn text(&self) -> Result<&str, Error> {
        std::str::from_utf8(&self.body).map_err(|utf8_error| Error::Failure {
            stage: FailureStage::TextDecode,
            kind: FailureKind::InvalidUtf8,
            cause: Some(FailureCause::sanitized(format!(
                "valid UTF-8 ends at byte {}",
                utf8_error.valid_up_to()
            ))),
        })
    }

    /// The body as UTF-8, replacing every invalid sequence with `U+FFFD`.
    ///
    /// Use this where the body is a preview rather than a payload: a caller
    /// that only wants a diagnostic excerpt should not have to invent a
    /// fallback for a body it is about to truncate anyway. For a strict read
    /// that reports invalid encoding distinctly, use [`Response::text`].
    #[must_use]
    pub fn text_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }
}

// ── Error ───────────────────────────────────────────────────────────────────

/// Typed HTTP refusal, including its stage and machine-readable failure class.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The URL is not an absolute http(s) URI. The offending URL is not
    /// carried: it can contain credentials in its userinfo or a token in its
    /// query string, and the caller already holds it.
    InvalidUrl,
    /// The response body reached [`Options::max_body_bytes`] without ending,
    /// under [`BodyPolicy::Whole`]. The ceiling is reported, the bytes read are
    /// not: they are a prefix, and handing a prefix to a caller that asked for
    /// a body is the whole failure this variant exists to refuse.
    BodyTooLarge {
        /// The declared ceiling the body reached.
        limit: usize,
    },
    /// A failure with stable stage and machine-readable class.
    Failure {
        /// Phase where the failure was observed.
        stage: FailureStage,
        /// Failure classification independent of display text.
        kind: FailureKind,
        /// Sanitized diagnostic detail, never a raw URL.
        cause: Option<FailureCause>,
    },
    /// A caller selected a redirect limit above the supported ceiling.
    RedirectLimitTooLarge {
        /// Requested redirect limit.
        requested: u8,
        /// Maximum supported redirect limit.
        maximum: u8,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Matched through `*self` so every arm's pattern is the enum's own
        // type; `Failure` binds its cause by reference, since `Error` is not
        // `Copy` and formatting must not consume it.
        match *self {
            Self::InvalidUrl => {
                write!(f, "invalid http(s) URL (absolute http(s) URI required)")
            }
            Self::BodyTooLarge { limit } => write!(
                f,
                "response body reached the declared {limit}-byte ceiling; raise \
                 `max_body_bytes` to read it whole, or select `BodyPolicy::Preview` to keep a \
                 prefix"
            ),
            Self::Failure {
                stage,
                kind,
                ref cause,
            } => {
                write!(f, "HTTP {kind:?} failure during {stage:?}")?;
                if let Some(detail) = cause.as_ref() {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            Self::RedirectLimitTooLarge { requested, maximum } => write!(
                f,
                "redirect limit {requested} exceeds the supported maximum {maximum}"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Failure { ref cause, .. } => cause.as_ref().map(failure_cause_source),
            _ => None,
        }
    }
}

/// Coerce a structured failure cause to its standard error source type.
fn failure_cause_source(cause: &FailureCause) -> &(dyn std::error::Error + 'static) {
    cause
}

// ── Refused header text ─────────────────────────────────────────────────────

/// A response header that was refused, rendered only in escaped form.
///
/// Constructed from the raw bytes a peer sent and immediately escaped: only the
/// escaped rendering is kept, so there is no copy of the untrusted bytes
/// anywhere for a later `Display` pass to reach by forgetting to escape. This
/// type has exactly one rendering, [`fmt::Display`], and it renders escaped.
#[derive(Clone, PartialEq, Eq)]
struct OpaqueHeader {
    /// `{:?}` rendering of the header bytes as received, computed once at
    /// construction. `{:?}` on a byte slice escapes every non-printable byte,
    /// so this is the only representation of the header that exists.
    escaped: String,
}

impl OpaqueHeader {
    /// Escape raw header bytes on the way in.
    fn new(header: &[u8]) -> Self {
        Self {
            escaped: format!("{header:?}"),
        }
    }

    /// The escaped rendering.
    fn escaped(&self) -> &str {
        &self.escaped
    }
}

impl fmt::Display for OpaqueHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `{:?}` on a byte slice escapes every non-printable byte, so no live
        // control character can reach a log line through this path.
        f.write_str(self.escaped())
    }
}

impl fmt::Debug for OpaqueHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for OpaqueHeader {}

/// GET `url` with default options.
///
/// Named for the verb, matching [`post`]: the default-options form of each
/// method carries the bare verb and the configured form takes the `_with`
/// suffix, so the pair reads the same way in both directions. This is a GET
/// request, not a lookup, so it is not an accessor and takes no `get_` prefix.
pub fn get(url: &str) -> Result<Response, Error> {
    get_with(url, &Options::default())
}

/// GET `url` with `options`.
pub fn get_with(url: &str, options: &Options) -> Result<Response, Error> {
    exchange(url, Method::Get, options)
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
    // Nothing about this request is written anywhere, and deliberately so: the
    // URL can carry credentials in its userinfo or a token in its query string,
    // and a library that prints on a request the caller itself authored tells
    // the caller nothing it does not already hold: it has the URL, the content
    // type, and the body length. A caller that wants a request trace owns that
    // decision, and owns redacting the URL when it does.
    exchange(url, Method::Post { content_type, body }, options)
}

// The exchange itself: URL validation, the per-call agent, the bounded body
// read, ureq's errors folded into this module's, and the redirect walk.
mod exchange;

pub use exchange::validate_url;
use exchange::{Method, exchange};

#[cfg(test)]
// These tests stand up a real loopback server on a real thread and hold it open
// past the client's read timeout. The thread is the fixture — this crate
// provides no surface that serves a socket from a test — and every one of them
// is named and joined by the test that started it. A fixture that plays a slow
// upstream waits on the test's "client done" channel with a timeout, so its
// delay ends with the client rather than running out a clock.
mod tests;
