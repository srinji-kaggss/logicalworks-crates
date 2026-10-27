//! Seeded streaming simulations: chunking and ceilings over loopback.
//!
//! One seed drives the body length, the server's write pieces, the declared
//! ceiling and the body policy. Time is not simulated — the exchange is
//! synchronous — but every other decision is, so a failing assertion prints
//! its seed and the same seed replays the same script. Each family checks
//! the outcome against a model written from the contract, never from the
//! implementation under test.
#![cfg(feature = "http")]
#![forbid(unsafe_code)]

use crate::loopback;
use crate::rng;

#[cfg(feature = "http")]
mod sim {
    use std::io::Write;
    use std::net::TcpListener;

    use lgwks_std::http::{BodyPolicy, Client, Error, Options, Truncation};

    use crate::sim_http_stream::loopback::read_request_head;
    use crate::sim_http_stream::rng::Rng;

    /// Seeds exercised per family.
    const SEEDS: u64 = 24;

    /// The non-power-of-two ceilings every family draws from.
    const CEILINGS: [usize; 4] = [73, 1000, 3003, 10_000];

    /// One seeded script for the stream-ceiling family.
    struct StreamScript {
        /// The declared body ceiling.
        ceiling: usize,
        /// The number of body bytes the server sends.
        body_len: usize,
        /// The piece sizes the server writes the body in.
        pieces: Vec<usize>,
        /// Whether the caller selected [`BodyPolicy::Preview`].
        preview: bool,
    }

    /// Derive the stream script from `seed` alone.
    fn stream_script(seed: u64) -> StreamScript {
        let mut rng = Rng::new(seed);
        let ceiling = CEILINGS[rng.below(CEILINGS.len())];
        let body_len = match rng.below(5) {
            0 => ceiling,
            1 => ceiling.saturating_sub(1).max(1),
            2 => ceiling.saturating_add(1),
            3 => ceiling
                .saturating_sub(rng.below(8).saturating_add(1))
                .max(1),
            _ => ceiling.saturating_mul(2),
        };
        let piece_count = rng.below(4).saturating_add(1);
        let pieces = (0..piece_count)
            .map(|_| rng.below(256).saturating_add(1))
            .collect();
        StreamScript {
            ceiling,
            body_len: body_len.max(1),
            pieces,
            preview: rng.below(2) == 1,
        }
    }

    /// Serve one chunked body in the script's pieces, then close.
    fn serve_chunked(listener: &TcpListener, script: &StreamScript) -> std::io::Result<()> {
        let (mut stream, _) = listener.accept()?;
        let head = read_request_head(&mut stream)?;
        if !head.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(());
        }
        stream.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")?;
        let mut written = 0_usize;
        let mut index = 0_usize;
        while written < script.body_len {
            let position = index.checked_rem(script.pieces.len());
            let piece = match position.and_then(|at| script.pieces.get(at)) {
                Some(piece) => *piece,
                None => 1,
            };
            let take = piece.min(script.body_len.saturating_sub(written)).max(1);
            let chunk = vec![b'y'; take];
            stream.write_all(format!("{:X}\r\n", take).as_bytes())?;
            stream.write_all(&chunk)?;
            stream.write_all(b"\r\n")?;
            written = written.saturating_add(take);
            index = index.saturating_add(1);
        }
        stream.write_all(b"0\r\n\r\n")?;
        Ok(())
    }

    /// Read a stream whole through 5-byte windows.
    fn drain(
        response: &mut lgwks_std::http::StreamResponse,
    ) -> Result<(Vec<u8>, Option<Truncation>), Error> {
        let mut body = Vec::new();
        let mut window = [0_u8; 5];
        loop {
            match response.body().read_chunk(&mut window)? {
                None => return Ok((body, response.body().truncation())),
                Some(0) => continue,
                Some(read) => {
                    let bytes = window
                        .get(..read)
                        .ok_or(Error::BodyTooLarge { limit: read })?;
                    body.extend_from_slice(bytes);
                }
            }
        }
    }

    /// Every seeded stream lands on the declared outcome.
    #[test]
    fn seeded_streams_match_the_declared_outcome() -> Result<(), Box<dyn std::error::Error>> {
        for seed in 0..SEEDS {
            let script = stream_script(seed);
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let port = listener.local_addr()?.port();
            let url = format!("http://127.0.0.1:{port}/");
            let options = Options::default()
                .max_body_bytes(script.ceiling)
                .body_policy(if script.preview {
                    BodyPolicy::Preview
                } else {
                    BodyPolicy::Whole
                });
            let result = std::thread::scope(|scope| {
                scope.spawn(|| serve_chunked(&listener, &script));
                let client = Client::new(options)?;
                let mut response = client.get_stream(&url)?;
                drain(&mut response)
            });
            let over_ceiling = script.body_len > script.ceiling;
            match result {
                Ok((body, truncation)) => {
                    assert!(
                        !over_ceiling || script.preview,
                        "seed {seed}: a {}-byte stream past a {}-byte ceiling under Whole must be \
                         refused, not returned",
                        script.body_len,
                        script.ceiling
                    );
                    let expected_len = script.body_len.min(script.ceiling);
                    assert_eq!(
                        body.len(),
                        expected_len,
                        "seed {seed}: stream body {} ceiling {} preview={}",
                        script.body_len,
                        script.ceiling,
                        script.preview
                    );
                    assert!(
                        body.iter().all(|byte| *byte == b'y'),
                        "seed {seed}: every streamed byte is the served byte"
                    );
                    let expected = if over_ceiling {
                        Truncation::Cut
                    } else {
                        Truncation::Complete
                    };
                    assert_eq!(
                        truncation,
                        Some(expected),
                        "seed {seed}: truncation is a fact about the stream"
                    );
                }
                Err(error) => {
                    assert!(
                        !script.preview && over_ceiling,
                        "seed {seed}: a stream within the ceiling must not be refused: {error:?}"
                    );
                    assert_eq!(
                        error,
                        Error::BodyTooLarge {
                            limit: script.ceiling
                        },
                        "seed {seed}: an over-ceiling whole stream is refused with its limit"
                    );
                }
            }
        }
        // The script is a pure function of the seed: the same seed replays.
        let first: Vec<(usize, usize)> = (0..SEEDS)
            .map(|seed| {
                let script = stream_script(seed);
                (script.body_len, script.ceiling)
            })
            .collect();
        let second: Vec<(usize, usize)> = (0..SEEDS)
            .map(|seed| {
                let script = stream_script(seed);
                (script.body_len, script.ceiling)
            })
            .collect();
        assert_eq!(
            first, second,
            "the same seed must derive the same script on every replay"
        );
        Ok(())
    }
}
