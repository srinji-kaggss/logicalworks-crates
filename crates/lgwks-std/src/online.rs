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
use std::num::NonZeroU32;
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
    let started = Instant::now();
    probe_candidates_with(
        addrs,
        timeout,
        || started.elapsed(),
        |candidate, remaining| TcpStream::connect_timeout(candidate, remaining).is_ok(),
    )
}

/// A candidate count at `u32` width, for a list this probe has already capped.
///
/// `probe_candidates_with` collects at most [`MAX_PROBE_ADDRESSES`] entries, so
/// the count is at most 64 and the low four bytes of its little-endian form are
/// the count itself. Reading them is total where a checked conversion would
/// carry a refusal arm for a bound the collector has already applied.
fn capped_candidate_count(count: usize) -> u32 {
    let bytes = count.to_le_bytes();
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Run bounded candidates through one shared clock and connection operation.
///
/// `elapsed` is the clock: the time spent since the budget started. The probe
/// passes the monotonic clock; the tests pass a simulated one, so the budget
/// arithmetic is checked to the nanosecond rather than against however long a
/// loaded host takes to wake a parked thread.
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
    mut elapsed: impl FnMut() -> Duration,
    mut connect: impl FnMut(&SocketAddr, Duration) -> bool,
) -> bool {
    // Bounded by the callers' `take(MAX_PROBE_ADDRESSES)`; collected only to
    // know how many shares the remainder is split into.
    let candidates: Vec<SocketAddr> = addrs.take(MAX_PROBE_ADDRESSES).collect();
    for (index, candidate) in candidates.iter().enumerate() {
        let remaining = timeout.saturating_sub(elapsed());
        if remaining.is_zero() {
            return false;
        }
        // `enumerate` over a non-empty `Vec` keeps `index < len`, so at least
        // this candidate is left. `NonZeroU32` carries that: the count of
        // candidates still to try is a divisor a share exists for, and a count
        // of zero has no share to take — the whole remainder then stands, which
        // is what one candidate with no successors is owed.
        let Some(left) = NonZeroU32::new(capped_candidate_count(
            candidates.len().saturating_sub(index),
        )) else {
            return false;
        };
        let share = match remaining.checked_div(left.get()) {
            Some(share) => share,
            None => remaining,
        };
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
    //
    // A dial that has to wait out its share parks rather than sleeps: this
    // crate forbids `std::thread::sleep`, and `park_timeout` is the sanctioned
    // synchronous wait. Each test only ever asserts that *at least* the share
    // elapsed, so an unpark that returns early cannot make one pass.
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

    /// Two documentation-range addresses (RFC 5737): never routed, so no test
    /// here can dial a real host even if a clock were wrong.
    const CANDIDATES: [SocketAddr; 2] = [
        SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 443),
        SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1)), 443),
    ];

    /// Every candidate receives only the remaining time from one budget.
    ///
    /// The clock is simulated: the first dial spends 60 ms of a 100 ms budget,
    /// so the second is offered exactly the 40 ms left, not a restarted 100 ms.
    #[test]
    fn address_candidates_share_one_remaining_budget() {
        let clock = std::cell::Cell::new(Duration::ZERO);
        let mut budgets = Vec::new();
        let result = probe_candidates_with(
            CANDIDATES.into_iter(),
            Duration::from_millis(100),
            || clock.get(),
            |_, remaining| {
                budgets.push(remaining);
                if budgets.len() == 1 {
                    clock.set(clock.get().saturating_add(Duration::from_millis(60)));
                }
                false
            },
        );
        assert!(!result, "all candidate failures report unreachable");
        assert_eq!(
            budgets,
            [Duration::from_millis(50), Duration::from_millis(40)],
            "the first is offered half, the second what is left after the first spent 60 ms"
        );
    }

    /// A budget the candidates have already spent dials nothing more.
    #[test]
    fn a_spent_budget_dials_no_further_candidate() {
        let clock = std::cell::Cell::new(Duration::ZERO);
        let mut dialed = 0u32;
        let result = probe_candidates_with(
            CANDIDATES.into_iter(),
            Duration::from_millis(100),
            || clock.get(),
            |_, _| {
                dialed = dialed.saturating_add(1);
                clock.set(Duration::from_millis(100));
                false
            },
        );
        assert!(!result);
        assert_eq!(
            dialed, 1,
            "the second candidate has no time left, so it is not dialed"
        );
    }

    /// A blackholed first candidate spends only its share, so the second is
    /// still dialed and its answer is still heard.
    #[test]
    fn a_blackholed_candidate_does_not_starve_the_next() {
        let clock = std::cell::Cell::new(Duration::ZERO);
        let timeout = Duration::from_millis(100);
        let mut budgets = Vec::new();
        let result = probe_candidates_with(
            CANDIDATES.into_iter(),
            timeout,
            || clock.get(),
            |_, share| {
                budgets.push(share);
                if budgets.len() == 1 {
                    // Blackholed: no answer, and the dial waits out all it was given.
                    clock.set(clock.get().saturating_add(share));
                    return false;
                }
                true
            },
        );
        assert!(result, "the second candidate answers, so the probe is true");
        assert_eq!(
            budgets,
            [timeout / 2, timeout / 2],
            "the first is offered its share, not the whole budget, and the second the rest"
        );
    }

    /// A deliberately slow resolver demonstrates the documented boundary:
    /// its synchronous work is outside the socket timeout.
    #[test]
    fn resolver_delay_is_outside_the_connection_budget() {
        struct SlowResolver;

        impl ToSocketAddrs for SlowResolver {
            type Iter = std::vec::IntoIter<SocketAddr>;

            fn to_socket_addrs(&self) -> std::io::Result<Self::Iter> {
                std::thread::park_timeout(Duration::from_millis(80));
                Ok(vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 9))].into_iter())
            }
        }

        let started = Instant::now();
        assert!(!probe(SlowResolver, Duration::from_millis(10)));
        assert!(started.elapsed() >= Duration::from_millis(80));
    }

    /// A whole multi-candidate failure is bounded by one budget: four
    /// candidates that each wait out all they are offered spend the 120 ms
    /// between them, not 120 ms each. The clock is simulated, so the total is
    /// exact rather than "within some slack of" what a loaded host measured.
    #[test]
    fn a_whole_probe_fits_one_budget() {
        let candidates = [
            SocketAddr::from(([192, 0, 2, 1], 443)),
            SocketAddr::from(([198, 51, 100, 1], 443)),
            SocketAddr::from(([203, 0, 113, 1], 443)),
            SocketAddr::from(([192, 0, 2, 2], 443)),
        ];
        let timeout = Duration::from_millis(120);
        let clock = std::cell::Cell::new(Duration::ZERO);
        let mut shares = Vec::new();
        let result = probe_candidates_with(
            candidates.into_iter(),
            timeout,
            || clock.get(),
            |_, share| {
                shares.push(share);
                clock.set(clock.get().saturating_add(share));
                false
            },
        );
        assert!(
            !result,
            "every candidate fails, so the probe reports unreachable"
        );
        assert_eq!(
            shares,
            [Duration::from_millis(30); 4],
            "each candidate is offered an equal share of what is left"
        );
        assert_eq!(
            clock.get(),
            timeout,
            "the four candidates share one budget of {timeout:?}, not four"
        );
    }
}
