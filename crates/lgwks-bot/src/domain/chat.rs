//! `chat` owns the chat/messaging domain. Requires `bot.net`.
//!
//! Chat is an Observe source (incoming messages), an Execute surface (send
//! replies), and a Query surface (read history). It is NOT a separate verb —
//! it is where triggers come from.

use crate::cap::Cap;
use crate::error::BotError;
use crate::verb;

/// An incoming chat message.
#[derive(Debug, Clone)]
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
    pub fn contains(&self, pattern: &str) -> bool {
        self.text.contains(pattern)
    }
}

/// Observe a Slack channel for incoming messages.
pub struct SlackChannel {
    channel: String,
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

    fn poll(&self) -> Result<ChatMessage, BotError> {
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            cause: format!("polling {} — binding required", self.channel),
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

    fn query(&self, _: &()) -> Result<Vec<ChatMessage>, BotError> {
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            cause: format!("querying {} — binding required", self.channel),
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
    path: String,
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

    fn poll(&self) -> Result<ChatMessage, BotError> {
        Err(BotError::DomainError {
            domain: self.domain_id().into(),
            cause: format!("polling {} — binding required", self.path),
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
