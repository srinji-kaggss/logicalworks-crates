//! The driver's HTTP edge: axum in, the world's answer out (ADR 0006 R4).
//!
//! [`serve`] runs axum's router on the process driver. Every request is handed
//! to a [`RequestFeed`], the world's request system, which answers it from the
//! state of the request's own tenant on the worker that read it, and the edge
//! writes that answer back as the response. axum carries the connection, the
//! HTTP parsing and the router, which rebuilding would not improve; tenancy
//! and the answer stay in the world.
//!
//! A request names its tenant in its path, `/t/{tenant}/{rest}`, and the world
//! sees `/{rest}`. What the edge answers itself, without the handler:
//!
//! | Status | When |
//! |---|---|
//! | `400` | the path names no tenant, or a tenant that is not a valid name |
//! | `413` | the body is over [`MAX_BODY_BYTES`] |
//! | `503` | the feed is closed, or the feed holds 65,536 tenants and this is a new one |
//!
//! No request waits on another thread, so the edge keeps no answer deadline:
//! the earlier design queued each request to one world thread and bounded the
//! wait for its answer, and the hop cost 15% of throughput against plain axum.
//!
//! While it serves, [`serve`] also drives the world on the calling thread: the
//! world ticks when one of its sources is woken (a handler holding a
//! [`WakeHandle`](crate::ecs::WakeHandle) wakes it) or an `Every` deadline or
//! retry backoff comes due on its clock, and not otherwise, so an idle world
//! costs nothing (ADR 0006 R5).
//!
//! [`serve`] returns when its [`EdgeStop`] is stopped: axum stops accepting and
//! drains its connections, the feed closes, and the world ticks once so its
//! systems see the closing state.

use core::fmt;
use std::future::IntoFuture as _;
use std::io;
use std::net::TcpListener;

use lgwks_deps::axum::{
    Router,
    body::{Body, Bytes, HttpBody as _, to_bytes},
    extract::{Request as HttpRequest, State},
    http::StatusCode,
    response::{IntoResponse, Response as HttpResponse},
};

use crate::ecs::{EcsBot, NextTick, Request, RequestFeed};
use crate::error::BotError;
use crate::rt::runtime::driver;
use crate::rt::sync::CancellationToken;

/// The largest request body the edge reads, in bytes.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Why [`serve`] ended other than by its stop.
#[derive(Debug)]
#[non_exhaustive]
pub enum EdgeError {
    /// The listener or the driver failed.
    Io(io::Error),
    /// The world's closing tick failed.
    World(BotError),
}

impl fmt::Display for EdgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Io(ref cause) => write!(f, "the HTTP edge's listener failed: {cause}"),
            Self::World(ref cause) => write!(f, "the world behind the HTTP edge failed: {cause}"),
        }
    }
}

impl std::error::Error for EdgeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Io(ref cause) => Some(cause),
            Self::World(ref cause) => Some(cause),
        }
    }
}

/// The signal that ends a [`serve`]. Cloning is a handle to one signal.
#[derive(Clone, Debug, Default)]
pub struct EdgeStop {
    /// Cancelled once, by [`EdgeStop::stop`].
    token: CancellationToken,
}

impl EdgeStop {
    /// A signal not yet given.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// End the [`serve`] this signal was given to.
    pub fn stop(&self) {
        self.token.cancel();
    }
}

/// Serve `listener` into `world`'s request system `feed` until `stop` is
/// given, blocking the calling thread.
///
/// # Errors
///
/// [`EdgeError::Io`] when the listener cannot be served or the driver cannot
/// start, and [`EdgeError::World`] when the closing tick fails. The feed is
/// closed however it ends.
pub fn serve(
    world: &mut EcsBot,
    feed: &RequestFeed,
    listener: TcpListener,
    stop: &EdgeStop,
) -> Result<(), EdgeError> {
    let served = serve_http(world, feed, listener, stop);
    feed.close();
    let ticked = world.tick().map(drop).map_err(EdgeError::World);
    served.and(ticked)
}

/// axum on the process driver, and the world driven beside it on this thread,
/// until `stop` is given and every connection has drained.
fn serve_http(
    world: &mut EcsBot,
    feed: &RequestFeed,
    listener: TcpListener,
    stop: &EdgeStop,
) -> Result<(), EdgeError> {
    listener.set_nonblocking(true).map_err(EdgeError::Io)?;
    let handle = driver().map_err(EdgeError::Io)?;
    let router = Router::new().fallback(ingress).with_state(feed.clone());
    let token = &stop.token;
    handle
        .block_on(async move {
            let listener = lgwks_deps::tokio::net::TcpListener::from_std(listener)?;
            let http = lgwks_deps::axum::serve(listener, router)
                .with_graceful_shutdown(token.clone().cancelled_owned())
                .into_future();
            let (served, ()) = lgwks_deps::tokio::join!(http, drive(world, token));
            served
        })
        .map_err(EdgeError::Io)
}

/// Tick `world` whenever its next tick is due, until `stop` is cancelled.
///
/// Waits on the world's own clock for a deadline and on its wakes, so a
/// virtual clock drives it without real time passing. A tick that fails is
/// logged and the world driven on: one held transition does not stop the edge.
async fn drive(world: &mut EcsBot, stop: &CancellationToken) {
    loop {
        match world.next_tick() {
            NextTick::Now => {}
            NextTick::In(wait) => {
                lgwks_deps::tokio::select! {
                    () = stop.cancelled() => return,
                    () = world.clock().sleep(wait) => {}
                    () = world.woken() => {}
                }
            }
            NextTick::Idle => {
                lgwks_deps::tokio::select! {
                    () = stop.cancelled() => return,
                    () = world.woken() => {}
                }
            }
        }
        if stop.is_cancelled() {
            return;
        }
        if let Err(error) = world.tick_async().await {
            lgwks_std::trace::debug!(%error, "edge: a driven tick did not finish clean");
        }
    }
}

/// The one handler: every request is answered by the world's request system.
async fn ingress(State(feed): State<RequestFeed>, request: HttpRequest) -> HttpResponse {
    let (parts, body) = request.into_parts();
    let Some((tenant, rest)) = tenant_of(parts.uri.path()) else {
        return answer(400, b"name a tenant: /t/{tenant}/...".to_vec());
    };
    // A body the framing says is empty is not read: most requests carry none.
    let body = if body.size_hint().exact() == Some(0) {
        Bytes::new()
    } else {
        match to_bytes(body, MAX_BODY_BYTES).await {
            Ok(bytes) => bytes,
            Err(_over) => return answer(413, b"the body is over the edge's ceiling".to_vec()),
        }
    };
    let Ok(event) = Request::new(tenant, parts.method.as_str(), rest, &body) else {
        return answer(400, b"the tenant is not a valid name".to_vec());
    };
    match feed.answer(&event) {
        Ok(response) => answer(response.status(), response.into_body()),
        Err(refusal) => answer(503, refusal.to_string().into_bytes()),
    }
}

/// `/t/{tenant}/{rest}` split into the tenant and `/{rest}`.
fn tenant_of(path: &str) -> Option<(&str, &str)> {
    let named = path.strip_prefix("/t/")?;
    match named.find('/') {
        Some(at) => Some(named.split_at(at)),
        None => Some((named, "/")),
    }
}

/// An HTTP response with `status` and `body`; a status HTTP does not define is
/// answered `500`.
fn answer(status: u16, body: Vec<u8>) -> HttpResponse {
    match StatusCode::from_u16(status) {
        Ok(status) => (status, Body::from(body)).into_response(),
        Err(_invalid) => {
            lgwks_std::trace::warn!(status, "edge: the world answered an undefined status");
            (StatusCode::INTERNAL_SERVER_ERROR, Body::from(body)).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tenant_of;

    #[test]
    fn a_tenant_path_splits_into_the_tenant_and_the_rest() {
        assert_eq!(tenant_of("/t/acme/count"), Some(("acme", "/count")));
        assert_eq!(tenant_of("/t/acme"), Some(("acme", "/")));
        assert_eq!(tenant_of("/count"), None);
    }
}
