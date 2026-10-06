//! `notify` owns the notification delivery domain. Requires `bot.notify` and
//! `bot.net` (delivery dials out).

use crate::cap::{Auth, Cap};
use crate::error::{BotError, unbound_domain};
use crate::verb;

/// A message payload for notification delivery.
///
/// `#[non_exhaustive]`: a transport grows payload fields (thread, blocks,
/// attachments), and a consumer that destructured this literally would break on
/// each addition. Build one with [`Message::new`] and read it with
/// [`Message::text`].
///
/// The text is private because it is the whole of what a delivery is asked to
/// send: a caller that could edit it after handing the message to a transport
/// would be editing the body the transport is about to put on the wire, which is
/// a different act from composing it.
///
/// ```
/// use lgwks_bot::domain::notify::Message;
///
/// let outgoing = Message::new("rollout 42 finished");
/// assert_eq!(outgoing.text(), "rollout 42 finished");
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Message {
    /// The message text.
    text: String,
}

impl Message {
    /// Create a notification message.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    /// The message text this delivery will send.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Send a notification to a Slack channel. Execute only.
#[derive(Debug)]
pub struct Slack {
    /// The destination channel; echoed in the "binding required" diagnostic.
    channel: String,
    /// Always `[bot.notify, bot.net]`, in that order, so a denied send names
    /// `bot.notify` first: delivery is a notification that happens to dial out,
    /// and the caller should see the domain capability before the transport one.
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

    async fn execute_action(&self, call: (Auth, &Message)) -> Result<(), BotError> {
        unbound_domain(
            &call.0,
            self.required_caps(),
            self.domain_id(),
            "notifying",
            &self.channel,
        )
    }

    fn domain_id(&self) -> &str {
        "notify::slack"
    }
}

/// Convenience: create a Slack notification executor.
pub fn slack(channel: impl Into<String>) -> Slack {
    Slack::new(channel)
}
