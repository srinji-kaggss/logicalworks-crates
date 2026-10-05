//! Seeded simulations of the HTTP exchange read path over controlled loopback
//! servers.
//!
//! One seed drives the declared ceiling, the body length, the selected
//! [`BodyPolicy`], the byte chunking the server uses, and whether the transport
//! is torn before its declared length. Time is not simulated — the exchange is
//! synchronous — but every other decision is, so a failing assertion prints its
//! seed and the same seed replays the same script. Each family checks the
//! outcome against a model written from the contract, never from the
//! implementation under test.
#![cfg(feature = "http")]
#![forbid(unsafe_code)]

use crate::rng;

#[cfg(feature = "http")]
mod sim {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    use lgwks_std::http::{
        self, BodyPolicy, Error, FailureKind, FailureStage, Options, Truncation,
    };

    use crate::sim_http::rng::Rng;

    /// Read through one HTTP request head, returning the bytes seen.
    ///
    /// The head is accumulated across reads so a `\r\n\r\n` split across two
    /// reads cannot end the scan early; both server shapes share this one read.
    fn read_request_head(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
        let mut request = [0_u8; 1024];
        let mut head = Vec::new();
        loop {
            let read = stream.read(&mut request)?;
            if read == 0 {
                break;
            }
            head.extend_from_slice(&request[..read]);
            if head.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        Ok(head)
    }

    /// The non-power-of-two ceilings every family draws from.
    const CEILINGS: [usize; 4] = [73, 1000, 3003, 10_000];

    /// Seeds exercised per family.
    const SEEDS: u64 = 48;

    /// Read one request head and answer it with `reply`, returning the head.
    fn serve_once(listener: &TcpListener, reply: &[u8]) -> std::io::Result<Vec<u8>> {
        let (mut stream, _) = listener.accept()?;
        let head = read_request_head(&mut stream)?;
        stream.write_all(reply)?;
        Ok(head)
    }

    /// A `200 OK` reply declaring `content_length` and carrying `body`.
    fn response(content_length: usize, body: &[u8]) -> Vec<u8> {
        let mut reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n"
        )
        .into_bytes();
        reply.extend_from_slice(body);
        reply
    }

    /// One seeded script for the ceiling family.
    struct CeilingScript {
        /// The declared body ceiling.
        ceiling: usize,
        /// The number of body bytes the server sends.
        body_len: usize,
        /// Whether the caller selected [`BodyPolicy::Preview`].
        preview: bool,
    }

    /// Derive the ceiling script from `seed` alone.
    fn ceiling_script(seed: u64) -> CeilingScript {
        let mut rng = Rng::new(seed);
        let ceiling = CEILINGS[rng.below(CEILINGS.len())];
        let body_len = match rng.below(5) {
            0 => ceiling,
            1 => ceiling.saturating_sub(1),
            2 => ceiling.saturating_add(1),
            3 => ceiling.saturating_sub(rng.below(8).saturating_add(1)),
            _ => ceiling.saturating_mul(2),
        };
        CeilingScript {
            ceiling,
            body_len: body_len.max(1),
            preview: rng.below(2) == 1,
        }
    }

    /// Every seeded ceiling and body length lands on the declared outcome.
    #[test]
    fn seeded_ceiling_families_match_the_declared_outcome() -> Result<(), Box<dyn std::error::Error>>
    {
        for seed in 0..SEEDS {
            let script = ceiling_script(seed);
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let port = listener.local_addr()?.port();
            let body = vec![b'x'; script.body_len];
            let reply = response(script.body_len, &body);
            let url = format!("http://127.0.0.1:{port}/");
            let options = Options::default()
                .max_body_bytes(script.ceiling)
                .body_policy(if script.preview {
                    BodyPolicy::Preview
                } else {
                    BodyPolicy::Whole
                });
            let result = std::thread::scope(|scope| {
                scope.spawn(|| serve_once(&listener, &reply));
                http::get_with(&url, &options)
            });
            let over_ceiling = script.body_len > script.ceiling;
            match result {
                Ok(response) => {
                    assert!(
                        !over_ceiling || script.preview,
                        "seed {seed}: a {}-byte body past a {}-byte ceiling under Whole must be \
                         refused, not returned",
                        script.body_len,
                        script.ceiling
                    );
                    let expected_len = script.body_len.min(script.ceiling);
                    assert_eq!(
                        response.body().len(),
                        expected_len,
                        "seed {seed}: body {} ceiling {} policy preview={}",
                        script.body_len,
                        script.ceiling,
                        script.preview
                    );
                    let expected_truncation = if over_ceiling {
                        Truncation::Cut
                    } else {
                        Truncation::Complete
                    };
                    assert_eq!(
                        response.truncation, expected_truncation,
                        "seed {seed}: truncation must be a fact about the body, not the policy"
                    );
                }
                Err(error) => {
                    assert!(
                        !script.preview && over_ceiling,
                        "seed {seed}: a body within the ceiling must not be refused: {error:?}"
                    );
                    assert_eq!(
                        error,
                        Error::BodyTooLarge {
                            limit: script.ceiling
                        },
                        "seed {seed}: an over-ceiling whole body is refused with its limit"
                    );
                }
            }
        }
        // The script is a pure function of the seed: the same seed replays.
        let first: Vec<usize> = (0..SEEDS)
            .map(|seed| ceiling_script(seed).body_len)
            .collect();
        let second: Vec<usize> = (0..SEEDS)
            .map(|seed| ceiling_script(seed).body_len)
            .collect();
        assert_eq!(
            first, second,
            "the same seed must derive the same script on every replay"
        );
        Ok(())
    }

    /// One seeded script for the torn-transport family.
    struct TornScript {
        /// The `Content-Length` the server declares.
        content_length: usize,
        /// The number of body bytes the server actually writes before closing.
        sent: usize,
        /// The piece sizes the server writes the body in.
        chunks: Vec<usize>,
    }

    /// Derive the torn-transport script from `seed` alone.
    fn torn_script(seed: u64) -> TornScript {
        let mut rng = Rng::new(seed ^ 0x7011_0000);
        let content_length = CEILINGS[rng.below(CEILINGS.len())];
        let complete = rng.below(2) == 1;
        let sent = if complete {
            content_length
        } else {
            rng.below(content_length)
        };
        let chunk_count = rng.below(4).saturating_add(1);
        let chunks = (0..chunk_count)
            .map(|_| rng.below(256).saturating_add(1))
            .collect();
        TornScript {
            content_length,
            sent,
            chunks,
        }
    }

    /// Write the declared header, then exactly `sent` body bytes in `chunks`.
    fn serve_torn(listener: &TcpListener, script: &TornScript) -> std::io::Result<()> {
        let (mut stream, _) = listener.accept()?;
        let _head = read_request_head(&mut stream)?;
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            script.content_length
        );
        stream.write_all(header.as_bytes())?;
        let mut written = 0_usize;
        let mut index = 0_usize;
        let pieces = script.chunks.len().max(1);
        while written < script.sent {
            let piece = script
                .chunks
                .get(index.checked_rem(pieces).unwrap_or(0))
                .copied()
                .unwrap_or(1);
            let take = piece.min(script.sent.saturating_sub(written)).max(1);
            stream.write_all(&vec![b'x'; take])?;
            written = written.saturating_add(take);
            index = index.saturating_add(1);
        }
        Ok(())
    }

    /// A transport torn before its declared length is never a complete body.
    #[test]
    fn a_seeded_torn_transport_is_never_a_complete_body() -> Result<(), Box<dyn std::error::Error>>
    {
        for seed in 0..SEEDS {
            let script = torn_script(seed);
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let port = listener.local_addr()?.port();
            let url = format!("http://127.0.0.1:{port}/");
            let options = Options::default().max_body_bytes(script.content_length);
            let result = std::thread::scope(|scope| {
                scope.spawn(|| serve_torn(&listener, &script));
                http::get_with(&url, &options)
            });
            if script.sent == script.content_length {
                let response = result.map_err(|error| {
                    format!("seed {seed}: a complete body must not error: {error:?}")
                })?;
                assert_eq!(
                    response.body().len(),
                    script.content_length,
                    "seed {seed}: every declared body byte is retained"
                );
                assert_eq!(
                    response.truncation,
                    Truncation::Complete,
                    "seed {seed}: a body that ended at its declared length is complete"
                );
            } else {
                let error = result.err().ok_or_else(|| {
                    format!(
                        "seed {seed}: a body torn at {} of {} bytes must be refused",
                        script.sent, script.content_length
                    )
                })?;
                assert!(
                    matches!(
                        error,
                        Error::Failure {
                            stage: FailureStage::Body,
                            kind: FailureKind::Transport,
                            ..
                        }
                    ),
                    "seed {seed}: a torn transport is a transport failure at the body stage, got \
                     {error:?}"
                );
            }
        }
        Ok(())
    }

    /// One tenant request: send `id` as the tenant header, return the status.
    fn tenant_request(url: &str, id: &str) -> Result<u16, Error> {
        let options = Options::default().header("X-Tenant", id).max_body_bytes(64);
        http::get_with(url, &options).map(|response| response.status)
    }

    /// Accept `count` connections and return each request head.
    fn serve_many(listener: &TcpListener, reply: &[u8], count: usize) -> Vec<Vec<u8>> {
        let mut heads = Vec::new();
        for _ in 0..count {
            match serve_once(listener, reply) {
                Ok(head) => heads.push(head),
                Err(_) => break,
            }
        }
        heads
    }

    /// Two tenants sharing one endpoint keep their headers to themselves.
    ///
    /// This is the multi-tenant axis: the same URL (one resource) is polled
    /// concurrently by two identities carrying different `X-Tenant` values, and
    /// each request must carry its own value and nobody else's. A shared buffer,
    /// a reused agent keyed on the URL, or a header leak would put one tenant's
    /// value on the other's request.
    #[test]
    fn two_tenants_on_one_endpoint_stay_isolated() -> Result<(), Box<dyn std::error::Error>> {
        for seed in 0..SEEDS {
            let mut rng = Rng::new(seed ^ 0x7e11_0000);
            let left_id = format!("tenant-{}-left", rng.below(1_000));
            let mut right_id = format!("tenant-{}-right", rng.below(1_000));
            if right_id == left_id {
                right_id.push('!');
            }
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let port = listener.local_addr()?.port();
            let reply = response(2, b"ok");
            let url = format!("http://127.0.0.1:{port}/");
            let (left_status, right_status, heads) =
                std::thread::scope(|scope| -> Result<_, Box<dyn std::error::Error>> {
                    let server = scope.spawn(|| serve_many(&listener, &reply, 2));
                    let left_url = url.clone();
                    let left_tenant = left_id.clone();
                    let left = scope.spawn(move || tenant_request(&left_url, &left_tenant));
                    let right_tenant = right_id.clone();
                    let right = scope.spawn(move || tenant_request(&url, &right_tenant));
                    let left_status = left.join().map_err(|_| "left tenant panicked")??;
                    let right_status = right.join().map_err(|_| "right tenant panicked")??;
                    let heads = server.join().map_err(|_| "tenant server panicked")?;
                    Ok((left_status, right_status, heads))
                })?;
            assert_eq!(
                (left_status, right_status),
                (200, 200),
                "seed {seed}: both tenants reach the endpoint"
            );
            let mut seen = Vec::new();
            for head in &heads {
                let text = String::from_utf8_lossy(head).to_ascii_lowercase();
                let values: Vec<String> = text
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .filter(|entry| entry.0.trim() == "x-tenant")
                    .map(|entry| entry.1.trim().to_owned())
                    .collect();
                assert_eq!(
                    values.len(),
                    1,
                    "seed {seed}: each request carries exactly one tenant header, saw {values:?}"
                );
                seen.push(values[0].clone());
            }
            seen.sort();
            let mut expected = vec![left_id.to_ascii_lowercase(), right_id.to_ascii_lowercase()];
            expected.sort();
            assert_eq!(
                seen, expected,
                "seed {seed}: each tenant's request carries only its own value"
            );
        }
        Ok(())
    }
}
