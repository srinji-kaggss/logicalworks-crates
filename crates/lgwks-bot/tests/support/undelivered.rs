//! A transient domain refusal, shared by the tick and ledger tests.

use lgwks_bot::{BotError, DispatchCertainty};

/// A domain failure that provably delivered nothing, so a chain may retry it.
///
/// `NotDelivered` and not `Refused`: these fixtures are *transient* refusals a
/// chain is expected to retry while it has budget, and `Refused` classifies as
/// `RetryClass::Never`, which abandons the entry on the first failure.
pub(crate) fn undelivered(domain: &str, cause: &str) -> BotError {
    BotError::DomainError {
        domain: domain.to_owned(),
        certainty: DispatchCertainty::NotDelivered,
        cause: cause.to_owned(),
    }
}
