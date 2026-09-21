//! `net` owns the network endpoint domain. Requires `bot.net`.
//!
//! Bound to [`lgwks_std::http`]: a poll is one GET with a 10s timeout and a
//! declared body ceiling. Reachable endpoints report status and a 4 KiB body
//! preview; transport failure reports unreachable with status 0 (so a bot can
//! condition on downtime); a malformed URL is a spec bug and errors.
//!
//! The preview is bounded where the bytes are read, not after: a poll asks the
//! transport for at most the byte ceiling it declares and keeps the prefix it
//! gets, so a server cannot make a probe allocate by sending a large body. The
//! ceiling and the preview are two numbers for one purpose, so the preview is
//! derived from [`BODY_PREVIEW`] rather than written down twice.

use std::time::Duration;

use lgwks_std::http::{self, BodyPolicy, Options};

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

/// Seconds a poll waits for the endpoint.
const POLL_TIMEOUT_SECS: u64 = 10;

/// Observe or query a network endpoint. Supports Observe and Query.
pub struct Endpoint {
    /// The URL probed on each poll, resolved at construction.
    url: String,
    /// Forced to `[bot.net]` by the constructor: a probe dials out, and no
    /// constructor omits the capability.
    caps: Vec<Cap>,
}

/// Network state returned by observation or query.
///
/// `#[non_exhaustive]`: the reported shape grows with the domain, and a
/// consumer that destructured this literally would break on each addition.
#[derive(PartialEq, Debug, Clone)]
#[non_exhaustive]
pub struct NetState {
    /// HTTP status code of the last probe. `0` means no response was received:
    /// the endpoint was unreachable, which is distinct from any real status a
    /// server can return.
    pub status_code: u16,
    /// Whether the endpoint is reachable.
    pub reachable: bool,
    /// Response body, truncated to [`BODY_PREVIEW`] characters for observation.
    ///
    /// Read under the transport's byte ceiling, derived from [`BODY_PREVIEW`],
    /// and decoded lossily, so a large or non-UTF-8 response still yields a
    /// bounded preview rather than failing the probe: a body worth watching is
    /// exactly the one too big to hold.
    pub body: String,
}

impl Endpoint {
    /// Create a network endpoint observer.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            caps: vec![Cap::net()],
        }
    }
}

impl verb::Observe for Endpoint {
    type Output = NetState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<NetState, BotError> {
        call.0.check(self.required_caps())?;
        let url = self.url.clone();
        // The ceiling is the preview's own budget, so the exchange reads what
        // the probe can use and nothing more. Preview rather than the refusing
        // default: a body larger than a preview is the normal case for a live
        // endpoint, and refusing it would trade a status code for nothing.
        let options = Options::default()
            .timeout(Duration::from_secs(POLL_TIMEOUT_SECS))
            .max_body_bytes(BODY_PREVIEW_BYTES)
            .body_policy(BodyPolicy::Preview);
        let exchange =
            lgwks_std::task::spawn_blocking(move || http::get_with(&url, &options)).await;
        match exchange {
            Ok(response) => Ok(NetState {
                status_code: response.status,
                reachable: true,
                body: response.text_lossy().chars().take(BODY_PREVIEW).collect(),
            }),
            Err(http::Error::InvalidUrl) => Err(BotError::DomainError {
                domain: self.domain_id().into(),
                certainty: DispatchCertainty::Refused,
                cause: "invalid endpoint URL (absolute http(s) URI required)".into(),
            }),
            Err(_) => Ok(NetState {
                status_code: 0,
                reachable: false,
                body: String::new(),
            }),
        }
    }

    fn domain_id(&self) -> &str {
        "net::endpoint"
    }
}

impl verb::Query for Endpoint {
    type Input = ();
    type Output = NetState;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn query(&self, call: (Auth, &())) -> Result<NetState, BotError> {
        let (auth, _) = call;
        verb::Observe::poll(self, (auth, ())).await
    }

    fn domain_id(&self) -> &str {
        "net::endpoint"
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
    use std::io::{Read, Write};
    use std::net::TcpListener;

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

    /// Serve exactly one HTTP request on an ephemeral loopback port.
    ///
    /// `lgwks_std::task::spawn_blocking` rather than a raw `std::thread::spawn`:
    /// it starts the thread immediately, so the listener is accepting before
    /// the client dials, and its handle is a future the caller must await,
    /// which is the difference between a joined thread and a leaked one. The
    /// closure returns its I/O failure instead of unwrapping it so a refused
    /// connection surfaces as a test error rather than a background panic.
    ///
    /// The body is owned rather than a `&'static str`: the ceiling tests send
    /// bodies larger than the preview, which are built rather than written out.
    fn serve_once(
        body: impl Into<String>,
    ) -> std::io::Result<(u16, lgwks_std::task::JoinHandle<std::io::Result<()>>)> {
        let body = body.into();
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let handle = lgwks_std::task::spawn_blocking(move || -> std::io::Result<()> {
            let (mut stream, _) = listener.accept()?;
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
        Ok((port, handle))
    }

    /// Poll the loopback server serving `body`.
    ///
    /// One poll and a joined fixture per call, so a ceiling test reads as the
    /// body it serves and the state it produced.
    fn poll_body(body: impl Into<String>) -> PollResult {
        use crate::verb::Observe;
        let (port, server) = serve_once(body)?;
        let state = lgwks_std::task::block_on(
            Endpoint::new(format!("http://127.0.0.1:{port}/")).poll((net_auth()?, ())),
        )?;
        lgwks_std::task::block_on(server)?;
        Ok(state)
    }

    #[test]
    fn poll_reports_reachable_loopback() -> TestResult {
        use crate::verb::Observe;
        let (port, server) = serve_once("alive")?;
        let state = lgwks_std::task::block_on(
            Endpoint::new(format!("http://127.0.0.1:{port}/")).poll((net_auth()?, ())),
        )?;
        assert!(
            state.reachable,
            "a loopback server that answered must report reachable"
        );
        assert_eq!(state.status_code, 200);
        assert_eq!(state.body, "alive");
        lgwks_std::task::block_on(server)?;
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
            state.reachable,
            "a server that answered with a large body is reachable"
        );
        assert_eq!(
            state.status_code, 200,
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
        use crate::verb::Observe;
        let payload = "x".repeat(BODY_PREVIEW_BYTES.saturating_mul(256));
        let (port, server) = serve_once(payload)?;
        let state = lgwks_std::task::block_on(
            Endpoint::new(format!("http://127.0.0.1:{port}/")).poll((net_auth()?, ())),
        )?;
        assert!(
            state.reachable,
            "a server that answered with a huge body is reachable"
        );
        assert_eq!(
            state.body.chars().count(),
            BODY_PREVIEW,
            "the preview stops at BODY_PREVIEW characters"
        );
        let served = lgwks_std::task::block_on(server);
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
            state.reachable,
            "a multibyte body is still a reachable server"
        );
        assert_eq!(state.status_code, 200);
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
            !state.reachable,
            "a refused connection must report unreachable, not error"
        );
        assert_eq!(state.status_code, 0);
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
