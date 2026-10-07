//! One effect per key: POST through a retrying proxy without sending twice.
//!
//! A proxy that duplicates and replays requests turns one POST into many, and
//! a non-idempotent effect sent twice is sent twice: two charges, two merges,
//! two launches. This module is the discipline Temporal and Restate apply to
//! the same hazard — at-least-once delivery with an idempotency key the
//! receiver deduplicates on — expressed as one
//! [`Execute`] adapter.
//!
//! [`IdempotentPost`](crate::idempotent::IdempotentPost) POSTs with the caller-generated `Idempotency-Key` the
//! receiver dedups on ([`lgwks_std::http::Options::idempotency_key`]). Every
//! retry of one operation reuses the key and the payload, so a replayed
//! request is the same request and the upstream counts exactly one effect. A
//! retry with a different payload under the same key is receiver-defined and
//! is not made safe here; the adapter sends what it was handed, byte for
//! byte, on every attempt.
//!
//! The failure vocabulary is the credential row's, reused rather than
//! redeclared: an upstream 401/403/404 is
//! [`BotError::CredentialRejected`]
//! carrying its [`NeedSet`] repair (never a generic
//! failure, never a silent retry — retrying the same credential gives the
//! same refusal), and any other failure is a [`BotError::DomainError`] that
//! says what the exchange established. A transport failure or a 5xx is
//! `NotDelivered` — a retry **is** a retry here, because the key makes it one.
//! That is the whole safety argument, and it is why the key is required
//! rather than optional.
//!
//! # Bounds
//!
//! The body the adapter will send is capped at [`MAX_POST_BYTES`](crate::idempotent::MAX_POST_BYTES), the preview
//! it keeps at [`BODY_PREVIEW`](crate::domain::net::BODY_PREVIEW) characters, and the exchange at
//! [`POST_TIMEOUT_SECS`](crate::idempotent::POST_TIMEOUT_SECS) seconds — the same preview ceiling the
//! [`domain::net`] probe reads under, referenced rather
//! than restated so the two cannot drift.

use std::time::Duration;

use lgwks_std::http::{self, BodyPolicy, Options};

use crate::cap::{Auth, Cap, upstream_credential_rejection};
use crate::domain::net::BODY_PREVIEW;
use crate::error::{BotError, DispatchCertainty};
use crate::verb;

/// Bytes of request body the adapter will send.
///
/// A bound on what one effect may carry, stated where the body is built
/// rather than after: a caller that needs a larger effect declares a larger
/// operation, and the adapter never truncates a body it then claims to have
/// sent.
pub const MAX_POST_BYTES: usize = 64 * 1024;

/// Seconds one exchange may take, deadline and per-phase timeout alike.
///
/// A redirect chain would otherwise give each hop's every phase its own
/// budget; the deadline caps the exchange as a whole.
pub const POST_TIMEOUT_SECS: u64 = 10;

/// One non-idempotent effect, keyed so a retrying proxy delivers it once.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PostInput {
    /// Where the effect is POSTed.
    url: String,
    /// The idempotency key every attempt of this operation carries.
    ///
    /// Caller-generated (`crate::id::Uuid` under `lgwks_std/random`, or any
    /// unique-per-operation string): the client never invents the key behind
    /// the caller's back, because two operations sharing a key are one
    /// operation to the receiver.
    key: String,
    /// The body's content type.
    content_type: String,
    /// The semantic payload: identical on every attempt of one operation.
    body: Vec<u8>,
}

impl PostInput {
    /// Name the target, the operation's key, and the payload every attempt
    /// sends byte for byte.
    ///
    /// # Errors
    ///
    /// [`BotError::DomainError`] with [`DispatchCertainty::Refused`] when the
    /// key is empty — an unkeyed POST through a retrying proxy is the defect
    /// this adapter exists to prevent, and it is refused at construction
    /// rather than sent — or when the body exceeds [`MAX_POST_BYTES`].
    pub fn new(
        url: impl Into<String>,
        key: impl Into<String>,
        content_type: impl Into<String>,
        body: Vec<u8>,
    ) -> Result<Self, BotError> {
        let key = key.into();
        if key.is_empty() {
            return Err(BotError::DomainError {
                domain: "idempotent::post".to_owned(),
                certainty: DispatchCertainty::Refused,
                cause: "an idempotent POST requires a non-empty idempotency key".to_owned(),
            });
        }
        if body.len() > MAX_POST_BYTES {
            return Err(BotError::DomainError {
                domain: "idempotent::post".to_owned(),
                certainty: DispatchCertainty::Refused,
                cause: format!(
                    "body is {} bytes past the {MAX_POST_BYTES}-byte cap",
                    body.len().saturating_sub(MAX_POST_BYTES)
                ),
            });
        }
        Ok(Self {
            url: url.into(),
            key,
            content_type: content_type.into(),
            body,
        })
    }

    /// Where the effect is POSTed.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The idempotency key every attempt carries.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The body's content type.
    #[must_use]
    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    /// The payload every attempt sends.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

/// What the upstream answered: its status and a bounded preview of its body.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PostOutput {
    /// The upstream's status code.
    status: u16,
    /// Up to [`BODY_PREVIEW`](crate::domain::net::BODY_PREVIEW) characters of the response body.
    preview: String,
}

impl PostOutput {
    /// The upstream's status code.
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// Up to [`BODY_PREVIEW`] characters of the response body.
    #[must_use]
    pub fn preview(&self) -> &str {
        &self.preview
    }
}

/// POST one keyed effect: the [`Execute`](crate::verb::Execute) adapter a
/// retrying proxy stands in front of.
///
/// The adapter the proxy row (issue #278, row 6) drives: the test's proxy
/// duplicates and replays every request, the upstream deduplicates on the
/// key, and the upstream's effect count is exactly one.
#[derive(Debug)]
pub struct IdempotentPost {
    /// Forced to `[bot.net]`: a POST dials out, and no constructor omits the
    /// capability.
    caps: Vec<Cap>,
}

impl IdempotentPost {
    /// Build the adapter. There is nothing to configure: the key rides in
    /// the input, the bounds are the module's.
    #[must_use]
    pub fn new() -> Self {
        Self {
            caps: vec![Cap::net()],
        }
    }
}

impl Default for IdempotentPost {
    /// The same adapter [`IdempotentPost::new`](crate::idempotent::IdempotentPost::new) builds: there is nothing to
    /// configure, so the default is not a second choice of settings.
    fn default() -> Self {
        Self::new()
    }
}

impl verb::Execute for IdempotentPost {
    type Input = PostInput;
    type Output = PostOutput;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    fn effect_lifetime(&self) -> verb::EffectLifetime {
        verb::EffectLifetime::External
    }

    async fn execute_action(&self, call: (Auth, &Self::Input)) -> Result<Self::Output, BotError> {
        call.0.check(self.required_caps())?;
        let input = call.1;
        let domain = self.domain_id().to_owned();
        let options = Options::default()
            .timeout(Duration::from_secs(POST_TIMEOUT_SECS))
            .deadline(Duration::from_secs(POST_TIMEOUT_SECS))
            .max_body_bytes(BODY_PREVIEW.saturating_mul(4))
            .body_policy(BodyPolicy::Preview)
            .idempotency_key(input.key());
        // The bounded form, as in `domain::net`: a burst of concurrent
        // effects waits for, or is refused by, the shared blocking pool
        // rather than starting one OS thread per effect. A refusal sent
        // nothing, so it is `Refused`.
        let url = input.url().to_owned();
        let content_type = input.content_type().to_owned();
        let body = input.body().to_vec();
        let request =
            lgwks_std::task::try_spawn_blocking(move || http::post_with(&url, &content_type, &body, &options))
                .map_err(|error| {
                    lgwks_std::trace::debug!(%error, "execute_action: the blocking pool refused the request");
                    BotError::DomainError {
                        domain: domain.clone(),
                        certainty: DispatchCertainty::Refused,
                        cause: format!("no thread was available for the request: {error}"),
                    }
                })?;
        match request.await {
            Ok(response) => {
                // The credential row's mapping, reused on this path: a
                // receiver that refused the credential calls for re-granting,
                // carried as a `NeedSet` repair — never a generic failure and
                // never a silent retry.
                if crate::cap::is_credential_status(response.status) {
                    return Err(upstream_credential_rejection(
                        &domain,
                        response.status,
                        self.required_caps(),
                    ));
                }
                Ok(PostOutput {
                    status: response.status,
                    preview: response
                        .text_lossy()
                        .chars()
                        .take(BODY_PREVIEW)
                        .collect::<String>(),
                })
            }
            Err(http::Error::InvalidUrl) => Err(BotError::DomainError {
                domain,
                certainty: DispatchCertainty::Refused,
                cause: "invalid effect URL (absolute http(s) URI required)".into(),
            }),
            Err(error) => Err(BotError::DomainError {
                domain,
                certainty: DispatchCertainty::NotDelivered,
                cause: format!("the effect did not reach its upstream: {error}"),
            }),
        }
    }

    fn domain_id(&self) -> &str {
        "idempotent::post"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The crate's test idiom: `Result` and `?`, real values in every assert.
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn an_empty_key_is_refused_before_anything_is_sent() -> TestResult {
        let Err(BotError::DomainError { cause, .. }) =
            PostInput::new("http://127.0.0.1:9/", "", "application/json", vec![1])
        else {
            return Err("an unkeyed POST must be refused at construction".into());
        };
        assert!(
            cause.contains("idempotency key"),
            "the refusal names the missing key, got {cause:?}"
        );
        Ok(())
    }

    #[test]
    fn every_attempt_of_one_operation_reuses_key_and_payload() -> TestResult {
        let first = PostInput::new(
            "http://127.0.0.1:9/effect",
            "op-9f2c",
            "application/json",
            br#"{"charge":1}"#.to_vec(),
        )?;
        let second = PostInput::new(
            "http://127.0.0.1:9/effect",
            "op-9f2c",
            "application/json",
            br#"{"charge":1}"#.to_vec(),
        )?;
        assert_eq!(first.key(), second.key(), "retries reuse the key");
        assert_eq!(
            first.body(),
            second.body(),
            "retries resend the payload byte for byte"
        );
        Ok(())
    }
}
