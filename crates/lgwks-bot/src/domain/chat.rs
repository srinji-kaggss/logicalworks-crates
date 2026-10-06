//! `chat` owns the chat/messaging domain. Requires `bot.net`.
//!
//! Chat is an Observe source (incoming messages) and a Query surface (read
//! history). It is NOT a separate verb; it is where triggers come from.

use crate::cap::{Auth, Cap};
use crate::error::{BotError, unbound_domain};
// `Observe` is in scope for the two inherent `unbound` helpers, which read the
// same two trait methods the verb bodies do; naming the trait is what lets each
// helper be one line instead of a second copy of the refusal.
use crate::verb::{self, Observe};

/// An incoming chat message.
///
/// `#[non_exhaustive]`: a chat provider's payload grows (threads, edits,
/// attachments), and a consumer that destructured this literally would break on
/// each addition. Read the fields through their accessors; build one through the
/// domain that produced it.
///
/// Every field is private behind an accessor because the four of them are one
/// observed message: a caller that could edit `text` while leaving `ts` alone
/// would hold a message no provider ever sent, and the condition that reads it
/// (`eval::contains`) would answer on a body nobody received.
///
/// ```
/// use lgwks_bot::domain::chat::ChatMessage;
///
/// let observed = ChatMessage::new("#deploys", "ci-bot", "rollout finished", "42");
/// assert!(observed.contains("finished"));
/// assert_eq!(observed.channel(), "#deploys");
/// assert_eq!(observed.sender(), "ci-bot");
/// assert_eq!(observed.text(), "rollout finished");
/// assert_eq!(observed.ts(), "42");
/// ```
#[derive(PartialEq, Debug, Clone)]
#[non_exhaustive]
pub struct ChatMessage {
    /// The channel or conversation the message arrived in.
    channel: String,
    /// The sender's identifier.
    sender: String,
    /// The message text.
    text: String,
    /// Timestamp or message identifier.
    ts: String,
}

impl ChatMessage {
    /// Build the message a provider's payload decodes to.
    ///
    /// The one constructor, because the fields are private: a `ChatMessage` is
    /// assembled whole or not at all, so a caller cannot hold half an observed
    /// message. A provider that grows a field adds it here, not as a public
    /// slot every consumer can leave unset.
    #[must_use]
    pub fn new(
        channel: impl Into<String>,
        sender: impl Into<String>,
        text: impl Into<String>,
        ts: impl Into<String>,
    ) -> Self {
        Self {
            channel: channel.into(),
            sender: sender.into(),
            text: text.into(),
            ts: ts.into(),
        }
    }

    /// The channel or conversation the message arrived in.
    #[must_use]
    pub fn channel(&self) -> &str {
        &self.channel
    }

    /// The address the message was sent from, exactly as the provider spelled it:
    /// no normalisation and no prefix stripping, so a condition can match on the
    /// provider's own spelling rather than on this crate's idea of it.
    #[must_use]
    pub fn sender(&self) -> &str {
        &self.sender
    }

    /// The message body, exactly as the provider delivered it — not trimmed, not
    /// normalised, and not decoded from any markup the transport carried.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Timestamp or message identifier.
    #[must_use]
    pub fn ts(&self) -> &str {
        &self.ts
    }

    /// Whether the message text contains a substring.
    #[must_use]
    pub fn contains(&self, pattern: &str) -> bool {
        self.text.contains(pattern)
    }
}

/// Observe a Slack channel for incoming messages.
#[derive(Debug)]
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

    /// The refusal an unbound read gives, naming the channel it was asked for.
    fn unbound<T>(&self, auth: &Auth, verb: &str) -> Result<T, BotError> {
        unbound_domain(
            auth,
            self.required_caps(),
            self.domain_id(),
            verb,
            &self.channel,
        )
    }
}

impl verb::Observe for SlackChannel {
    type Output = ChatMessage;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<ChatMessage, BotError> {
        self.unbound(&call.0, "polling")
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
        self.unbound(&call.0, "querying")
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
#[derive(Debug)]
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

    /// The refusal an unbound read gives, naming the webhook path it was asked
    /// for.
    fn unbound<T>(&self, auth: &Auth, verb: &str) -> Result<T, BotError> {
        unbound_domain(
            auth,
            self.required_caps(),
            self.domain_id(),
            verb,
            &self.path,
        )
    }
}

impl verb::Observe for HttpWebhook {
    type Output = ChatMessage;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<ChatMessage, BotError> {
        self.unbound(&call.0, "polling")
    }

    fn domain_id(&self) -> &str {
        "chat::http_webhook"
    }
}

/// Convenience: create an HTTP webhook observer.
pub fn http_webhook(path: impl Into<String>) -> HttpWebhook {
    HttpWebhook::new(path)
}
