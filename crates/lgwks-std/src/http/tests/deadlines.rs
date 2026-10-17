//! `http`'s real-time bounds, derived from the host that runs them (#272).
//!
//! A test that stops a client at a deadline needs the fixture thread to have
//! written its half before the deadline passes. A constant that held on an idle
//! host did not when thirteen runners shared one, so the bound here follows the
//! cost of one measured exchange instead of a guess at it.

use super::*;

/// How many exchanges' worth of time a bound that waits on one gets: the race
/// these bounds guard is the fixture thread being scheduled before the client's
/// clock runs out, one measured exchange is that thread's cost on this host at
/// this load, and twenty of them survive a stall nineteen times what it saw.
const HOST_EXCHANGES: u32 = 20;

/// The time a real-time bound gets when one loopback exchange took `exchange`:
/// [`HOST_EXCHANGES`] of them, no shorter than `floor` and no longer than `cap`.
///
/// Pure, so the arithmetic is pinned without a clock. The floor is the constant
/// the bound had before it was measured, so an idle host runs the test it always
/// ran; the cap keeps a pathological host from turning a timeout test into a
/// minute-long wait. `floor` must not exceed `cap`.
fn scaled_budget(exchange: Duration, floor: Duration, cap: Duration) -> Duration {
    exchange.saturating_mul(HOST_EXCHANGES).clamp(floor, cap)
}

/// A real-time bound for this host, measured now: one whole `get_with` against
/// an instant loopback reply, scaled by [`scaled_budget`].
pub(super) fn host_budget(
    floor: Duration,
    cap: Duration,
) -> Result<Duration, Box<dyn std::error::Error>> {
    let (port, server) = serve(vec![("200 OK", String::new())])?;
    let started = std::time::Instant::now();
    let response = get_with(&format!("http://127.0.0.1:{port}/"), &quiet())?;
    let exchange = started.elapsed();
    join_server(server)?;
    assert_eq!(response.status, 200, "the probe exchange completed");
    Ok(scaled_budget(exchange, floor, cap))
}

/// A host budget scales with one measured exchange and stays between its bounds.
///
/// Requirement (#272): a bound around real time follows how long one exchange
/// takes on the host that runs it, never drops below the constant the test was
/// written with, and never exceeds a named cap. The failing runs: the main run
/// after PR 598 (run 39041681878, load average 38), where a 150 ms timeout
/// expired before the fixture wrote the response head and a 500 ms deadline
/// before the second redirect hop was requested.
#[test]
fn a_host_budget_scales_with_the_exchange_between_a_floor_and_a_cap() {
    let floor = Duration::from_millis(150);
    let cap = Duration::from_secs(5);
    let at = |exchange: Duration| scaled_budget(exchange, floor, cap);
    assert_eq!(at(Duration::ZERO), floor, "no time leaves the floor");
    assert_eq!(
        at(Duration::from_millis(1)),
        floor,
        "a fast exchange does not shrink the bound below what the test was written with"
    );
    assert_eq!(
        at(Duration::from_millis(50)),
        Duration::from_secs(1),
        "twenty exchanges of 50 ms"
    );
    assert_eq!(
        at(Duration::from_secs(1)),
        cap,
        "a slow host is held to the cap"
    );
    assert_eq!(
        at(Duration::MAX),
        cap,
        "the multiplication saturates, not overflows"
    );
}

/// The whole-call deadline is carried across hops: each hop here answers
/// well inside the per-phase timeout, yet the chain as a whole overruns
/// the deadline and is refused at the deadline stage (#191).
///
/// Every bound is a multiple of one host-measured unit (200 ms at the floor,
/// where they are the 200 ms hop, 500 ms deadline, 2 s phase timeout and 1.5 s
/// elapsed bound this test was written with), so a loaded host stretches them
/// together and the ratios the assertions rest on do not move (#272).
#[test]
fn a_deadline_bounds_the_whole_redirect_chain() -> Result<(), Box<dyn std::error::Error>> {
    let unit = host_budget(Duration::from_millis(200), Duration::from_secs(4))?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    // The test drops `done` once the client has given up, which disconnects
    // the channel; the server's per-hop delay is a wait on that event, so it
    // ends with the client instead of running out a clock.
    let (done, client_done) = std::sync::mpsc::sync_channel::<()>(1);
    // Counts requests the client made, not replies completed: a hop the
    // deadline cut short still shows the client followed the redirect to it,
    // and a count of completed replies would make the bound below a race
    // between the per-hop gap and the deadline on a loaded host.
    let server = fixture_thread(move || -> std::io::Result<usize> {
        let mut requested = 0;
        while requested < 10 {
            let (mut stream, _) = listener.accept()?;
            if !matches!(
                client_done.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ) {
                break;
            }
            let _ = read_request_head(&mut stream)?;
            requested += 1;
            // A slow upstream: one unit per hop, well inside the per-phase
            // timeout, so only the whole-call deadline can stop the chain.
            let gap = client_done.recv_timeout(unit);
            if !matches!(gap, Err(std::sync::mpsc::RecvTimeoutError::Timeout)) {
                break;
            }
            let reply = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/hop\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            // The client may already have given up on this hop, which is
            // the point of the test, so a refused write ends the chain.
            if stream.write_all(reply.as_bytes()).is_err() {
                break;
            }
        }
        Ok(requested)
    })?;
    let started = std::time::Instant::now();
    let result = get_with(
        &format!("http://127.0.0.1:{port}/hop"),
        &quiet()
            .timeout(unit.saturating_mul(10))
            .deadline(unit.saturating_mul(5) / 2)
            .redirect_policy(RedirectPolicy::FollowAtMost(10)),
    );
    let elapsed = started.elapsed();
    // Tell the server the client is done, and wake an accept it may be
    // blocked in; whether that connection lands is the server's business.
    drop(done);
    drop(TcpStream::connect(("127.0.0.1", port)));
    assert!(
        matches!(
            result,
            Err(Error::Failure {
                stage: FailureStage::Deadline,
                kind: FailureKind::Timeout,
                ..
            })
        ),
        "the chain is refused at its deadline, got {result:?}"
    );
    assert!(
        elapsed < unit.saturating_mul(15) / 2,
        "the deadline bounds the chain, not ten hops of {unit:?}: {elapsed:?}"
    );
    let requested = join_server(server)?;
    assert!(
        (2..10).contains(&requested),
        "the client followed at least one redirect, then stopped at the deadline: \
         {requested} requests"
    );
    Ok(())
}
