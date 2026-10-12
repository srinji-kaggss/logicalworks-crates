//! Seeded simulation of the keyed effect a retrying proxy may replay.
//!
//! One seed draws URLs, keys, content types and bodies on each side of the two
//! refusals [`PostInput::new`] makes — an empty key, a body past
//! [`MAX_POST_BYTES`] — and every family asserts what a caller sees:
//!
//! - a non-empty key and a body within the cap are admitted, and every field
//!   is held byte for byte;
//! - an empty key is refused as `Refused`, nothing sent, whatever else is
//!   drawn, and it is reported before an oversize body;
//! - the cap itself is admitted and one byte past it refused, with the excess
//!   named exactly;
//! - two attempts built from one operation are one value, and a different key
//!   or a different body is a different operation;
//! - the adapter dials out, so it requires exactly `bot.net` and its effects
//!   outlive the process;
//! - the same seed replays the same verdicts and distinct seeds diverge.
//!
//! Sources: draft-ietf-httpapi-idempotency-key-header (the key the receiver
//! deduplicates on), and Stripe's and Temporal's at-least-once delivery
//! discipline the module names.

use crate::error::{BotError, DispatchCertainty};
use crate::idempotent::{IdempotentPost, MAX_POST_BYTES, PostInput};
use crate::sim_stream::{RECEIPT_START, Stream, fold};
use crate::verb::{EffectLifetime, Execute};

/// The seeds every family replays.
const SEEDS: [u64; 4] = [
    0x1DE7_0000_0000_0001,
    0x1DE7_0000_0000_0002,
    0x1DE7_FFFF_0000_0003,
    0x9057_1DE7_0000_0004,
];

/// Effects each family draws per seed.
const DRAWS: usize = 96;

/// The URLs effects are posted to.
const URLS: [&str; 4] = [
    "http://127.0.0.1:9/effect",
    "https://api.example.test/v1/charges",
    "http://[::1]:8080/merge",
    "https://hooks.example.test/launch?tenant=acme",
];

/// The content types payloads are sent as.
const TYPES: [&str; 3] = ["application/json", "application/x-www-form-urlencoded", "text/plain"];

/// A family's result.
type Outcome = Result<(), String>;

/// One drawn effect.
struct Effect {
    /// Where it is posted.
    url: &'static str,
    /// Its idempotency key; empty when the draw plants the refusal.
    key: String,
    /// Its content type.
    content_type: &'static str,
    /// Its payload.
    body: Vec<u8>,
}

impl Effect {
    /// An effect drawn from `stream`: a key of 0 to 40 bytes (empty one time
    /// in eight), and a body near the cap one time in four.
    fn draw(stream: &mut Stream) -> Result<Self, String> {
        let key_len = if stream.below(8)? == 0 {
            0
        } else {
            stream.below(40)?.saturating_add(1)
        };
        let key = (0..key_len)
            .map(|_| stream.choose(b"0123456789abcdef-").map(char::from))
            .collect::<Result<String, String>>()?;
        let body_len = if stream.below(4)? == 0 {
            MAX_POST_BYTES
                .saturating_sub(8)
                .saturating_add(stream.below(17)?)
        } else {
            stream.below(512)?
        };
        let byte = u8::try_from(stream.below(256)?).map_err(|error| error.to_string())?;
        Ok(Self {
            url: stream.choose(&URLS)?,
            key,
            content_type: stream.choose(&TYPES)?,
            body: vec![byte; body_len],
        })
    }

    /// Build the effect's input.
    fn input(&self) -> Result<PostInput, BotError> {
        PostInput::new(self.url, self.key.as_str(), self.content_type, self.body.clone())
    }
}

/// Every drawn effect under each seed.
fn sweep(mut check: impl FnMut(u64, &Effect) -> Outcome) -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        for _ in 0..DRAWS {
            let effect = Effect::draw(&mut stream)?;
            check(seed, &effect)?;
        }
    }
    Ok(())
}

/// The refusal's cause, when the construction was refused as `Refused`.
fn refused_cause(result: Result<PostInput, BotError>) -> Result<Option<String>, String> {
    match result {
        Ok(_) => Ok(None),
        Err(BotError::DomainError {
            domain,
            certainty: DispatchCertainty::Refused,
            cause,
        }) if domain == "idempotent::post" => Ok(Some(cause)),
        Err(other) => Err(format!("expected a refusal that sent nothing, got {other}")),
    }
}

#[test]
/// A non-empty key and a body within the cap are admitted, every field held
/// byte for byte.
fn sim_an_admitted_effect_holds_every_field_byte_for_byte() -> Outcome {
    sweep(|seed, effect| {
        if effect.key.is_empty() || effect.body.len() > MAX_POST_BYTES {
            return Ok(());
        }
        let input = effect
            .input()
            .map_err(|error| format!("seed {seed:#x}: {error}"))?;
        assert_eq!(input.url(), effect.url, "seed {seed:#x}: url");
        assert_eq!(input.key(), effect.key, "seed {seed:#x}: key");
        assert_eq!(input.content_type(), effect.content_type, "seed {seed:#x}: type");
        assert_eq!(input.body(), effect.body.as_slice(), "seed {seed:#x}: body");
        Ok(())
    })
}

#[test]
/// An empty key is refused as `Refused` whatever else is drawn, and it is
/// reported before an oversize body.
fn sim_an_unkeyed_effect_is_refused_before_anything_else() -> Outcome {
    sweep(|seed, effect| {
        let unkeyed = Effect {
            url: effect.url,
            key: String::new(),
            content_type: effect.content_type,
            body: effect.body.clone(),
        };
        let cause = refused_cause(unkeyed.input())?
            .ok_or_else(|| format!("seed {seed:#x}: an unkeyed effect was admitted"))?;
        assert!(
            cause.contains("idempotency key"),
            "seed {seed:#x}: the refusal names the key: {cause}"
        );
        Ok(())
    })
}

#[test]
/// The verdict on every draw is the one the two rules give, in their order.
fn sim_every_verdict_is_the_rules_verdict() -> Outcome {
    sweep(|seed, effect| {
        let cause = refused_cause(effect.input())?;
        let expected_refusal = effect.key.is_empty() || effect.body.len() > MAX_POST_BYTES;
        assert_eq!(
            cause.is_some(),
            expected_refusal,
            "seed {seed:#x}: key {:?}, {} bytes",
            effect.key,
            effect.body.len()
        );
        Ok(())
    })
}

#[test]
/// The cap is admitted, and a body past it is refused naming its excess.
fn sim_the_cap_is_admitted_and_the_excess_named() -> Outcome {
    sweep(|seed, effect| {
        let key = if effect.key.is_empty() { "k" } else { effect.key.as_str() };
        let at_cap = PostInput::new(effect.url, key, effect.content_type, vec![0; MAX_POST_BYTES]);
        at_cap.map_err(|error| format!("seed {seed:#x}: the cap was refused: {error}"))?;
        let excess = effect.body.len().wrapping_rem(64).saturating_add(1);
        let past = PostInput::new(
            effect.url,
            key,
            effect.content_type,
            vec![0; MAX_POST_BYTES.saturating_add(excess)],
        );
        let cause = refused_cause(past)?
            .ok_or_else(|| format!("seed {seed:#x}: {excess} bytes past the cap was admitted"))?;
        assert!(
            cause.contains(&format!("body is {excess} bytes past")),
            "seed {seed:#x}: the excess is named: {cause}"
        );
        Ok(())
    })
}

#[test]
/// Two attempts of one operation are one value; a different key or body is a
/// different operation.
fn sim_retries_are_one_operation_and_changes_are_another() -> Outcome {
    sweep(|seed, effect| {
        let (Ok(first), Ok(retry)) = (effect.input(), effect.input()) else {
            return Ok(());
        };
        assert_eq!(first, retry, "seed {seed:#x}: a retry is the same request");
        let rekeyed = PostInput::new(
            effect.url,
            format!("{}-2", effect.key),
            effect.content_type,
            effect.body.clone(),
        )
        .map_err(|error| error.to_string())?;
        assert_ne!(first, rekeyed, "seed {seed:#x}: another key is another operation");
        let mut body = effect.body.clone();
        body.push(1);
        if let Ok(reshaped) = PostInput::new(effect.url, effect.key.as_str(), effect.content_type, body) {
            assert_ne!(first, reshaped, "seed {seed:#x}: another body is another payload");
        }
        Ok(())
    })
}

#[test]
/// The adapter dials out: it requires exactly `bot.net` and its effects
/// outlive the process, built either way.
fn sim_the_adapter_requires_the_network_and_nothing_else() {
    for adapter in [IdempotentPost::new(), IdempotentPost::default()] {
        let required: Vec<&str> = adapter.required_caps().iter().map(|cap| cap.as_str()).collect();
        assert_eq!(required, vec![crate::cap::Cap::NET], "exactly bot.net");
        assert_eq!(adapter.effect_lifetime(), EffectLifetime::External, "external effects");
        assert_eq!(adapter.domain_id(), "idempotent::post", "its domain");
    }
}

/// The replay receipt of every verdict drawn under `seed`.
fn verdict_trace(seed: u64) -> Result<u64, String> {
    let mut stream = Stream::new(seed);
    let mut trace = RECEIPT_START;
    for _ in 0..DRAWS {
        let effect = Effect::draw(&mut stream)?;
        let verdict = refused_cause(effect.input())?;
        trace = fold(trace, format!("{}:{verdict:?};", effect.body.len()).as_bytes());
    }
    Ok(trace)
}

#[test]
/// The replay oracle: one seed, one verdict trace.
fn sim_the_same_seed_replays_the_same_verdicts() -> Outcome {
    for seed in SEEDS {
        assert_eq!(verdict_trace(seed)?, verdict_trace(seed)?, "seed {seed:#x} replays");
    }
    Ok(())
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn sim_distinct_seeds_draw_different_verdicts() -> Outcome {
    assert_ne!(
        verdict_trace(SEEDS[0])?,
        verdict_trace(SEEDS[1])?,
        "two seeds drew alike"
    );
    Ok(())
}
