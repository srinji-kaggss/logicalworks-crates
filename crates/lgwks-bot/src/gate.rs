//! `gate` owns capability admission and enforces INV-BOT-SAME-GATE: shipped
//! and custom domains pass the identical capability check at `Bot::build()`.

use std::collections::HashSet;

use super::cap::{Auth, Cap};
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

    /// Check that every required capability is granted.
    pub fn admit(&self, required: &[Cap]) -> Result<(), BotError> {
        for cap in required {
            if !self.granted.contains(cap) {
                return Err(BotError::CapabilityDenied {
                    required: cap.clone(),
                });
            }
        }
        Ok(())
    }

    /// Mint a sealed [`Auth`] proof covering `required`, or deny naming the
    /// first missing capability. This is the only constructor path for
    /// `Auth`: presenting the proof is what authorizes a verb call.
    pub fn issue(&self, required: &[Cap]) -> Result<Auth, BotError> {
        self.admit(required)?;
        Ok(Auth::new(required.to_vec()))
    }
}
