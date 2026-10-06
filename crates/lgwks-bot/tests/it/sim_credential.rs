//! Row 3 of issue #278, as a deterministic simulation: a credential that
//! expires while a bot holds it.
//!
//! One seed drives everything — the capability set, whether the grant named a
//! lifetime, how long that lifetime is, how much of it the run spends, and which
//! status an upstream reports. The clock is the shared [`Sim`]'s virtual one, so
//! a credential that lapses after three days costs three arithmetic operations.
//! Nothing reads the wall clock, so the same seed replays to the same trace hash
//! on a busy CI box and on a laptop.
//!
//! The subject is the shipped machinery: `GrantSet::issue_at` over a virtual
//! `Clock`, and `Auth::check` as every verb call makes it. Nothing here is a
//! reimplementation of the expiry rule — the model only says what the answer
//! *should* be, and a rule that disagreed with it would fail.
//!
//! The properties, one per family:
//!
//! - a proof is refused exactly when the clock has passed its lifetime, and the
//!   refusal names the capabilities it covered;
//! - a grant that named no lifetime is never refused for expiry, whatever the
//!   clock says;
//! - an upstream refusal maps to the repair for a credential status and to no
//!   repair for a transport one, and neither is retryable;
//! - the same seed gives the same trace hash, and adjacent seeds diverge.

use std::error::Error;
use std::time::Duration;

use lgwks_bot::cap::{Cap, is_credential_status, upstream_credential_rejection};
use lgwks_bot::clock::Clock;
use lgwks_bot::error::BotError;
use lgwks_bot::spec::Need;
use lgwks_bot::{GrantSet, RetryClass};

use crate::sim::Sim;

type TestResult = Result<(), Box<dyn Error>>;

/// The seed space this file sweeps.
///
/// Wider than the shared `sim::SEED_SPACE` because a scenario here is a grant,
/// a clock advance and a check — no I/O, no processes — so a thousand seeds
/// cost microseconds and the coverage the row asks for is a thousand draws.
const SEEDS: u64 = 1024;

/// The first seed of the sweep. Not zero: a zero seed is the one value the
/// shared generator remaps.
const FIRST_SEED: u64 = 1;

/// The four shipped capabilities, as the draw indexes into.
///
/// A function rather than a constant because `Cap`'s constructors are not
/// `const`: a capability is a borrowed-or-owned name, and building one at
/// compile time would pin the borrowed spelling that `must_use`-style
/// comparisons treat as identical to the owned one.
fn capabilities() -> [Cap; 4] {
    [Cap::net(), Cap::fs(), Cap::sys(), Cap::notify()]
}

/// The statuses a receiver can report, and what each one means here.
///
/// [`TRANSPORT`] stands for "the transport never reached a receiver at all",
/// which is the case with no status and therefore no credential verdict.
const STATUSES: [u16; 5] = [401, 403, 404, 500, TRANSPORT];

/// The status a receiver that was never reached reports.
const TRANSPORT: u16 = 0;

/// Return a typed test failure, emitting it first.
///
/// The emission is what the `scan` lane requires of every `return Err`: a
/// caller that sees only a value has no signal. Built in one place so every
/// scenario's refusal is identical rather than one line per call site.
fn refuse(cause: impl Into<String>) -> Result<(), Box<dyn Error>> {
    let refusal: Result<(), Box<dyn Error>> = Err(cause.into().into());
    lgwks_std::trace::debug!(
        error = ?refusal.as_ref().err(),
        "scenario: returning an error to the caller"
    );
    refusal
}

/// What one seed asked for.
struct Draw {
    /// How many capabilities the run needs, as the draw cut it.
    width: u32,
    /// The capabilities the run needs.
    required: Vec<Cap>,
    /// Whether the grant named a lifetime at all.
    expiring: bool,
    /// The lifetime, in milliseconds.
    ttl_ms: u64,
    /// How much of it the run spends, in milliseconds.
    spent_ms: u64,
    /// The status the upstream reports.
    status: u16,
}

/// Cut one seed's draw.
///
/// The width is drawn within the capability list's own length and the status
/// index within [`STATUSES`]'s, so neither draw can name an element that is not
/// there; the lookups still refuse rather than substitute, so a list edited
/// without its draw fails here instead of quietly sweeping a smaller space.
fn draw_for(sim: &mut Sim) -> Result<Draw, Box<dyn Error>> {
    let shipped = capabilities();
    let status_count = u32::try_from(STATUSES.len())?;
    let rng = sim.rng();
    let width = rng.between(1, u32::try_from(shipped.len())?);
    let required: Vec<Cap> = shipped.into_iter().take(usize::try_from(width)?).collect();
    let status_index = usize::try_from(rng.below(status_count))?;
    let status = *STATUSES
        .get(status_index)
        .ok_or("a status index drawn below the list's length")?;
    Ok(Draw {
        width,
        // One seed in four grants a perpetual credential, so the "no lifetime is
        // not expiry" family is exercised on the same sweep as the rest rather
        // than in a separate world where it always holds.
        expiring: rng.chance(750),
        ttl_ms: u64::from(rng.between(1, 5_000)),
        spent_ms: u64::from(rng.between(0, 6_000)),
        status,
        required,
    })
}

/// The grant a draw asks for, and the lifetime it names.
fn grant_for(draw: &Draw) -> GrantSet {
    let mut grants = GrantSet::empty();
    for capability in &draw.required {
        grants = if draw.expiring {
            grants.grant_expiring(capability.clone(), Duration::from_millis(draw.ttl_ms))
        } else {
            grants.grant(capability.clone())
        };
    }
    grants
}

/// One scenario: mint the proof, spend the clock, check, and record the verdict.
fn scenario(sim: &mut Sim) -> Result<(), Box<dyn Error>> {
    let draw = draw_for(sim)?;
    let clock = Clock::virtual_at(Duration::ZERO);
    let proof = grant_for(&draw).issue_at(&draw.required, &clock)?;

    sim.trace.record_number("ttl_ms", draw.ttl_ms);
    sim.trace.record_number("spent_ms", draw.spent_ms);
    sim.trace.record_number("required", u64::from(draw.width));
    sim.record(if draw.expiring {
        "expiring"
    } else {
        "perpetual"
    });

    // The model: a grant with no lifetime is never expired, and one with a
    // lifetime is expired once the clock reaches it. Equality expires, because a
    // credential's life is spent at its expiry rather than one tick after it.
    let expired = draw.expiring && draw.spent_ms >= draw.ttl_ms;

    clock.advance(Duration::from_millis(draw.spent_ms))?;
    match proof.check(&draw.required) {
        Ok(()) => {
            sim.record("admitted");
            if expired {
                return refuse("a credential past its lifetime must not be admitted");
            }
        }
        Err(BotError::CredentialExpired {
            ref capabilities,
            expired_at,
            now,
        }) => {
            sim.record("expired");
            if !expired {
                return refuse("a credential inside its lifetime must not be refused");
            }
            if capabilities.len() != draw.required.len() {
                return refuse("the refusal must name every capability the proof covered");
            }
            let expected = Duration::from_millis(draw.ttl_ms);
            if expired_at != expected || now != Duration::from_millis(draw.spent_ms) {
                return refuse("the refusal must carry the two readings it decided on");
            }
        }
        Err(other) => {
            return refuse(format!(
                "a lapsed credential is CredentialExpired, not {other}"
            ));
        }
    }

    // The upstream half, on the same draw. The classifier is the shipped
    // predicate, and the model says what it must answer for every status the
    // sweep can draw: only a permission status is a credential refusal.
    if is_credential_status(draw.status) != matches!(draw.status, 401 | 403 | 404) {
        return refuse(format!("status {} is misclassified", draw.status));
    }
    if !is_credential_status(draw.status) {
        // A transport failure has not refused the credential, so no adapter may
        // build the credential repair for it. Recorded rather than asserted,
        // because "no repair" here is the absence of a call rather than a value.
        sim.record("transport");
        return Ok(());
    }
    let rejection = upstream_credential_rejection("net::probe", draw.status, &draw.required);
    if !matches!(rejection.retry_class(), RetryClass::Never) {
        return refuse(format!(
            "status {} rejected the credential, and the same credential is never retried",
            draw.status
        ));
    }
    sim.record("no-retry");
    match rejection {
        BotError::CredentialRejected { ref needs, .. } => {
            // Exactly one need, re-granting exactly what the call required: an
            // empty need set would pass an `all` check and repair nothing.
            let mut sorted = draw.required.clone();
            sorted.sort_unstable();
            let repairable = matches!(
                needs.needs(),
                [Need::CredentialExpired { capabilities, .. }] if *capabilities == sorted
            );
            if !repairable {
                return refuse(format!(
                    "status {} is a credential refusal and must carry its repair",
                    draw.status
                ));
            }
        }
        other => {
            return refuse(format!(
                "an upstream refusal is CredentialRejected, not {other}"
            ));
        }
    }
    Ok(())
}

/// Every seed in the space, run once, and the trace hash each produced.
fn sweep() -> Result<Vec<u64>, Box<dyn Error>> {
    let mut hashes = Vec::new();
    for seed in FIRST_SEED..FIRST_SEED.saturating_add(SEEDS) {
        let mut sim = Sim::new(seed);
        scenario(&mut sim)?;
        hashes.push(sim.hash());
    }
    Ok(hashes)
}

/// The main property over the whole declared space: a proof is admitted
/// exactly while its credential lives, and refused exactly once it has not.
#[test]
fn a_seeded_credential_lives_exactly_as_long_as_its_grant() -> TestResult {
    let hashes = sweep()?;
    assert_eq!(
        hashes.len(),
        usize::try_from(SEEDS)?,
        "the sweep must cover every seed it declares"
    );
    Ok(())
}

/// The control the admitted arm needs: a grant that named no lifetime is never
/// refused, however far the clock is driven. Without it, a family that refused
/// everything would pass the one above.
#[test]
fn a_perpetual_grant_is_never_refused_however_far_the_clock_advances() -> TestResult {
    let mut admitted = 0usize;
    for _seed in FIRST_SEED..FIRST_SEED.saturating_add(SEEDS) {
        let clock = Clock::virtual_at(Duration::ZERO);
        let proof = GrantSet::empty()
            .grant(Cap::net())
            .issue_at(&[Cap::net()], &clock)?;
        clock.advance(Duration::from_secs(86_400))?;
        if proof.check(&[Cap::net()]).is_ok() {
            admitted = admitted.saturating_add(1);
        }
    }
    assert_eq!(
        admitted,
        usize::try_from(SEEDS)?,
        "a day of logical time must not expire a credential that named no lifetime"
    );
    Ok(())
}

/// The replay receipt: one seed, one hash, twice — with adjacent seeds measured
/// for divergence, which is what makes a differing hash mean anything.
#[test]
fn the_same_seed_replays_to_the_same_credential_trace() -> TestResult {
    let first = sweep()?;
    let second = sweep()?;
    assert_eq!(
        first, second,
        "the same seed produced two different traces, so the hash is not a receipt"
    );
    let collisions = first.windows(2).filter(|pair| pair[0] == pair[1]).count();
    let pairs = first.len().saturating_sub(1);
    assert!(
        collisions * 10 <= pairs,
        "adjacent seeds must not share a trace: {collisions} of {pairs} pairs collided, so the \
         hash is not seed-sensitive"
    );
    Ok(())
}
