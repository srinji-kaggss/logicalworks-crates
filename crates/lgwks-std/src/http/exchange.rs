//! One exchange: URL validation, the agent, bounded body reads, and the mapping of every transport failure to its stage and kind.

use std::fmt;
use std::io::Read;

use iri_string::types::UriAbsoluteStr;
use ureq::ResponseExt;

use super::redirect::{Method, exchange};
use super::{
    BodyPolicy, Error, FailureCause, FailureKind, FailureStage, MAX_REDIRECT_HOPS, Options,
    READ_CHUNK_BYTES, RedirectPolicy, Response, Truncation,
};

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
pub(super) fn agent(options: &Options, literal: bool) -> ureq::Agent {
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
pub(super) fn redirect_limit(options: &Options) -> Result<u32, Error> {
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
pub(super) fn sanitized_target(uri: &str) -> String {
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
pub(super) fn response_of(
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
pub(super) fn read_bounded(
    reader: &mut impl Read,
    options: &Options,
) -> Result<(Vec<u8>, Truncation), Error> {
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
pub(super) fn reserve_for(
    body: &mut Vec<u8>,
    incoming: usize,
    ceiling: usize,
) -> Result<(), Error> {
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
pub(super) fn probe_for_more(reader: &mut impl Read) -> Result<Truncation, Error> {
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
pub(super) fn timeout_stage(timeout: ureq::Timeout) -> FailureStage {
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
pub(super) fn failure(stage: FailureStage, kind: FailureKind, cause: Option<String>) -> Error {
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
pub(super) fn failure_unprintable_location(header: &[u8]) -> Error {
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
pub(super) struct OpaqueHeader {
    /// `{:?}` rendering of the header bytes as received, computed once at
    /// construction. `{:?}` on a byte slice escapes every non-printable byte,
    /// so this is the only representation of the header that exists.
    pub(super) escaped: String,
}

impl OpaqueHeader {
    /// Escape raw header bytes on the way in.
    fn new(header: &[u8]) -> Self {
        Self {
            escaped: format!("{header:?}"),
        }
    }

    /// The escaped rendering.
    pub(super) fn escaped(&self) -> &str {
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
pub(super) fn map_read_error(error: std::io::Error, stage: FailureStage) -> Error {
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
pub(super) fn map_error(error: ureq::Error, is_tls: bool) -> Error {
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
pub(super) fn is_https(url: &str) -> bool {
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
