//! `chat` owns the chat/messaging domain. Requires `bot.net`.
//!
//! Chat is an Observe source (incoming messages) and a Query surface (read
//! history). It is NOT a separate verb; it is where triggers come from.

use crate::cap::{Auth, Cap};
use crate::error::{BotError, DispatchCertainty};
use crate::verb;

/// An incoming chat message.
///
/// `#[non_exhaustive]`: a chat provider's payload grows (threads, edits,
/// attachments), and a consumer that destructured this literally would break on
/// each addition. Read the fields; build one through the domain that produced it.
#[derive(PartialEq, Debug, Clone)]
#[non_exhaustive]
pub struct ChatMessage {
    /// The channel or conversation the message arrived in.
    pub channel: String,
    /// The sender's identifier.
    pub sender: String,
    /// The message text.
    pub text: String,
    /// Timestamp or message identifier.
    pub ts: String,
}

impl ChatMessage {
    /// Whether the message text contains a substring.
    #[must_use]
    pub fn contains(&self, pattern: &str) -> bool {
        self.text.contains(pattern)
    }
}

/// Observe a Slack channel for incoming messages.
pub struct SlackChannel {
    /// The channel observed; echoed in the "binding required" diagnostic so an
    /// unbound domain says which channel it was asked for.
    channel: String,
    /// Forced to `[bot.net]` by the constructor: a Slack read dials out, and
    /// there is no constructor that omits the capability.
    caps: Vec<Cap>,
}

impl SlackChannel {
    /// Create a Slack channel observer.
    pub fn new(channel: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            caps: vec![Cap::net()],
        }
    }
}

impl verb::Observe for SlackChannel {
    type Output = ChatMessage;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<ChatMessage, BotError> {
        call.0.check(self.required_caps())?;
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            certainty: DispatchCertainty::Refused,
            cause: format!("polling {:?} — binding required", self.channel),
        })
    }

    fn domain_id(&self) -> &str {
        "chat::slack_message"
    }
}

impl verb::Query for SlackChannel {
    type Input = ();
    type Output = Vec<ChatMessage>;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn query(&self, call: (Auth, &())) -> Result<Vec<ChatMessage>, BotError> {
        call.0.check(self.required_caps())?;
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            certainty: DispatchCertainty::Refused,
            cause: format!("querying {:?} — binding required", self.channel),
        })
    }

    fn domain_id(&self) -> &str {
        "chat::slack_message"
    }
}

/// Convenience: create a Slack channel observer.
pub fn slack_message(channel: impl Into<String>) -> SlackChannel {
    SlackChannel::new(channel)
}

/// Observe an HTTP webhook for incoming messages.
pub struct HttpWebhook {
    /// The webhook path observed; echoed in the "binding required" diagnostic.
    path: String,
    /// Forced to `[bot.net]` by the constructor: receiving from a webhook
    /// still requires the network capability, so there is no ungated path.
    caps: Vec<Cap>,
}

impl HttpWebhook {
    /// Create an HTTP webhook observer.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            caps: vec![Cap::net()],
        }
    }
}

impl verb::Observe for HttpWebhook {
    type Output = ChatMessage;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<ChatMessage, BotError> {
        call.0.check(self.required_caps())?;
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            certainty: DispatchCertainty::Refused,
            cause: format!("polling {:?} — binding required", self.path),
        })
    }

    fn domain_id(&self) -> &str {
        "chat::http_webhook"
    }
}

/// Convenience: create an HTTP webhook observer.
pub fn http_webhook(path: impl Into<String>) -> HttpWebhook {
    HttpWebhook::new(path)
}
