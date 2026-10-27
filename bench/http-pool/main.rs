//! Keep-alive pool comparison: one `Client` against one agent per call.
//!
//! A loopback server answers every request on a connection until the peer
//! goes away, and counts the connections it accepted. The rig fires one
//! sequential burst of small GETs through a pooled [`lgwks_std::http::Client`]
//! and the same burst through the one-shot free functions, then reports
//! per-request p50/p95/p99 for each side and how many connections each side
//! cost. The numbers are loopback numbers for this host, not a cross-platform
//! claim: the point is the ratio and the connection count, which is what the
//! pool exists to change.
//!
//! ```sh
//! cargo run --release --locked --manifest-path bench/http-pool/Cargo.toml
//! cargo run --release --locked --manifest-path bench/http-pool/Cargo.toml -- --requests 500
//! cargo run --release --locked --manifest-path bench/http-pool/Cargo.toml -- --json bench/http-pool/results.json
//! ```

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// The one request-head reader every loopback server uses, included by
/// path like the `lgwks_std` test harnesses include it: this root is
/// invisible to the estate dependency contract, so a file-level include
/// adds no edge while keeping one server behavior everywhere.
#[path = "../../crates/lgwks-std/tests/support/loopback.rs"]
mod loopback;

use loopback::read_request_head;

/// Requests per side unless `--requests` says otherwise.
const REQUESTS: usize = 200;

/// The response every request gets: small, fixed, keep-alive friendly.
const REPLY: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";

/// One line to the locked stdout; a write error reaches `main` as an I/O error.
fn say(line: &str) -> std::io::Result<()> {
    writeln!(std::io::stdout().lock(), "{line}")
}

/// Serve one connection until the peer goes away; a torn head or a refused
/// write is returned to the join that owns this thread.
fn serve_connection(mut stream: TcpStream, requests: &AtomicUsize) -> std::io::Result<()> {
    loop {
        let head = read_request_head(&mut stream)?;
        if !head.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(());
        }
        requests.fetch_add(1, Ordering::SeqCst);
        stream.write_all(REPLY)?;
    }
}

/// The outcome of one burst: per-request latencies and connections opened.
struct Burst {
    /// Per-request latency, in microseconds, in firing order.
    latencies_us: Vec<u128>,
    /// Connections the server accepted during the burst.
    accepts: usize,
}

/// Sort a copy and read one quantile, or `None` when there is nothing to read.
fn quantile(sorted: &[u128], ratio: f64) -> Option<u128> {
    let last = sorted.len().checked_sub(1)?;
    let at = (last as f64 * ratio).round() as usize;
    sorted.get(at.min(last)).copied()
}

/// Render a joined thread's panic payload as text, never echoing it raw.
///
/// A panic payload can carry anything the fixture thread formatted, so the
/// failure names the fixture instead of echoing the payload.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast_ref::<&str>() {
        Some(text) => (*text).to_owned(),
        None => match payload.downcast_ref::<String>() {
            Some(text) => text.clone(),
            None => "a payload that is not text".to_owned(),
        },
    }
}

/// Print one burst row and return its p50/p95/p99; an empty burst is refused.
fn report(name: &str, burst: &Burst) -> Result<(u128, u128, u128), Box<dyn std::error::Error>> {
    let mut sorted = burst.latencies_us.clone();
    sorted.sort_unstable();
    let (Some(p50), Some(p95), Some(p99)) = (
        quantile(&sorted, 0.50),
        quantile(&sorted, 0.95),
        quantile(&sorted, 0.99),
    ) else {
        lgwks_std::trace::error!(name, "the burst recorded no latencies");
        return Err(format!("the {name} burst recorded no latencies").into());
    };
    say(&format!(
        "{name:>12}  p50 {p50:>7} us  p95 {p95:>7} us  p99 {p99:>7} us  connections {accepts}",
        accepts = burst.accepts
    ))?;
    Ok((p50, p95, p99))
}

/// Read `--requests N` and `--json PATH` from the arguments.
fn args() -> (usize, Option<String>) {
    let mut requests = REQUESTS;
    let mut json: Option<String> = None;
    let mut rest = std::env::args().skip(1);
    while let Some(arg) = rest.next() {
        if arg == "--requests" {
            if let Some(value) = rest.next()
                && let Ok(count) = value.parse()
            {
                requests = count;
            }
        } else if arg == "--json" {
            json = rest.next();
        }
    }
    (requests.max(1), json)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (requests, json) = args();
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let accepts = Arc::new(AtomicUsize::new(0));
    let served = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let url = format!("http://127.0.0.1:{port}/");

    let handlers: Arc<std::sync::Mutex<Vec<std::thread::JoinHandle<std::io::Result<()>>>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let server = std::thread::Builder::new()
        .name("lgwks-bench-http-pool-server".to_owned())
        .spawn({
            let accepts = Arc::clone(&accepts);
            let served = Arc::clone(&served);
            let stop = Arc::clone(&stop);
            let handlers = Arc::clone(&handlers);
            move || -> std::io::Result<()> {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    let stream = stream?;
                    accepts.fetch_add(1, Ordering::SeqCst);
                    let served = Arc::clone(&served);
                    let handler = std::thread::Builder::new()
                        .name("lgwks-bench-http-pool-conn".to_owned())
                        .spawn(move || serve_connection(stream, &served))?;
                    let Ok(mut held) = handlers.lock() else {
                        lgwks_std::trace::error!("bench handler list poisoned");
                        return Err(std::io::Error::other(
                            "bench handler list poisoned: the fixture cannot track its threads",
                        ));
                    };
                    held.push(handler);
                }
                Ok(())
            }
        })?;

    // Warm up the process-global TLS and allocator state so the measured
    // bursts do not pay for first use. The warm client is dropped right
    // away: a pooled client that lived on would hold its keep-alive
    // connection open and the server thread serving it would never join.
    let warm = lgwks_std::http::Client::new(lgwks_std::http::Options::default())?;
    drop(warm.get_with(&url, warm.options())?);
    drop(warm);

    // The pool under measurement, pinned to the values
    // `PoolConfig::default` carries (ureq's own): the burst below measures
    // pooling behavior, so the configuration it measures is written down,
    // not inherited silently.
    let pool = lgwks_std::http::PoolConfig::default()
        .max_idle_connections(10)
        .max_idle_connections_per_host(3)
        .max_idle_age(Duration::from_secs(15));

    let pooled = |requests: usize| -> Result<Burst, Box<dyn std::error::Error>> {
        let before = accepts.load(Ordering::SeqCst);
        let client = lgwks_std::http::Client::with_pool(lgwks_std::http::Options::default(), pool)?;
        let mut latencies_us = Vec::with_capacity(requests);
        for _ in 0..requests {
            let started = Instant::now();
            let response = client.get_with(&url, client.options())?;
            if response.status != 200 {
                lgwks_std::trace::error!(status = response.status, "pooled burst saw a failure");
                return Err(format!("pooled burst saw {}", response.status).into());
            }
            latencies_us.push(started.elapsed().as_micros());
        }
        drop(client);
        Ok(Burst {
            latencies_us,
            accepts: accepts.load(Ordering::SeqCst).saturating_sub(before),
        })
    };

    let one_shot = |requests: usize| -> Result<Burst, Box<dyn std::error::Error>> {
        let before = accepts.load(Ordering::SeqCst);
        let mut latencies_us = Vec::with_capacity(requests);
        for _ in 0..requests {
            let started = Instant::now();
            let response = lgwks_std::http::get(&url)?;
            if response.status != 200 {
                lgwks_std::trace::error!(status = response.status, "one-shot burst saw a failure");
                return Err(format!("one-shot burst saw {}", response.status).into());
            }
            latencies_us.push(started.elapsed().as_micros());
        }
        Ok(Burst {
            latencies_us,
            accepts: accepts.load(Ordering::SeqCst).saturating_sub(before),
        })
    };

    let timeout = Duration::from_secs(120);
    let deadline = Instant::now() + timeout;
    let pooled = pooled(requests)?;
    if Instant::now() > deadline {
        lgwks_std::trace::error!(?timeout, "pooled burst overran its deadline");
        return Err("pooled burst overran its deadline".into());
    }
    let single = one_shot(requests)?;

    say(&format!("keep-alive burst of {requests} sequential GETs, loopback, this host:"))?;
    let (pooled_p50, pooled_p95, pooled_p99) = report("pooled", &pooled)?;
    let (single_p50, single_p95, single_p99) = report("one-shot", &single)?;
    say(&format!("served {} requests in total", served.load(Ordering::SeqCst)))?;

    if let Some(path) = json {
        let document = format!(
            "{{\n  \"requests\": {requests},\n  \"pooled\": {{\"p50_us\": {pooled_p50}, \"p95_us\": {pooled_p95}, \"p99_us\": {pooled_p99}, \"connections\": {}}},\n  \"one_shot\": {{\"p50_us\": {single_p50}, \"p95_us\": {single_p95}, \"p99_us\": {single_p99}, \"connections\": {}}}\n}}\n",
            pooled.accepts, single.accepts
        );
        std::fs::write(&path, document)?;
        say(&format!("wrote {path}"))?;
    }

    stop.store(true, Ordering::SeqCst);
    drop(TcpStream::connect(("127.0.0.1", port))?);
    match server.join() {
        Ok(served) => served?,
        Err(payload) => {
            let message = panic_message(payload);
            lgwks_std::trace::error!(%message, "the bench server thread panicked");
            return Err(format!("the bench server thread panicked: {message}").into());
        }
    }
    let Ok(mut handlers) = handlers.lock() else {
        lgwks_std::trace::error!("the bench handler list is poisoned");
        return Err("the bench handler list is poisoned: the fixture cannot join its threads".into());
    };
    for (index, handler) in handlers.drain(..).enumerate() {
        match handler.join() {
            Ok(served) => served?,
            Err(payload) => {
                let message = panic_message(payload);
                lgwks_std::trace::error!(index, %message, "a bench connection panicked");
                return Err(format!("bench connection {index} panicked: {message}").into());
            }
        }
    }
    Ok(())
}
