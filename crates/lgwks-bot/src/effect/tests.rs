use super::*;

const RUN: &str = "0102030405060708090a0b0c0d0e0f10";
const ACTION: &str = "1112131415161718191a1b1c1d1e1f20";
const ENV: &str = "2122232425262728292a2b2c2d2e2f30";
const FLOW_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const DIGEST_HEX: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeffe0e1e2e3e4e5e6e7e8e9eaebecedeeef";

/// The crate refuses a panicking path anywhere, tests included, so a test
/// that needs a parsed value returns `Result` and propagates. A failure
/// then names the cause instead of a line number in a macro.
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn nonzero(value: u64) -> Result<NonZeroU64, CounterError> {
    NonZeroU64::new(value).ok_or(CounterError::Zero)
}

/// The one `MintError` arm a test can reach, and only from in here: the
/// enum is `#[non_exhaustive]` so a caller outside has to write a wildcard
/// arm, which also means a caller outside cannot construct an arm to check
/// its rendering. `Entropy` needs the operating system to fail, so it is
/// covered by the type it carries rather than by a test.
#[cfg(feature = "ephemeral")]
#[test]
fn the_all_zero_mint_error_names_the_value_it_refused() {
    let rendered = MintError::Zero.to_string();
    assert!(
        rendered.contains("all-zero"),
        "an operator reading this cannot tell what was refused: {rendered}"
    );
    assert!(
        std::error::Error::source(&MintError::Zero).is_none(),
        "a refusal with no cause must not claim one"
    );
}

fn key(attempt: u64, epoch: u64) -> Result<EffectKey, EffectKeyError> {
    let key_run = RunId::from_hex(RUN)?;
    let key_action = ActionId::from_hex(ACTION)?;
    let key_attempt = AttemptId::new(nonzero(attempt)?);
    let key_flow_revision = FlowRevision::from_tagged("blake3_256", FLOW_HEX)?;
    let key_digest = ActionDigest::from_tagged("blake3_256", DIGEST_HEX)?;
    let key_environment = EnvironmentId::from_hex(ENV)?;
    let key_epoch = EnvironmentEpoch::new(nonzero(epoch)?);
    Ok(
        EffectIdentity::new(key_run, key_environment, key_flow_revision).key(
            key_action,
            key_attempt,
            key_digest,
            key_epoch,
        ),
    )
}

#[test]
fn id_round_trips_through_its_hex_form() -> TestResult {
    let id = Id128::from_hex(RUN)?;
    assert_eq!(id.to_hex(), RUN);
    Ok(())
}

#[test]
fn id_round_trips_a_value_with_the_high_bit_set() -> TestResult {
    // Big-endian means the first byte is the most significant; a value
    // whose top bit is set is where a sign-confusion bug would show.
    let text = "ffffffffffffffffffffffffffffffff";
    let id = Id128::from_hex(text)?;
    assert_eq!(id.to_hex(), text);
    assert_eq!(id.get().get(), u128::MAX);
    Ok(())
}

#[test]
fn id_rejects_the_all_zero_value() {
    assert_eq!(
        Id128::from_hex("00000000000000000000000000000000"),
        Err(IdError::Zero)
    );
}

#[test]
fn id_rejects_uppercase_rather_than_folding_it() -> TestResult {
    let upper = RUN.to_uppercase();
    // The offset is derived rather than hardcoded: the leading bytes of
    // this constant are digits, which are valid hex in either case, so the
    // first refusal lands wherever the first letter happens to be.
    match Id128::from_hex(&upper) {
        Err(IdError::NotHex { at }) => {
            assert!(
                upper.as_bytes()[at].is_ascii_uppercase(),
                "offset {at} does not name the uppercase byte"
            );
        }
        other => return Err(format!("expected a NotHex refusal, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn id_reports_the_observed_length_not_a_rebased_offset() {
    assert_eq!(
        Id128::from_hex("abcd"),
        Err(IdError::WrongLength { len: 4 })
    );
}

#[test]
fn id_rejects_a_non_ascii_input_without_panicking() {
    // Exactly 32 bytes of UTF-8, so the length check passes and the byte
    // check is what has to catch it.
    let text = "é".repeat(16);
    assert_eq!(text.len(), 32);
    assert_eq!(Id128::from_hex(&text), Err(IdError::NotHex { at: 0 }));
}

#[test]
fn id_accepts_a_nonzero_value_by_construction() {
    let id = Id128::from_nonzero(NonZeroU128::MIN);
    assert_eq!(id.get().get(), 1);
    assert_eq!(id.to_hex(), "00000000000000000000000000000001");
}

#[test]
fn every_id_role_parses_the_same_wire_form() -> TestResult {
    let run = RunId::from_hex(RUN)?;
    let action = ActionId::from_hex(RUN)?;
    let environment = EnvironmentId::from_hex(RUN)?;
    // The same 32 bytes, three types. That is the point of the newtypes: a
    // call site that swaps them does not compile, which is not something a
    // runtime assertion here could establish.
    assert_eq!(run.id(), action.id());
    assert_eq!(action.id(), environment.id());
    assert_eq!(run.to_string(), RUN);
    Ok(())
}

#[test]
fn attempt_rejects_zero_and_non_decimal() {
    assert_eq!(AttemptId::from_decimal("0"), Err(CounterError::Zero));
    assert_eq!(AttemptId::from_decimal(""), Err(CounterError::NotDecimal));
    assert_eq!(AttemptId::from_decimal("1a"), Err(CounterError::NotDecimal));
    assert_eq!(AttemptId::from_decimal("-1"), Err(CounterError::NotDecimal));
    assert_eq!(
        AttemptId::from_decimal("18446744073709551616"),
        Err(CounterError::Overflow)
    );
    assert_eq!(AttemptId::from_decimal("42").map(AttemptId::get), Ok(42));
}

#[test]
fn attempt_refuses_to_wrap_rather_than_reusing_an_identity() {
    let last = AttemptId::new(NonZeroU64::MAX);
    assert!(last.is_exhausted());
    assert_eq!(last.checked_next(), None);
    assert_eq!(AttemptId::FIRST.get(), 1);
    assert_eq!(AttemptId::FIRST.checked_next().map(AttemptId::get), Some(2));
}

#[test]
fn the_two_counter_roles_keep_their_own_names() {
    assert_eq!(AttemptId::role(), "attempt");
    assert_eq!(EnvironmentEpoch::role(), "environment epoch");
    assert_eq!(EnvironmentEpoch::FIRST.to_string(), "1");
}

#[test]
fn a_sha256_tag_is_unsupported_rather_than_malformed() {
    assert!(matches!(
        ActionDigest::from_tagged("sha256", DIGEST_HEX),
        Err(DigestError::UnsupportedAlgorithm {
            algorithm: DigestAlgorithm::Sha256
        })
    ));
    assert!(matches!(
        ActionDigest::from_tagged("md5", DIGEST_HEX),
        Err(DigestError::UnknownAlgorithm)
    ));
}

#[test]
fn a_malformed_digest_body_is_distinct_from_an_unsupported_tag() {
    assert!(matches!(
        ActionDigest::from_tagged("blake3_256", "nothex"),
        Err(DigestError::Malformed(
            lgwks_std::hash::DigestParseError::WrongLength { len: 6 }
        ))
    ));
}

#[test]
fn the_two_digest_roles_carry_the_same_algorithm_tag() {
    assert_eq!(FlowRevision::algorithm(), DigestAlgorithm::Blake3_256);
    assert_eq!(ActionDigest::algorithm().as_str(), "blake3_256");
    assert_eq!(FlowRevision::role(), "flow revision");
    assert_eq!(ActionDigest::role(), "action digest");
}

#[test]
fn a_key_round_trips_every_field() -> TestResult {
    let bound = key(3, 7)?;
    assert_eq!(bound.run().to_string(), RUN);
    assert_eq!(bound.action().to_string(), ACTION);
    assert_eq!(bound.attempt().get(), 3);
    assert_eq!(bound.flow().to_hex(), FLOW_HEX);
    assert_eq!(bound.digest().to_hex(), DIGEST_HEX);
    assert_eq!(bound.environment().to_string(), ENV);
    assert_eq!(bound.epoch().get(), 7);
    Ok(())
}

#[test]
fn with_attempt_moves_only_the_attempt() -> TestResult {
    let base = key(1, 4)?;
    let next = base.with_attempt(AttemptId::new(nonzero(2)?));
    assert_ne!(base, next);
    assert_eq!(base.run(), next.run());
    assert_eq!(base.action(), next.action());
    assert_eq!(base.flow(), next.flow());
    assert_eq!(base.digest(), next.digest());
    assert_eq!(base.environment(), next.environment());
    assert_eq!(base.epoch(), next.epoch());
    assert_eq!(next.attempt().get(), 2);
    Ok(())
}

#[test]
fn an_epoch_bump_under_one_command_is_recognised() -> TestResult {
    let prepared = key(1, 4)?;
    let after_restart = key(1, 5)?;
    assert!(prepared.same_attempt_earlier_epoch(after_restart));
    assert!(!after_restart.same_attempt_earlier_epoch(prepared));
    Ok(())
}

#[test]
fn a_different_input_digest_is_a_different_key_and_not_an_epoch_move() -> TestResult {
    let base = key(1, 4)?;
    let moved = EffectIdentity::new(base.run(), base.environment(), base.flow()).key(
        base.action(),
        base.attempt(),
        ActionDigest::from_tagged(
            "blake3_256",
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e2f",
        )?,
        EnvironmentEpoch::new(nonzero(5)?),
    );
    assert_ne!(base.digest(), moved.digest());
    assert!(!base.same_attempt_earlier_epoch(moved));
    Ok(())
}

#[test]
fn a_key_renders_every_field_for_a_refusal_message() -> TestResult {
    let rendered = key(2, 9)?.to_string();
    assert!(rendered.contains(RUN), "{rendered}");
    assert!(rendered.contains("attempt 2"), "{rendered}");
    assert!(rendered.contains("epoch 9"), "{rendered}");
    assert!(rendered.contains("env "), "{rendered}");
    Ok(())
}
