//! `gate` owns capability admission and enforces INV-BOT-SAME-GATE: shipped
//! and custom domains pass the identical capability check at `Bot::build()`.

use std::collections::HashSet;

use super::cap::{Auth, Cap, Deficit, Demand, Shortage, uncovered};
use super::error::BotError;

/// A set of granted capabilities. The bot builder checks every domain's
/// required capabilities against this set.
#[derive(Debug, Clone)]
pub struct GrantSet {
    /// The granted names. A `HashSet` because membership is the only question
    /// asked of it (admission and proof minting are both `contains`), and a
    /// duplicate `grant` for the same capability must be idempotent rather than
    /// accumulate.
    granted: HashSet<Cap>,
}

impl GrantSet {
    /// An empty grant: nothing is permitted.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            granted: HashSet::new(),
        }
    }

    /// Grant all shipped capabilities (`bot.net`, `bot.fs`, `bot.sys`, `bot.notify`).
    #[must_use]
    pub fn all_shipped() -> Self {
        let mut granted = HashSet::new();
        granted.insert(Cap::net());
        granted.insert(Cap::fs());
        granted.insert(Cap::sys());
        granted.insert(Cap::notify());
        Self { granted }
    }

    /// Grant a single capability. Idempotent: granting a capability already
    /// present leaves the set unchanged, so a caller need not track what a
    /// prior grant added.
    #[must_use]
    pub fn grant(mut self, cap: Cap) -> Self {
        self.granted.insert(cap);
        self
    }

    /// Whether this set grants `cap`.
    #[must_use]
    pub fn grants(&self, cap: &Cap) -> bool {
        self.granted.contains(cap)
    }

    /// The requirements in `required` that this set does not grant, in
    /// declaration order, each named once, attributed to `demand`.
    ///
    /// The total form of [`admit`](GrantSet::admit), and the primitive bot
    /// admission is built from: an admission that walks every chain and collects
    /// from each can name every unmet requirement in the bot from one pass,
    /// rather than stopping at the first one it meets.
    #[must_use]
    pub fn uncovered(&self, required: &[Cap], demand: &Demand) -> Vec<Shortage> {
        uncovered(required, |cap| self.grants(cap), Some(demand))
    }

    /// Every requirement in `required` this set does not grant, as one deficit.
    ///
    /// # Errors
    ///
    /// [`BotError::CapabilityDenied`] naming **every** ungranted requirement,
    /// not the first.
    pub fn admit(&self, required: &[Cap]) -> Result<(), BotError> {
        match Deficit::from_shortages(uncovered(required, |cap| self.grants(cap), None)) {
            Some(deficit) => Err(BotError::CapabilityDenied { deficit }),
            None => Ok(()),
        }
    }

    /// Mint a sealed [`Auth`] proof covering `required`, or deny naming every
    /// missing capability. This is the only constructor path for `Auth`:
    /// presenting the proof is what authorizes a verb call.
    ///
    /// # Errors
    ///
    /// [`BotError::CapabilityDenied`] naming every requirement this set does not
    /// grant.
    pub fn issue(&self, required: &[Cap]) -> Result<Auth, BotError> {
        self.admit(required)?;
        Ok(Auth::new(required.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::GrantSet;
    use crate::cap::{Cap, Demand};
    use crate::error::{BotError, DispatchCertainty};

    /// A typed test failure; this workspace forbids `panic!`.
    fn failed(cause: impl Into<String>) -> BotError {
        BotError::DomainError {
            domain: "gate::tests".into(),
            certainty: DispatchCertainty::NotDelivered,
            cause: cause.into(),
        }
    }

    /// The shipped four, which is the requirement list a real bot is most
    /// likely to be short of at once.
    fn all_four() -> [Cap; 4] {
        [Cap::net(), Cap::fs(), Cap::sys(), Cap::notify()]
    }

    /// Admission as a loop versus admission as an answer.
    ///
    /// Three of four capabilities ungranted must produce three shortages from
    /// one refusal. The defect this closes is not that any individual refusal
    /// was wrong — each named a true fact — but that the repair was serialised
    /// against the check.
    #[test]
    fn admission_names_every_missing_capability_in_one_pass() -> Result<(), BotError> {
        let grants = GrantSet::empty().grant(Cap::net());

        match grants.admit(&all_four()) {
            Err(BotError::CapabilityDenied { deficit }) => {
                let named: Vec<&str> = deficit
                    .shortages()
                    .map(|shortage| shortage.required().as_str())
                    .collect();
                assert_eq!(
                    named,
                    vec![Cap::FS, Cap::SYS, Cap::NOTIFY],
                    "one refusal must carry the whole shortfall: a caller granting what they \
                     were told should not have to be refused three times to learn three \
                     words: {deficit}"
                );
                assert_eq!(deficit.len(), 3);
                Ok(())
            }
            other => Err(failed(format!("expected a denial of three: {other:?}"))),
        }
    }

    #[test]
    fn the_repair_a_denial_derives_closes_the_requirement() -> Result<(), BotError> {
        let required = all_four();
        // One of the four is held, so the deficit is a strict subset of the
        // requirement — which is what makes "the shortfall, not the
        // requirement" a claim this test can actually falsify.
        let held = GrantSet::empty().grant(Cap::net());

        match held.admit(&required) {
            Err(BotError::CapabilityDenied { deficit }) => {
                let missing: Vec<Cap> = deficit
                    .shortages()
                    .map(|shortage| shortage.required().clone())
                    .collect();
                let repair = deficit.to_grant_set();

                // `is_ok` rather than comparing the `Result`: `BotError` is not
                // `PartialEq` and should not be, since two causes being equal
                // is not a fact any caller acts on.
                assert!(
                    repair.admit(&missing).is_ok(),
                    "the derived set must admit what the deficit named: {deficit}"
                );
                assert!(
                    !repair.grants(&Cap::net()),
                    "and only that — `bot.net` was granted, so it is not in the repair: {deficit}"
                );

                let closed = missing
                    .iter()
                    .fold(held.clone(), |set, cap| set.grant(cap.clone()));
                assert!(
                    closed.admit(&required).is_ok(),
                    "the repair folded into the held set must close the requirement"
                );
                assert!(
                    held.admit(&required).is_err(),
                    "control: the held set alone must not, or this proves nothing"
                );
                Ok(())
            }
            other => Err(failed(format!("expected a denial of three: {other:?}"))),
        }
    }

    #[test]
    fn a_granted_requirement_is_admitted_and_reports_nothing_uncovered() -> Result<(), BotError> {
        let grants = GrantSet::all_shipped();
        grants.admit(&all_four())?;
        assert!(
            grants
                .uncovered(&all_four(), &Demand::new("test::domain"))
                .is_empty(),
            "the total form and the Result form must agree on the admitted case"
        );
        assert!(grants.grants(&Cap::sys()));
        assert!(!GrantSet::empty().grants(&Cap::sys()));
        Ok(())
    }

    #[test]
    fn a_capability_outside_the_shipped_four_is_denied_like_any_other() -> Result<(), BotError> {
        // `Cap::new` accepts any dotted name, so nothing is special-cased at
        // check time; `all_shipped` grants four names and this is a fifth.
        let custom = Cap::new("your.domain.cap");
        match GrantSet::all_shipped().admit(std::slice::from_ref(&custom)) {
            Err(BotError::CapabilityDenied { deficit }) => {
                assert_eq!(deficit.first().required(), &custom);
                Ok(())
            }
            other => Err(failed(format!(
                "a capability nothing grants must be denied: {other:?}"
            ))),
        }
    }
}
