//! `net` owns the network endpoint domain. Requires `bot.net`.
//!
//! Bound to [`lgwks_std::http`] through one pooled client per endpoint
//! (`client`): a poll is one streamed GET, redirects and body included,
//! bounded by a 10s deadline, with a declared body ceiling. Reachable
//! endpoints report status and a 4 KiB body preview, or the first event's data
//! when the endpoint streams server-sent events; transport failure reports
//! unreachable with status 0 (so a bot can condition on downtime); a malformed
//! URL is a spec bug and errors. A query walks the endpoint as a JSON-array
//! listing across its `Link: rel="next"` pages, and an action posts a body to
//! it and reports the reply.
//!
//! The preview is bounded where the bytes are read, not after: a poll asks the
//! transport for at most the byte ceiling it declares and keeps the prefix it
//! gets, so a server cannot make a probe allocate by sending a large body. The
//! ceiling and the preview are two numbers for one purpose, so the preview is
//! derived from [`BODY_PREVIEW`](crate::domain::net::BODY_PREVIEW) rather than written down twice.

use lgwks_std::http::{self, Paginate};
use lgwks_std::json;

use crate::cap::{Auth, Cap};
use crate::error::{BotError, DispatchCertainty};
use crate::verb;

/// Characters of body kept for observation.
///
/// Public because it bounds a public field: a consumer deciding whether
/// [`NetState::body`] is enough to work with needs the number, and stating it
/// in prose beside the field is how a documented cap drifts from the real one.
/// The field's doc links here instead, so the two cannot disagree.
pub const BODY_PREVIEW: usize = 4096;

/// Bytes the transport may read to fill that preview: four per character.
///
/// A character is at most four UTF-8 bytes, so this is the most a preview of
/// [`BODY_PREVIEW`] characters can need, and a body that reaches it is cut
/// rather than refused ([`BodyPolicy::Preview`]): the probe wants a preview,
/// and refusing would discard the status code and the prefix it can show. A
/// response whose body is larger still reports its status, which is the fact a
/// monitor is watching for; the preview is what it costs to know it.
const BODY_PREVIEW_BYTES: usize = BODY_PREVIEW.saturating_mul(4);

mod client;

/// Observe, query or post to a network endpoint. Supports Observe, Query and
/// Execute.
#[derive(Debug)]
pub struct Endpoint {
    /// The URL probed on each poll, resolved at construction.
    url: String,
    /// Forced to `[bot.net]` by the constructor: a probe dials out, and no
    /// constructor omits the capability.
    caps: Vec<Cap>,
    /// The pooled client every exchange of this endpoint shares, or why it
    /// could not be built; the reason is reported by the first call, typed.
    client: Result<http::Client, client::Setup>,
}

/// Network state returned by observation or query.
///
/// `#[non_exhaustive]`: the reported shape grows with the domain, and a
/// consumer that destructured this literally would break on each addition.
///
/// The three fields are one probe's reading, so all three are private behind
/// accessors: a caller that could set `reachable` on a state the probe reported
/// unreachable would be editing the observation rather than reading it, and
/// `status_code: 0` is a fact about the endpoint rather than a value to choose.
///
/// `a_network_reading_is_one_probes_status_reachability_and_preview` (`tests/it/domain_values.rs`)
/// reads `reachable`, `status_code` and `body` of one.
#[derive(PartialEq, Debug, Clone)]
#[non_exhaustive]
pub struct NetState {
    /// HTTP status code of the last probe. `0` means no response was received:
    /// the endpoint was unreachable, which is distinct from any real status a
    /// server can return.
    status_code: u16,
    /// Whether the endpoint is reachable.
    reachable: bool,
    /// Response body, truncated to [`BODY_PREVIEW`] characters for observation.
    ///
    /// Read under the transport's byte ceiling, derived from
    /// [`BODY_PREVIEW`](crate::domain::net::BODY_PREVIEW),
    /// and decoded lossily, so a large or non-UTF-8 response still yields a
    /// bounded preview rather than failing the probe: a body worth watching is
    /// exactly the one too big to hold.
    ///
    /// Crate-private so the bound is a property of every value a consumer can
    /// hold: read it through [`NetState::body`].
    pub(crate) body: String,
}

impl NetState {
    /// Build the reading a probe produced.
    ///
    /// The one constructor, because the fields are private: a probe's status,
    /// reachability and preview are the same exchange, so a caller cannot hold
    /// two halves of one that never happened.
    #[must_use]
    pub fn new(status_code: u16, reachable: bool, body: impl Into<String>) -> Self {
        Self {
            status_code,
            reachable,
            body: body.into(),
        }
    }

    /// HTTP status code of the last probe, or `0` when the endpoint was
    /// unreachable.
    #[must_use]
    pub const fn status_code(&self) -> u16 {
        self.status_code
    }

    /// Whether the last probe reached the endpoint.
    #[must_use]
    pub const fn reachable(&self) -> bool {
        self.reachable
    }

    /// Response body, at most [`BODY_PREVIEW`] characters of it.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}

impl Endpoint {
    /// Create a network endpoint observer.
    ///
    /// The pooled client is built here, its TLS material (if the environment
    /// names any) proven before any socket opens. The ceiling is the
    /// preview's own budget, so an exchange reads what the probe can use and
    /// nothing more; preview rather than refuse, because a body larger than a
    /// preview is the normal case for a live endpoint and refusing it would
    /// trade a status code for nothing.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            caps: vec![Cap::net()],
            client: client::build(BODY_PREVIEW_BYTES),
        }
    }

    /// A refusal of this endpoint's, with `certainty` and `cause`.
    fn refusal(&self, certainty: DispatchCertainty, cause: String) -> BotError {
        BotError::DomainError {
            domain: ENDPOINT.into(),
            certainty,
            cause,
        }
    }

    /// The malformed-URL refusal: a spec bug, and nothing was sent.
    fn invalid_url(&self) -> BotError {
        self.refusal(
            DispatchCertainty::Refused,
            "invalid endpoint URL (absolute http(s) URI required)".into(),
        )
    }

    /// The pooled client, or the typed reason it could not be built.
    fn pooled(&self) -> Result<http::Client, BotError> {
        match self.client {
            Ok(ref pooled) => Ok(pooled.clone()),
            Err(ref setup) => Err(self.refusal(
                DispatchCertainty::Refused,
                format!("the endpoint's client could not be built: {setup}"),
            )),
        }
    }

    /// Run `job` on the shared blocking pool.
    ///
    /// The bounded form: a burst of concurrent calls waits for, or is refused
    /// by, the shared blocking pool rather than starting one OS thread per
    /// call. A refusal sent nothing, so it is `Refused`.
    async fn blocking<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, BotError> {
        let request = lgwks_std::task::try_spawn_blocking(job).map_err(|error| {
            lgwks_std::trace::debug!(%error, "net: the blocking pool refused the request");
            self.refusal(
                DispatchCertainty::Refused,
                format!("no thread was available for the request: {error}"),
            )
        })?;
        Ok(request.await)
    }
}

/// The domain identifier every verb of [`Endpoint`] reports.
const ENDPOINT: &str = "net::endpoint";

/// At most [`BODY_PREVIEW`] characters of `bytes`, decoded lossily.
fn preview_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(BODY_PREVIEW)
        .collect()
}

impl verb::Observe for Endpoint {
    type Output = NetState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<NetState, BotError> {
        call.0.check(self.required_caps())?;
        let pooled = self.pooled()?;
        let url = self.url.clone();
        match self
            .blocking(move || client::preview(&pooled, &url))
            .await?
        {
            Ok((status, bytes)) => Ok(NetState::new(status, true, preview_text(&bytes))),
            Err(http::Error::InvalidUrl) => Err(self.invalid_url()),
            Err(transport) => {
                lgwks_std::trace::debug!(%transport, "poll: the endpoint is unreachable");
                Ok(NetState::new(0, false, String::new()))
            }
        }
    }

    fn domain_id(&self) -> &str {
        ENDPOINT
    }
}

impl verb::Query for Endpoint {
    type Input = ();
    type Output = Vec<json::Value>;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    /// Every item of the endpoint's JSON-array listing, across its
    /// `Link: rel="next"` pages, in server order; a walk past its page or item
    /// ceiling is refused whole rather than handed back as a prefix.
    async fn query(&self, call: (Auth, &())) -> Result<Vec<json::Value>, BotError> {
        call.0.check(verb::Query::required_caps(self))?;
        let pooled = self.pooled()?;
        let url = self.url.clone();
        match self
            .blocking(move || client::listing(&pooled, &url))
            .await?
        {
            Ok(items) => Ok(items),
            Err(Paginate::Fetch(client::Listing::Http(http::Error::InvalidUrl))) => {
                Err(self.invalid_url())
            }
            Err(refused @ Paginate::LimitExceeded { .. }) => {
                Err(self.refusal(DispatchCertainty::Refused, refused.to_string()))
            }
            Err(failed) => Err(self.refusal(DispatchCertainty::NotDelivered, failed.to_string())),
        }
    }

    fn domain_id(&self) -> &str {
        ENDPOINT
    }
}

impl verb::Execute for Endpoint {
    /// The content type and the body posted.
    type Input = (String, Vec<u8>);
    type Output = NetState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    /// POST the body and report the reply's status and preview. A transport
    /// failure after the request may have left is `Unsettled`: the receiver
    /// may hold the post, and saying it did not would invite a duplicate.
    async fn execute_action(&self, call: (Auth, &(String, Vec<u8>))) -> Result<NetState, BotError> {
        call.0.check(verb::Execute::required_caps(self))?;
        let pooled = self.pooled()?;
        let url = self.url.clone();
        let (content_type, body) = call.1.clone();
        let posted = self
            .blocking(move || client::post(&pooled, &url, &content_type, &body))
            .await?;
        match posted {
            Ok(reply) => Ok(NetState::new(
                reply.status,
                true,
                preview_text(reply.body()),
            )),
            Err(http::Error::InvalidUrl) => Err(self.invalid_url()),
            Err(transport) => Err(self.refusal(
                DispatchCertainty::Unsettled,
                format!("the post may not have reached the endpoint: {transport}"),
            )),
        }
    }

    fn domain_id(&self) -> &str {
        ENDPOINT
    }
}

/// Convenience: create a network endpoint observer.
pub fn endpoint(url: impl Into<String>) -> Endpoint {
    Endpoint::new(url)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::GrantSet;
    use lgwks_std::task::spawn_blocking;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Tests here mix three error domains (socket I/O, `BotError`, and the
    /// executor), so the tests report `Box<dyn Error>` and propagate each with
    /// `?`, rather than reducing every failure to an unwind.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A poll's outcome for the tests that inspect what it observed.
    ///
    /// `TestResult` is fixed to `()`, so a test that needs the state back
    /// states its own result type rather than unwrapping the state out of a
    /// tuple: the error domain is the same one.
    type PollResult = Result<NetState, Box<dyn std::error::Error>>;

    fn net_auth() -> Result<Auth, BotError> {
        GrantSet::empty().grant(Cap::net()).issue(&[Cap::net()])
    }

    /// Serve exactly one HTTP request on an ephemeral loopback port while
    /// `client` runs against it, then join the server.
    ///
    /// The server thread is spawned, cancelled and joined here, so no caller
    /// can leak it. Cancellation is a wake dial: a client that never connected
    /// (it failed before dialling) would leave `accept` blocked forever, so the
    /// fixture dials its own port once, which ends the accept, and the join
    /// returns. The closure returns its I/O failure instead of unwrapping it,
    /// so a refused connection surfaces as a test error rather than a
    /// background panic.
    ///
    /// The body is owned rather than a `&'static str`: the ceiling tests send
    /// bodies larger than the preview, which are built rather than written out.
    fn serve_while<T>(
        body: impl Into<String>,
        client: impl FnOnce(u16) -> T,
    ) -> std::io::Result<(T, std::io::Result<()>)> {
        let body = body.into();
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let accepted = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&accepted);
        let server = spawn_blocking(move || -> std::io::Result<()> {
            let (mut stream, _) = listener.accept()?;
            seen.store(true, Ordering::Release);
            let mut request = vec![0u8; 1024];
            let mut head = Vec::new();
            loop {
                let read = stream.read(&mut request)?;
                head.extend_from_slice(&request[..read]);
                if head.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes())
        });
        let outcome = client(port);
        if !accepted.load(Ordering::Acquire) {
            // The cancellation: a refused wake dial means the server already
            // returned, so there is nothing left to release.
            drop(std::net::TcpStream::connect(("127.0.0.1", port)));
        }
        let served = lgwks_std::task::block_on(server);
        Ok((outcome, served))
    }

    /// One observation of the loopback endpoint on `port`.
    fn poll_port(port: u16) -> PollResult {
        use crate::verb::Observe;
        Ok(lgwks_std::task::block_on(
            Endpoint::new(format!("http://127.0.0.1:{port}/")).poll((net_auth()?, ())),
        )?)
    }

    /// Poll the loopback server serving `body`.
    ///
    /// One poll and a joined fixture per call, so a ceiling test reads as the
    /// body it serves and the state it produced.
    fn poll_body(body: impl Into<String>) -> PollResult {
        let (state, served) = serve_while(body, poll_port)?;
        served?;
        state
    }

    #[test]
    fn poll_reports_reachable_loopback() -> TestResult {
        let (state, served) = serve_while("alive", poll_port)?;
        let state = state?;
        served?;
        assert!(
            state.reachable(),
            "a loopback server that answered must report reachable"
        );
        assert_eq!(state.status_code(), 200);
        assert_eq!(state.body, "alive");
        Ok(())
    }

    /// A body larger than the preview is cut, not held.
    ///
    /// This is the defect: the probe used to read the whole body and then keep
    /// 4096 characters of it, so a server chose the probe's memory. The status
    /// and the prefix are both still reported — a body worth watching is
    /// exactly the one too big to hold, and refusing it would trade the fact a
    /// monitor is watching for (the endpoint answered) for nothing.
    #[test]
    fn poll_cuts_a_body_larger_than_the_preview() -> TestResult {
        let payload = "x".repeat(BODY_PREVIEW_BYTES.saturating_add(64));
        let state = poll_body(payload)?;
        assert!(
            state.reachable(),
            "a server that answered with a large body is reachable"
        );
        assert_eq!(
            state.status_code(),
            200,
            "the status survives the body being cut"
        );
        assert_eq!(
            state.body.chars().count(),
            BODY_PREVIEW,
            "the preview stops at BODY_PREVIEW characters"
        );
        assert!(
            state.body.chars().all(|character| character == 'x'),
            "the preview is the body's own prefix"
        );
        Ok(())
    }

    /// The probe stops reading at its ceiling, and the stop is observable.
    ///
    /// The fixture serves a body larger than a socket buffer can hold, so it can
    /// only finish writing it to a client that keeps draining it. A probe that
    /// stops at [`BODY_PREVIEW_BYTES`] leaves the fixture's write refused, and
    /// that refusal is the only evidence the ceiling is real: the preview the
    /// state reports is byte-identical whether the transport stopped at the
    /// ceiling or read the whole body and kept a prefix of it, so the preview
    /// alone cannot tell the fix from the defect.
    #[test]
    fn poll_stops_reading_at_its_preview_ceiling() -> TestResult {
        let payload = "x".repeat(BODY_PREVIEW_BYTES.saturating_mul(256));
        let (state, served) = serve_while(payload, poll_port)?;
        let state = state?;
        assert!(
            state.reachable(),
            "a server that answered with a huge body is reachable"
        );
        assert_eq!(
            state.body.chars().count(),
            BODY_PREVIEW,
            "the preview stops at BODY_PREVIEW characters"
        );
        assert!(
            served.is_err(),
            "a 4 MiB body can only be written in full to a client that keeps reading, so a \
             completed write means the probe drained it: a preview must stop at its ceiling"
        );
        Ok(())
    }

    /// A preview that ends mid-character is decoded lossily, and is still
    /// bounded: the byte ceiling cuts the wire bytes, and the character cap
    /// holds on the decoded text.
    #[test]
    fn poll_previews_a_multibyte_body_without_panicking() -> TestResult {
        let payload = "€".repeat(BODY_PREVIEW.saturating_add(1));
        let state = poll_body(payload)?;
        assert!(
            state.reachable(),
            "a multibyte body is still a reachable server"
        );
        assert_eq!(state.status_code(), 200);
        assert_eq!(
            state.body.chars().count(),
            BODY_PREVIEW,
            "the character cap holds on the decoded preview"
        );
        assert!(
            state.body.chars().all(|character| character == '€'),
            "a cut inside a multi-byte character must not disturb the prefix: {:?}",
            state.body
        );
        Ok(())
    }

    /// A body exactly at the preview is kept whole: the ceiling is inclusive,
    /// and a preview one character short of the body would be a quieter bug
    /// than the one being fixed.
    #[test]
    fn poll_keeps_a_body_exactly_at_the_preview() -> TestResult {
        let payload = "y".repeat(BODY_PREVIEW);
        let state = poll_body(payload)?;
        assert_eq!(
            state.body.chars().count(),
            BODY_PREVIEW,
            "a body of exactly BODY_PREVIEW characters is kept whole"
        );
        assert!(
            state.body.chars().all(|character| character == 'y'),
            "no character of an at-the-limit body is dropped or replaced"
        );
        Ok(())
    }

    #[test]
    fn poll_reports_unreachable_closed_port() -> TestResult {
        use crate::verb::Observe;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let state = lgwks_std::task::block_on(
            Endpoint::new(format!("http://127.0.0.1:{port}/")).poll((net_auth()?, ())),
        )?;
        assert!(
            !state.reachable(),
            "a refused connection must report unreachable, not error"
        );
        assert_eq!(state.status_code(), 0);
        Ok(())
    }

    /// Ten thousand observers against a saturated blocking pool, then against
    /// a free one.
    ///
    /// Saturated: every thread and every queue slot is held by a job parked on
    /// a gate, so each poll must be refused as `Refused`, naming the pool, and
    /// must send nothing; the listener having no connection waiting is the
    /// proof. Free: the gate opens, the parked jobs drain, and the same ten
    /// thousand polls all run to an answer, none refused. The burst neither
    /// starts a thread per poll nor panics, at either end.
    #[test]
    fn ten_thousand_observers_are_refused_typed_when_the_pool_is_full_and_answered_when_free()
    -> TestResult {
        use crate::verb::Observe;
        use std::sync::{Arc, Condvar, Mutex};
        const OBSERVERS: usize = 10_000;
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let mut parked = Vec::new();
        loop {
            let held = Arc::clone(&gate);
            let job = lgwks_std::task::try_spawn_blocking(move || {
                let (ref open, ref opened) = *held;
                let mut is_open = crate::journal::owner::lock(open);
                while !*is_open {
                    is_open = match opened.wait(is_open) {
                        Ok(guard) => guard,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                }
            });
            match job {
                Ok(handle) => parked.push(handle),
                Err(lgwks_std::task::SpawnError::AtCapacity { .. }) => break,
                Err(other) => return Err(other.into()),
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let live = Endpoint::new(format!(
            "http://127.0.0.1:{}/",
            listener.local_addr()?.port()
        ));
        let auth = net_auth()?;
        let full = lgwks_std::task::block_on(lgwks_std::task::join_all(
            (0..OBSERVERS).map(|_| live.poll((auth.clone(), ()))),
        ));
        let refused = full
            .iter()
            .filter(|outcome| {
                matches!(outcome, Err(BotError::DomainError {
                    certainty: DispatchCertainty::Refused,
                    cause,
                    ..
                }) if cause.starts_with("no thread was available"))
            })
            .count();
        assert_eq!(
            refused, OBSERVERS,
            "every poll against a full pool is refused, typed"
        );
        assert!(
            matches!(listener.accept(), Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "a refused poll sent nothing: no connection reached the listener"
        );

        {
            let (ref open, ref opened) = *gate;
            *crate::journal::owner::lock(open) = true;
            opened.notify_all();
        }
        drop(lgwks_std::task::block_on(lgwks_std::task::join_all(parked)));

        let closed = {
            let probe = TcpListener::bind("127.0.0.1:0")?;
            probe.local_addr()?.port()
        };
        let free = Endpoint::new(format!("http://127.0.0.1:{closed}/"));
        let answered = lgwks_std::task::block_on(lgwks_std::task::join_all(
            (0..OBSERVERS).map(|_| free.poll((auth.clone(), ()))),
        ));
        let unreachable = answered
            .iter()
            .filter(|outcome| matches!(outcome, Ok(state) if !state.reachable()))
            .count();
        assert_eq!(
            unreachable, OBSERVERS,
            "with the pool free every poll runs to an answer, none refused"
        );
        Ok(())
    }

    #[test]
    fn poll_rejects_malformed_url_as_spec_bug() -> TestResult {
        use crate::verb::Observe;
        let Err(error) =
            lgwks_std::task::block_on(Endpoint::new("not a url").poll((net_auth()?, ())))
        else {
            return Err("a malformed URL is a spec bug and must error".into());
        };
        assert!(
            matches!(error, BotError::DomainError { .. }),
            "a malformed URL must be a typed DomainError, got {error:?}"
        );
        Ok(())
    }

    #[test]
    fn poll_without_net_proof_is_denied() -> TestResult {
        use crate::verb::Observe;
        let vacuous = GrantSet::empty().issue(&[])?;
        let Err(error) =
            lgwks_std::task::block_on(Endpoint::new("http://127.0.0.1:9/").poll((vacuous, ())))
        else {
            return Err("a proof covering no capability must not authorize `bot.net`".into());
        };
        assert!(
            matches!(error, BotError::CapabilityDenied { .. }),
            "a capped endpoint must deny a vacuous proof, got {error:?}"
        );
        Ok(())
    }
}
