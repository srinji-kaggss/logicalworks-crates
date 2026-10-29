//! Requests into the world: the HTTP edge's events (ADR 0006 R4).
//!
//! A [`RequestFeed`] is the world's request system and every tenant's state.
//! The edge hands it each request and writes back the [`Response`] it settles.
//! The system runs on the thread that received the request, holding only the
//! partition its tenant lives in, so requests for tenants in different
//! partitions run in parallel and none of them waits on a hop to another
//! thread. A queue to one world thread was measured first and lost: the hop
//! there and back cost 15% of throughput against plain axum (ADR 0006 R4).
//!
//! Tenancy is the feed's, not the handler's: the system keeps one state per
//! tenant and hands a request only its own tenant's, so no handler can read a
//! neighbour's state however it is written. The tenants a feed holds are
//! bounded ([`MAX_TENANTS`]); a request for one more is answered `503` without
//! reaching the handler.
//!
//! A handler is synchronous and holds its tenant's partition for exactly its
//! own call, never across an `.await`, so a request waits at most for the
//! requests of its own partition already running, one per driver worker.
//!
//! The feed names no driver (ADR 0006 decision 4): partitions are `std`
//! locks and a request is answered by a plain call.

use core::fmt;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(test)]
mod sim_requests;

/// The most tenants one feed keeps state for.
pub(crate) const MAX_TENANTS: usize = 65_536;

/// The longest tenant name a request may carry, in bytes.
pub(crate) const MAX_TENANT_BYTES: usize = 64;

/// How many partitions a feed splits its tenants over; a power of two no
/// larger than 256.
const PARTITIONS: usize = 64;

/// One request, as the world's request system receives it: a view of what
/// the edge read, so answering one copies nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request<'a> {
    /// Who the request is for; validated by [`Request::new`].
    tenant: &'a str,
    /// The HTTP method, upper case.
    method: &'a str,
    /// The path below the tenant prefix, starting with `/`.
    path: &'a str,
    /// The body, read whole by the edge under its own ceiling.
    body: &'a [u8],
}

/// Why a request was not answered by the handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) enum RequestRefused {
    /// The tenant is empty, longer than [`MAX_TENANT_BYTES`], or holds a byte
    /// outside `[A-Za-z0-9_-]`.
    Tenant,
    /// The feed was closed.
    Closed,
}

impl fmt::Display for RequestRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match *self {
            Self::Tenant => "the tenant is not a name of 1..=64 bytes in [A-Za-z0-9_-]",
            Self::Closed => "the request feed is closed",
        })
    }
}

impl std::error::Error for RequestRefused {}

impl<'a> Request<'a> {
    /// A request for `tenant`.
    ///
    /// # Errors
    ///
    /// [`RequestRefused::Tenant`] when `tenant` is not a valid tenant name.
    pub(crate) fn new(
        tenant: &'a str,
        method: &'a str,
        path: &'a str,
        body: &'a [u8],
    ) -> Result<Self, RequestRefused> {
        let valid = !tenant.is_empty()
            && tenant.len() <= MAX_TENANT_BYTES
            && tenant
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
        if !valid {
            lgwks_std::trace::debug!(tenant, "request: refused tenant name");
            return Err(RequestRefused::Tenant);
        }
        Ok(Self {
            tenant,
            method,
            path,
            body,
        })
    }

    /// Who the request is for.
    #[must_use]
    pub const fn tenant(&self) -> &'a str {
        self.tenant
    }

    /// The verb as the client sent it on the request line, `GET` or `POST`
    /// for most handlers; the edge passes it through unchanged.
    #[must_use]
    pub const fn method(&self) -> &'a str {
        self.method
    }

    /// The path below the tenant prefix.
    #[must_use]
    pub const fn path(&self) -> &'a str {
        self.path
    }

    /// Every byte the client sent after the head, read whole by the edge up to
    /// its 1 MiB ceiling; empty when the framing declared none.
    #[must_use]
    pub const fn body(&self) -> &'a [u8] {
        self.body
    }
}

/// The world's answer to one request: the settled effect the edge writes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The HTTP status.
    status: u16,
    /// The body.
    body: Vec<u8>,
}

impl Response {
    /// An answer with `status` and `body`.
    #[must_use]
    pub const fn new(status: u16, body: Vec<u8>) -> Self {
        Self { status, body }
    }

    /// The HTTP status.
    #[must_use]
    pub(crate) const fn status(&self) -> u16 {
        self.status
    }

    /// The body, owned.
    #[must_use]
    pub(crate) fn into_body(self) -> Vec<u8> {
        self.body
    }
}

/// The counts a feed has recorded. Each is read on its own, so under load
/// they are a moment's reading, not one consistent cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct RequestCounts {
    /// Requests the handler answered.
    pub settled: u64,
    /// Requests refused before the handler: the feed closed, or the tenant
    /// ceiling reached.
    pub refused: u64,
    /// Tenants the feed holds state for.
    pub tenants: usize,
}

/// A bound system, answering a request in its partition on any thread.
type System = Box<dyn Fn(usize, &Request<'_>) -> Answer + Send + Sync>;

/// One partition's tenants and their states, behind its lock.
type Partition<S> = Mutex<BTreeMap<Box<str>, S>>;

/// What the system did with one request.
enum Answer {
    /// The handler answered.
    Settled(Response),
    /// The tenant ceiling refused it.
    Full,
}

/// A counter on a cache line of its own, so partitions counting at once do
/// not contend for one line.
#[derive(Default)]
#[repr(align(128))]
struct Counter(AtomicU64);

/// The world's request system and every tenant's state.
///
/// Cloning is a handle: the edge and the world share one feed.
#[derive(Clone)]
pub struct RequestFeed {
    /// The system, its partitions' counts and the closed flag.
    shared: Arc<Shared>,
}

/// What a feed's handles share.
struct Shared {
    /// The bound system.
    system: System,
    /// Requests each partition settled.
    settled: Box<[Counter]>,
    /// Requests refused before the handler.
    refused: AtomicU64,
    /// Tenants held across every partition.
    tenants: Arc<AtomicUsize>,
    /// Whether [`RequestFeed::close`] was called.
    closed: AtomicBool,
}

impl RequestFeed {
    /// A feed answering every request with `serve`, given the state of the
    /// request's own tenant, created by `S::default()` on that tenant's first
    /// request.
    #[must_use]
    pub fn new<S: Default + Send + 'static>(
        serve: impl Fn(&Request<'_>, &mut S) -> Response + Send + Sync + 'static,
    ) -> Self {
        let partitions: Box<[Partition<S>]> = (0..PARTITIONS)
            .map(|_| Mutex::new(BTreeMap::new()))
            .collect();
        let tenants = Arc::new(AtomicUsize::new(0));
        let held = Arc::clone(&tenants);
        let system: System = Box::new(move |at: usize, request: &Request<'_>| {
            let Some(partition) = partitions.get(at) else {
                return Answer::Full;
            };
            let mut partition = match partition.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            // A known tenant is found by borrow; only a tenant's first request
            // copies its name into the map.
            if let Some(state) = partition.get_mut(request.tenant()) {
                return Answer::Settled(serve(request, state));
            }
            if held.fetch_add(1, Ordering::AcqRel) >= MAX_TENANTS {
                held.fetch_sub(1, Ordering::AcqRel);
                return Answer::Full;
            }
            let state = partition.entry(request.tenant.into()).or_default();
            Answer::Settled(serve(request, state))
        });
        Self {
            shared: Arc::new(Shared {
                system,
                settled: (0..PARTITIONS).map(|_| Counter::default()).collect(),
                refused: AtomicU64::new(0),
                tenants,
                closed: AtomicBool::new(false),
            }),
        }
    }

    /// Answer `request` from its own tenant's state, on the calling thread.
    ///
    /// # Errors
    ///
    /// [`RequestRefused::Closed`] once the feed is closed. A request for a
    /// tenant past [`MAX_TENANTS`] is answered `503` here, not refused.
    pub(crate) fn answer(&self, request: &Request<'_>) -> Result<Response, RequestRefused> {
        let shared = &*self.shared;
        if shared.closed.load(Ordering::Acquire) {
            shared.refused.fetch_add(1, Ordering::Relaxed);
            lgwks_std::trace::debug!(tenant = request.tenant(), "request feed: closed, refusing");
            return Err(RequestRefused::Closed);
        }
        let at = partition_of(request.tenant());
        match (shared.system)(at, request) {
            Answer::Settled(response) => {
                if let Some(counter) = shared.settled.get(at) {
                    counter.0.fetch_add(1, Ordering::Relaxed);
                }
                Ok(response)
            }
            Answer::Full => {
                shared.refused.fetch_add(1, Ordering::Relaxed);
                lgwks_std::trace::warn!(
                    tenant = request.tenant(),
                    "request feed: tenant ceiling reached"
                );
                Ok(Response::new(503, b"tenant ceiling reached".to_vec()))
            }
        }
    }

    /// Refuse every later request. A request already inside the handler
    /// finishes.
    pub(crate) fn close(&self) {
        self.shared.closed.store(true, Ordering::Release);
    }

    /// The counts this feed has recorded.
    #[must_use]
    pub fn counts(&self) -> RequestCounts {
        let shared = &*self.shared;
        RequestCounts {
            settled: shared
                .settled
                .iter()
                .map(|counter| counter.0.load(Ordering::Relaxed))
                .fold(0, u64::saturating_add),
            refused: shared.refused.load(Ordering::Relaxed),
            tenants: shared.tenants.load(Ordering::Acquire),
        }
    }
}

impl fmt::Debug for RequestFeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestFeed")
            .field("counts", &self.counts())
            .finish_non_exhaustive()
    }
}

/// The partition `tenant` lives in: FNV-1a over its bytes, masked.
fn partition_of(tenant: &str) -> usize {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in tenant.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
    }
    // FNV-1a's low byte mixes every input byte, and 256 covers the partitions.
    usize::from(hash.to_le_bytes()[0]) & (PARTITIONS - 1)
}
