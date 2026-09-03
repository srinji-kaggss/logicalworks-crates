//! `net` owns the network endpoint domain. Requires `bot.net`.

use crate::cap::Cap;
use crate::error::BotError;
use crate::verb;

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

    fn poll(&self) -> Result<NetState, BotError> {
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            cause: format!("polling {} — binding required", self.url),
        })
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

    fn query(&self, _: &()) -> Result<NetState, BotError> {
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            cause: format!("querying {} — binding required", self.url),
        })
    }

    fn domain_id(&self) -> &str {
        "net::endpoint"
    }
}

/// Convenience: create a network endpoint observer.
pub fn endpoint(url: impl Into<String>) -> Endpoint {
    Endpoint::new(url)
}
