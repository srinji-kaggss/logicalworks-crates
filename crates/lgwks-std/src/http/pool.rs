//! A caller-owned pooled client: one agent, many exchanges.
//!
//! The free functions ([`get`](super::get), [`post`](super::post) and their
//! `_with` forms) build a fresh agent per call. That is the documented cost
//! model for a single shot: a burst pays a fresh TCP and TLS handshake per
//! request and keep-alive is never reused. [`Client`] is the other path. It
//! holds one [`ureq::Agent`] across exchanges, so a second request to an
//! origin the client already reached reuses the idle keep-alive connection
//! instead of opening another one.
//!
//! Pooling is identity-aware. Connections are pooled per origin, and a
//! request whose agent-level settings (timeouts, identity, pool sizes) differ
//! from the client's runs on a one-shot agent instead: still correct, just
//! unpooled for that call. Two clones of one client share the same pool; two
//! clients built separately share nothing.
//!
//! ```rust
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use lgwks_std::http::{Client, Options, PoolConfig};
//! let pool = PoolConfig::default().max_idle_connections_per_host(4);
//! let client = Client::with_pool(Options::default(), pool)?;
//! assert_eq!(
//!     client.options().pool.max_idle_connections_per_host,
//!     4,
//!     "the client carries the pool it was built with"
//! );
//! # Ok(())
//! # }
//! ```
//!
//! [`Client`]: super::Client

use super::stream::StreamResponse;
use super::walk::Method;
use super::{Error, Options, Response};

/// How many idle connections a [`Client`](super::Client) keeps, and for how long.
///
/// Idle means a connection with its whole response read: only a fully
/// consumed exchange returns its socket to the pool, which is why streaming
/// bodies that are dropped half-read never pool. All three bounds mirror
/// ureq's own defaults, so a default client pools exactly like a default
/// ureq agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PoolConfig {
    /// Ceiling on idle connections across all origins. Zero disables pooling:
    /// every exchange opens its own connection even through a [`Client`].
    pub max_idle_connections: usize,
    /// Ceiling on idle connections per host and port pair.
    pub max_idle_connections_per_host: usize,
    /// How long an idle connection is kept before it is closed. Zero expires
    /// every connection the moment its exchange ends, which keeps the
    /// pooling path while reusing nothing.
    pub max_idle_age: std::time::Duration,
}

impl Default for PoolConfig {
    /// ureq's own pool defaults: ten idle connections overall, three per
    /// host, fifteen seconds of idleness.
    fn default() -> Self {
        Self {
            max_idle_connections: 10,
            max_idle_connections_per_host: 3,
            max_idle_age: std::time::Duration::from_secs(15),
        }
    }
}

impl PoolConfig {
    /// Ceiling on idle connections across all origins.
    #[must_use]
    pub fn max_idle_connections(mut self, max_idle_connections: usize) -> Self {
        self.max_idle_connections = max_idle_connections;
        self
    }

    /// Ceiling on idle connections per host and port pair.
    #[must_use]
    pub fn max_idle_connections_per_host(mut self, max_idle_connections_per_host: usize) -> Self {
        self.max_idle_connections_per_host = max_idle_connections_per_host;
        self
    }

    /// How long an idle connection is kept before it is closed.
    #[must_use]
    pub fn max_idle_age(mut self, max_idle_age: std::time::Duration) -> Self {
        self.max_idle_age = max_idle_age;
        self
    }
}

/// A blocking HTTP client that reuses keep-alive connections.
///
/// Built from [`Options`] once and then used for any number of exchanges:
/// `get_stream` sends with the construction options, `get_with` and
/// `post_with` take per-call options whose request-level settings (headers,
/// body ceiling and policy, redirect policy, deadline remainder) override
/// the client's for that call. Agent-level settings (timeouts, identity,
/// pool sizes) come from the construction options; a per-call value that
/// differs runs that call on a one-shot agent instead of the pool.
///
/// Cloning shares the pool: every clone reuses the same idle connections.
/// Dropping every clone closes every pooled connection.
#[derive(Debug, Clone)]
pub struct Client {
    /// The pooled agent. Shared across clones by ureq's own `Arc`.
    agent: ureq::Agent,
    /// The construction options: defaults for every exchange.
    options: Options,
}

impl Client {
    /// Build a client with the default pool.
    ///
    /// The TLS material in `options` is validated now, before any socket
    /// opens, so a bad file is a construction-time refusal rather than a
    /// first-request surprise.
    pub fn new(options: Options) -> Result<Self, Error> {
        Self::with_pool(options, PoolConfig::default())
    }

    /// Build a client with an explicit pool.
    pub fn with_pool(options: Options, pool: PoolConfig) -> Result<Self, Error> {
        let agent = super::exchange::agent_for(&options, &pool)?;
        Ok(Self {
            agent,
            options: options.pool(pool),
        })
    }

    /// The construction options every exchange defaults to.
    #[must_use]
    pub fn options(&self) -> &Options {
        &self.options
    }

    /// GET `url` with per-call `options`; pass [`Client::options`] for the
    /// construction ones.
    pub fn get_with(&self, url: &str, options: &Options) -> Result<Response, Error> {
        self.exchange(url, Method::Get, options)
    }

    /// POST `body` to `url` with `content_type` and per-call `options`; pass
    /// [`Client::options`] for the construction ones.
    pub fn post_with(
        &self,
        url: &str,
        content_type: &str,
        body: &[u8],
        options: &Options,
    ) -> Result<Response, Error> {
        self.exchange(url, Method::Post { content_type, body }, options)
    }

    /// Run one exchange on this client's agent, or on a one-shot agent when
    /// the per-call options change an agent-level setting.
    fn exchange(
        &self,
        url: &str,
        method: Method<'_>,
        options: &Options,
    ) -> Result<Response, Error> {
        if super::exchange::agent_settings_differ(&self.options, options) {
            let agent = super::exchange::agent_for(options, &options.pool)?;
            return super::walk::exchange_on(&agent, url, method, options);
        }
        super::walk::exchange_on(&self.agent, url, method, options)
    }

    /// GET `url` as a stream, with the construction options.
    ///
    /// Redirects are followed the buffered way; only the final body stays
    /// on the wire. Reading it to the end returns the connection to the
    /// pool; dropping it half-read does not.
    ///
    /// A server-sent event stream is the same call:
    /// [`StreamResponse::into_sse`] dispatches its body as events.
    pub fn get_stream(&self, url: &str) -> Result<StreamResponse, Error> {
        super::walk::exchange_stream_on(&self.agent, url, Method::Get, &self.options)
    }
}
