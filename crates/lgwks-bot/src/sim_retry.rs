//! Seeded model check of RQ-009's retry rule.
//!
//! One seed draws every fact [`retry_admissible`] reads — the class, the
//! budget and attempts used, live authority, the dispatched and proposed
//! payloads, the evidence, a deduplication contract over a drawn key and
//! payload with a drawn window and late-arrival behaviour, and the time since
//! the attempt — and every family checks the decision against a reference
//! written from the rule as RQ-009 states it:
//!
//! ```text
//! retry_admissible = budget_remaining AND within_deadline AND live_authority
//!   AND unchanged_logical_intent
//!   AND (proven_not_applied OR contract_valid_for_same_key_and_payload)
//! ```
//!
//! `within_deadline` is `lgwks_std::retry`'s rule (INV-STD-RETRY): the elapsed
//! time reaching the policy's deadline refuses, so equality refuses.
//!
//! - the decision is the reference's on every draw, refusals in their order;
//! - a permanent failure short-circuits to that one refusal;
//! - granting live authority never adds a refusal;
//! - a contract covers a resend inside its window exactly when key and payload
//!   match, and past it only when the remote refuses late duplicates;
//! - a refusal reads as every reason, in order;
//! - a scope label is admitted up to its ceiling and refused past it with the
//!   observed length;
//! - the same seed replays the same decisions and distinct seeds diverge.
//!
//! Sources: RQ-009 (REQUIREMENTS.md), the IETF idempotency-key draft's
//! retention window, and Stripe's 24-hour key expiry as the shape of a window
//! past which a resend is a new request.

use core::time::Duration;

use crate::effect::{
    ActionDigest, ActionId, AttemptId, EffectIdentity, EffectKey, EnvironmentEpoch, EnvironmentId,
    FlowRevision, RunId,
};
use crate::error::RetryClass;
use crate::journal::EffectEvidence;
use crate::retry::{
    DedupScope, DeduplicationContract, LateArrival, MAX_SCOPE_BYTES, RetryDecision, RetryFacts,
    RetryRefusal, retry_admissible,
};
use crate::sim_stream::{RECEIPT_START, Stream, fold};
use crate::spec::RetryPolicy;

/// The seeds every family replays.
const SEEDS: [u64; 4] = [
    0x9E7A_0000_0000_0001,
    0x9E7A_0000_0000_0002,
    0x9E7A_FFFF_0000_0003,
    0xDEDB_9E7A_0000_0004,
];

/// Fact sets each family draws per seed.
const DRAWS: usize = 256;

/// The backoff base a policy is built with: `retry_admissible` reads the budget
/// and the deadline, never the delay.
const BASE_DELAY: Duration = Duration::from_millis(100);

/// The two logical request keys a draw picks from.
const ACTIONS: [&str; 2] = [
    "1112131415161718191a1b1c1d1e1f20",
    "3132333435363738393a3b3c3d3e3f40",
];

/// The two payload bindings a draw picks from.
const DIGESTS: [&str; 2] = [
    "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef",
    "0102030405060708090a0b0c0d0e0f10f1f2f3f4f5f6f7f8f9fafbfcfdfeff00",
];

/// A family's result.
type Outcome = Result<(), Box<dyn std::error::Error>>;

/// The attempt that may have landed, on `action` carrying `digest`.
fn dispatched(action: &str, digest: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
    let identity = EffectIdentity::new(
        RunId::from_hex("0102030405060708090a0b0c0d0e0f10")?,
        EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30")?,
        FlowRevision::from_tagged(
            "blake3_256",
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        )?,
    );
    let action = ActionId::from_hex(action)?;
    let attempt = AttemptId::from_decimal("1")?;
    let payload = ActionDigest::from_tagged("blake3_256", digest)?;
    let epoch = EnvironmentEpoch::from_decimal("1")?;
    Ok(identity.key(action, attempt, payload, epoch))
}

/// One drawn contract: key, payload, window and late-arrival behaviour.
#[derive(Debug, Clone, Copy)]
struct Terms {
    /// Index into [`ACTIONS`].
    action: usize,
    /// Index into [`DIGESTS`].
    payload: usize,
    /// The retention window.
    retention: Duration,
    /// What happens past it.
    late: LateArrival,
}

/// One drawn set of facts, kept as plain values so the reference can read
/// them without the type under test.
#[derive(Debug, Clone, Copy)]
struct Draw {
    /// What the failure permits.
    class: RetryClass,
    /// Attempts the policy allows.
    max_attempts: u32,
    /// Attempts already used.
    attempts_used: u32,
    /// Whether authority is live.
    authority: bool,
    /// Index into [`ACTIONS`] of the dispatched attempt.
    action: usize,
    /// Index into [`DIGESTS`] of the dispatched payload.
    dispatched: usize,
    /// Index into [`DIGESTS`] of a redrawn proposed payload, or `None` when the
    /// proposal is the dispatched payload unchanged.
    proposed: Option<usize>,
    /// What the caller established.
    evidence: Option<EffectEvidence>,
    /// The contract, when one is recorded.
    contract: Option<Terms>,
    /// Time since the attempt.
    elapsed: Duration,
    /// The policy's total budget across attempts.
    deadline: Duration,
}

impl Draw {
    /// Facts drawn from `stream`.
    fn from_stream(stream: &mut Stream) -> Result<Self, Box<dyn std::error::Error>> {
        let class = stream.choose(&[
            RetryClass::Never,
            RetryClass::Safe,
            RetryClass::RequiresEvidence,
        ])?;
        let max_attempts = u32::try_from(stream.below(5)?)?.saturating_add(1);
        let attempts_used = u32::try_from(stream.below(7)?)?;
        let evidence = stream.choose(&[
            None,
            Some(EffectEvidence::Applied),
            Some(EffectEvidence::NotApplied),
        ])?;
        let contract = if stream.below(2)? == 0 {
            None
        } else {
            let action = stream.below(ACTIONS.len())?;
            let payload = stream.below(DIGESTS.len())?;
            let seconds = u64::try_from(stream.below(100)?)?;
            let late = stream.choose(&[
                LateArrival::Refused,
                LateArrival::Applied,
                LateArrival::Unspecified,
            ])?;
            Some(Terms {
                action,
                payload,
                retention: Duration::from_secs(seconds),
                late,
            })
        };
        let authority = stream.below(4)? != 0;
        let action = stream.below(ACTIONS.len())?;
        let dispatched = stream.below(DIGESTS.len())?;
        let proposed = if stream.below(4)? == 0 {
            Some(stream.below(DIGESTS.len())?)
        } else {
            None
        };
        let elapsed = Duration::from_secs(u64::try_from(stream.below(200)?)?);
        let deadline = Duration::from_secs(u64::try_from(stream.below(250)?)?);
        Ok(Self {
            class,
            max_attempts,
            attempts_used,
            authority,
            action,
            dispatched,
            proposed,
            evidence,
            contract,
            elapsed,
            deadline,
        })
    }

    /// The proposed payload: the dispatched one unless the draw changed it.
    fn proposed_digest(&self) -> usize {
        match self.proposed {
            Some(redrawn) => redrawn,
            None => self.dispatched,
        }
    }

    /// Whether the recorded contract rules out a second application.
    fn covered(&self) -> bool {
        self.contract.is_some_and(|terms| {
            terms.action == self.action
                && terms.payload == self.dispatched
                && (self.elapsed < terms.retention || terms.late == LateArrival::Refused)
        })
    }

    /// The rule as RQ-009 states it.
    fn reference(&self) -> RetryDecision {
        if self.class == RetryClass::Never {
            return RetryDecision::Refused(vec![RetryRefusal::PermanentFailure]);
        }
        let proven = self.evidence == Some(EffectEvidence::NotApplied);
        let reasons: Vec<RetryRefusal> = [
            (self.attempts_used >= self.max_attempts).then_some(RetryRefusal::BudgetExhausted {
                attempts_used: self.attempts_used,
                max_attempts: self.max_attempts,
            }),
            (self.elapsed >= self.deadline).then_some(RetryRefusal::DeadlineExhausted {
                elapsed: self.elapsed,
                deadline: self.deadline,
            }),
            (!self.authority).then_some(RetryRefusal::AuthorityNotLive),
            (self.proposed_digest() != self.dispatched).then_some(RetryRefusal::IntentChanged),
            (!proven && !self.covered()).then_some(RetryRefusal::NotProvenUndone),
        ]
        .into_iter()
        .flatten()
        .collect();
        match (reasons.is_empty(), proven) {
            (false, _) => RetryDecision::Refused(reasons),
            (true, true) => RetryDecision::Safe,
            (true, false) => RetryDecision::UnderContract,
        }
    }

    /// The facts as the type under test reads them.
    fn facts(&self) -> Result<RetryFacts, Box<dyn std::error::Error>> {
        let digest = |index: usize| -> Result<&str, String> {
            DIGESTS
                .get(index)
                .copied()
                .ok_or_else(|| format!("no digest {index}"))
        };
        let action = ACTIONS.get(self.action).copied().ok_or("no such action")?;
        let mut facts = RetryFacts::new(
            RetryPolicy::new(self.max_attempts, BASE_DELAY, self.deadline),
            self.attempts_used,
            self.class,
            dispatched(action, digest(self.dispatched)?)?,
            ActionDigest::from_tagged("blake3_256", digest(self.proposed_digest())?)?,
        )
        .with_elapsed(self.elapsed);
        if self.authority {
            facts = facts.with_live_authority();
        }
        if let Some(evidence) = self.evidence {
            facts = facts.with_evidence(evidence);
        }
        if let Some(terms) = self.contract {
            let terms_action = ACTIONS.get(terms.action).copied().ok_or("no such action")?;
            let terms_payload = digest(terms.payload)?;
            facts = facts.with_contract(DeduplicationContract::new(
                ActionId::from_hex(terms_action)?,
                ActionDigest::from_tagged("blake3_256", terms_payload)?,
                DedupScope::new("tenant-7")?,
                terms.retention,
                terms.late,
            ));
        }
        Ok(facts)
    }
}

/// Every drawn fact set under each seed.
fn sweep(mut check: impl FnMut(u64, &Draw) -> Outcome) -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        for _ in 0..DRAWS {
            let draw = Draw::from_stream(&mut stream)?;
            check(seed, &draw)?;
        }
    }
    Ok(())
}

#[test]
/// The decision is the reference's on every draw, refusals in their order.
fn sim_the_decision_is_the_rule_as_stated() -> Outcome {
    sweep(|seed, draw| {
        assert_eq!(
            retry_admissible(&draw.facts()?),
            draw.reference(),
            "seed {seed:#x}: {draw:?}"
        );
        Ok(())
    })
}

#[test]
/// A decision permits a retry exactly when it carries no refusal.
fn sim_permission_and_refusals_agree() -> Outcome {
    sweep(|seed, draw| {
        let decision = retry_admissible(&draw.facts()?);
        assert_eq!(
            decision.permits_retry(),
            decision.refusals().is_empty(),
            "seed {seed:#x}: {decision}"
        );
        Ok(())
    })
}

#[test]
/// A permanent failure short-circuits to that one refusal, whatever else holds.
fn sim_a_permanent_failure_short_circuits() -> Outcome {
    sweep(|seed, draw| {
        let never = Draw {
            class: RetryClass::Never,
            ..*draw
        };
        assert_eq!(
            retry_admissible(&never.facts()?).refusals(),
            &[RetryRefusal::PermanentFailure],
            "seed {seed:#x}: {never:?}"
        );
        Ok(())
    })
}

#[test]
/// Granting live authority never adds a refusal, and removes exactly the
/// authority refusal.
fn sim_live_authority_only_ever_removes_its_own_refusal() -> Outcome {
    sweep(|seed, draw| {
        let without = retry_admissible(
            &Draw {
                authority: false,
                ..*draw
            }
            .facts()?,
        );
        let with = retry_admissible(
            &Draw {
                authority: true,
                ..*draw
            }
            .facts()?,
        );
        let expected: Vec<RetryRefusal> = without
            .refusals()
            .iter()
            .copied()
            .filter(|reason| *reason != RetryRefusal::AuthorityNotLive)
            .collect();
        assert_eq!(
            with.refusals(),
            expected.as_slice(),
            "seed {seed:#x}: {draw:?}"
        );
        Ok(())
    })
}

#[test]
/// A contract covers a resend inside its window exactly when key and payload
/// match, and past it only when the remote refuses late duplicates.
fn sim_a_contract_covers_exactly_its_key_payload_and_window() -> Outcome {
    sweep(|seed, draw| {
        let Some(terms) = draw.contract else {
            return Ok(());
        };
        let action = ACTIONS.get(draw.action).copied().ok_or("no such action")?;
        let terms_action = ACTIONS.get(terms.action).copied().ok_or("no such action")?;
        let payload = DIGESTS
            .get(draw.dispatched)
            .copied()
            .ok_or("no such digest")?;
        let terms_payload = DIGESTS
            .get(terms.payload)
            .copied()
            .ok_or("no such digest")?;
        let contract = DeduplicationContract::new(
            ActionId::from_hex(terms_action)?,
            ActionDigest::from_tagged("blake3_256", terms_payload)?,
            DedupScope::new("tenant-7")?,
            terms.retention,
            terms.late,
        );
        let rules_out = contract.rules_out_duplicate(
            ActionId::from_hex(action)?,
            ActionDigest::from_tagged("blake3_256", payload)?,
            draw.elapsed,
        );
        assert_eq!(rules_out, draw.covered(), "seed {seed:#x}: {draw:?}");
        Ok(())
    })
}

#[test]
/// A refusal reads as every reason, in order, after "do not retry".
fn sim_a_refusal_reads_as_every_reason_in_order() -> Outcome {
    sweep(|seed, draw| {
        let decision = retry_admissible(&draw.facts()?);
        if decision.permits_retry() {
            return Ok(());
        }
        let reasons: Vec<String> = decision
            .refusals()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            decision.to_string(),
            format!("do not retry: {}", reasons.join("; ")),
            "seed {seed:#x}: {draw:?}"
        );
        Ok(())
    })
}

#[test]
/// A scope label is admitted up to its ceiling and refused past it, naming the
/// observed length.
fn sim_a_scope_label_is_bounded_and_the_refusal_measured() -> Outcome {
    for seed in SEEDS {
        let mut stream = Stream::new(seed);
        for _ in 0..DRAWS {
            let len = stream.below(MAX_SCOPE_BYTES.saturating_mul(2))?;
            let label = "s".repeat(len);
            match DedupScope::new(label.clone()) {
                Ok(scope) => {
                    assert!(
                        len <= MAX_SCOPE_BYTES,
                        "seed {seed:#x}: {len} bytes admitted"
                    );
                    assert_eq!(scope.as_str(), label, "seed {seed:#x}: held as given");
                }
                Err(error) => {
                    assert!(len > MAX_SCOPE_BYTES, "seed {seed:#x}: {len} bytes refused");
                    assert_eq!(error.observed_len(), len, "seed {seed:#x}: measured");
                }
            }
        }
    }
    Ok(())
}

/// The replay receipt of every decision drawn under `seed`.
fn decision_trace(seed: u64) -> Result<u64, Box<dyn std::error::Error>> {
    let mut stream = Stream::new(seed);
    let mut trace = RECEIPT_START;
    for _ in 0..DRAWS {
        let decision = retry_admissible(&Draw::from_stream(&mut stream)?.facts()?);
        trace = fold(trace, format!("{decision};").as_bytes());
    }
    Ok(trace)
}

#[test]
/// The replay oracle: one seed, one decision trace.
fn sim_the_same_seed_replays_the_same_decisions() -> Outcome {
    for seed in SEEDS {
        assert_eq!(
            decision_trace(seed)?,
            decision_trace(seed)?,
            "seed {seed:#x} replays"
        );
    }
    Ok(())
}

#[test]
/// The divergence oracle: a sweep that ignored its seed would pass the replay.
fn sim_distinct_seeds_draw_different_decisions() -> Outcome {
    assert_ne!(
        decision_trace(SEEDS[0])?,
        decision_trace(SEEDS[1])?,
        "two seeds drew alike"
    );
    Ok(())
}
