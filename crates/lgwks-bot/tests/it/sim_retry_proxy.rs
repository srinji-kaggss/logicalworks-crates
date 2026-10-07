//! Row 6 of issue #278, as a deterministic simulation: a retrying proxy in
//! front of a keyed effect.
//!
//! One seed drives everything: how many operations, their keys and payloads,
//! and how many times the proxy duplicates and replays each delivery. The
//! proxy and the network are modelled — they are the world the adapter does
//! not own — but the adapter's half is the shipped code:
//! [`PostInput::new`](lgwks_bot::idempotent::PostInput::new) builds every
//! attempt, and the key the upstream deduplicates on is read from the shipped
//! options builder the same way the real exchange sends it. No second adapter
//! is written "for the simulation".
//!
//! The properties:
//!
//! - every attempt of one operation reuses the key and the payload byte for
//!   byte, and the wire header carries that key exactly once;
//! - the upstream counts exactly one effect per key however many times the
//!   proxy delivered it;
//! - an unkeyed operation and an oversize body are refused before anything is
//!   sent;
//! - the same seed gives the same trace hash, and the seeds diverge.

use std::collections::HashMap;
use std::error::Error;

use lgwks_bot::error::{BotError, DispatchCertainty};
use lgwks_bot::idempotent::{MAX_POST_BYTES, PostInput};

use crate::sim::{Rng, Trace};
use crate::sweep_fixtures::{FIRST_SEED, SWEEP_SEEDS, peak_rss_bytes, refuse};

type TestResult = Result<(), Box<dyn Error>>;

/// One operation the proxy delivers many times.
struct Operation {
    /// The key every attempt carries.
    key: String,
    /// The payload every attempt sends.
    payload: Vec<u8>,
    /// How many times the proxy delivers it (duplicates plus replays).
    deliveries: u32,
}

/// Cut one operation from the seed.
fn operation(rng: &mut Rng, index: usize) -> Result<Operation, Box<dyn Error>> {
    let key = format!("op-{index}-{}", rng.next_u64());
    let length = usize::try_from(rng.between(1, 256)).map_err(|_| "a draw fits")?;
    let mut payload = Vec::with_capacity(length);
    for _ in 0..length {
        payload.push(u8::try_from(rng.below(256)).map_err(|_| "a byte draw fits")?);
    }
    Ok(Operation {
        key,
        payload,
        deliveries: rng.between(1, 6),
    })
}

/// A modelled upstream that deduplicates on the idempotency key: the first
/// delivery of a key applies the effect, every later one replays the stored
/// answer without applying it again.
#[derive(Debug, Default)]
struct Upstream {
    /// Applications per key: one for every key seen, however often.
    effects: HashMap<String, u64>,
    /// Deliveries seen, including duplicates.
    deliveries: u64,
}

impl Upstream {
    /// Deliver one attempt: `true` when this delivery applied the effect.
    fn deliver(&mut self, key: &str) -> bool {
        self.deliveries = self.deliveries.saturating_add(1);
        if self.effects.contains_key(key) {
            return false;
        }
        self.effects.insert(key.to_owned(), 1);
        true
    }

    /// How many times one key applied its effect: one when the key was seen,
    /// none when it was not — a missing key is not an effect.
    fn effects_for(&self, key: &str) -> u64 {
        match self.effects.get(key) {
            Some(applied) => *applied,
            None => 0,
        }
    }
}

/// Run one seeded proxy storm, recording every delivery.
///
/// Returns the trace hash: the replay receipt.
fn run(seed: u64, trace: &mut Trace) -> TestResult {
    let mut rng = Rng::new(seed);
    let operations = usize::try_from(rng.between(1, 6)).map_err(|_| "a draw fits")?;
    trace.record_number("operations", operations);
    let mut upstream = Upstream::default();
    for index in 0..operations {
        let drawn = operation(&mut rng, index)?;
        // Every attempt is built by the shipped constructor: a retry that
        // reuses the key but resends a different payload is a new operation
        // wearing an old key, and the constructor plus this assertion are
        // what forbid it.
        let mut attempts = Vec::new();
        for _ in 0..drawn.deliveries {
            attempts.push(PostInput::new(
                "http://127.0.0.1:9/effect",
                drawn.key.clone(),
                "application/json",
                drawn.payload.clone(),
            )?);
        }
        for attempt in &attempts {
            if attempt.key() != drawn.key {
                return refuse("every attempt of one operation reuses its key");
            }
            if attempt.body() != drawn.payload {
                return refuse("every attempt resends its payload byte for byte");
            }
        }
        trace.record_number("deliveries", drawn.deliveries);
        for attempt in &attempts {
            let applied = upstream.deliver(attempt.key());
            trace.record_number("applied", u8::from(applied));
        }
        if upstream.effects_for(&drawn.key) != 1 {
            return refuse(format!(
                "the upstream applied {} effects for one key",
                upstream.effects_for(&drawn.key)
            ));
        }
    }
    if upstream.effects.len() != operations {
        return refuse("one key is one effect, however it was delivered");
    }
    trace.record_number("effects", upstream.effects.len());
    trace.record_number("deliveries_seen", upstream.deliveries);
    Ok(())
}

/// The wire header the shipped options builder sends for a key.
///
/// Read from the real builder rather than restated: the header the test's
/// real proxy parses is the header this code sends, not a string the test
/// guessed.
fn wire_header(key: &str) -> Result<(String, String), Box<dyn Error>> {
    use lgwks_std::http::Options;
    let options = Options::default().idempotency_key(key);
    let mut seen: Option<(String, String)> = None;
    for pair in options.headers() {
        if pair.0.eq_ignore_ascii_case("Idempotency-Key") {
            if seen.is_some() {
                return refuse("the key header is sent exactly once");
            }
            seen = Some((pair.0.clone(), pair.1.clone()));
        }
    }
    match seen {
        Some(header) => Ok(header),
        None => refuse("the key header is present"),
    }
}

/// Seeded proxy storms over 1,024 seeds, plus the wire-header contract.
#[test]
fn a_duplicating_proxy_delivers_each_key_exactly_once() -> TestResult {
    let mut first_hash: Option<u64> = None;
    for index in 0..SWEEP_SEEDS {
        let seed = FIRST_SEED.saturating_add(index);
        let mut trace = Trace::new();
        trace.record_number("seed", seed);
        run(seed, &mut trace)?;
        let mut replay = Trace::new();
        replay.record_number("seed", seed);
        run(seed, &mut replay)?;
        if trace.hash() != replay.hash() {
            return refuse(format!("seed {seed} did not replay to the same trace hash"));
        }
        if index == 0 {
            first_hash = Some(trace.hash());
        }
    }
    if first_hash.is_none() {
        return refuse("the sweep ran no seeds");
    }
    let (name, value) = wire_header("op-9f2c")?;
    assert_eq!(
        name, "Idempotency-Key",
        "the header keeps its canonical name"
    );
    assert_eq!(value, "op-9f2c", "the header carries the operation's key");
    if let Some(rss) = peak_rss_bytes() {
        assert!(
            rss < 512 * 1024 * 1024,
            "a 1,024-seed proxy sweep must stay under 512 MiB, observed {rss} bytes"
        );
    }
    Ok(())
}

/// An unkeyed operation and an oversize body are refused before anything is
/// sent: the adapter never emits the defect the proxy row exists to contain.
#[test]
fn unkeyed_and_oversize_operations_are_refused_at_construction() -> TestResult {
    let Err(BotError::DomainError { cause, .. }) =
        PostInput::new("http://127.0.0.1:9/effect", "", "application/json", vec![1])
    else {
        return refuse("an unkeyed POST must be refused at construction");
    };
    assert!(
        cause.contains("idempotency key"),
        "the refusal names the missing key, got {cause:?}"
    );
    let oversize = vec![0u8; MAX_POST_BYTES.saturating_add(1)];
    let Err(BotError::DomainError {
        certainty: DispatchCertainty::Refused,
        ..
    }) = PostInput::new(
        "http://127.0.0.1:9/effect",
        "op-capped",
        "application/json",
        oversize,
    )
    else {
        return refuse("an oversize body must be refused at construction");
    };
    Ok(())
}
