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
use std::io::Read;
use std::time::Duration;

use iri_string::format::ToDedicatedString;
use iri_string::types::{UriAbsoluteStr, UriReferenceStr};
use ureq::ResponseExt;

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
    /// values. Order is upstream `HeaderMap` iteration order.
    ///
    /// ```rust
    /// use lgwks_std::http::{get_with, Options};
    ///
    /// let response = get_with("http://127.0.0.1:1/", &Options::default());
    /// if let Ok(response) = response {
    ///     for (name, value) in response.header_values() {
    ///         let _ = (name, value);
    ///     }
    /// }
    /// ```
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
    // Class-only, for the reason `resolve_location` states: ureq's message
    // embeds the raw URI.
    if UriAbsoluteStr::new(url).is_err() {
        let refusal = Err(Error::InvalidUrl);
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "validate_url: returning an error to the caller");
        return refusal;
    }
    let Some(scheme) = url.split_once(':').map(|(scheme, _)| scheme) else {
        let refusal = Err(Error::InvalidUrl);
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "validate_url: returning an error to the caller");
        return refusal;
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        let refusal = Err(Error::InvalidUrl);
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "validate_url: returning an error to the caller");
        return refusal;
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
        let refusal = Err(Error::InvalidUrl);
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "validate_url: returning an error to the caller");
        return refusal;
    };
    let Some(authority) = rest.strip_prefix("//") else {
        let refusal = Err(Error::InvalidUrl);
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "validate_url: returning an error to the caller");
        return refusal;
    };
    // With no `/`, `?` or `#` after it, the authority is the whole remainder: a
    // URL may end at its authority, and refusing that would refuse a valid one.
    let host_end = match authority.find(['/', '?', '#']) {
        Some(end) => end,
        None => authority.len(),
    };
    if authority[..host_end].is_empty() {
        let refusal = Err(Error::InvalidUrl);
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "validate_url: returning an error to the caller");
        return refusal;
    }
    Ok(())
}

/// Build the one-shot agent for `options`.
///
/// A fresh agent per call is the documented cost model: no connection pooling,
/// no shared state between requests. `http_status_as_error(false)` is what makes
/// a 4xx/5xx a [`Response`] rather than an [`Error`]; the timeout is applied
/// for each transport phase, so the error identifies the phase whose bound
/// elapsed. Body reads use the same bound directly in the body reader.
///
/// The agent never follows a redirect itself: [`exchange`] follows them, so it
/// decides which caller headers each hop carries. ureq strips only
/// `Authorization` and `Cookie` on a redirect and forwards every other header,
/// so an `X-Api-Key` sent to one origin used to reach any origin it redirected
/// to.
///
/// `literal` is whether the hop's host is an IP address. ureq enforces a
/// resolve timeout by starting a detached OS thread for the lookup on every
/// request, and it does so for an IP literal too, whose lookup is a parse and
/// cannot block. Each call to `127.0.0.1` therefore paid a thread start and
/// left a thread it never joined: the client's loopback p50 was 119 us against
/// a bare exchange's 11 us on a Linux runner. A literal hop sets no resolve
/// timeout, so ureq resolves it on the calling thread; a name keeps the bound,
/// because a lookup that hangs is what the bound is for. **Not claimed:** a
/// caller that sets [`Options::deadline`] gives each hop a global timeout, and
/// ureq times the resolve phase against it, so a literal hop under a deadline
/// still starts that thread.
fn agent(options: &Options, literal: bool) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        // Do not also set ureq's global timeout: when it equals the phase
        // limits, its earlier absolute deadline masks the typed phase timeout.
        .timeout_global(None)
        .timeout_resolve((!literal).then_some(options.timeout))
        .timeout_connect(Some(options.timeout))
        .timeout_send_request(Some(options.timeout))
        .timeout_send_body(Some(options.timeout))
        .timeout_recv_response(Some(options.timeout))
        .timeout_recv_body(Some(options.timeout))
        // Zero: every 3xx comes back as a response, never an error, and
        // `exchange` owns the hop.
        .max_redirects(0)
        .http_status_as_error(false)
        .user_agent(&options.user_agent)
        .build();
    ureq::Agent::new_with_config(config)
}

/// Resolve a caller policy into a bounded redirect count.
fn redirect_limit(options: &Options) -> Result<u32, Error> {
    match options.redirect_policy {
        RedirectPolicy::NoFollow => Ok(0),
        RedirectPolicy::Follow => Ok(u32::from(MAX_REDIRECT_HOPS)),
        RedirectPolicy::FollowAtMost(requested) if requested <= MAX_REDIRECT_HOPS => {
            Ok(u32::from(requested))
        }
        RedirectPolicy::FollowAtMost(requested) => Err(Error::RedirectLimitTooLarge {
            requested,
            maximum: MAX_REDIRECT_HOPS,
        }),
    }
}

/// Remove userinfo, query and fragment from an absolute target for receipts.
fn sanitized_target(uri: &str) -> String {
    let Some((scheme, remainder)) = uri.split_once("://") else {
        return String::from("<invalid-target>");
    };
    // Both spans below end at a delimiter this function found, or at the end of
    // the remainder, so each is a slice of it rather than a lookup that could be
    // absent.
    let authority_end = match remainder.find(['/', '?', '#']) {
        Some(end) => end,
        None => remainder.len(),
    };
    let span = &remainder[..authority_end];
    // Everything before the last `@` of the authority is userinfo and is dropped;
    // an authority with no `@` is kept whole.
    let authority = match span.rsplit_once('@') {
        Some((_userinfo, host)) => host,
        None => span,
    };
    let suffix = &remainder[authority_end..];
    // The query and the fragment are dropped as well, so the path is the span
    // before the first of either, or the whole suffix when it has neither.
    let path_end = match suffix.find(['?', '#']) {
        Some(end) => end,
        None => suffix.len(),
    };
    let path = &suffix[..path_end];
    let path = if path.is_empty() { "/" } else { path };
    format!("{scheme}://{authority}{path}")
}

/// Read a completed ureq response into the owned [`Response`].
///
/// Header bytes are retained alongside their lossy String compatibility view.
/// The body, by
/// contrast, is read strictly: only the I/O read can fail here, and a partial
/// read is reported as an [`Error::Failure`] at the body stage rather than returned as a short
/// body. Non-UTF-8 *bodies* are not an error at this layer: [`Response::text`]
/// is where that is decided.
///
/// The body is read under `options.max_body_bytes`; see [`read_bounded`] for
/// how the ceiling is enforced on the way in rather than checked on the way
/// out.
fn response_of(
    mut response: ureq::http::Response<ureq::Body>,
    options: &Options,
    redirect_chain: Vec<String>,
) -> Result<Response, Error> {
    let status = response.status().as_u16();
    let header_bytes: Vec<_> = response
        .headers()
        .iter()
        .map(|(name, value)| (name.to_string(), value.as_bytes().to_vec()))
        .collect();
    let headers = header_bytes
        .iter()
        .map(|header| {
            (
                header.0.clone(),
                String::from_utf8_lossy(&header.1).into_owned(),
            )
        })
        .collect();
    let final_target = sanitized_target(&response.get_uri().to_string());
    let (body, truncation) = read_bounded(&mut response.body_mut().as_reader(), options)?;
    Ok(Response {
        status,
        headers,
        final_target,
        redirect_chain,
        header_bytes,
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
            .map_err(|read_error| map_read_error(read_error, FailureStage::Body))?;
        if read == 0 {
            break Truncation::Complete;
        }
        reserve_for(&mut body, read, ceiling)?;
        body.extend_from_slice(&chunk[..read]);
    };
    if options.body_policy == BodyPolicy::Whole && truncation == Truncation::Cut {
        let refusal = Err(Error::BodyTooLarge { limit: ceiling });
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "read_bounded: returning an error to the caller");
        return refusal;
    }
    Ok((body, truncation))
}

/// Make room for `incoming` more bytes, growing geometrically up to `ceiling`.
///
/// Reserving exactly each chunk made a large body reallocate once per 8 KiB
/// read, O(n / 8 KiB) copies of a growing buffer. Doubling makes that O(log n),
/// and clamping at the ceiling keeps the capacity, like the length, within the
/// bound the caller set. `incoming` never takes the length past `ceiling`,
/// because the read window was already clamped to what remains.
fn reserve_for(body: &mut Vec<u8>, incoming: usize, ceiling: usize) -> Result<(), Error> {
    let needed = body.len().saturating_add(incoming);
    if needed <= body.capacity() {
        return Ok(());
    }
    let target = body
        .len()
        .saturating_mul(2)
        .max(READ_CHUNK_BYTES)
        .min(ceiling)
        .max(needed);
    body.try_reserve_exact(target.saturating_sub(body.len()))
        .map_err(|reserve_error| {
            failure(
                FailureStage::Body,
                FailureKind::Resource,
                Some(reserve_error.to_string()),
            )
        })
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
        .map_err(|read_error| map_read_error(read_error, FailureStage::EofProbe))?;
    Ok(if read == 0 {
        Truncation::Complete
    } else {
        Truncation::Cut
    })
}

/// Fold a ureq failure into this crate's [`Error`].
///
/// A timeout keeps its own [`FailureKind::Timeout`] and the phase ureq names,
/// because it is the one transport failure a caller can act on by widening
/// [`Options::timeout`]. `BadUri` is
/// collapsed to [`Error::InvalidUrl`] for the same reason `validate_url` is
/// class-only: ureq's message embeds the raw URI, so the string is dropped
/// rather than carried into a caller's logs. Everything else keeps the
/// underlying detail, which names DNS, TCP, TLS, or protocol failure.
fn timeout_stage(timeout: ureq::Timeout) -> FailureStage {
    match timeout {
        ureq::Timeout::Resolve => FailureStage::Resolve,
        ureq::Timeout::Connect => FailureStage::Connect,
        ureq::Timeout::SendRequest | ureq::Timeout::SendBody | ureq::Timeout::Await100 => {
            FailureStage::Send
        }
        ureq::Timeout::RecvResponse => FailureStage::Headers,
        ureq::Timeout::RecvBody => FailureStage::Body,
        ureq::Timeout::Global | ureq::Timeout::PerCall => FailureStage::Deadline,
        _ => FailureStage::Deadline,
    }
}

/// Build the canonical, text-safe failure without including a raw URL.
fn failure(stage: FailureStage, kind: FailureKind, cause: Option<String>) -> Error {
    Error::Failure {
        stage,
        kind,
        cause: cause.map(FailureCause::sanitized),
    }
}

/// Build the failure for a `Location` header whose bytes are not visible ASCII.
///
/// The `ToStrError` is not discarded: its class reaches the error through
/// [`OpaqueHeader`], whose [`Display`] renders the rejected header through
/// `{:?}` and states the byte length. That is the whole trick, and it is
/// deliberate. The module's policy is that a `Location` may carry a credential
/// or token the caller never saw and must never be *echoed* into a message as
/// live text; an escaped rendering satisfies that in the sense that matters —
/// control characters are inert and cannot forge a second log line — while the
/// bytes themselves stay reachable, so a developer debugging a redirect still
/// sees what the server sent. What the message does not do is emit them
/// unquoted, so anything scraping for them reads the escapes too.
fn failure_unprintable_location(header: &[u8]) -> Error {
    Error::Failure {
        stage: FailureStage::Redirect,
        kind: FailureKind::Transport,
        cause: Some(FailureCause::opaque(OpaqueHeader::new(header))),
    }
}

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

/// Classify a body-reader error by its typed source and I/O kind.
fn map_read_error(error: std::io::Error, stage: FailureStage) -> Error {
    let ureq_timeout = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<ureq::Error>())
        .and_then(|source| match source {
            &ureq::Error::Timeout(timeout) => Some(timeout),
            _ => None,
        });
    // The whole-call deadline can expire during a body read; it is reported as
    // the deadline, not as the body phase's own bound.
    let stage = match ureq_timeout {
        Some(timeout) if timeout_stage(timeout) == FailureStage::Deadline => FailureStage::Deadline,
        _ => stage,
    };
    let timed_out = ureq_timeout.is_some() || error.kind() == std::io::ErrorKind::TimedOut;
    // `EINTR` is its own class at every stage: a signal landing in a socket
    // read is not a severed connection, and reporting it as one is what made a
    // signal look like an outage. The stage says how far the exchange got,
    // which is what decides whether a retry could repeat an effect.
    let kind = if timed_out {
        FailureKind::Timeout
    } else if error.kind() == std::io::ErrorKind::Interrupted {
        FailureKind::Interrupted
    } else {
        FailureKind::Transport
    };
    failure(stage, kind, Some(error.to_string()))
}

/// Fold ureq's typed error variants without interpreting display strings.
fn map_error(error: ureq::Error, is_tls: bool) -> Error {
    match error {
        ureq::Error::Timeout(timeout) => {
            failure(timeout_stage(timeout), FailureKind::Timeout, None)
        }
        // ureq's `BadUri` message embeds the raw URI. `validate_url` rejects the
        // shapes that reach it, but collapse it to the class-only variant
        // anyway: a caller that logs the returned error must not receive the
        // userinfo or query string back.
        ureq::Error::BadUri(_) => Error::InvalidUrl,
        ureq::Error::HostNotFound => failure(FailureStage::Resolve, FailureKind::Transport, None),
        ureq::Error::Tls(cause) => failure(
            FailureStage::Tls,
            FailureKind::Transport,
            Some(cause.to_owned()),
        ),
        ureq::Error::Rustls(cause) => failure(
            FailureStage::Tls,
            FailureKind::Transport,
            Some(cause.to_string()),
        ),
        ureq::Error::TooManyRedirects => {
            failure(FailureStage::Redirect, FailureKind::RedirectLimit, None)
        }
        ureq::Error::RedirectFailed => {
            failure(FailureStage::Redirect, FailureKind::Transport, None)
        }
        ureq::Error::Io(cause) => {
            let stage = match cause.kind() {
                std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::AddrNotAvailable
                | std::io::ErrorKind::NetworkUnreachable => FailureStage::Connect,
                std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof if is_tls => {
                    FailureStage::Tls
                }
                _ => FailureStage::Request,
            };
            map_read_error(cause, stage)
        }
        ureq::Error::Protocol(cause) => failure(
            FailureStage::Headers,
            FailureKind::Transport,
            Some(cause.to_string()),
        ),
        ureq::Error::ConnectionFailed => {
            failure(FailureStage::Connect, FailureKind::Transport, None)
        }
        ureq::Error::Http(_) => failure(FailureStage::Request, FailureKind::InvalidRequest, None),
        ureq::Error::BodyExceedsLimit(_) => {
            failure(FailureStage::Body, FailureKind::Transport, None)
        }
        _ => failure(FailureStage::Request, FailureKind::Transport, None),
    }
}

/// Whether `url` names the `https` scheme. Schemes are case-insensitive
/// (RFC 3986 §3.1) and [`validate_url`] admits `HTTPS://`, so a byte-exact
/// prefix check would file that spelling's TLS failures under the wrong stage.
fn is_https(url: &str) -> bool {
    url.get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
}

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

// ── Redirects ───────────────────────────────────────────────────────────────

/// The request one hop sends: a GET, or a POST with its content type and body.
#[derive(Debug, Clone, Copy)]
enum Method<'body> {
    /// A GET, which carries no body.
    Get,
    /// A POST of `body` labelled `content_type`.
    Post {
        /// The `Content-Type` sent with the body.
        content_type: &'body str,
        /// The request body.
        body: &'body [u8],
    },
}

/// Headers never forwarded past the first hop, whatever the target's origin.
///
/// The two ureq stripped on every redirect, plus the proxy credential it did
/// not: a redirect is the server's instruction, and a credential the caller
/// addressed to one request is not re-sent on a server's say-so.
const CREDENTIAL_HEADERS: [&str; 3] = ["Authorization", "Cookie", "Proxy-Authorization"];

/// Scheme, host and port of an absolute http(s) target, compared to decide
/// whether a redirect hop may carry the caller's headers.
///
/// Scheme and host are lowercased and a missing port is the scheme's default,
/// so `HTTP://Example.com` and `http://example.com:80` are one origin. An
/// `https` to `http` hop on the same host is a different origin, so a
/// downgrade never carries them either.
#[derive(Debug, PartialEq, Eq)]
struct Origin {
    /// Lowercased scheme.
    scheme: String,
    /// Lowercased host, brackets kept for an IP literal.
    host: String,
    /// Explicit port, or the scheme's default.
    port: Option<u16>,
}

/// The origin of `target`, or `None` when its authority cannot be read,
/// which makes every comparison with it cross-origin.
fn origin_of(target: &str) -> Option<Origin> {
    let target = UriAbsoluteStr::new(target).ok()?;
    let authority = target.authority_components()?;
    let scheme = target.scheme_str().to_ascii_lowercase();
    let port = match authority.port() {
        Some(port) if !port.is_empty() => Some(port.parse::<u16>().ok()?),
        _ => match scheme.as_str() {
            "http" => Some(80),
            "https" => Some(443),
            _ => None,
        },
    };
    Some(Origin {
        scheme,
        host: authority.host().to_ascii_lowercase(),
        port,
    })
}

/// Whether `target`'s host is an IP address, which resolves without a lookup.
///
/// An IPv6 literal is read inside its brackets. A zoned IPv6 literal, an
/// `IPvFuture` and a target whose authority cannot be read answer `false`, so
/// they keep the resolve bound: misreading a name as a literal would drop the
/// bound from a lookup that can hang, and misreading a literal as a name costs
/// only the thread this exists to save.
fn host_is_literal(target: &str) -> bool {
    UriAbsoluteStr::new(target)
        .ok()
        .and_then(|target| target.authority_components())
        .is_some_and(|authority| {
            let host = authority.host();
            let address = match host.strip_prefix('[') {
                Some(bracketed) => bracketed.strip_suffix(']'),
                None => Some(host),
            };
            address.is_some_and(|address| address.parse::<std::net::IpAddr>().is_ok())
        })
}

/// Resolve a `Location` value against the target that returned it.
///
/// The resolved target must itself be an absolute http(s) URI, and its
/// fragment is dropped: a fragment is never sent in a request. Every refusal
/// is class-only, like [`validate_url`]: a `Location` can carry a token the
/// caller never saw, so it is not echoed into the error.
fn resolve_location(base: &str, location: &str) -> Result<String, Error> {
    let refused = |detail: &str| {
        failure(
            FailureStage::Redirect,
            FailureKind::Transport,
            Some(detail.to_owned()),
        )
    };
    // Explicit matches rather than `map_err(|_| ..)` throughout: the parse
    // errors here embed the URI that produced them, and the function's contract
    // -- stated above -- is that a `Location` can carry a token the caller never
    // saw and is not echoed into the error. Writing the refusal in the arm is
    // what makes that visible at each site rather than at the doc comment.
    let base = match UriAbsoluteStr::new(base) {
        Ok(base) => base,
        Err(_not_absolute) => {
            let refusal = Err(refused("redirect base is not absolute"));
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "resolve_location: returning an error to the caller");
            return refusal;
        }
    };
    let reference = match UriReferenceStr::new(location) {
        Ok(reference) => reference,
        Err(_not_a_reference) => {
            let refusal = Err(refused("Location is not a URI reference"));
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "resolve_location: returning an error to the caller");
            return refusal;
        }
    };
    let resolved = reference.resolve_against(base);
    if resolved.ensure_rfc3986_normalizable().is_err() {
        let refusal = Err(refused(
            "Location does not resolve to one unambiguous target",
        ));
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "resolve_location: returning an error to the caller");
        return refusal;
    }
    let resolved = match resolved.try_to_dedicated_string() {
        Ok(resolved) => resolved,
        Err(_not_representable) => {
            let refusal = Err(failure(FailureStage::Redirect, FailureKind::Resource, None));
            #[cfg(feature = "trace")]
            crate::trace::debug!(error = ?refusal.as_ref().err(), "resolve_location: returning an error to the caller");
            return refusal;
        }
    };
    let target = resolved
        .as_str()
        .split_once('#')
        .map_or(resolved.as_str(), |(target, _fragment)| target);
    if validate_url(target).is_err() {
        let refusal = Err(refused("Location leaves absolute http(s)"));
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "resolve_location: returning an error to the caller");
        return refusal;
    }
    Ok(target.to_owned())
}

/// The `Location` of a response that asks to be followed, or `None`.
///
/// Mirrors ureq's rule: any 3xx but 304 with a `Location` is a redirect, and
/// the last `Location` wins when a server sends several. A value that is not
/// visible ASCII is refused rather than read lossily.
fn redirect_location(response: &ureq::http::Response<ureq::Body>) -> Result<Option<&str>, Error> {
    let status = response.status();
    if !status.is_redirection() || status == ureq::http::StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    let Some(location) = response
        .headers()
        .get_all(ureq::http::header::LOCATION)
        .iter()
        .next_back()
    else {
        return Ok(None);
    };
    // The `to_str` failure is translated, never discarded: the header's bytes
    // reach the error through `OpaqueHeader`, whose only rendering is an escaped
    // one. Nothing here can put live untrusted bytes into a message, so the
    // module's no-echo policy holds — see `failure_unprintable_location`.
    match location.to_str() {
        Ok(text) => Ok(Some(text)),
        Err(_not_visible_ascii) => Err(failure_unprintable_location(location.as_bytes())),
    }
}

/// The method the next hop uses, or a refusal when following would resend a
/// body.
///
/// ureq's rule, kept: 307 and 308 keep the method, so a POST under them would
/// replay its body at a target the caller never named, and is refused; every
/// other redirect turns a POST into a bodiless GET, as curl and browsers do.
fn next_method<'body>(
    status: ureq::http::StatusCode,
    method: Method<'body>,
) -> Result<Method<'body>, Error> {
    let keeps_method = matches!(status.as_u16(), 307 | 308);
    match method {
        Method::Post { .. } if keeps_method => Err(failure(
            FailureStage::Redirect,
            FailureKind::Transport,
            Some("a 307/308 redirect would resend the request body; not followed".to_owned()),
        )),
        Method::Post { .. } | Method::Get => Ok(Method::Get),
    }
}

/// Send one hop of `method` to `target` with the given caller headers.
///
/// `remaining` is what is left of the whole-call deadline, if one was set. It
/// becomes this hop's ureq global timeout, which runs from DNS lookup to the
/// last body byte, so the body read after this returns is bounded by it too.
fn send_hop<'headers>(
    agent: &ureq::Agent,
    target: &str,
    method: Method<'_>,
    headers: impl Iterator<Item = &'headers (String, String)>,
    remaining: Option<Duration>,
) -> Result<ureq::http::Response<ureq::Body>, Error> {
    let result = match method {
        Method::Get => headers
            .fold(
                agent.get(target).config().timeout_global(remaining).build(),
                |call, header| call.header(header.0.as_str(), header.1.as_str()),
            )
            .call(),
        Method::Post { content_type, body } => headers
            .fold(
                agent
                    .post(target)
                    .config()
                    .timeout_global(remaining)
                    .build()
                    .header("Content-Type", content_type),
                |call, header| call.header(header.0.as_str(), header.1.as_str()),
            )
            .send(body),
    };
    result.map_err(|error| map_error(error, is_https(target)))
}

/// Run one call, following redirects under the caller's policy.
///
/// The first hop carries every caller header. A later hop to the first hop's
/// origin carries them all but [`CREDENTIAL_HEADERS`]; a hop to any other
/// origin carries none. The hop count is bounded by [`redirect_limit`], and
/// each hop's target is recorded, sanitized, in the response's chain.
fn exchange(url: &str, method: Method<'_>, options: &Options) -> Result<Response, Error> {
    validate_url(url)?;
    let limit = redirect_limit(options)?;
    let origin = origin_of(url);
    let mut target = url.to_owned();
    let mut method = method;
    let mut chain = vec![sanitized_target(url)];
    let mut hops: u32 = 0;
    // A deadline too far away to represent is no deadline at all.
    let deadline_at = options
        .deadline
        .and_then(|deadline| std::time::Instant::now().checked_add(deadline));
    loop {
        let remaining = match deadline_at {
            Some(at) => match at.checked_duration_since(std::time::Instant::now()) {
                Some(left) if !left.is_zero() => Some(left),
                _ => {
                    let refusal = Err(failure(FailureStage::Deadline, FailureKind::Timeout, None));
                    #[cfg(feature = "trace")]
                    crate::trace::debug!(error = ?refusal.as_ref().err(), "exchange: returning an error to the caller");
                    return refusal;
                }
            },
            None => None,
        };
        let first = hops == 0;
        let same_origin = origin.is_some() && origin_of(&target) == origin;
        let headers = options.headers.iter().filter(|header| {
            first
                || (same_origin
                    && !CREDENTIAL_HEADERS
                        .iter()
                        .any(|credential| header.0.eq_ignore_ascii_case(credential)))
        });
        // One agent per hop: a redirect can move the call from an IP literal
        // to a name, and only a name's lookup needs the resolve bound.
        let agent = agent(options, host_is_literal(&target));
        let response = send_hop(&agent, &target, method, headers, remaining)?;
        // No-follow returns whatever came back, redirect or not, unread.
        if limit == 0 {
            return response_of(response, options, chain);
        }
        match next_hop(&target, &response, limit, hops, method)? {
            Hop::Final => return response_of(response, options, chain),
            Hop::Follow {
                next,
                method: next_method,
                hops: climbed,
            } => {
                method = next_method;
                hops = climbed;
                chain.push(sanitized_target(&next));
                target = next;
            }
        }
    }
}

/// What one response means for the redirect walk.
enum Hop<'body> {
    /// The walk stops and the response just seen is the answer.
    Final,
    /// The walk continues to `next` with `method`.
    Follow {
        /// The absolute URL this hop targets.
        next: String,
        /// The method this hop carries, which a 303 rewrites to `GET`.
        method: Method<'body>,
        /// How many redirects the walk has now taken, for the limit check.
        hops: u32,
    },
}

/// Decides whether a response ends the redirect walk or continues it.
///
/// A helper rather than the loop's second half inline: as one block the walk
/// carried four propagation operators across the location parse, the limit
/// check, the location resolve and the method rewrite, so a reader following
/// the redirect rules had to hold the whole tail of the loop in view to see
/// which of them could refuse.
fn next_hop<'body>(
    current: &str,
    response: &ureq::http::Response<ureq::Body>,
    limit: u32,
    hops: u32,
    method: Method<'body>,
) -> Result<Hop<'body>, Error> {
    let Some(location) = redirect_location(response)? else {
        return Ok(Hop::Final);
    };
    if hops >= limit {
        let refusal = Err(failure(
            FailureStage::Redirect,
            FailureKind::RedirectLimit,
            None,
        ));
        #[cfg(feature = "trace")]
        crate::trace::debug!(error = ?refusal.as_ref().err(), "next_hop: returning an error to the caller");
        return refusal;
    }
    let next = resolve_location(current, location)?;
    let method = next_method(response.status(), method)?;
    Ok(Hop::Follow {
        next,
        method,
        hops: hops.saturating_add(1),
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
// These tests stand up a real loopback server on a real thread and hold it open
// past the client's read timeout. The thread is the fixture — this crate
// provides no surface that serves a socket from a test — and every one of them
// is named and joined by the test that started it. Their waits are
// `thread::park_timeout`, the synchronous wait this workspace sanctions in
// place of the banned `thread::sleep`.
mod tests {
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
    fn serve_raw(
        reply: Vec<u8>,
    ) -> std::io::Result<(u16, thread::JoinHandle<std::io::Result<()>>)> {
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
    fn legal_header_bytes_and_repeated_values_are_preserved()
    -> Result<(), Box<dyn std::error::Error>> {
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
            stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
            )?;
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

    /// The whole-call deadline is carried across hops: each hop here answers
    /// well inside the per-phase timeout, yet the chain as a whole overruns
    /// the deadline and is refused at the deadline stage (#191).
    #[test]
    fn a_deadline_bounds_the_whole_redirect_chain() -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let server = fixture_thread(move || -> std::io::Result<usize> {
            let mut served = 0;
            // Nonblocking accept so the server stops once the client gives up,
            // rather than waiting for hops that will never come.
            listener.set_nonblocking(true)?;
            let quiet_since = std::time::Instant::now();
            while quiet_since.elapsed() < Duration::from_secs(3) && served < 10 {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::park_timeout(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                stream.set_nonblocking(false)?;
                let _ = read_request_head(&mut stream)?;
                thread::park_timeout(Duration::from_millis(200));
                let reply = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/hop\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                // The client may already have given up on this hop, which is
                // the point of the test, so a refused write ends the chain.
                if stream.write_all(reply.as_bytes()).is_err() {
                    served += 1;
                    break;
                }
                served += 1;
            }
            Ok(served)
        })?;
        let started = std::time::Instant::now();
        let result = get_with(
            &format!("http://127.0.0.1:{port}/hop"),
            &quiet()
                .timeout(Duration::from_secs(2))
                .deadline(Duration::from_millis(500))
                .redirect_policy(RedirectPolicy::FollowAtMost(10)),
        );
        let elapsed = started.elapsed();
        assert!(
            matches!(
                result,
                Err(Error::Failure {
                    stage: FailureStage::Deadline,
                    kind: FailureKind::Timeout,
                    ..
                })
            ),
            "the chain is refused at its deadline, got {result:?}"
        );
        assert!(
            elapsed < Duration::from_millis(1_500),
            "the deadline bounds the chain, not ten hops of 200 ms: {elapsed:?}"
        );
        let served = server.join().map_err(|_| "redirect server panicked")??;
        assert!(
            (2..10).contains(&served),
            "the chain made progress, then stopped at the deadline: {served} hops"
        );
        Ok(())
    }

    /// A body that trickles in under the per-phase timeout is still stopped by
    /// the whole-call deadline, and the failure names the deadline.
    #[test]
    fn a_deadline_bounds_a_trickled_body() -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let server = fixture_thread(move || -> std::io::Result<()> {
            let (mut stream, _) = listener.accept()?;
            let _ = read_request_head(&mut stream)?;
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\n\r\n")?;
            for _ in 0..64 {
                thread::park_timeout(Duration::from_millis(50));
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
    fn body_capacity_grows_geometrically_within_the_ceiling()
    -> Result<(), Box<dyn std::error::Error>> {
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
    fn a_cross_origin_redirect_carries_no_caller_header() -> Result<(), Box<dyn std::error::Error>>
    {
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
    fn a_see_other_redirect_turns_a_post_into_a_bodiless_get()
    -> Result<(), Box<dyn std::error::Error>> {
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

    /// 307 keeps the method, so following it would replay the POST body at a
    /// target the caller never named; it is refused without a second request.
    #[test]
    fn a_method_keeping_redirect_of_a_post_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let (port, server) =
            serve_recording(vec![redirect_reply("307 Temporary Redirect", "/again")])?;
        let result = post_with(
            &format!("http://127.0.0.1:{port}/submit"),
            "application/json",
            b"",
            &quiet(),
        );
        assert!(
            matches!(
                result,
                Err(Error::Failure {
                    stage: FailureStage::Redirect,
                    kind: FailureKind::Transport,
                    ..
                })
            ),
            "{result:?}"
        );
        let heads = server.join().map_err(|_| "server panicked")??;
        assert_eq!(heads.len(), 1, "the body was not replayed");
        Ok(())
    }

    /// Origins compare lowercased scheme and host and the effective port.
    #[test]
    fn origins_compare_scheme_host_and_effective_port() {
        assert!(origin_of("HTTP://Example.COM/a").is_some());
        assert_eq!(
            origin_of("HTTP://Example.COM/a"),
            origin_of("http://example.com:80/b")
        );
        assert_eq!(
            origin_of("https://example.com/"),
            origin_of("https://example.com:443/")
        );
        assert_ne!(
            origin_of("https://example.com/"),
            origin_of("http://example.com/"),
            "a downgrade is another origin"
        );
        assert_ne!(
            origin_of("http://example.com:8080/"),
            origin_of("http://example.com/")
        );
        assert_eq!(
            origin_of("http://[::1]:8080/"),
            origin_of("http://[::1]:8080/x")
        );
        assert!(
            origin_of("http://example.com:99999/").is_none(),
            "an unreadable port is never the same origin"
        );
    }

    /// A `Location` resolves to an absolute http(s) target or is refused
    /// without echoing it.
    #[test]
    fn a_location_resolves_to_an_absolute_http_target_or_is_refused() -> Result<(), Error> {
        assert_eq!(
            resolve_location("http://a.test/dir/page", "next?x=1#top")?,
            "http://a.test/dir/next?x=1"
        );
        assert_eq!(
            resolve_location("http://a.test/dir/page", "//b.test/p")?,
            "http://b.test/p"
        );
        for hostile in [
            "ftp://a.test/token-secret",
            "javascript:token-secret",
            "http://",
            "not a uri token-secret",
        ] {
            let refusal = resolve_location("http://a.test/", hostile);
            assert!(
                matches!(
                    refusal,
                    Err(Error::Failure {
                        stage: FailureStage::Redirect,
                        ..
                    })
                ),
                "{hostile} must be refused"
            );
            assert!(
                !format!("{refusal:?}").contains("token-secret"),
                "the Location is not echoed"
            );
        }
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

    #[test]
    fn silent_server_hits_timeout() -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        // Held open past the client's timeout: the accept must succeed (so the
        // dial is not what fails) and the reply must never come, leaving the
        // client's read timeout as the only thing that can end the request.
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let handle = fixture_thread(move || -> std::io::Result<()> {
            let (_stream, _) = listener.accept()?;
            let _released = released.recv();
            Ok(())
        })?;
        let options = quiet().timeout(Duration::from_millis(200));
        let Err(error) = get_with(&format!("http://127.0.0.1:{port}/"), &options) else {
            return Err("a silent server must hit the read timeout".into());
        };
        // A signal can interrupt the read on the way to the timeout, and the
        // kernel may deliver it before the deadline. That is a real and
        // different outcome — see `an_interrupted_request_is_not_a_transport
        // failure` — so a silent server yields either the header-stage
        // timeout or the interruption, and never a transport failure.
        assert!(
            matches!(
                error,
                Error::Failure {
                    stage: FailureStage::Headers,
                    kind: FailureKind::Timeout,
                    ..
                } | Error::Failure {
                    kind: FailureKind::Interrupted,
                    ..
                }
            ),
            "a silent server yields a header timeout or an interruption, not {error:?}"
        );
        release
            .send(())
            .map_err(|_| "timeout fixture receiver ended")?;
        handle
            .join()
            .map_err(|_| "timeout fixture thread panicked")??;
        Ok(())
    }

    /// A stalled TLS handshake is reported at the connect stage with timeout class.
    #[test]
    fn tls_handshake_timeout_preserves_connect_stage() -> Result<(), Box<dyn std::error::Error>> {
        let (port, release, server) = serve_held(None)?;
        let result = get_with(
            &format!("https://127.0.0.1:{port}/"),
            &quiet().timeout(Duration::from_millis(150)),
        );
        assert!(matches!(
            result,
            Err(Error::Failure {
                stage: FailureStage::Connect,
                kind: FailureKind::Timeout,
                ..
            })
        ));
        release.send(()).map_err(|_| "TLS fixture receiver ended")?;
        server.join().map_err(|_| "TLS fixture thread panicked")??;
        Ok(())
    }

    /// Malformed TLS bytes remain a TLS-class transport failure.
    #[test]
    fn malformed_tls_handshake_preserves_tls_stage() -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let server = fixture_thread(move || -> std::io::Result<()> {
            let (mut stream, _) = listener.accept()?;
            // Read the ClientHello before answering and drain until the client
            // hangs up: closing a socket with unread bytes sends RST, and a
            // reset that overtakes the reply is a connection failure, not the
            // malformed record this test is about.
            let mut hello = [0_u8; 1024];
            let _hello_len = stream.read(&mut hello)?;
            stream.write_all(b"not-a-tls-record")?;
            // How the client hangs up (FIN or reset) is not under test.
            let _hung_up = std::io::copy(&mut stream, &mut std::io::sink());
            Ok(())
        })?;
        let result = get_with(
            &format!("https://127.0.0.1:{port}/"),
            &quiet().timeout(Duration::from_secs(2)),
        );
        assert!(matches!(
            result,
            Err(Error::Failure {
                stage: FailureStage::Tls,
                kind: FailureKind::Transport,
                ..
            })
        ));
        server
            .join()
            .map_err(|_| "malformed TLS fixture panicked")??;
        Ok(())
    }

    /// A stalled request-body write reports the send stage and timeout class.
    #[test]
    fn request_body_timeout_preserves_send_stage() -> Result<(), Box<dyn std::error::Error>> {
        let (port, release, server) = serve_held(None)?;
        let body = vec![b'x'; 32 * 1024 * 1024];
        let result = post_with(
            &format!("http://127.0.0.1:{port}/"),
            "application/octet-stream",
            &body,
            &quiet().timeout(Duration::from_millis(150)),
        );
        assert!(matches!(
            result,
            Err(Error::Failure {
                stage: FailureStage::Send,
                kind: FailureKind::Timeout,
                ..
            })
        ));
        release
            .send(())
            .map_err(|_| "send fixture receiver ended")?;
        server
            .join()
            .map_err(|_| "send fixture thread panicked")??;
        Ok(())
    }

    /// A body timeout keeps its body phase and timeout class after headers land.
    #[test]
    fn body_timeout_preserves_stage_and_class() -> Result<(), Box<dyn std::error::Error>> {
        let (port, release, server) = serve_held(Some(
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: keep-alive\r\n\r\nx".to_vec(),
        ))?;
        let result = get_with(
            &format!("http://127.0.0.1:{port}/"),
            &quiet().timeout(Duration::from_millis(150)),
        );
        assert!(matches!(
            result,
            Err(Error::Failure {
                stage: FailureStage::Body,
                kind: FailureKind::Timeout,
                ..
            })
        ));
        release
            .send(())
            .map_err(|_| "body fixture receiver ended")?;
        server
            .join()
            .map_err(|_| "body fixture thread panicked")??;
        Ok(())
    }

    /// The exact-cap EOF probe reports its own timeout stage.
    #[test]
    fn eof_probe_timeout_preserves_stage_and_class() -> Result<(), Box<dyn std::error::Error>> {
        let limit = SMALL_CEILING;
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
            limit.saturating_add(1)
        )
        .into_bytes();
        response.extend(std::iter::repeat_n(b'x', limit));
        let (port, release, server) = serve_held(Some(response))?;
        let result = get_with(
            &format!("http://127.0.0.1:{port}/"),
            &previewing(limit).timeout(Duration::from_millis(150)),
        );
        assert!(matches!(
            result,
            Err(Error::Failure {
                stage: FailureStage::EofProbe,
                kind: FailureKind::Timeout,
                ..
            })
        ));
        release.send(()).map_err(|_| "EOF fixture receiver ended")?;
        server.join().map_err(|_| "EOF fixture thread panicked")??;
        Ok(())
    }

    /// A short content-length response is transport failure, never ordinary EOF.
    #[test]
    fn truncated_transport_is_not_reported_as_complete_eof()
    -> Result<(), Box<dyn std::error::Error>> {
        let (port, server) = serve_raw(
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nabc".to_vec(),
        )?;
        let result = get_with(&format!("http://127.0.0.1:{port}/"), &quiet());
        assert!(matches!(
            result,
            Err(Error::Failure {
                stage: FailureStage::Body,
                kind: FailureKind::Transport,
                ..
            })
        ));
        join_server(server)?;
        Ok(())
    }

    /// Invalid UTF-8 belongs to payload decoding after the HTTP response exists.
    #[test]
    fn invalid_utf8_is_a_payload_decoding_failure() -> Result<(), Box<dyn std::error::Error>> {
        let (port, server) = serve_raw(
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n\xff".to_vec(),
        )?;
        let response = get_with(&format!("http://127.0.0.1:{port}/"), &quiet())?;
        assert_eq!(response.status, 200, "the HTTP exchange completed");
        assert!(matches!(
            response.text(),
            Err(Error::Failure {
                stage: FailureStage::TextDecode,
                kind: FailureKind::InvalidUtf8,
                ..
            })
        ));
        join_server(server)?;
        Ok(())
    }

    /// Logical Vec capacity stays inside a non-power-of-two body ceiling.
    #[test]
    fn non_power_of_two_ceiling_bounds_retained_capacity() -> Result<(), Box<dyn std::error::Error>>
    {
        let limit = 73;
        let mut reader = std::io::Cursor::new(filler(limit).into_bytes());
        let (body, truncation) = read_bounded(&mut reader, &ceiling(limit))?;
        assert_eq!(body.len(), limit, "the exact body is retained");
        assert!(
            body.capacity() <= limit,
            "body capacity stays within the declared ceiling"
        );
        assert_eq!(truncation, Truncation::Complete);
        Ok(())
    }

    /// The body reader is handed one fixed-size window per call, never the
    /// declared body length, and the retained capacity is clamped to a
    /// non-power-of-two ceiling.
    ///
    /// This is the *scratch* measurement the ceiling's contract names: the
    /// window handed to the transport is the scratch the read path commits,
    /// and it is `min(remaining, READ_CHUNK_BYTES)` — independent of how large
    /// the body claims to be. A mutant that passed `remaining` straight through
    /// would hand the reader `limit` bytes here and fail the `READ_CHUNK_BYTES`
    /// bound for the 10 000-byte ceiling, which is why that ceiling is one of
    /// the measured cases: it is the smallest that forces more than one chunk.
    #[test]
    fn the_read_window_is_a_fixed_chunk_and_capacity_is_clamped_to_the_ceiling()
    -> Result<(), Box<dyn std::error::Error>> {
        /// A reader that yields one byte per call and records the largest
        /// buffer it was ever handed.
        struct WindowRecorder {
            /// The bytes still to yield.
            remaining: usize,
            /// The largest `read` window observed.
            largest_window: usize,
            /// Number of `read` calls, including the EOF probe.
            reads: usize,
        }

        impl Read for WindowRecorder {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.largest_window = self.largest_window.max(buffer.len());
                self.reads = self.reads.saturating_add(1);
                if buffer.is_empty() || self.remaining == 0 {
                    return Ok(0);
                }
                buffer[0] = b'x';
                self.remaining = self.remaining.saturating_sub(1);
                Ok(1)
            }
        }

        for limit in [73_usize, 1000, 3003, 10_000] {
            let mut reader = WindowRecorder {
                remaining: limit,
                largest_window: 0,
                reads: 0,
            };
            let (body, truncation) = read_bounded(&mut reader, &ceiling(limit))?;
            assert_eq!(
                body.len(),
                limit,
                "the exact body is retained at a {limit}-byte ceiling"
            );
            assert_eq!(
                body.capacity(),
                limit,
                "capacity is clamped to the non-power-of-two ceiling at {limit}: {}",
                body.capacity()
            );
            assert_eq!(
                truncation,
                Truncation::Complete,
                "an exactly-at-ceiling body ends on its own at {limit}"
            );
            assert!(
                reader.largest_window <= READ_CHUNK_BYTES,
                "the reader window is one fixed chunk at {limit}, not the body: {}",
                reader.largest_window
            );
            if limit > READ_CHUNK_BYTES {
                assert_eq!(
                    reader.largest_window, READ_CHUNK_BYTES,
                    "the first window of a body larger than one chunk is exactly one chunk"
                );
            }
            assert_eq!(
                reader.reads,
                limit.saturating_add(1),
                "one read per byte plus the single EOF probe at {limit}"
            );
        }
        Ok(())
    }

    /// An interruption is its own class at every stage, never a transport failure.
    ///
    /// ureq's TCP transport maps `TimedOut` and `WouldBlock` to its timeout
    /// variant and lets `EINTR` through as an I/O error, which arrived here as
    /// a transport failure. Signals land on whichever thread they land on, so
    /// under load this was frequent enough to break CI. The request path and
    /// the body path classify it the same way, and each keeps its stage.
    #[test]
    fn an_interruption_is_classified_the_same_way_at_every_stage() {
        let interrupted = || std::io::Error::from(std::io::ErrorKind::Interrupted);
        assert!(
            matches!(
                map_error(ureq::Error::Io(interrupted()), false),
                Error::Failure {
                    kind: FailureKind::Interrupted,
                    ..
                }
            ),
            "EINTR on the request path must not be reported as a transport failure"
        );
        for stage in [FailureStage::Body, FailureStage::EofProbe] {
            assert!(
                matches!(
                    map_read_error(interrupted(), stage),
                    Error::Failure {
                        kind: FailureKind::Interrupted,
                        stage: observed,
                        ..
                    } if observed == stage
                ),
                "EINTR while reading at {stage:?} keeps its stage and its class"
            );
        }
        // A genuine transport error still is one, so the split is meaningful
        // rather than every I/O failure collapsing to an interruption.
        assert!(matches!(
            map_error(
                ureq::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionReset)),
                false,
            ),
            Error::Failure {
                kind: FailureKind::Transport,
                ..
            }
        ));
    }

    /// The scheme check behind TLS stage attribution ignores case, as
    /// [`validate_url`] does.
    #[test]
    fn the_https_scheme_is_recognised_in_any_case() {
        assert!(is_https("https://example.com/"));
        assert!(is_https("HTTPS://example.com/"));
        assert!(is_https("HtTpS://example.com/"));
        assert!(!is_https("http://example.com/"));
        assert!(!is_https("https:"));
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

    /// A zero-byte ceiling accepts an empty body after its EOF probe.
    #[test]
    fn zero_ceiling_accepts_an_empty_body() -> Result<(), Box<dyn std::error::Error>> {
        let (port, server) = serve_raw(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        )?;
        let response = get_with(&format!("http://127.0.0.1:{port}/"), &ceiling(0))?;
        assert!(
            response.body.is_empty(),
            "zero ceiling retains no body bytes"
        );
        assert_eq!(response.truncation, Truncation::Complete);
        join_server(server)?;
        Ok(())
    }

    /// A zero-byte ceiling distinguishes an empty response from one body byte.
    #[test]
    fn zero_ceiling_refuses_a_nonempty_whole_body() -> Result<(), Box<dyn std::error::Error>> {
        let (port, server) = serve_raw(
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx".to_vec(),
        )?;
        let result = get_with(&format!("http://127.0.0.1:{port}/"), &ceiling(0));
        assert!(matches!(result, Err(Error::BodyTooLarge { limit: 0 })));
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
        assert_refused_at_small_ceiling(port, "a chunked body past the ceiling");
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
        assert_refused_at_small_ceiling(port, "a close-delimited body past the ceiling");
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
        assert_refused_at_small_ceiling(port, "a multi-byte body past the ceiling");
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
