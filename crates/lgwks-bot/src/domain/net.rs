//! `net` owns the network endpoint domain. Requires `bot.net`.
//!
//! Bound to [`lgwks_std::http`]: a poll is one GET with a 10s timeout.
//! Reachable endpoints report status and a 4 KiB body preview; transport
//! failure reports unreachable with status 0 (so a bot can condition on
//! downtime); a malformed URL is a spec bug and errors.

use std::time::Duration;

use lgwks_std::http::{self, Options};

use crate::cap::{Auth, Cap};
use crate::error::BotError;
use crate::verb;

/// Characters of body kept for observation.
///
/// Public because it bounds a public field: a consumer deciding whether
/// [`NetState::body`] is enough to work with needs the number, and stating it
/// in prose beside the field is how a documented cap drifts from the real one.
/// The field's doc links here instead, so the two cannot disagree.
pub const BODY_PREVIEW: usize = 4096;

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
    /// HTTP status code of the last probe. `0` means no response was received —
    /// the endpoint was unreachable — which is distinct from any real status a
    /// server can return.
    pub status_code: u16,
    /// Whether the endpoint is reachable.
    pub reachable: bool,
    /// Response body, truncated to [`BODY_PREVIEW`] characters for observation.
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
        let options = Options::default().timeout(Duration::from_secs(POLL_TIMEOUT_SECS));
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

    /// Tests here mix three error domains — socket I/O, `BotError`, and the
    /// executor — so the tests report `Box<dyn Error>` and propagate each with
    /// `?`, rather than reducing every failure to an unwind.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn net_auth() -> Result<Auth, BotError> {
        GrantSet::empty().grant(Cap::net()).issue(&[Cap::net()])
    }

    /// Serve exactly one HTTP request on an ephemeral loopback port.
    ///
    /// `lgwks_std::task::spawn_blocking` rather than a raw `std::thread::spawn`:
    /// it starts the thread immediately, so the listener is accepting before
    /// the client dials, and its handle is a future the caller must await —
    /// which is the difference between a joined thread and a leaked one. The
    /// closure returns its I/O failure instead of unwrapping it so a refused
    /// connection surfaces as a test error rather than a background panic.
    fn serve_once(
        body: &'static str,
    ) -> std::io::Result<(u16, lgwks_std::task::JoinHandle<std::io::Result<()>>)> {
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
