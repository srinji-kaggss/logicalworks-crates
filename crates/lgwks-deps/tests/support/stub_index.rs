//! A sparse-index registry on a loopback port, for driving `freshness` end to
//! end without the network.
//!
//! It speaks just enough HTTP/1.1 for `curl`: one request per connection, the
//! path routed to a canned reply, `Connection: close`. It records the paths it
//! served and the most requests it ever held open at once, which is how a test
//! reads the command's concurrency bound off the wire instead of off its own
//! counters.
//!
//! The optional gate withholds every response until that many requests are in
//! flight together, then opens for good. A client that runs fewer lookups at a
//! time than the gate needs never gets an answer, so the gate proves a lower
//! bound on concurrency without measuring a clock; the peak proves the upper.
//!
//! Every thread is owned: the acceptor and each handler are recorded as they
//! start, and `Drop` stops the listener and joins all of them.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{Builder, JoinHandle};

/// What the stub answers for one path.
#[derive(Debug, Clone)]
pub enum Reply {
    /// HTTP 200 with this body.
    Body(String),
    /// This status with an empty body.
    Status(u16),
    /// HTTP 200 with no length, then bytes until the client hangs up: a
    /// registry that never finishes, which only a cap on the read can end.
    Endless,
}

impl Reply {
    /// The complete HTTP response for this reply; for `Endless`, its head.
    fn render(&self) -> String {
        match *self {
            Self::Body(ref body) => format_response(200, body),
            Self::Status(code) => format_response(code, ""),
            Self::Endless => "HTTP/1.1 200 X\r\nConnection: close\r\n\r\n".to_owned(),
        }
    }
}

/// What the stub has seen, and whether the gate is open.
#[derive(Debug, Default)]
struct State {
    /// Requests that have arrived and not yet been answered.
    in_flight: usize,
    /// The most `in_flight` ever reached.
    peak: usize,
    /// Requests that have arrived, answered or not.
    arrivals: usize,
    /// Whether `arrivals` has reached the gate. Once open it stays open.
    open: bool,
    /// Whether the stub is shutting down.
    stop: bool,
    /// The path of every request answered, in answer order.
    served: Vec<String>,
}

/// The threads a stub owns, joined when it is dropped.
type Threads = Mutex<Vec<JoinHandle<()>>>;

/// What the acceptor and the handlers share.
struct Shared {
    /// The reply for each path; any other path is a 404.
    routes: BTreeMap<String, Reply>,
    /// How many requests must be in flight together before any is answered.
    gate: usize,
    /// The counters above.
    state: Mutex<State>,
    /// Signalled when the gate opens or the stub stops.
    changed: Condvar,
    /// Every thread started so far.
    threads: Threads,
}

impl Shared {
    /// The state, whether or not a handler panicked while holding it: a test
    /// that reads a counter after a failure must still see the counter.
    fn lock(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Records `handle` so the stub joins it.
    fn own(&self, handle: JoinHandle<()>) {
        match self.threads.lock() {
            Ok(mut held) => held.push(handle),
            Err(poisoned) => poisoned.into_inner().push(handle),
        }
    }

    /// The next thread to join, or `None` once all have been.
    fn disown(&self) -> Option<JoinHandle<()>> {
        match self.threads.lock() {
            Ok(mut held) => held.pop(),
            Err(poisoned) => poisoned.into_inner().pop(),
        }
    }
}

/// A running stub index. Dropping it stops the listener and joins every thread.
pub struct Stub {
    /// What the threads share.
    shared: Arc<Shared>,
    /// Where it listens.
    address: SocketAddr,
}

impl Stub {
    /// Starts a stub answering `routes`, withholding responses until `gate`
    /// requests are in flight together (`0` or `1` answers at once).
    pub fn start(routes: &[(&str, Reply)], gate: usize) -> Result<Self, Box<dyn Error>> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let shared = Arc::new(Shared {
            routes: routes
                .iter()
                .map(|route| (route.0.to_owned(), route.1.clone()))
                .collect(),
            gate,
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            threads: Mutex::new(Vec::new()),
        });
        let for_acceptor = Arc::clone(&shared);
        let acceptor = Builder::new()
            .name("stub-index-accept".to_owned())
            .spawn(move || accept(&listener, &for_acceptor))?;
        shared.own(acceptor);
        Ok(Self { shared, address })
    }

    /// The base URL to hand `--index`.
    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// The most requests that were in flight together.
    pub fn peak_in_flight(&self) -> usize {
        self.shared.lock().peak
    }

    /// The paths answered, sorted.
    pub fn served(&self) -> Vec<String> {
        let mut paths = self.shared.lock().served.clone();
        paths.sort();
        paths
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.shared.lock().stop = true;
        self.shared.changed.notify_all();
        // The acceptor is parked in `accept`; one connection wakes it to see the
        // flag. A failed connect means the listener is already gone.
        if TcpStream::connect(self.address).is_err() {
            // Nothing is listening, so nothing is left to wake.
        }
        while let Some(handle) = self.shared.disown() {
            if handle.join().is_err() {
                // A handler that panicked has already failed its own request.
            }
        }
    }
}

/// Accepts connections until the stub stops, one handler thread each.
fn accept(listener: &TcpListener, shared: &Arc<Shared>) {
    for connection in listener.incoming() {
        if shared.lock().stop {
            return;
        }
        let Ok(stream) = connection else {
            continue;
        };
        let for_handler = Arc::clone(shared);
        let started = Builder::new()
            .name("stub-index-handle".to_owned())
            .spawn(move || handle(stream, &for_handler));
        if let Ok(handler) = started {
            shared.own(handler);
        }
    }
}

/// Answers one request.
fn handle(mut stream: TcpStream, shared: &Shared) {
    let Some(path) = read_path(&mut stream) else {
        return;
    };
    {
        let mut state = shared.lock();
        state.arrivals = state.arrivals.saturating_add(1);
        state.in_flight = state.in_flight.saturating_add(1);
        state.peak = state.peak.max(state.in_flight);
        if state.arrivals >= shared.gate {
            state.open = true;
            shared.changed.notify_all();
        }
        while !state.open && !state.stop {
            state = match shared.changed.wait(state) {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }
    let response = match shared.routes.get(&path) {
        Some(reply) => reply.render(),
        None => format_response(404, "not found"),
    };
    let endless = matches!(shared.routes.get(&path), Some(&Reply::Endless));
    // Recorded before the bytes leave: the client may exit the instant it has the
    // response, and a test reading `served` after that must already find the path.
    shared.lock().served.push(path);
    if stream.write_all(response.as_bytes()).is_err() {
        // The client gave up first (its timeout); the counters below still hold.
    } else if endless {
        flood(&mut stream);
    }
    let mut state = shared.lock();
    state.in_flight = state.in_flight.saturating_sub(1);
}

/// Writes until the client hangs up, which is the only way this ends.
fn flood(stream: &mut TcpStream) {
    let chunk = [b'x'; 8_192];
    while stream.write_all(&chunk).is_ok() {}
}

/// The path of the request line, once the headers have all arrived.
fn read_path(stream: &mut TcpStream) -> Option<String> {
    let mut received = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !received.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = stream.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        received.extend_from_slice(chunk.get(..count)?);
    }
    let text = String::from_utf8(received).ok()?;
    let line = text.lines().next()?;
    let mut words = line.split_whitespace();
    if words.next()? != "GET" {
        return None;
    }
    words
        .next()
        .map(|path| path.trim_start_matches('/').to_owned())
}

/// A complete HTTP/1.1 response.
fn format_response(code: u16, body: &str) -> String {
    format!(
        "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
