//! An ephemeral scope: a run with no persistence, and the boundary that makes
//! it safe to hand to real code.
//!
//! The claim under test is not "a scope can be built without arguments". It is
//! that the arguments it builds itself are ones a run can actually use: two
//! scopes share no identity, the minted environment is live in the broker it
//! was registered with, and the in-memory journal refuses to be the record
//! behind an effect that leaves the process. That last one is the whole reason
//! this is a capability rather than a testing shortcut — without it, an
//! ephemeral scope would silently accept a dispatch it cannot remember.
#![cfg(feature = "ephemeral")]

use std::collections::BTreeSet;

use lgwks_bot::broker::Broker;
use lgwks_bot::effect::{EnvironmentEpoch, RunId};
use lgwks_bot::journal::{DurabilityPromise, JournalError};
use lgwks_bot::spec::EffectScope;

/// The tests cross `MintError`, `BrokerError` and `JournalError`, and none of
/// the three converts into another, so each propagates through one box rather
/// than being flattened into a name that would be wrong twice.
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn two_ephemeral_scopes_share_no_identity() -> TestResult {
    let first = EffectScope::ephemeral()?;
    let second = EffectScope::ephemeral()?;

    assert_ne!(
        first.identity().run(),
        second.identity().run(),
        "two ephemeral runs minted the same run id"
    );
    assert_ne!(
        first.identity().environment(),
        second.identity().environment(),
        "two ephemeral runs minted the same environment"
    );
    assert_eq!(
        first.identity().flow(),
        second.identity().flow(),
        "an ephemeral run has no flow document, so every one binds the same revision"
    );
    Ok(())
}

/// Distinctness is a property over draws, not over one pair: a mint that
/// repeated on the third call would pass the test above and still be useless.
#[test]
fn minted_run_ids_do_not_repeat() -> TestResult {
    let mut seen = BTreeSet::new();
    for _ in 0..256 {
        let minted = RunId::mint()?;
        assert!(
            seen.insert(minted),
            "a minted run id repeated within 256 draws"
        );
    }
    assert_eq!(seen.len(), 256);
    Ok(())
}

/// A minted id has to be one the crate's own parser accepts, which is what
/// makes it wire-representable at all: 32 lowercase hex characters, never zero.
#[test]
fn a_minted_id_round_trips_through_its_wire_form() -> TestResult {
    for _ in 0..64 {
        let minted = RunId::mint()?;
        let rendered = minted.to_string();
        assert_eq!(rendered.len(), 32);
        assert_eq!(RunId::from_hex(&rendered)?, minted);
    }
    Ok(())
}

#[test]
fn the_minted_environment_is_registered_and_open() -> TestResult {
    let scope = EffectScope::ephemeral()?;
    let environment = scope.identity().environment();

    assert_eq!(
        scope.broker().epoch(environment),
        Some(EnvironmentEpoch::FIRST),
        "the ephemeral broker does not know the environment it minted"
    );
    assert!(
        scope.broker().is_open(environment),
        "a freshly minted ephemeral environment is not open"
    );
    Ok(())
}

/// The boundary that makes the constructor safe. An ephemeral journal reports
/// [`DurabilityPromise::Ephemeral`], and the trait's own guard refuses it as
/// the record behind an effect that has left the process.
#[test]
fn an_ephemeral_scope_refuses_an_external_handoff() -> TestResult {
    let scope = EffectScope::ephemeral()?;

    assert_eq!(scope.journal().durability(), DurabilityPromise::Ephemeral);

    match scope.journal().admit_external_handoff() {
        Err(JournalError::PromiseUnmet { required, offered }) => {
            assert_eq!(required, DurabilityPromise::ProcessCrash);
            assert_eq!(offered, DurabilityPromise::Ephemeral);
        }
        other => {
            return Err(
                format!("an ephemeral scope admitted an external handoff: {other:?}").into(),
            );
        }
    }
    Ok(())
}

/// The three pieces stay separable, which is the documented reason the
/// constructor supplies all three rather than fusing them: a caller that
/// outgrows the ephemeral case can read the identity back out and pair it with
/// a durable journal without rebuilding it.
#[test]
fn the_identity_survives_a_move_to_another_journal() -> TestResult {
    let scope = EffectScope::ephemeral()?;
    let identity = scope.identity();
    let journal = scope.into_journal();

    let mut broker = Broker::new();
    broker.register(identity.environment())?;
    let reopened = EffectScope::new(identity, broker, journal);

    assert_eq!(reopened.identity(), identity);
    assert_eq!(reopened.identity().run(), identity.run());
    Ok(())
}
