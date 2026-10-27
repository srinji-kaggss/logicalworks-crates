//! The pooled client's keep-alive behavior, counted connection by connection.
//!
//! The server below answers every request on a connection until the peer
//! goes away, and counts the connections it accepted. A pooled burst must
//! cost one connection; the one-shot path must cost one per call; an
//! expired pool must behave like the one-shot path.

use super::*;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

/// A loopback server that counts what it accepted and saw.
///
/// Keep-alive by construction: replies carry a `Content-Length` and no
/// `Connection` header, so HTTP/1.1 keeps the connection open for the next
/// request and a client that pools finds it still there. `stop` drops the
/// wakeup connection uncounted, then joins the accept thread and every
/// connection thread the test started.
struct KeepAlive {
    /// The bound port.
    port: u16,
    /// Connections accepted so far.
    accepts: Arc<AtomicUsize>,
    /// Requests served so far.
    requests: Arc<AtomicUsize>,
    /// `x-tenant` values seen, in arrival order.
    tenants: Arc<std::sync::Mutex<Vec<String>>>,
    /// Set to stop the accept loop; the waiter is woken by one connection.
    stop: Arc<AtomicBool>,
    /// The accept thread, joined by `stop`.
    server: thread::JoinHandle<()>,
    /// The connection threads, joined by `stop`.
    handlers: Arc<std::sync::Mutex<Vec<thread::JoinHandle<()>>>>,
}

/// Serve one connection until the peer goes away, counting requests.
fn serve_connection(
    mut stream: TcpStream,
    requests: Arc<AtomicUsize>,
    tenants: Arc<std::sync::Mutex<Vec<String>>>,
) {
    loop {
        let head = match read_request_head(&mut stream) {
            Ok(head) => head,
            Err(_) => return,
        };
        if head.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&head).into_owned();
        if let Some(tenant) = text
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|entry| entry.0.trim().eq_ignore_ascii_case("x-tenant"))
        {
            match tenants.lock() {
                Ok(mut tenants) => tenants.push(tenant.1.trim().to_owned()),
                Err(_) => return,
            }
        }
        requests.fetch_add(1, Ordering::SeqCst);
        if stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .is_err()
        {
            return;
        }
    }
}

/// Start the keep-alive server and hand back its handle.
fn serve_keep_alive() -> std::io::Result<KeepAlive> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let accepts = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(AtomicUsize::new(0));
    let tenants = Arc::new(std::sync::Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let handlers = Arc::new(std::sync::Mutex::new(Vec::new()));
    let server = {
        let accepts = Arc::clone(&accepts);
        let requests = Arc::clone(&requests);
        let tenants = Arc::clone(&tenants);
        let stop = Arc::clone(&stop);
        let handlers = Arc::clone(&handlers);
        fixture_thread(move || {
            loop {
                let (stream, _) = match listener.accept() {
                    Ok(served) => served,
                    Err(_) => return,
                };
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                accepts.fetch_add(1, Ordering::SeqCst);
                let requests = Arc::clone(&requests);
                let tenants = Arc::clone(&tenants);
                let handler = match fixture_thread(move || {
                    serve_connection(stream, requests, tenants);
                }) {
                    Ok(handler) => handler,
                    Err(_) => return,
                };
                match handlers.lock() {
                    Ok(mut handlers) => handlers.push(handler),
                    Err(_) => return,
                };
            }
        })?
    };
    Ok(KeepAlive {
        port,
        accepts,
        requests,
        tenants,
        stop,
        server,
        handlers,
    })
}

impl KeepAlive {
    /// The base URL of this server.
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    /// Requests served so far.
    fn served(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    /// Connections accepted so far.
    fn accepted(&self) -> usize {
        self.accepts.load(Ordering::SeqCst)
    }

    /// `x-tenant` values seen, in arrival order.
    fn seen_tenants(&self) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let tenants = match self.tenants.lock() {
            Ok(tenants) => tenants,
            Err(_) => {
                #[cfg(feature = "trace")]
                crate::trace::error!("the tenant log is poisoned");
                return Err("the tenant log is poisoned".into());
            }
        };
        Ok(tenants.clone())
    }

    /// Join one fixture thread, naming what it was.
    ///
    /// A panic payload is read, never echoed raw: it can carry anything the
    /// fixture thread formatted, and the test failure names the fixture
    /// instead.
    fn join_handler(
        handler: thread::JoinHandle<()>,
        what: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        match handler.join() {
            Ok(()) => Ok(()),
            Err(payload) => {
                let message = match payload.downcast_ref::<&str>() {
                    Some(text) => (*text).to_owned(),
                    None => match payload.downcast_ref::<String>() {
                        Some(text) => text.clone(),
                        None => "a payload that is not text".to_owned(),
                    },
                };
                #[cfg(feature = "trace")]
                crate::trace::error!(%message, "a keep-alive fixture thread panicked");
                Err(format!("{what} panicked: {message}").into())
            }
        }
    }

    /// Stop the server and join every thread it started.
    ///
    /// The client is dropped first, so pooled idle connections are already
    /// closed and their handlers have seen EOF: every join below terminates.
    fn stop(self) -> Result<(), Box<dyn std::error::Error>> {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop; the wakeup connection is refused by the stop
        // flag above, so it is never counted or served.
        drop(TcpStream::connect(("127.0.0.1", self.port))?);
        Self::join_handler(self.server, "the keep-alive server")?;
        let mut handlers = match self.handlers.lock() {
            Ok(handlers) => handlers,
            Err(_) => {
                #[cfg(feature = "trace")]
                crate::trace::error!("the keep-alive handler list is poisoned");
                return Err("the keep-alive handler list is poisoned".into());
            }
        };
        for (index, handler) in handlers.drain(..).enumerate() {
            Self::join_handler(handler, &format!("keep-alive connection {index}"))?;
        }
        Ok(())
    }
}

/// A pooled burst of eight sequential exchanges costs one connection.
#[test]
fn pooled_client_reuses_one_connection_for_a_sequential_burst()
-> Result<(), Box<dyn std::error::Error>> {
    let keeper = serve_keep_alive()?;
    let url = keeper.url();
    let client = Client::with_pool(Options::default(), PoolConfig::default())?;
    for _ in 0..8 {
        let response = client.get_with(&url, client.options())?;
        assert_eq!(response.status, 200, "every pooled exchange answers");
        assert_eq!(response.body(), b"ok", "every pooled exchange is whole");
    }
    drop(client);
    let (accepted, served) = (keeper.accepted(), keeper.served());
    keeper.stop()?;
    assert_eq!(served, 8, "every exchange reached the server");
    assert_eq!(
        accepted, 1,
        "eight sequential exchanges through one client cost one connection, cost {accepted}"
    );
    Ok(())
}

/// The one-shot path still costs one connection per call.
#[test]
fn one_shot_functions_open_one_connection_per_call() -> Result<(), Box<dyn std::error::Error>> {
    let keeper = serve_keep_alive()?;
    let url = keeper.url();
    for _ in 0..4 {
        let response = get_with(&url, &quiet())?;
        assert_eq!(response.status, 200, "every one-shot exchange answers");
    }
    let (accepted, served) = (keeper.accepted(), keeper.served());
    keeper.stop()?;
    assert_eq!(served, 4, "every exchange reached the server");
    assert_eq!(
        accepted, 4,
        "four one-shot exchanges cost four connections, cost {accepted}"
    );
    Ok(())
}

/// Four exchanges through a client built on `pool` cost four connections.
fn every_exchange_reopens(pool: PoolConfig, why: &str) -> Result<(), Box<dyn std::error::Error>> {
    let keeper = serve_keep_alive()?;
    let url = keeper.url();
    let client = Client::with_pool(Options::default(), pool)?;
    for _ in 0..4 {
        let response = client.get_with(&url, client.options())?;
        assert_eq!(response.status, 200, "every exchange answers");
    }
    drop(client);
    let (accepted, served) = (keeper.accepted(), keeper.served());
    keeper.stop()?;
    assert_eq!(served, 4, "every exchange reached the server");
    assert_eq!(accepted, 4, "{why}, cost {accepted}");
    Ok(())
}

/// An idle age of zero expires every connection: the pooled path behaves
/// like the one-shot path, without a clock anywhere in the test.
#[test]
fn zero_idle_age_never_reuses_a_connection() -> Result<(), Box<dyn std::error::Error>> {
    every_exchange_reopens(
        PoolConfig::default().max_idle_age(std::time::Duration::ZERO),
        "an immediately expiring pool reopens every time",
    )
}

/// A pool of zero disables pooling: every exchange opens its own connection.
#[test]
fn zero_pool_size_disables_pooling() -> Result<(), Box<dyn std::error::Error>> {
    every_exchange_reopens(
        PoolConfig::default().max_idle_connections(0),
        "a zero pool reopens every time",
    )
}

/// Clones share one pool: exchanges through either clone reuse the same
/// connection.
#[test]
fn clones_share_one_pool() -> Result<(), Box<dyn std::error::Error>> {
    let keeper = serve_keep_alive()?;
    let url = keeper.url();
    let client = Client::with_pool(Options::default(), PoolConfig::default())?;
    let second = client.clone();
    for _ in 0..3 {
        assert_eq!(
            client.get_with(&url, client.options())?.status,
            200,
            "the client answers"
        );
        assert_eq!(
            second.get_with(&url, second.options())?.status,
            200,
            "its clone answers"
        );
    }
    drop(client);
    drop(second);
    let (accepted, served) = (keeper.accepted(), keeper.served());
    keeper.stop()?;
    assert_eq!(served, 6, "every exchange reached the server");
    assert_eq!(
        accepted, 1,
        "six exchanges through two clones cost one connection, cost {accepted}"
    );
    Ok(())
}

/// Tenant headers stay isolated when the connection is shared.
#[test]
fn tenant_headers_stay_isolated_through_one_client() -> Result<(), Box<dyn std::error::Error>> {
    let keeper = serve_keep_alive()?;
    let url = keeper.url();
    let client = Client::with_pool(Options::default(), PoolConfig::default())?;
    for tenant in ["tenant-a", "tenant-b", "tenant-a"] {
        let options = quiet().header("X-Tenant", tenant);
        let response = client.get_with(&url, &options)?;
        assert_eq!(response.status, 200, "every tenant reaches the endpoint");
    }
    drop(client);
    let seen = keeper.seen_tenants()?;
    let served = keeper.served();
    keeper.stop()?;
    assert_eq!(served, 3, "every exchange reached the server");
    assert_eq!(
        seen,
        vec!["tenant-a", "tenant-b", "tenant-a"],
        "each request carries only its own tenant: {seen:?}"
    );
    Ok(())
}
