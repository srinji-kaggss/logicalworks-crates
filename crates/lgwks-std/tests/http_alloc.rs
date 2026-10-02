//! Allocation budget of the public HTTP read path, measured with a counting
//! global allocator.
//!
//! `cli.rs`/`codec_contract.rs` established the pattern: the workspace forbids
//! `unsafe`, and a counting global allocator needs `GlobalAlloc`, so the
//! allocator lives in a *separate* package that this test builds and runs. That
//! package drives the shipped client through its public API against a loopback
//! server and prints one labelled number per quantity, which this test then
//! asserts.
//!
//! Five quantities are measured separately, all at non-power-of-two ceilings:
//!
//! | Key | Quantity |
//! |---|---|
//! | `exact-body-len` | retained body length |
//! | `exact-retained` | live heap held while the `Response` is alive (body `Vec` capacity, headers, strings) |
//! | `exact-header-bytes` | exact retained header value bytes |
//! | `exact-peak` | peak live heap during the call (engine/header buffering included) |
//! | `cut-*` | the same three, for a preview of a body far past the ceiling |
//!
//! `eager-peak` measures a stand-in for the mutant the ceiling exists to stop:
//! a reader that reserves the declared length and drains the reader unbounded.
//! It must exceed the ceiling, which is what makes the `cut-*` bound below
//! meaningful rather than vacuous — the same metric that stays small on the
//! real path reports eager buffering as large.
#![forbid(unsafe_code)]

#[path = "support/consumer_probe.rs"]
mod consumer_probe;

#[cfg(feature = "http")]
mod probe {
    use std::collections::BTreeMap;
    use std::path::Path;

    /// The non-power-of-two body ceiling — 3003 is `8192`-spanning and not a
    /// power of two, so capacity has to be clamped rather than rounded.
    const CEILING: u64 = 3003;

    /// How many body bytes the preview case serves: far past the ceiling, and
    /// far above any slack a bounded reader could need.
    const CUT_BODY: u64 = 3_075_072;

    /// Head room for engine and header buffering beyond the body ceiling.
    ///
    /// Measured, not guessed: `exact-peak` for a 3 003-byte body is about
    /// 267 KB while `exact-retained` is 324 bytes, so ureq's per-call engine
    /// buffering — not the body — dominates the peak. 512 KiB is roughly twice
    /// that measured transient, which is why the preview bound below still
    /// discriminates a reader that buffers the 3 MB body: the real peak stays
    /// near 270 KB whether the body is 3 KB or 3 MB.
    const SLACK: u64 = 524_288;

    /// The manifest of the throwaway probe crate.
    fn manifest() -> String {
        format!(
            "[package]\nname = \"http_alloc_probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
             [dependencies]\nlgwks_std = {{ path = {:?}, default-features = false, features = [\"http\"] }}\n",
            Path::new(env!("CARGO_MANIFEST_DIR"))
        )
    }

    /// Read one labelled `u64` from the probe's stdout, failing with the whole
    /// output when the key is absent.
    fn value(stdout: &str, key: &str) -> Result<u64, Box<dyn std::error::Error>> {
        let needle = format!("{key} ");
        for line in stdout.lines() {
            if let Some(rest) = line.strip_prefix(&needle) {
                return Ok(rest.trim().parse::<u64>()?);
            }
        }
        Err(format!("probe output is missing `{key}`:\n{stdout}").into())
    }

    /// Assert one labelled quantity is at most `limit`, naming the case.
    fn assert_bounded(
        stdout: &str,
        key: &str,
        limit: u64,
        detail: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let measured = value(stdout, key)?;
        assert!(
            measured <= limit,
            "{detail}: {measured} > {limit}\n{stdout}"
        );
        Ok(())
    }

    /// Every labelled number the probe prints, keyed by label.
    fn measurements(stdout: &str) -> Result<BTreeMap<String, u64>, Box<dyn std::error::Error>> {
        let mut values = BTreeMap::new();
        for line in stdout.lines() {
            if let Some((key, rest)) = line.split_once(' ')
                && let Ok(parsed) = rest.trim().parse::<u64>()
            {
                values.insert(key.to_owned(), parsed);
            }
        }
        assert!(
            !values.is_empty(),
            "probe printed no labelled measurements:\n{stdout}"
        );
        Ok(values)
    }

    /// The public read path holds the ceiling, not the body, and the probe
    /// discriminates the eager mutant that holds the body.
    #[test]
    fn the_read_path_retains_the_ceiling_not_the_body() -> Result<(), Box<dyn std::error::Error>> {
        let stdout = crate::consumer_probe::build_and_run(&manifest(), PROBE_MAIN)?;
        measurements(&stdout)?;
        let get = |key: &str| -> Result<u64, Box<dyn std::error::Error>> { value(&stdout, key) };

        // Body length and exact-ceiling completion.
        assert_eq!(
            get("exact-body-len")?,
            CEILING,
            "a body at the ceiling is retained whole:\n{stdout}"
        );
        assert_eq!(
            get("exact-complete")?,
            1,
            "a body exactly at the ceiling ends on its own:\n{stdout}"
        );
        assert!(
            get("exact-header-bytes")? > 0,
            "the server's header bytes are retained and measured:\n{stdout}"
        );

        // Retained and peak heap at the ceiling: body capacity + headers + any
        // engine buffering, all bounded by the ceiling plus a fixed slack.
        let bound = CEILING.saturating_add(SLACK);
        assert_bounded(
            &stdout,
            "exact-retained",
            bound,
            "retained heap must be bounded by the ceiling plus slack",
        )?;
        assert_bounded(
            &stdout,
            "exact-peak",
            bound,
            "peak heap must be bounded by the ceiling plus slack",
        )?;

        // The preview of a body 1000x the ceiling still holds only the ceiling.
        assert_eq!(
            get("cut-body-len")?,
            CEILING,
            "a preview stops at the ceiling:\n{stdout}"
        );
        assert_eq!(
            get("cut-truncation")?,
            1,
            "a body past the ceiling is reported as cut:\n{stdout}"
        );
        assert_bounded(
            &stdout,
            "cut-retained",
            bound,
            &format!("a {CUT_BODY}-byte body must not be retained whole"),
        )?;
        assert_bounded(
            &stdout,
            "cut-peak",
            bound,
            &format!("a {CUT_BODY}-byte body must not be buffered whole"),
        )?;

        // The negative control: the same peak metric reports eager buffering.
        assert_eq!(
            get("eager-len")?,
            CUT_BODY,
            "the eager stand-in read the whole body:\n{stdout}"
        );
        assert!(
            get("eager-peak")? >= CUT_BODY,
            "an eager reader's peak must reach the body size it buffered: {} < {CUT_BODY}\n{stdout}",
            get("eager-peak")?
        );
        assert!(
            get("eager-peak")? > get("cut-peak")?,
            "the probe must rank the eager mutant above the bounded read path:\n{stdout}"
        );

        // Concurrency: every request answers, and peak heap stays bounded per
        // in-flight request rather than growing without limit.
        for level in [100_u64, 1000] {
            assert_eq!(
                get(&format!("concurrency-{level}-errors"))?,
                0,
                "every concurrent request at level {level} must answer:\n{stdout}"
            );
            let peak = get(&format!("concurrency-{level}-peak"))?;
            let bound = level.saturating_mul(1_048_576);
            assert!(
                peak <= bound,
                "peak heap at level {level} must stay bounded: {peak} > {bound}\n{stdout}"
            );
        }

        // Latency percentiles are ordered, and the p99 is a real number.
        let p50 = get("latency-p50")?;
        let p95 = get("latency-p95")?;
        let p99 = get("latency-p99")?;
        assert!(
            p50 <= p95 && p95 <= p99,
            "percentiles must be ordered: p50={p50} p95={p95} p99={p99}\n{stdout}"
        );
        assert!(
            p99 < 50_000,
            "the loopback p99 must be under 50 ms: {p99} us\n{stdout}"
        );
        Ok(())
    }

    /// The probe crate: a counting allocator, a minimal loopback server, and the
    /// shipped client driven through its public API.
    const PROBE_MAIN: &str = r#"
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};

use lgwks_std::http::{self, BodyPolicy, Options, Truncation};

const CEILING: usize = 3003;
const CUT_BODY: usize = 3_075_072;

/// A pass-through allocator that accounts live and peak bytes.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            add(layout.size());
        }
        ptr
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            add(layout.size());
        }
        ptr
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let next = unsafe { System.realloc(ptr, layout, size) };
        if !next.is_null() {
            if size > layout.size() {
                add(size - layout.size());
            } else {
                release(layout.size() - size);
            }
        }
        next
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        release(layout.size());
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Account `bytes` as live and raise the peak.
fn add(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::SeqCst).wrapping_add(bytes);
    let mut peak = PEAK.load(Ordering::SeqCst);
    while live > peak {
        match PEAK.compare_exchange_weak(peak, live, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return,
            Err(actual) => peak = actual,
        }
    }
}

/// Account `bytes` as freed.
fn release(bytes: usize) {
    LIVE.fetch_sub(bytes, Ordering::SeqCst);
}

/// Live heap right now.
fn live() -> usize {
    LIVE.load(Ordering::SeqCst)
}

/// Start a fresh peak from the current live heap.
fn arm() {
    PEAK.store(live(), Ordering::SeqCst);
}

/// Peak live heap since `arm`.
fn peak_now() -> usize {
    PEAK.load(Ordering::SeqCst)
}

/// A reply with `Content-Length: body_len`, optional extra headers, and that
/// many `x` bytes.
fn reply_for(body_len: usize, extra_headers: &str) -> Vec<u8> {
    let mut reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {body_len}\r\nConnection: close\r\n{extra_headers}\r\n"
    )
    .into_bytes();
    reply.extend(std::iter::repeat_n(b'x', body_len));
    reply
}

/// Read one request head into a stack buffer, then write `reply`.
fn serve_once(stream: &mut TcpStream, reply: &[u8]) {
    let mut request = [0u8; 1024];
    loop {
        match stream.read(&mut request) {
            Ok(0) => break,
            Ok(read) => {
                if request[..read].windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = stream.write_all(reply);
}

/// Accept one connection and answer it.
///
/// The caller joins this thread before the next measurement, so its read and
/// write must end even if the client stops reading first: a blocked join would
/// be a hung probe rather than a measured number. The timeouts bound that.
fn serve(listener: TcpListener, reply: Vec<u8>) {
    if let Ok((mut stream, _)) = listener.accept() {
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(5)));
        serve_once(&mut stream, &reply);
    }
}

/// A pool of server workers sharing one listener, each answering until stopped.
///
/// One thread accepting and serving serially could not keep up with a
/// thousand-thread connect burst, so connects were refused and a handful of
/// clients exhausted their retries. A small pool drains the backlog in
/// parallel, and stopping is explicit so a client that never dials cannot leave
/// a worker blocked in `accept` forever and wedge the join.
struct ServerPool {
    /// Set to ask every worker to finish its current answer and exit.
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// One joinable handle per worker.
    workers: Vec<std::thread::JoinHandle<()>>,
}

/// Start `workers` threads answering connections on `listener` until stopped.
fn start_pool(listener: std::sync::Arc<TcpListener>, reply: Vec<u8>, workers: usize) -> ServerPool {
    let _ = listener.set_nonblocking(true);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let listener = std::sync::Arc::clone(&listener);
        let reply = reply.clone();
        let stop = std::sync::Arc::clone(&stop);
        handles.push(std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        serve_once(&mut stream, &reply);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::yield_now();
                    }
                    Err(_) => break,
                }
            }
        }));
    }
    ServerPool {
        stop,
        workers: handles,
    }
}

impl ServerPool {
    /// Stop every worker and join it.
    fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        for worker in self.workers {
            let _ = worker.join();
        }
    }
}

/// Measure one exchange: body length, retained bytes, peak bytes, retained
/// header bytes.
struct Measured {
    body_len: usize,
    retained: usize,
    peak: usize,
    header_bytes: usize,
    cut: u8,
}

fn measure(url: &str, options: &Options) -> Result<Measured, Box<dyn std::error::Error>> {
    let baseline = live();
    arm();
    let response = http::get_with(url, options)?;
    let peak = peak_now().saturating_sub(baseline);
    let retained = live().saturating_sub(baseline);
    let header_bytes: usize = response.header_values().map(|(_, value)| value.len()).sum();
    Ok(Measured {
        body_len: response.body().len(),
        retained,
        peak,
        header_bytes,
        cut: u8::from(response.truncation == Truncation::Cut),
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Warm up ureq's and rustls' process-global state so it is not charged to
    // the first measured call.
    {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let reply = reply_for(16, "");
        let server = std::thread::spawn(move || serve(listener, reply));
        let _ = http::get_with(
            &format!("http://127.0.0.1:{port}/"),
            &Options::default(),
        );
        let _ = server.join();
    }

    // Exact fit at the non-power-of-two ceiling.
    {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let reply = reply_for(CEILING, "X-Probe: allocation\r\n");
        let server = std::thread::spawn(move || serve(listener, reply));
        let url = format!("http://127.0.0.1:{port}/");
        let options = Options::default()
            .max_body_bytes(CEILING)
            .body_policy(BodyPolicy::Whole);
        let measured = measure(&url, &options)?;
        let _ = server.join();
        println!("exact-body-len {}", measured.body_len);
        println!("exact-retained {}", measured.retained);
        println!("exact-peak {}", measured.peak);
        println!("exact-header-bytes {}", measured.header_bytes);
        println!("exact-complete {}", u8::from(measured.cut == 0));
    }

    // A preview of a body far past the ceiling: thousands of times the ceiling.
    {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let reply = reply_for(CUT_BODY, "");
        let server = std::thread::spawn(move || serve(listener, reply));
        let url = format!("http://127.0.0.1:{port}/");
        let options = Options::default()
            .max_body_bytes(CEILING)
            .body_policy(BodyPolicy::Preview);
        let measured = measure(&url, &options)?;
        let _ = server.join();
        println!("cut-body-len {}", measured.body_len);
        println!("cut-retained {}", measured.retained);
        println!("cut-peak {}", measured.peak);
        println!("cut-truncation {}", measured.cut);
    }

    // The eager stand-in: reserve the declared length and drain the reader.
    {
        let payload = vec![b'x'; CUT_BODY];
        let baseline = live();
        arm();
        let mut cursor = std::io::Cursor::new(payload);
        let mut body = Vec::with_capacity(CUT_BODY);
        cursor.read_to_end(&mut body)?;
        let peak = peak_now().saturating_sub(baseline);
        println!("eager-peak {peak}");
        println!("eager-len {}", body.len());
    }

    // Concurrency saturation: peak heap and error count at two levels.
    for &level in &[100_usize, 1000_usize] {
        let listener = std::sync::Arc::new(TcpListener::bind("127.0.0.1:0")?);
        let port = listener.local_addr()?.port();
        let reply = reply_for(64, "");
        let pool = start_pool(std::sync::Arc::clone(&listener), reply, level.min(16).max(1));
        let url = format!("http://127.0.0.1:{port}/");
        let options = Options::default().max_body_bytes(64);
        let errors = AtomicUsize::new(0);
        let baseline = live();
        arm();
        std::thread::scope(|scope| {
            for _ in 0..level {
                let url = url.as_str();
                let options = &options;
                let errors = &errors;
                scope.spawn(move || {
                    let mut answered = false;
                    for _ in 0..200 {
                        if http::get_with(url, options).is_ok() {
                            answered = true;
                            break;
                        }
                        // A connect refused by a full accept backlog is
                        // transient, not a client failure; retry it so the
                        // sweep measures the read path, not the listener's
                        // backlog under a thread-start burst.
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    if !answered {
                        errors.fetch_add(1, Ordering::SeqCst);
                    }
                });
            }
        });
        let peak = peak_now().saturating_sub(baseline);
        println!("concurrency-{level}-errors {}", errors.load(Ordering::SeqCst));
        println!("concurrency-{level}-peak {peak}");
        pool.stop();
    }

    // Latency percentiles on the real path.
    {
        let count = 501_usize;
        let listener = std::sync::Arc::new(TcpListener::bind("127.0.0.1:0")?);
        let port = listener.local_addr()?.port();
        let reply = reply_for(64, "");
        let pool = start_pool(std::sync::Arc::clone(&listener), reply, 4);
        let url = format!("http://127.0.0.1:{port}/");
        let options = Options::default().max_body_bytes(64);
        let mut samples: Vec<u128> = Vec::with_capacity(count);
        for _ in 0..count {
            let started = std::time::Instant::now();
            let _ = http::get_with(&url, &options)?;
            samples.push(started.elapsed().as_micros());
        }
        samples.sort_unstable();
        let pick = |quantile: usize| -> u128 {
            let last = samples.len().saturating_sub(1);
            let index = last
                .saturating_mul(quantile)
                .checked_div(100)
                .unwrap_or(0)
                .min(last);
            samples[index]
        };
        println!("latency-p50 {}", pick(50));
        println!("latency-p95 {}", pick(95));
        println!("latency-p99 {}", pick(99));
        pool.stop();
    }

    Ok(())
}
"#;
}
