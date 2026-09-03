//! `notify` owns the notification delivery domain. Requires `bot.notify`.

use crate::cap::Cap;
use crate::error::BotError;
use crate::verb;

/// A message payload for notification delivery.
#[derive(Debug, Clone)]
pub struct Message {
    /// The message text.
    pub text: String,
}

impl Message {
    /// Create a notification message.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// Send a notification to a Slack channel. Execute only.
pub struct Slack {
    channel: String,
    caps: Vec<Cap>,
}

impl Slack {
    /// Create a Slack notification executor.
    pub fn new(channel: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            caps: vec![Cap::notify(), Cap::net()],
        }
    }
}

impl verb::Execute for Slack {
    type Input = Message;
    type Output = ();

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    fn run(&self, _input: &Message) -> Result<(), BotError> {
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            cause: format!("notifying {} — binding required", self.channel),
        })
    }

    fn domain_id(&self) -> &str {
        "notify::slack"
    }
}

/// Convenience: create a Slack notification executor.
pub fn slack(channel: impl Into<String>) -> Slack {
    Slack::new(channel)
}
