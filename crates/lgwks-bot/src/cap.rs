//! `cap` owns the capability token for bot domains and enforces
//! INV-BOT-CAP-DOTTED: capabilities are dotted string names drawn from the
//! bot vocabulary, consistent with the global IR's `Capability` model.

use lgwks_std::json::{Deserialize, Serialize};
use std::fmt;

/// A capability permission required by a bot domain.
///
/// Dotted string name — `bot.net`, `bot.fs`, `bot.sys`, `bot.notify`. Compared
/// by name equality. The gate checks `required ⊆ granted` before a bot builds.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
pub struct Cap(String);

impl Cap {
    /// Network access — HTTP, WebSocket, API calls.
    pub const NET: &str = "bot.net";
    /// Filesystem access — read, write, watch paths.
    pub const FS: &str = "bot.fs";
    /// System access — process control, environment.
    pub const SYS: &str = "bot.sys";
    /// Notification delivery — Slack, email, webhook push.
    pub const NOTIFY: &str = "bot.notify";

    /// Construct a capability from its dotted name.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The dotted name — the stable identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Shorthand for `Cap::new(Cap::NET)`.
    pub fn net() -> Self {
        Self::new(Self::NET)
    }

    /// Shorthand for `Cap::new(Cap::FS)`.
    pub fn fs() -> Self {
        Self::new(Self::FS)
    }

    /// Shorthand for `Cap::new(Cap::SYS)`.
    pub fn sys() -> Self {
        Self::new(Self::SYS)
    }

    /// Shorthand for `Cap::new(Cap::NOTIFY)`.
    pub fn notify() -> Self {
        Self::new(Self::NOTIFY)
    }
}

impl fmt::Display for Cap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
