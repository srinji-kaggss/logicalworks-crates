//! `online` owns reachability probing with zero dependencies.
//!
//! [`probe`](crate::online::probe) tests one socket address;
//! [`is_online`](crate::online::is_online) tests the public
//! internet via anycast endpoints. Both return plain booleans: a failed
//! probe is a signal, not an error.
//!
//! [`is_online`](crate::online::is_online) exercises the live internet and is
//! not covered by the hermetic test suite; pin your own endpoint with
//! [`probe`](crate::online::probe) instead.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// Maximum resolved candidates considered by one probe.
const MAX_PROBE_ADDRESSES: usize = 64;

// ── Probing ─────────────────────────────────────────────────────────────────

/// True when any resolved address behind `addr` accepts TCP within the
/// connection budget.
///
/// DNS resolution is synchronous and occurs before the connection budget
/// starts; arbitrary caller-provided [`ToSocketAddrs`] implementations cannot
/// be preempted. At most the first 64 resolved addresses are tried, sharing
/// one monotonic timeout. DNS failures, refused connections, and timeouts all
/// report false.
pub fn probe(addr: impl ToSocketAddrs, timeout: Duration) -> bool {
    let Ok(addrs) = addr.to_socket_addrs() else {
        return false;
    };
    probe_candidates(addrs.take(MAX_PROBE_ADDRESSES), timeout)
}

/// Probe a caller-resolved address slice under one total monotonic budget.
///
/// At most the first 64 addresses are attempted. DNS is outside this function,
/// so callers that need a strict connection budget can resolve first and pass
/// the resulting addresses here.
///
/// ```rust
/// use std::net::SocketAddr;
/// use std::time::Duration;
///
/// let local = SocketAddr::from(([127, 0, 0, 1], 1));
/// let _reachable = lgwks_std::online::probe_resolved(&[local], Duration::from_millis(20));
/// ```
#[must_use]
pub fn probe_resolved(addrs: &[SocketAddr], timeout: Duration) -> bool {
    probe_candidates(addrs.iter().copied().take(MAX_PROBE_ADDRESSES), timeout)
}

/// Share one remaining timeout across the bounded address sequence.
fn probe_candidates(addrs: impl Iterator<Item = SocketAddr>, timeout: Duration) -> bool {
    probe_candidates_with(addrs, timeout, |candidate, remaining| {
        TcpStream::connect_timeout(candidate, remaining).is_ok()
    })
}

/// Run bounded candidates through one shared clock and connection operation.
///
/// Each candidate is offered an equal share of what remains, not all of it:
/// the budget is spent across the candidates rather than by the first. With
/// the whole remainder, one blackholed address (a filtered IPv6 route, a
/// dropped anycast prefix) consumed the entire budget and every later address,
/// including one that would have answered at once, was never dialed. The total
/// is still bounded by `timeout`, and a candidate that fails fast hands its
/// unused share on to the ones after it.
fn probe_candidates_with(
    addrs: impl Iterator<Item = SocketAddr>,
    timeout: Duration,
    mut connect: impl FnMut(&SocketAddr, Duration) -> bool,
) -> bool {
    // Bounded by the callers' `take(MAX_PROBE_ADDRESSES)`; collected only to
    // know how many shares the remainder is split into.
    let candidates: Vec<SocketAddr> = addrs.take(MAX_PROBE_ADDRESSES).collect();
    let started = Instant::now();
    for (index, candidate) in candidates.iter().enumerate() {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return false;
        }
        // `index < len`, so at least this candidate is left; the conversion
        // cannot fail for a list capped at 64.
        let left = u32::try_from(candidates.len().saturating_sub(index)).unwrap_or(u32::MAX);
        let share = remaining.checked_div(left).unwrap_or(remaining);
        if connect(candidate, share) {
            return true;
        }
    }
    false
}

/// True when the public internet is reachable within `timeout`.
///
/// Dials literal anycast endpoints over TCP (1.1.1.1:443, 8.8.8.8:53) under
/// one shared monotonic budget, each offered at least half of it, so a
/// blackholed first endpoint cannot starve the second; true when either
/// answers within its share. This is a heuristic
/// for UI gating and diagnostics, not a guarantee a given host is reachable;
/// use [`crate::online::probe`] for that.
#[must_use]
pub fn is_online(timeout: Duration) -> bool {
    let addresses = [
        SocketAddr::from(([1, 1, 1, 1], 443)),
        SocketAddr::from(([8, 8, 8, 8], 53)),
    ];
    probe_resolved(&addresses, timeout)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, TcpListener};

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

    /// Resolved probe succeeds without invoking a resolver.
    #[test]
    fn resolved_probe_reaches_a_loopback_listener() -> Result<(), std::io::Error> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        assert!(probe_resolved(&[address], WAIT));
        Ok(())
    }

    /// Every candidate receives only the remaining time from one monotonic budget.
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the injected dial delay verifies that later candidates receive the shared remaining budget"
    )]
    fn address_candidates_share_one_remaining_budget() {
        let candidates = [
            SocketAddr::from(([192, 0, 2, 1], 443)),
            SocketAddr::from(([198, 51, 100, 1], 443)),
        ];
        let mut budgets = Vec::new();
        let result = probe_candidates_with(
            candidates.into_iter(),
            Duration::from_millis(100),
            |_, remaining| {
                budgets.push(remaining);
                if budgets.len() == 1 {
                    std::thread::sleep(Duration::from_millis(60));
                }
                false
            },
        );
        assert!(!result, "all candidate failures report unreachable");
        assert_eq!(
            budgets.len(),
            2,
            "both addresses fit before the shared deadline"
        );
        assert!(
            budgets[1] <= Duration::from_millis(50),
            "the second candidate does not receive a restarted timeout"
        );
    }

    /// A blackholed first candidate spends only its share, so the second is
    /// still dialed and its answer is still heard.
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the injected dial sleeps out its whole share, which is what a blackholed address does"
    )]
    fn a_blackholed_candidate_does_not_starve_the_next() {
        let candidates = [
            SocketAddr::from(([192, 0, 2, 1], 443)),
            SocketAddr::from(([198, 51, 100, 1], 443)),
        ];
        let timeout = Duration::from_millis(100);
        let mut budgets = Vec::new();
        let result = probe_candidates_with(candidates.into_iter(), timeout, |_, share| {
            budgets.push(share);
            if budgets.len() == 1 {
                // Blackholed: no answer, and the dial waits out all it was given.
                std::thread::sleep(share);
                return false;
            }
            true
        });
        assert!(result, "the second candidate answers, so the probe is true");
        assert_eq!(budgets.len(), 2, "the second candidate was dialed");
        assert!(
            budgets[0] <= timeout / 2,
            "the first candidate is offered its share, not the whole budget"
        );
        assert!(
            !budgets[1].is_zero(),
            "the second candidate still has time left to answer"
        );
    }

    /// A deliberately slow resolver demonstrates the documented boundary:
    /// its synchronous work is outside the socket timeout.
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "this controlled resolver delay proves DNS work is outside the socket connection budget"
    )]
    fn resolver_delay_is_outside_the_connection_budget() {
        struct SlowResolver;

        impl ToSocketAddrs for SlowResolver {
            type Iter = std::vec::IntoIter<SocketAddr>;

            fn to_socket_addrs(&self) -> std::io::Result<Self::Iter> {
                std::thread::sleep(Duration::from_millis(80));
                Ok(vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 9))].into_iter())
            }
        }

        let started = Instant::now();
        assert!(!probe(SlowResolver, Duration::from_millis(10)));
        assert!(started.elapsed() >= Duration::from_millis(80));
    }
}
