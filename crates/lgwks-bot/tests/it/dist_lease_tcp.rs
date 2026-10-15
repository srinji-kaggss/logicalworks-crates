//! Black-box acceptance for the cross-host grant channel (#319).
//!
//! A real [`AuthorityServer`] on a loopback socket answers real
//! [`TcpGrantChannel`]s, and a [`Coordinator`] fenced by the channel commits to
//! the real [`TenantStore`]. Facts pinned here, each from a named source:
//!
//! - **A stale remote coordinator cannot grant** (KEEL-SPEC S5.4, ACCEPTANCE
//!   AT07): a second host's acquire across the wire stales the first host's
//!   lease, and the first host's dispatch is refused as stale.
//! - **A forged key mints nothing** (CWE-345, insufficient verification of
//!   data authenticity): the server refuses the frame and the epoch stays put.
//! - **Hostile frames are refused without a reply** (CWE-400 and CWE-20): a
//!   foreign protocol, an unknown op, a 64 KiB name length, a truncated frame
//!   and a zero tag each close the connection, and the server keeps answering.
//! - **A replayed reply is refused** (CWE-294, capture-replay): an answer
//!   recorded at epoch 1 and replayed after the epoch moved does not validate
//!   the epoch-1 lease.
//! - **An unreachable authority withholds grants** (the partition rule of the
//!   module documentation): nothing is written, and a zero timeout answers
//!   nothing.

use std::error::Error;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use lgwks_bot::dist_lease::{
    AuthorityHandle, AuthorityServer, Coordinator, DispatchError, GrantChannel, LeaseError,
    ServeReport, TcpGrantChannel, WireKey,
};
use lgwks_bot::tenant_store::{RunStart, TenantStore};

use crate::scratch::Scratch;

/// The test result: fixtures bubble with `?`, like the rest of the estate.
pub(crate) type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// The key both ends share in these tests.
pub(crate) const KEY: WireKey = WireKey::from_bytes([7; 32]);

/// The bound on every connect, read and write: long enough for a loaded CI
/// host, and a hang still ends.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(5);

/// The bytes before a request's name: magic, op, nonce, name length.
pub(crate) const HEADER: usize = 23;

/// A check request: the header, no name, the tag.
const QUESTION: usize = HEADER + 32;

/// The lanes the runs plan.
const LANES: [&str; 2] = ["fmt", "clippy"];

/// A loopback listener on a free port.
pub(crate) fn loopback() -> TestResult<(TcpListener, SocketAddr)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    Ok((listener, addr))
}

/// The value a scoped thread returned; a thread that panicked re-raises its
/// panic here, so the test fails with the thread's own message.
fn joined<T>(handle: thread::ScopedJoinHandle<'_, T>) -> T {
    match handle.join() {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// Serve exactly `connections` for `authority` while `client` runs, and
/// return the client's value with the server's report.
///
/// The server thread is scoped and always joined. When the client fails
/// early, the connections it never made are opened and dropped, so the server
/// ends rather than waiting on an accept that will not come.
pub(crate) fn with_server<T>(
    authority: &AuthorityHandle,
    connections: usize,
    client: impl FnOnce(SocketAddr) -> TestResult<T>,
) -> TestResult<(T, ServeReport)> {
    let (listener, addr) = loopback()?;
    let server = AuthorityServer::new(authority.clone(), KEY, TIMEOUT);
    thread::scope(|scope| {
        let serving = scope.spawn(|| server.serve(&listener, connections));
        let outcome = client(addr);
        if outcome.is_err() {
            for _ in 0..connections {
                drop(TcpStream::connect(addr));
            }
        }
        let report = joined(serving);
        Ok((outcome?, report))
    })
}

/// The exact acquire frame a channel sends for `holder`, captured by a
/// listener that records it and answers nothing.
pub(crate) fn captured_acquire(holder: &str) -> TestResult<Vec<u8>> {
    let (listener, addr) = loopback()?;
    let channel = TcpGrantChannel::new(addr, KEY, TIMEOUT);
    thread::scope(|scope| {
        let asking = scope.spawn(move || channel.acquire(holder).is_err());
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        let mut frame = vec![0u8; HEADER.saturating_add(holder.len()).saturating_add(32)];
        stream.read_exact(&mut frame)?;
        drop(stream);
        assert!(joined(asking), "an unanswered acquire must fail closed");
        Ok(frame)
    })
}

/// Send `frame` raw, close the write side, and return whatever came back.
///
/// A server that refuses after reading only the header closes with the rest
/// of the frame unread, and TCP answers that with a reset (RFC 9293 §3.6.1):
/// a reset with nothing read is the same fact as an empty reply.
pub(crate) fn send_raw(addr: SocketAddr, frame: &[u8]) -> TestResult<Vec<u8>> {
    let mut stream = TcpStream::connect_timeout(&addr, TIMEOUT)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    let mut reply = Vec::new();
    let exchanged = stream
        .write_all(frame)
        .and_then(|()| stream.shutdown(Shutdown::Write))
        .and_then(|()| stream.read_to_end(&mut reply));
    match exchanged {
        Ok(_) => Ok(reply),
        Err(error)
            if reply.is_empty()
                && matches!(
                    error.kind(),
                    ErrorKind::ConnectionReset | ErrorKind::BrokenPipe | ErrorKind::NotConnected
                ) =>
        {
            Ok(reply)
        }
        Err(error) => Err(error.into()),
    }
}

/// A run start for `run_id` on `host`.
fn start<'a>(run_id: &'a str, host: &'a str) -> RunStart<'a> {
    RunStart::new(run_id, "acme", "/repo", host, 1, "lock", "t-000001")
}

/// A stale remote coordinator cannot grant: the second acquire across the
/// wire stales the first lease, the first host's dispatch is refused as stale,
/// and the second host's dispatch commits exactly once.
#[test]
fn a_remote_stale_coordinator_cannot_grant() -> TestResult {
    let scratch = Scratch::new("dist-lease-tcp-fence")?;
    let authority = AuthorityHandle::new();
    let ((), report) = with_server(&authority, 6, |addr| {
        let channel = TcpGrantChannel::new(addr, KEY, TIMEOUT);
        let store = TenantStore::open(scratch.path())?;
        let host = Coordinator::with_fence(store, "host-a", channel.clone());
        let first = channel.acquire("coord-a")?;
        host.begin_run(&first, &start("run-tcp-001", "host-a"), &LANES)?;
        let second = channel.acquire("coord-b")?;
        match host.dispatch_lane(&first, "run-tcp-001", 0) {
            Err(DispatchError::Lease(LeaseError::StaleEpoch { presented, current })) => {
                assert!(presented < current, "the refusal must name a newer epoch");
            }
            other => return Err(format!("the stale dispatch must be stale: {other:?}").into()),
        }
        host.dispatch_lane(&second, "run-tcp-001", 0)?;
        match host.dispatch_lane(&second, "run-tcp-001", 0) {
            Err(DispatchError::Store(_)) => Ok(()),
            other => Err(format!("a second dispatch must meet the commit: {other:?}").into()),
        }
    })?;
    assert_eq!(report.answered(), 6, "every question must be answered");
    assert_eq!(report.refused(), 0, "no honest frame may be refused");
    assert_eq!(
        authority.epoch(),
        2,
        "two acquires must move the epoch twice"
    );
    Ok(())
}

/// A forged key mints nothing: the server refuses the frame without a reply
/// and the epoch never moves.
#[test]
fn a_forged_key_mints_nothing() -> TestResult {
    let authority = AuthorityHandle::new();
    let ((), report) = with_server(&authority, 1, |addr| {
        let forged = TcpGrantChannel::new(addr, WireKey::from_bytes([8; 32]), TIMEOUT);
        match forged.acquire("mallory") {
            Err(_) => Ok(()),
            Ok(lease) => Err(format!("a forged acquire must not mint: {lease:?}").into()),
        }
    })?;
    assert_eq!(report.refused(), 1, "the forged frame must be refused");
    assert_eq!(report.answered(), 0, "the forged frame must get no answer");
    assert_eq!(
        authority.epoch(),
        0,
        "a forged acquire must not move the epoch"
    );
    Ok(())
}

/// Hostile frames are refused without a reply, and the server answers an
/// honest question afterwards.
#[test]
fn hostile_frames_are_refused_without_a_reply() -> TestResult {
    let authority = AuthorityHandle::new();
    let mut unknown_op = b"LGLQ\x09".to_vec();
    unknown_op.extend_from_slice(&[0; 18]);
    let mut huge_name = b"LGLQ\x01".to_vec();
    huge_name.extend_from_slice(&[0; 16]);
    huge_name.extend_from_slice(&u16::MAX.to_le_bytes());
    let mut zero_tag = b"LGLQ\x02".to_vec();
    zero_tag.extend_from_slice(&[0; 16]);
    zero_tag.extend_from_slice(&4u16.to_le_bytes());
    zero_tag.extend_from_slice(b"evil");
    zero_tag.extend_from_slice(&[0; 32]);
    let frames: [&[u8]; 5] = [
        b"GET / HTTP/1.1\r\nHost: authority\r\n\r\n",
        &unknown_op,
        &huge_name,
        b"LGLQ\x01short",
        &zero_tag,
    ];
    let (validated, report) = with_server(&authority, frames.len().saturating_add(1), |addr| {
        for frame in frames {
            let reply = send_raw(addr, frame)?;
            assert!(reply.is_empty(), "a hostile frame must get no reply");
        }
        let lease = authority.acquire("coord-a");
        Ok(TcpGrantChannel::new(addr, KEY, TIMEOUT).validate(&lease)?)
    })?;
    assert!(validated, "the server must still answer an honest question");
    assert_eq!(
        report.refused(),
        frames.len(),
        "every hostile frame must be refused"
    );
    assert_eq!(report.answered(), 1, "only the honest question is answered");
    assert_eq!(authority.epoch(), 1, "no hostile frame may move the epoch");
    Ok(())
}

/// The documented stop path of a sized `serve`: it parks in `accept` with no
/// deadline, and an owner ends it early by opening and dropping the
/// connections its peers did not make. Each is refused and counted, and the
/// call returns with the honest answer intact (#437 review: the accept wait
/// is unbounded, so the way out of it is pinned).
#[test]
fn an_owner_ends_a_sized_serve_with_empty_connections() -> TestResult {
    let authority = AuthorityHandle::new();
    let (listener, addr) = loopback()?;
    let server = AuthorityServer::new(authority.clone(), KEY, TIMEOUT);
    let (validated, report) = thread::scope(|scope| -> TestResult<(bool, ServeReport)> {
        let serving = scope.spawn(|| server.serve(&listener, 3));
        let lease = authority.acquire("coord-a");
        let validated = TcpGrantChannel::new(addr, KEY, TIMEOUT).validate(&lease);
        // Opened whatever `validate` returned and never `?`-ed: the scope
        // joins the server, so an early return here would park the test in
        // the very accept this pins.
        for _ in 0..2 {
            drop(TcpStream::connect_timeout(&addr, TIMEOUT));
        }
        let report = joined(serving);
        Ok((validated?, report))
    })?;
    assert!(validated, "the honest question is answered before the stop");
    assert_eq!(report.answered(), 1, "only the honest question is answered");
    assert_eq!(
        report.refused(),
        2,
        "each empty connection is refused and counted"
    );
    assert_eq!(authority.epoch(), 1, "an empty connection moves nothing");
    Ok(())
}

/// Relay one honest exchange from `proxy` to `server` and record the reply,
/// then answer the next question on `proxy` with that recorded reply.
fn relay_then_replay(proxy: &TcpListener, server: SocketAddr) -> TestResult<Vec<u8>> {
    let (mut inbound, _) = proxy.accept()?;
    inbound.set_read_timeout(Some(TIMEOUT))?;
    let mut request = vec![0u8; QUESTION];
    inbound.read_exact(&mut request)?;
    let mut upstream = TcpStream::connect_timeout(&server, TIMEOUT)?;
    upstream.set_read_timeout(Some(TIMEOUT))?;
    upstream.write_all(&request)?;
    let mut recorded = Vec::new();
    upstream.read_to_end(&mut recorded)?;
    inbound.write_all(&recorded)?;
    drop(inbound);
    let (mut inbound, _) = proxy.accept()?;
    inbound.set_read_timeout(Some(TIMEOUT))?;
    inbound.read_exact(&mut request)?;
    inbound.write_all(&recorded)?;
    Ok(recorded)
}

/// A replayed reply is refused: the answer recorded at epoch 1, replayed after
/// a successor moved the epoch, does not validate the epoch-1 lease.
#[test]
fn a_replayed_reply_is_refused() -> TestResult {
    let authority = AuthorityHandle::new();
    let (outcome, report) = with_server(&authority, 1, |server| {
        let (proxy, proxy_addr) = loopback()?;
        let channel = TcpGrantChannel::new(proxy_addr, KEY, TIMEOUT);
        thread::scope(|scope| {
            let relay = scope
                .spawn(|| relay_then_replay(&proxy, server).map_err(|error| error.to_string()));
            let lease = authority.acquire("coord-a");
            let honest = channel.validate(&lease);
            let successor = authority.acquire("coord-b");
            let replayed = channel.validate(&lease);
            let recorded = joined(relay)?;
            Ok((honest?, replayed, recorded.len(), successor.epoch()))
        })
    })?;
    let (honest, replayed, recorded, successor) = outcome;
    assert!(honest, "the relayed honest answer must validate");
    assert_eq!(recorded, 60, "the recorded reply must be one whole frame");
    assert_eq!(successor, 2, "the successor must hold epoch 2");
    match replayed {
        Err(error) => assert!(
            error.to_string().contains("another question"),
            "the replay must be refused for its nonce: {error}"
        ),
        Ok(valid) => return Err(format!("a replayed reply must not answer: {valid}").into()),
    }
    assert_eq!(
        report.answered(),
        1,
        "the server answered only the honest question"
    );
    Ok(())
}

/// An unreachable authority withholds grants: the begin is unvalidated,
/// nothing is written, and a zero timeout answers nothing.
#[test]
fn an_unreachable_authority_withholds_grants() -> TestResult {
    let scratch = Scratch::new("dist-lease-tcp-dead")?;
    let (listener, addr) = loopback()?;
    drop(listener);
    let lease = AuthorityHandle::new().acquire("coord-a");
    {
        let store = TenantStore::open(scratch.path())?;
        let host =
            Coordinator::with_fence(store, "host-a", TcpGrantChannel::new(addr, KEY, TIMEOUT));
        match host.begin_run(&lease, &start("run-tcp-dead", "host-a"), &LANES) {
            Err(DispatchError::Unvalidated(_)) => {}
            other => {
                return Err(format!(
                    "a dead authority must leave the grant unvalidated: {other:?}"
                )
                .into());
            }
        }
    }
    let store = TenantStore::open(scratch.path())?;
    assert!(
        store.run("acme", "run-tcp-dead").is_err(),
        "an unvalidated begin must write nothing"
    );
    match TcpGrantChannel::new(addr, KEY, Duration::ZERO).validate(&lease) {
        Err(error) => assert!(
            error.to_string().contains("zero timeout"),
            "the refusal must name the zero timeout: {error}"
        ),
        Ok(valid) => return Err(format!("a zero timeout must answer nothing: {valid}").into()),
    }
    Ok(())
}
