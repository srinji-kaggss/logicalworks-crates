//! Tests of the effect broker: authority, preparation against the journal tail, and every refusal.

use super::*;
use crate::effect::{ActionDigest, ActionId, AttemptId, EnvironmentId, FlowRevision, RunId};
use crate::journal::{EffectEvidence, MemoryJournal};
use std::collections::HashMap;

const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
const OTHER_ENV: &str = "5152535455565758595a5b5c5d5e5f60";
const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const DIGEST_HEX: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";

/// The crate refuses a panicking path anywhere, tests included, so a test
/// that needs a parsed value returns `Result` and propagates.
type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A key for the shared run and action, on the named environment and
/// generation.
fn key_on(env: &str, epoch: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
    let key_run = RunId::from_hex(RUN)?;
    let key_action = ActionId::from_hex(ACTION)?;
    let key_attempt = AttemptId::from_decimal("1")?;
    let key_flow_revision = FlowRevision::from_tagged("blake3_256", FLOW_HEX)?;
    let key_digest = ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?;
    let key_environment = EnvironmentId::from_hex(env)?;
    let key_epoch = EnvironmentEpoch::from_decimal(epoch)?;
    Ok(
        crate::effect::EffectIdentity::new(key_run, key_environment, key_flow_revision).key(
            key_action,
            key_attempt,
            key_digest,
            key_epoch,
        ),
    )
}

/// The environment id most of these tests use.
fn env() -> Result<EnvironmentId, Box<dyn std::error::Error>> {
    Ok(EnvironmentId::from_hex(ENV)?)
}

/// A key on the shared environment at the named generation.
fn key(epoch: &str) -> Result<EffectKey, Box<dyn std::error::Error>> {
    key_on(ENV, epoch)
}

/// Walk an attempt to the point where a dispatch may be prepared.
fn admitted(journal: &mut MemoryJournal, key: EffectKey) -> Result<(), Box<dyn std::error::Error>> {
    let tail = journal.tail();
    journal.compare_and_append(tail, &EffectEvent::IntentAdmitted { key })?;
    Ok(())
}

/// The shared subject of the dispatch tests: an environment the broker will
/// authorize, the key carrying its current generation, and a journal that
/// has already admitted that attempt.
///
/// One struct rather than a four-tuple, because the three facts have to be
/// the *same* facts: a test that assembled its own could admit one key and
/// authorize another, and every assertion after that would be about a
/// situation the module cannot produce.
struct AdmittedEnvironment {
    /// The broker, holding the environment open at `generation`.
    broker: Broker,
    /// The generation the broker issued, which `key` carries.
    generation: EnvironmentEpoch,
    /// The admitted attempt, on that environment and at that generation.
    key: EffectKey,
    /// A journal with that attempt already admitted.
    journal: MemoryJournal,
}

/// A broker holding the shared environment at its first generation, that
/// generation's key, and a journal with the attempt already admitted.
fn admitted_environment() -> Result<AdmittedEnvironment, Box<dyn std::error::Error>> {
    let mut broker = Broker::new();
    let generation = broker.register(env()?)?;
    let key = key(&generation.get().to_string())?;
    let mut journal = MemoryJournal::new();
    admitted(&mut journal, key)?;
    Ok(AdmittedEnvironment {
        broker,
        generation,
        key,
        journal,
    })
}

#[test]
fn register_takes_the_first_generation() -> TestResult {
    let mut broker = Broker::new();
    let epoch = broker.register(env()?)?;
    assert_eq!(broker.epoch(env()?), Some(epoch));
    assert!(broker.is_open(env()?));
    Ok(())
}

#[test]
fn a_second_register_of_one_environment_is_refused() -> TestResult {
    let mut broker = Broker::new();
    let first = broker.register(env()?)?;
    let refused = broker.register(env()?);
    assert!(
        matches!(refused, Err(BrokerError::AlreadyRegistered { .. })),
        "a double register must not silently reset anything"
    );
    assert_eq!(broker.epoch(env()?), Some(first));
    Ok(())
}

#[test]
fn a_current_generation_authorizes() -> TestResult {
    let mut broker = Broker::new();
    let first = broker.register(env()?)?;
    let authority = broker.authorize(key(&first.get().to_string())?)?;
    assert_eq!(authority.environment(), env()?);
    assert_eq!(authority.epoch(), first);
    Ok(())
}

#[test]
fn an_unknown_environment_is_refused() -> TestResult {
    let broker = Broker::new();
    let refused = broker.authorize(key("1")?);
    assert!(matches!(
        refused,
        Err(BrokerError::UnknownEnvironment { .. })
    ));
    Ok(())
}

#[test]
fn replacement_supersedes_a_command_prepared_before_it() -> TestResult {
    let mut broker = Broker::new();
    let first = broker.register(env()?)?;
    let stale = key(&first.get().to_string())?;
    assert!(broker.authorize(stale).is_ok());

    let second = broker.replace(env()?)?;
    assert_ne!(second, first);

    match broker.authorize(stale) {
        Err(BrokerError::Superseded {
            environment,
            presented,
            current,
        }) => {
            assert_eq!(environment, env()?);
            assert_eq!(presented, first);
            assert_eq!(current, second);
        }
        Err(other) => return Err(format!("expected a superseded refusal, got {other}").into()),
        Ok(authority) => {
            return Err(format!(
                "a command from a replaced generation must not be authorized, got {authority}"
            )
            .into());
        }
    }
    Ok(())
}

#[test]
fn a_generation_the_broker_never_issued_is_not_reported_as_stale() -> TestResult {
    let mut broker = Broker::new();
    let first = broker.register(env()?)?;
    let ahead = EnvironmentEpoch::new(NonZeroU64::new(first.get() + 1).ok_or("increment")?);
    let forged = key(&ahead.get().to_string())?;

    match broker.authorize(forged) {
        Err(BrokerError::NeverIssued { current, .. }) => assert_eq!(current, first),
        Err(other) => {
            return Err(format!("expected a never-issued refusal, got {other}").into());
        }
        Ok(authority) => {
            return Err(format!(
                "a generation the broker never issued must not authorize, got {authority}"
            )
            .into());
        }
    }
    Ok(())
}

#[test]
fn a_warrant_minted_before_a_replacement_is_refused_at_the_boundary() -> TestResult {
    let mut broker = Broker::new();
    let first = broker.register(env()?)?;
    let warrant = broker.authorize(key(&first.get().to_string())?)?;
    assert!(broker.revalidate(&warrant).is_ok());

    // The warrant was minted legitimately. A replacement between the mint
    // and the handoff is what makes it stale, and the boundary check is the
    // only thing that catches it.
    let second = broker.replace(env()?)?;

    match broker.revalidate(&warrant) {
        Err(BrokerError::Superseded {
            presented, current, ..
        }) => {
            assert_eq!(presented, first);
            assert_eq!(current, second);
        }
        Err(other) => return Err(format!("expected a superseded refusal, got {other}").into()),
        Ok(()) => {
            return Err("a warrant from before the replacement must not revalidate".into());
        }
    }
    Ok(())
}

#[test]
fn a_warrant_is_refused_at_the_boundary_once_the_environment_closes() -> TestResult {
    let mut broker = Broker::new();
    let first = broker.register(env()?)?;
    let warrant = broker.authorize(key(&first.get().to_string())?)?;
    broker.close(env()?)?;
    assert!(matches!(
        broker.revalidate(&warrant),
        Err(BrokerError::Closed { .. })
    ));
    Ok(())
}

#[test]
fn a_closed_environment_mints_nothing() -> TestResult {
    let mut broker = Broker::new();
    let first = broker.register(env()?)?;
    let live = key(&first.get().to_string())?;
    broker.close(env()?)?;

    assert!(!broker.is_open(env()?));
    assert!(matches!(
        broker.authorize(live),
        Err(BrokerError::Closed { .. })
    ));
    assert!(matches!(
        broker.replace(env()?),
        Err(BrokerError::Closed { .. })
    ));
    Ok(())
}

#[test]
fn closing_twice_is_allowed() -> TestResult {
    let mut broker = Broker::new();
    broker.register(env()?)?;
    broker.close(env()?)?;
    broker.close(env()?)?;
    Ok(())
}

#[test]
fn closing_an_unknown_environment_is_refused() -> TestResult {
    let mut broker = Broker::new();
    assert!(matches!(
        broker.close(env()?),
        Err(BrokerError::UnknownEnvironment { .. })
    ));
    Ok(())
}

#[test]
fn an_exhausted_generation_space_is_named_rather_than_wrapped() -> TestResult {
    let id = env()?;
    let last = EnvironmentEpoch::new(NonZeroU64::MAX);
    let mut broker = Broker {
        environments: HashMap::from([(
            id,
            Environment {
                epoch: last,
                open: true,
            },
        )]),
    };
    assert!(matches!(
        broker.replace(id),
        Err(BrokerError::Exhausted { .. })
    ));
    assert_eq!(broker.epoch(id), Some(last));
    Ok(())
}

#[test]
fn one_broker_does_not_authorize_another_environments_key() -> TestResult {
    let mut broker = Broker::new();
    broker.register(env()?)?;
    let other = key_on(OTHER_ENV, "1")?;
    assert!(matches!(
        broker.authorize(other),
        Err(BrokerError::UnknownEnvironment { .. })
    ));
    Ok(())
}

/// The synchronous shape of [`prepare_dispatch`], for a test with no
/// executor of its own.
///
/// One definition rather than a `block_on` at each call site: these tests
/// are about what the order records and refuses, and the await is not part
/// of either question.
fn prepare(
    broker: &Broker,
    journal: &mut MemoryJournal,
    tail: JournalPosition,
    key: EffectKey,
) -> Result<Prepared, DispatchError> {
    lgwks_std::task::block_on(prepare_dispatch(broker, journal, tail, key))
}

#[test]
fn prepare_dispatch_records_the_attempt_it_authorized() -> TestResult {
    let AdmittedEnvironment {
        broker,
        generation,
        key,
        mut journal,
    } = admitted_environment()?;

    let tail = journal.tail();
    let (authority, ack) = prepare(&broker, &mut journal, tail, key)?.into_parts();
    assert_eq!(authority.epoch(), generation);
    assert_eq!(ack.position(), journal.tail());
    assert_eq!(journal.committed().len(), 2);
    Ok(())
}

#[test]
fn a_superseded_key_never_reaches_the_journal() -> TestResult {
    let AdmittedEnvironment {
        mut broker,
        key: stale,
        mut journal,
        ..
    } = admitted_environment()?;
    let before = journal.tail();

    broker.replace(env()?)?;

    let refused = prepare(&broker, &mut journal, before, stale);
    assert!(
        matches!(
            refused,
            Err(DispatchError::Broker(BrokerError::Superseded { .. }))
        ),
        "a fenced command must be refused before it can be recorded"
    );
    assert_eq!(journal.tail(), before);
    assert_eq!(journal.committed().len(), 1);
    Ok(())
}

#[test]
fn a_second_preparation_of_one_attempt_is_refused_by_the_ladder() -> TestResult {
    let AdmittedEnvironment {
        broker,
        key,
        mut journal,
        ..
    } = admitted_environment()?;

    let tail = journal.tail();
    let first_prepare = prepare(&broker, &mut journal, tail, key)?;
    let spent = first_prepare.into_parts();

    let tail = journal.tail();
    let refused = prepare(&broker, &mut journal, tail, key);
    assert!(
        matches!(
            refused,
            Err(DispatchError::Journal(JournalError::OutOfOrder { .. }))
        ),
        "the environment still authorizes, and the journal is what refuses the resend"
    );
    assert_eq!(spent.1.position(), journal.tail());
    assert_eq!(journal.committed().len(), 2);
    Ok(())
}

#[test]
fn an_unadmitted_attempt_is_refused_even_with_authority() -> TestResult {
    let mut broker = Broker::new();
    let first = broker.register(env()?)?;
    let key = key(&first.get().to_string())?;
    let mut journal = MemoryJournal::new();

    // The environment would authorize this attempt: the generation is
    // current. The order still refuses it, because nothing was admitted.
    assert!(broker.authorize(key).is_ok());
    let tail = journal.tail();
    let refused = prepare(&broker, &mut journal, tail, key);
    assert!(matches!(
        refused,
        Err(DispatchError::Journal(JournalError::OutOfOrder { .. }))
    ));
    assert!(journal.committed().is_empty());
    Ok(())
}

#[test]
fn a_stale_controller_cannot_append_after_recovery() -> TestResult {
    let AdmittedEnvironment {
        broker,
        key,
        mut journal,
        ..
    } = admitted_environment()?;

    // A second controller reads the tail before the first appends.
    let stale_tail = journal.tail();
    let tail = journal.tail();
    prepare(&broker, &mut journal, tail, key)?;

    let late = journal.compare_and_append(
        stale_tail,
        &EffectEvent::OutcomeObserved {
            key,
            evidence: EffectEvidence::Applied,
        },
    );
    assert!(
        matches!(late, Err(JournalError::TailMismatch { .. })),
        "the tail check is what stops the second controller writing over the first"
    );
    Ok(())
}
