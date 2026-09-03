//! `gate` owns capability admission and enforces INV-BOT-SAME-GATE: shipped
//! and custom domains pass the identical capability check at `Bot::build()`.

use std::collections::HashSet;

use super::cap::Cap;
use super::error::BotError;

/// A set of granted capabilities. The bot builder checks every domain's
/// required capabilities against this set.
#[derive(Debug, Clone)]
pub struct GrantSet {
    granted: HashSet<Cap>,
}

impl GrantSet {
    /// An empty grant — nothing is permitted.
    pub fn empty() -> Self {
        Self {
            granted: HashSet::new(),
        }
    }

    /// Grant all shipped capabilities (`bot.net`, `bot.fs`, `bot.sys`, `bot.notify`).
    pub fn all_shipped() -> Self {
        let mut granted = HashSet::new();
        granted.insert(Cap::net());
        granted.insert(Cap::fs());
        granted.insert(Cap::sys());
        granted.insert(Cap::notify());
        Self { granted }
    }

    /// Grant a single capability.
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
}
