//! `online` owns reachability probing with zero dependencies.
//!
//! [`probe`](crate::online::probe) tests one socket address;
//! [`is_online`](crate::online::is_online) tests the public
//! internet via anycast endpoints. Both return plain booleans — a failed
//! probe is a signal, not an error.
//!
//! [`is_online`](crate::online::is_online) exercises the live internet and is
//! not covered by the hermetic test suite; pin your own endpoint with
//! [`probe`](crate::online::probe) instead.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

// ── Probing ─────────────────────────────────────────────────────────────────

/// True when any address behind `addr` accepts TCP within `timeout`.
///
/// DNS failures, refused connections, and timeouts all report false.
pub fn probe(addr: impl ToSocketAddrs, timeout: Duration) -> bool {
    let Ok(mut addrs) = addr.to_socket_addrs() else {
        return false;
    };
    addrs.any(|candidate| TcpStream::connect_timeout(&candidate, timeout).is_ok())
}

/// True when the public internet is reachable within `timeout`.
///
/// Dials anycast endpoints over TCP (1.1.1.1:443, 8.8.8.8:53); true when
/// either answers. This is a heuristic for UI gating and diagnostics, not
/// a guarantee a given host is reachable — use [`crate::online::probe`] for that.
#[must_use]
pub fn is_online(timeout: Duration) -> bool {
    probe("1.1.1.1:443", timeout) || probe("8.8.8.8:53", timeout)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    const WAIT: Duration = Duration::from_secs(2);

    // These tests return `Result` rather than unwrapping: a bind refusal
    // reports its own `Debug` on failure, which is the same report `.unwrap`
    // would have panicked with, without an `unwrap` in the tree.
    #[test]
    fn open_port_probes_true() -> Result<(), std::io::Error> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        assert!(probe(format!("127.0.0.1:{port}"), WAIT));
        Ok(())
    }

    #[test]
    fn closed_port_probes_false() -> Result<(), std::io::Error> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        assert!(!probe(format!("127.0.0.1:{port}"), WAIT));
        Ok(())
    }

    #[test]
    fn unresolvable_host_probes_false() {
        assert!(!probe("nonexistent.invalid:80", WAIT));
    }
}
