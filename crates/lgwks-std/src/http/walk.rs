//! The redirect walk: origins, headers per hop, and the loop.
//!
//! Split from the exchange so the walk reads as one decision: which headers
//! each hop carries, when a `Location` is followed, and what shape the final
//! response takes. The one-shot entry ([`exchange`](super::exchange)) and the
//! buffered body read stay in the exchange; everything about following a
//! redirect lives here.

use std::time::Duration;

use iri_string::format::ToDedicatedString;
use iri_string::types::{UriAbsoluteStr, UriReferenceStr};

use super::exchange::{failure, is_https, map_error, response_of, stream_of, validate_url};
use super::{
    Error, FailureKind, FailureStage, MAX_REDIRECT_HOPS, Options, RedirectPolicy, Response,
};

// ── Redirects ───────────────────────────────────────────────────────────────

/// The request one hop sends: a GET, or a POST with its content type and body.
#[derive(Debug, Clone, Copy)]
pub(super) enum Method<'body> {
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
pub(super) struct Origin {
    /// Lowercased scheme.
    pub(super) scheme: String,
    /// Lowercased host, brackets kept for an IP literal.
    pub(super) host: String,
    /// Explicit port, or the scheme's default.
    pub(super) port: Option<u16>,
}

/// The origin of `target`, or `None` when its authority cannot be read,
/// which makes every comparison with it cross-origin.
pub(super) fn origin_of(target: &str) -> Option<Origin> {
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
pub(super) fn host_is_literal(target: &str) -> bool {
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

/// Resolve a `Location` value against the target that returned it.
///
/// The resolved target must itself be an absolute http(s) URI, and its
/// fragment is dropped: a fragment is never sent in a request. Every refusal
/// is class-only: a `Location` can carry a token the
/// caller never saw, so it is not echoed into the error.
pub(super) fn resolve_location(base: &str, location: &str) -> Result<String, Error> {
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
    // The fragment is the client's and never sent; a target without one is
    // already whole.
    let target = match resolved.as_str().split_once('#') {
        Some((target, _fragment)) => target,
        None => resolved.as_str(),
    };
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

/// Build the failure for a `Location` header whose bytes are not visible ASCII.
///
/// The `ToStrError` is not discarded: its class reaches the error through
/// [`super::OpaqueHeader`], whose [`Display`] renders the rejected header through
/// `{:?}` and states the byte length. That is the whole trick, and it is
/// deliberate. The module's policy is that a `Location` may carry a credential
/// or token the caller never saw and must never be *echoed* into a message as
/// live text; an escaped rendering satisfies that in the sense that matters —
/// control characters are inert and cannot forge a second log line — while the
/// bytes themselves stay reachable, so a developer debugging a redirect still
/// sees what the server sent. What the message does not do is emit them
/// unquoted, so anything scraping for them reads the escapes too.
///
/// [`Display`]: std::fmt::Display
fn failure_unprintable_location(header: &[u8]) -> Error {
    Error::Failure {
        stage: FailureStage::Redirect,
        kind: FailureKind::Transport,
        cause: Some(super::FailureCause::opaque(super::OpaqueHeader::new(
            header,
        ))),
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
///
/// `resolve` is this hop's resolve bound: `None` for an IP literal, whose
/// lookup is a parse that cannot block, and the caller's timeout for a name.
/// Request-level timeouts do not affect pool sharing, so literals keep the
/// fast path on a shared agent too.
fn send_hop<'headers>(
    agent: &ureq::Agent,
    target: &str,
    method: Method<'_>,
    headers: impl Iterator<Item = &'headers (String, String)>,
    remaining: Option<Duration>,
    resolve: Option<Duration>,
) -> Result<ureq::http::Response<ureq::Body>, Error> {
    let result = match method {
        Method::Get => headers
            .fold(
                agent
                    .get(target)
                    .config()
                    .timeout_global(remaining)
                    .timeout_resolve(resolve)
                    .build(),
                |call, header| call.header(header.0.as_str(), header.1.as_str()),
            )
            .call(),
        Method::Post { content_type, body } => headers
            .fold(
                agent
                    .post(target)
                    .config()
                    .timeout_global(remaining)
                    .timeout_resolve(resolve)
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
///
/// `agent` lives longer than one call when it comes from a
/// [`Client`](super::Client): a second request to an origin this agent
/// already reached reuses the idle keep-alive connection. Request-level
/// timeouts never affect pool sharing, so the per-hop resolve override below
/// keeps the literal fast path on a shared agent too.
pub(super) fn exchange_on(
    agent: &ureq::Agent,
    url: &str,
    method: Method<'_>,
    options: &Options,
) -> Result<Response, Error> {
    let answered = walk(agent, url, method, options)?;
    match answered {
        Answered::Buffered(response) => Ok(response),
        Answered::Streamed(_) => Err(failure(
            FailureStage::Request,
            FailureKind::InvalidRequest,
            Some("streamed answer on a buffered walk".to_owned()),
        )),
    }
}

/// Run one call for a streaming body, following redirects the same way.
///
/// The redirect walk is the buffered walk's: same headers per hop, same
/// bounds, same chain. Only the final hop differs — its body stays on the
/// wire as a [`StreamResponse`](super::StreamResponse) instead of being
/// drained under the ceiling.
pub(super) fn exchange_stream_on(
    agent: &ureq::Agent,
    url: &str,
    method: Method<'_>,
    options: &Options,
) -> Result<super::StreamResponse, Error> {
    let answered = walk_stream(agent, url, method, options)?;
    match answered {
        Answered::Streamed(response) => Ok(response),
        Answered::Buffered(_) => Err(failure(
            FailureStage::Request,
            FailureKind::InvalidRequest,
            Some("buffered answer on a streamed walk".to_owned()),
        )),
    }
}

/// What a redirect walk delivers: one buffered or one streamed answer.
///
/// One enum rather than two walks: the hop logic is identical and the two
/// constructors diverge only at the final response.
enum Answered {
    /// The final response drained under the ceiling.
    Buffered(Response),
    /// The final response with its body still on the wire.
    Streamed(super::StreamResponse),
}

/// Run the redirect walk, draining the final response under the ceiling.
fn walk(
    agent: &ureq::Agent,
    url: &str,
    method: Method<'_>,
    options: &Options,
) -> Result<Answered, Error> {
    walk_with(agent, url, method, options, false)
}

/// Run the redirect walk, leaving the final body on the wire.
fn walk_stream(
    agent: &ureq::Agent,
    url: &str,
    method: Method<'_>,
    options: &Options,
) -> Result<Answered, Error> {
    walk_with(agent, url, method, options, true)
}

/// Run one call, following redirects under the caller's policy.
///
/// `stream` decides what the final hop delivers: drained under the ceiling
/// or left on the wire. Every hop before the last is identical either way.
fn walk_with(
    agent: &ureq::Agent,
    url: &str,
    method: Method<'_>,
    options: &Options,
    stream: bool,
) -> Result<Answered, Error> {
    validate_url(url)?;
    let limit = redirect_limit(options)?;
    let origin = origin_of(url);
    let mut target = url.to_owned();
    let mut method = method;
    let mut chain = vec![super::exchange::sanitized_target(url)];
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
                    crate::trace::debug!(error = ?refusal.as_ref().err(), "walk_with: returning an error to the caller");
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
        let resolve = (!host_is_literal(&target)).then_some(options.timeout);
        let response = send_hop(agent, &target, method, headers, remaining, resolve)?;
        // No-follow returns whatever came back, redirect or not, unread.
        if limit == 0 {
            return finish(response, options, chain, stream);
        }
        match next_hop(&target, &response, limit, hops, method)? {
            Hop::Final => return finish(response, options, chain, stream),
            Hop::Follow {
                next,
                method: next_method,
                hops: climbed,
            } => {
                method = next_method;
                hops = climbed;
                chain.push(super::exchange::sanitized_target(&next));
                target = next;
            }
        }
    }
}

/// Deliver the final response of a walk in the shape the walk promised.
fn finish(
    response: ureq::http::Response<ureq::Body>,
    options: &Options,
    chain: Vec<String>,
    stream: bool,
) -> Result<Answered, Error> {
    if stream {
        return stream_of(response, options, chain).map(Answered::Streamed);
    }
    response_of(response, options, chain).map(Answered::Buffered)
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
