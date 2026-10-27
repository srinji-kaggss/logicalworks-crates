//! Seeded pooling simulations: burst math over a keep-alive loopback server.
//!
//! One seed drives the burst length and whether the pool expires
//! immediately. Time is not simulated — the exchange is synchronous — but
//! every connection decision is, so a failing assertion prints its seed and
//! the same seed replays the same script. Each family checks the outcome
//! against a model written from the contract, never from the implementation
//! under test.
#![cfg(feature = "http")]
#![forbid(unsafe_code)]

use crate::loopback;
use crate::rng;

#[cfg(feature = "http")]
mod sim {
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use lgwks_std::http::{Client, Options, PoolConfig};

    use crate::sim_http_pool::loopback::read_request_head;
    use crate::sim_http_pool::rng::Rng;

    /// Seeds exercised per family.
    const SEEDS: u64 = 24;

    /// One seeded script for the reuse family.
    struct ReuseScript {
        /// Exchanges in the burst.
        burst: usize,
        /// Whether the pool expires every connection at once.
        zero_age: bool,
    }

    /// Derive the reuse script from `seed` alone.
    fn reuse_script(seed: u64) -> ReuseScript {
        let mut rng = Rng::new(seed);
        let burst = [1_usize, 2, 4, 8, 16, 32][rng.below(6)];
        ReuseScript {
            burst,
            zero_age: rng.below(2) == 1,
        }
    }

    /// Serve `script.burst` requests on as few connections as the client
    /// allows. Returns the connections accepted.
    fn serve_script(script: &ReuseScript) -> Result<usize, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let served = Arc::new(AtomicUsize::new(0));
        let served_in = Arc::clone(&served);
        let burst = script.burst;
        let url = format!("http://127.0.0.1:{port}/");
        let pool = if script.zero_age {
            PoolConfig::default().max_idle_age(std::time::Duration::ZERO)
        } else {
            PoolConfig::default()
        };
        let (accepted, answered) =
            std::thread::scope(|scope| -> Result<_, Box<dyn std::error::Error>> {
                let server = scope.spawn(move || {
                    let mut connections = 0_usize;
                    while served_in.load(Ordering::SeqCst) < burst {
                        let (mut stream, _) = match listener.accept() {
                            Ok(served) => served,
                            Err(_) => return connections,
                        };
                        connections = connections.saturating_add(1);
                        loop {
                            let open = match read_request_head(&mut stream) {
                                Ok(head) => head.windows(4).any(|window| window == b"\r\n\r\n"),
                                Err(_) => false,
                            };
                            if !open {
                                break;
                            }
                            served_in.fetch_add(1, Ordering::SeqCst);
                            if stream
                                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                                .is_err()
                            {
                                break;
                            }
                            if served_in.load(Ordering::SeqCst) >= burst {
                                break;
                            }
                        }
                    }
                    connections
                });
                let client = Client::with_pool(Options::default(), pool)?;
                for _ in 0..burst {
                    let response = client.get_with(&url, client.options())?;
                    assert_eq!(response.status, 200, "every seeded exchange answers");
                }
                drop(client);
                let accepted = server.join().map_err(|payload| {
                    let message = match payload.downcast_ref::<&str>() {
                        Some(text) => (*text).to_owned(),
                        None => match payload.downcast_ref::<String>() {
                            Some(text) => text.clone(),
                            None => "a payload that is not text".to_owned(),
                        },
                    };
                    format!("reuse server panicked: {message}")
                })?;
                Ok((accepted, served.load(Ordering::SeqCst)))
            })?;
        assert_eq!(
            answered, script.burst,
            "the server answered every exchange it was asked for"
        );
        Ok(accepted)
    }

    /// Every seeded burst costs one connection pooled, or one per exchange
    /// when the pool expires immediately.
    #[test]
    fn seeded_bursts_match_the_pooling_model() -> Result<(), Box<dyn std::error::Error>> {
        for seed in 0..SEEDS {
            let script = reuse_script(seed);
            let accepted = serve_script(&script)?;
            let expected = if script.zero_age { script.burst } else { 1 };
            assert_eq!(
                accepted, expected,
                "seed {seed}: a burst of {} with zero_age={} costs {expected}, cost {accepted}",
                script.burst, script.zero_age
            );
        }
        // The script is a pure function of the seed: the same seed replays.
        let first: Vec<(usize, bool)> = (0..SEEDS)
            .map(|seed| {
                let script = reuse_script(seed);
                (script.burst, script.zero_age)
            })
            .collect();
        let second: Vec<(usize, bool)> = (0..SEEDS)
            .map(|seed| {
                let script = reuse_script(seed);
                (script.burst, script.zero_age)
            })
            .collect();
        assert_eq!(
            first, second,
            "the same seed must derive the same script on every replay"
        );
        Ok(())
    }
}
