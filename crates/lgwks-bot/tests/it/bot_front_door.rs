//! The bot builder's documented examples, run as tests (#526).
//!
//! Each used to be a doctest that named the crate by path: the quick start, the zero poll
//! deadline, the lease authority's staling of a predecessor, the proposal boundary and its seeded
//! model, and the host's limits and definition identity. They are tests here, so each documented
//! claim runs under nextest with the rest of the suite and the module documentation says what a
//! test holds instead of printing a program.

use std::error::Error;
use std::time::Duration;

use lgwks_bot::dist_lease::AuthorityHandle;
use lgwks_bot::{Bot, BotError, GrantSet};

use crate::effects::memory_scope;

/// What every fallible test here returns.
type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn a_bot_built_with_an_effect_scope_ticks_with_no_chains() -> TestResult {
    let mut bot = Bot::builder("my-bot")
        .with_effects(memory_scope()?)
        .build(&GrantSet::all_shipped())?;
    assert_eq!(bot.tick()?, 0, "a bot with no chain fires nothing");
    Ok(())
}

#[test]
fn a_poll_deadline_of_zero_is_refused_at_build() -> TestResult {
    let refused = Bot::builder("zero")
        .with_poll_deadline(Duration::ZERO)
        .with_effects(memory_scope()?)
        .build(&GrantSet::empty());
    assert!(matches!(
        refused,
        Err(BotError::PollDeadlineUnbounded { .. })
    ));
    Ok(())
}

#[test]
fn the_second_acquire_stales_the_first_lease() {
    let authority = AuthorityHandle::new();
    let first = authority.acquire("host-a");
    let second = authority.acquire("host-b");
    assert!(
        authority.check(&first).is_err(),
        "the first lease must go stale"
    );
    assert!(
        authority.check(&second).is_ok(),
        "the second lease must be current"
    );
    assert_eq!(
        authority.epoch(),
        second.epoch(),
        "the epoch must be the second mint"
    );
}

#[cfg(feature = "script")]
mod proposal {
    use lgwks_bot::cap::Cap;
    use lgwks_bot::proposal::{
        Completion, Decoder, Intervention, LedgerLimits, Outcome, PlanBudget, PlanLimits,
        Provenance, Refusal, RepairLedger, Source, StubModel, Surface,
    };

    use super::TestResult;

    #[test]
    fn the_boundary_admits_what_is_registered_and_held_and_refuses_the_rest() -> TestResult {
        // The host declares what a proposal is allowed to say, and what the run holds.
        let surface = Surface::builder("acme")?
            .operation("read-report", &[Cap::fs()])?
            .holding(&[Cap::fs()])
            .build();
        let decoder = Decoder::new(PlanLimits::default());

        // Untrusted bytes naming an operation nobody registered.
        let refused = decoder.decode(&surface, b"op=install-dependency", Source::Model);
        assert!(
            matches!(refused.refusal(), Some(Refusal::UnknownOperation { .. })),
            "an unregistered operation is refused, whatever asked for it"
        );

        // A proposal naming only registered operations is admitted, and the capability it needs
        // is one the run already holds.
        let admitted = decoder.decode(&surface, b"op=read-report\nnote=ok", Source::Model);
        assert!(admitted.is_admitted(), "a registered operation is admitted");

        // The same operation for a run that does not hold `bot.fs` is refused by naming the
        // capability, so the repair is a deliberate grant.
        let poor = Surface::builder("acme")?
            .operation("read-report", &[Cap::fs()])?
            .build();
        let denied = decoder.decode(&poor, b"op=read-report", Source::Model);
        assert!(
            matches!(denied.refusal(), Some(Refusal::CapabilityNotHeld { .. })),
            "a capability the run does not hold is refused by name"
        );
        Ok(())
    }

    #[test]
    fn a_completion_claim_is_admitted_only_with_its_evidence_present() {
        let claim = Completion::claim("done", &["report-1"]);
        assert!(
            claim.admit(&[String::from("report-1")]).is_ok(),
            "its evidence is present"
        );
        assert!(
            matches!(claim.admit(&[]), Err(Refusal::NotEvidenced { .. })),
            "a claim naming absent evidence is NotEvidenced"
        );
    }

    #[test]
    fn repeated_unchanged_failure_reaches_a_finite_typed_intervention() {
        // Three identical failures are tolerated; the fourth is the intervention.
        let provenance = Provenance::for_model("acme", b"op=read-report");
        let mut ledger = RepairLedger::new(LedgerLimits::new(3, 8), provenance);
        for attempt in 1..=3 {
            assert!(
                ledger.record_failure("fingerprint-a").is_admitted(),
                "failure {attempt} of 3 is still within the repair ceiling"
            );
        }
        assert!(
            matches!(
                ledger.record_failure("fingerprint-a"),
                Outcome::Intervention(Intervention::NoProgress { repetitions: 4, .. })
            ),
            "the fourth identical failure is a typed intervention, not another repair"
        );

        // New evidence is recorded beside the root spend and does not erase it.
        assert!(
            ledger.record_evidence("fingerprint-a"),
            "evidence is recorded"
        );
        assert_eq!(ledger.spent(), 4, "the root spend is still four failures");
        assert_eq!(
            ledger.repetitions("fingerprint-a"),
            4,
            "and the count the ceiling is measured against has not moved"
        );
    }

    #[test]
    fn bounded_repair_is_a_declared_ceiling() {
        let mut budget = PlanBudget::new(2);
        assert!(
            budget.charge().is_ok(),
            "the first admission is within budget"
        );
        assert!(budget.charge().is_ok(), "so is the second");
        assert!(
            matches!(budget.charge(), Err(Refusal::Limit { .. })),
            "the third admission is refused with its ceiling named"
        );
    }

    #[test]
    fn a_seeded_model_is_a_pure_function_of_its_seed_and_its_output_names_its_source() -> TestResult
    {
        let surface = Surface::builder("acme")?
            .operation("read-report", &[])?
            .holding(&[Cap::fs()])
            .build();
        let decoder = Decoder::new(PlanLimits::default());

        // The same seed is the same bytes, every time.
        let model = StubModel::from_seed(7);
        assert_eq!(
            model.output(),
            StubModel::from_seed(7).output(),
            "a seeded model is a pure function of its seed"
        );

        // And what it emits is admitted or refused by the same rule as anything else.
        let outcome = decoder.decode(&surface, model.output(), Source::Model);
        match outcome.provenance() {
            Some(provenance) => assert!(
                provenance.source() == Source::Model,
                "every outcome names the model as the payload's source"
            ),
            None => return Err("an outcome carries the provenance of its payload".into()),
        }
        Ok(())
    }
}

#[cfg(feature = "script")]
mod host {
    use std::num::NonZeroUsize;
    use std::time::Duration;

    use lgwks_bot::effect::InputIdentity;
    use lgwks_bot::task::Host;
    use lgwks_std::hash::Hasher;

    use super::TestResult;

    #[test]
    fn a_host_reads_back_its_limits_and_a_clone_shares_the_budget() -> TestResult {
        let host = Host::builder("acme")?
            .max_concurrent_tasks(NonZeroUsize::new(8).ok_or("a non-zero ceiling")?)
            .default_deadline(Duration::from_secs(5))
            .build()?;

        assert_eq!(host.tenant().as_str(), "acme");
        assert_eq!(host.limits().max_concurrent_tasks(), 8);
        assert_eq!(host.limits().default_deadline(), Duration::from_secs(5));
        assert_eq!(host.admission().available_permits(), 8);

        let same_budget = host.clone();
        assert_eq!(
            same_budget.admission().available_permits(),
            8,
            "a clone shares the budget rather than minting one"
        );

        host.cancel();
        assert!(host.is_cancelled());
        Ok(())
    }

    #[test]
    fn a_host_names_the_definition_every_run_records_against() -> TestResult {
        let host = Host::builder("acme")?.build()?;
        let mut hasher = Hasher::new();
        7u32.write_identity(&mut hasher);
        let identity = host.definition("nightly", 3, Some(hasher.finalize()), 5);
        assert_eq!(identity.revision(), 3);
        assert_eq!(identity.steps(), 5);
        Ok(())
    }
}
