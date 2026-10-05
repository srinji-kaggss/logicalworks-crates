//! The effect key the example harnesses append, one per step.
//!
//! Included by path (`#[path = "support/effect_key.rs"] mod effect_key;`), so it is
//! a module of each example rather than a target of its own, for the reason
//! `measure.rs` is.

use lgwks_bot::effect::{
    ActionDigest, ActionId, AttemptId, EffectKey, EnvironmentEpoch, EnvironmentId, FlowRevision,
    RunId,
};

/// The effect key every journal append uses, one per measurement step.
///
/// Distinct per step so each is its own append rather than a repeated one the
/// journal would answer as already committed, which would measure the ladder check
/// instead of the flush.
pub(crate) fn key(index: u32) -> Result<EffectKey, Box<dyn std::error::Error>> {
    // Every id here is one-based: `AttemptId` and `EnvironmentEpoch` are non-zero
    // counters by construction, and an attempt of zero is not an attempt.
    let at = u64::from(index).saturating_add(1);
    let run = RunId::from_hex(&format!("{at:032x}"))?;
    let action = ActionId::from_hex(&format!("{:032x}", at.saturating_add(1)))?;
    let attempt = AttemptId::from_decimal(&at.to_string())?;
    let flow = FlowRevision::from_tagged("blake3_256", &format!("{at:064x}"))?;
    let digest =
        ActionDigest::from_tagged("blake3_256", &format!("{:064x}", at.saturating_add(2)))?;
    let environment = EnvironmentId::from_hex(&format!("{:032x}", at.saturating_add(3)))?;
    let epoch = EnvironmentEpoch::from_decimal(&at.to_string())?;
    Ok(EffectKey::new(
        run,
        action,
        attempt,
        flow,
        digest,
        environment,
        epoch,
    ))
}
