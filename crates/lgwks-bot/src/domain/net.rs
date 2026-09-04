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
const BODY_PREVIEW: usize = 4096;

/// Seconds a poll waits for the endpoint.
const POLL_TIMEOUT_SECS: u64 = 10;

/// Observe or query a network endpoint. Supports Observe, Execute, Query.
pub struct Endpoint {
    url: String,
    caps: Vec<Cap>,
}

/// Network state returned by observation or query.
#[derive(Debug, Clone)]
pub struct NetState {
    /// HTTP status code of the last probe.
    pub status_code: u16,
    /// Whether the endpoint is reachable.
    pub reachable: bool,
    /// Response body (truncated for observation).
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

    fn poll(&self, call: (Auth, ())) -> Result<NetState, BotError> {
        call.0.check(self.required_caps())?;
        let options = Options {
            timeout: Duration::from_secs(POLL_TIMEOUT_SECS),
            ..Options::default()
        };
        match http::get_with(&self.url, &options) {
            Ok(response) => Ok(NetState {
                status_code: response.status,
                reachable: true,
                body: response
                    .text()
                    .unwrap_or_default()
                    .chars()
                    .take(BODY_PREVIEW)
                    .collect(),
            }),
            Err(http::Error::InvalidUrl(_)) => Err(BotError::DomainError {
                domain: self.domain_id().into(),
                cause: format!("invalid endpoint URL {}", self.url),
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

    fn query(&self, call: (Auth, &())) -> Result<NetState, BotError> {
        let (auth, _) = call;
        verb::Observe::poll(self, (auth, ()))
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
    use std::thread;

    fn net_auth() -> Auth {
        GrantSet::empty()
            .grant(Cap::net())
            .issue(&[Cap::net()])
            .expect("net granted")
    }

    fn serve_once(body: &'static str) -> (u16, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0u8; 1024];
            let mut head = Vec::new();
            loop {
                let n = stream.read(&mut request).unwrap();
                head.extend_from_slice(&request[..n]);
                if head.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
        });
        (port, handle)
    }

    #[test]
    fn poll_reports_reachable_loopback() {
        use crate::verb::Observe;
        let (port, server) = serve_once("alive");
        let state = Endpoint::new(format!("http://127.0.0.1:{port}/"))
            .poll((net_auth(), ()))
            .unwrap();
        assert!(state.reachable);
        assert_eq!(state.status_code, 200);
        assert_eq!(state.body, "alive");
        server.join().unwrap();
    }

    #[test]
    fn poll_reports_unreachable_closed_port() {
        use crate::verb::Observe;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let state = Endpoint::new(format!("http://127.0.0.1:{port}/"))
            .poll((net_auth(), ()))
            .unwrap();
        assert!(!state.reachable);
        assert_eq!(state.status_code, 0);
    }

    #[test]
    fn poll_rejects_malformed_url_as_spec_bug() {
        use crate::verb::Observe;
        let error = Endpoint::new("not a url")
            .poll((net_auth(), ()))
            .unwrap_err();
        assert!(matches!(error, BotError::DomainError { .. }));
    }

    #[test]
    fn poll_without_net_proof_is_denied() {
        use crate::verb::Observe;
        let vacuous = GrantSet::empty().issue(&[]).expect("empty coverage");
        let error = Endpoint::new("http://127.0.0.1:9/")
            .poll((vacuous, ()))
            .unwrap_err();
        assert!(matches!(error, BotError::CapabilityDenied { .. }));
    }
}
