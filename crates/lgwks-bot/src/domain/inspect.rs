//! `inspect` owns the structural-inspection domain: R8's operation exposed on
//! the existing verbs, so a bot can reach it the way it reaches every other
//! domain.
//!
//! The operation itself lives in [`crate::inspect`](mod@crate::inspect) and never executes the
//! subject. This module is the *wiring*: it turns that operation into
//! something a [`BotSpec`](crate::BotSpec) or a native bot can call through the
//! same registry and admission path every other domain uses, and into a
//! [`Task`](crate::task::Task) a [`Host`](crate::task::Host) can run. Both reach
//! the one operation; neither reimplements it, so a report from one entry point
//! is byte-for-byte the report from another.
//!
//! # Two verbs, one operation
//!
//! - [`Subject`] is an [`Observe`](crate::verb::Observe) source. Its target is
//!   an artifact path; each poll reads the artifact's bytes (an ordinary `bot.fs`
//!   read, bounded before it happens) and inspects them. Register it with
//!   `domains!` under `inspect::subject` and a spec or a native bot can bind it
//!   to a chain. Because it reads a file it requires `bot.fs`, so a spec naming
//!   it without the grant is refused at admission exactly like every other
//!   filesystem domain — the registry buys no reach.
//! - [`Inspector`] is a [`Query`](crate::verb::Query). Its input carries the
//!   subject's bytes directly, so it needs no filesystem capability and reads
//!   nothing the caller did not hand it. It is the in-process library boundary
//!   R8 asks for: an ordinary caller or an agent calls it with an [`Auth`] proof
//!   and reads the typed [`Inspection`].
//!
//! # What the domain does not do
//!
//! [`Subject::poll`] reads the artifact's bytes and hands them to the strict
//! operation. Reading a subject is inspection; it is not compilation, import,
//! build-script evaluation, dependency installation, a shell, or a dynamic
//! library load, and none of those happens here. The read is bounded by
//! `Budgets::max_source_bytes` before it happens, so an artifact past the budget
//! is refused rather than loaded to be refused.
//!
//! [`Subject`]: crate::domain::inspect::Subject
//! [`Subject::poll`]: crate::verb::Observe::poll
//! [`Inspector`]: crate::domain::inspect::Inspector
//! [`Auth`]: crate::cap::Auth
//! [`Inspection`]: crate::inspect::Inspection

use crate::cap::{Auth, Cap};
use crate::effect::InputIdentity;
use crate::error::{BotError, DispatchCertainty};
use crate::inspect::{Budgets, InspectRequest, Inspection, RuleSet, Scope as InspectScope};
use crate::verb;

/// The domain identifier both verbs report.
pub const DOMAIN: &str = "inspect::subject";

/// A structural-inspection job: the typed input of [`Inspector`].
///
/// Owned rather than borrowed, because a [`Query`](crate::verb::Query) input
/// crosses the verb boundary and a borrowed request would tie the caller's
/// lifetime to the call. It is the [`InspectRequest`]'s fields in owned form;
/// [`InspectionJob::inspect`] is the one place they are turned back into a
/// request and run, so the job and the free function cannot drift.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct InspectionJob {
    /// The artifact identity the report carries.
    artifact: String,
    /// The subject's bytes.
    subject: String,
    /// The language, when the caller knows it.
    language: Option<lgwks_ast::Language>,
    /// The declared language version, when the caller knows it.
    language_version: Option<String>,
    /// The rule set the caller expects.
    rules: RuleSet,
    /// The scope the caller asks to inspect.
    scope: InspectScope,
    /// The bounds to apply.
    budgets: Budgets,
}

impl InspectionJob {
    /// A job inspecting `subject` as `artifact` under the default rule set,
    /// scope and budgets.
    #[must_use]
    pub fn new(artifact: impl Into<String>, subject: impl Into<String>) -> Self {
        Self {
            artifact: artifact.into(),
            subject: subject.into(),
            language: None,
            language_version: None,
            rules: RuleSet::STRUCTURAL_V1,
            scope: InspectScope::Structural,
            budgets: Budgets::default_budgets(),
        }
    }

    /// Declare the subject's language, overriding extension detection.
    #[must_use]
    pub fn with_language(mut self, language: lgwks_ast::Language) -> Self {
        self.language = Some(language);
        self
    }

    /// Declare the language version the subject is written against.
    #[must_use]
    pub fn with_language_version(mut self, version: impl Into<String>) -> Self {
        self.language_version = Some(version.into());
        self
    }

    /// Bind the job to a specific rule set.
    #[must_use]
    pub fn with_rules(mut self, rules: RuleSet) -> Self {
        self.rules = rules;
        self
    }

    /// Declare the scope to inspect.
    #[must_use]
    pub fn with_scope(mut self, scope: InspectScope) -> Self {
        self.scope = scope;
        self
    }

    /// Replace the budgets.
    #[must_use]
    pub fn with_budgets(mut self, budgets: Budgets) -> Self {
        self.budgets = budgets;
        self
    }

    /// The artifact identity.
    #[must_use]
    pub fn artifact(&self) -> &str {
        &self.artifact
    }

    /// The subject's bytes.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Run the inspection this job describes.
    ///
    /// The single call into [`crate::inspect::inspect`]; there is no second
    /// scanner, queue or authority bypass here, and the report is the operation's
    /// own.
    #[must_use]
    pub fn inspect(&self) -> Inspection {
        let mut request = InspectRequest::new(&self.artifact, &self.subject)
            .rules(self.rules)
            .scope(self.scope)
            .budgets(self.budgets);
        if let Some(language) = self.language {
            request = request.language(language);
        }
        if let Some(version) = self.language_version.as_deref() {
            request = request.language_version(version);
        }
        crate::inspect::inspect(&request)
    }
}

/// The structural-inspection [`Query`](crate::verb::Query).
///
/// A read with no side effect and no causal chain: the caller supplies the
/// subject's bytes in the input and reads the typed report. It performs no
/// filesystem read, so it requires no capability; the [`Auth`] proof is still
/// presented and checked, so the call site is uniform with every other verb and
/// a future side effect would be gated here.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct Inspector;

impl Inspector {
    /// The one inspector.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl verb::Query for Inspector {
    type Input = InspectionJob;
    type Output = Inspection;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn query(&self, call: (Auth, &InspectionJob)) -> Result<Inspection, BotError> {
        let (auth, job) = call;
        auth.check(verb::Query::required_caps(self))?;
        Ok(job.inspect())
    }

    fn domain_id(&self) -> &str {
        DOMAIN
    }
}

/// The structural-inspection [`Observe`](crate::verb::Observe) source.
///
/// Its target is an artifact path. Each poll reads the artifact's bytes under
/// `bot.fs` and inspects them; the output is the [`Inspection`]. Register it
/// with `domains!` under [`DOMAIN`] so a spec or a native bot can bind it to a
/// chain.
#[derive(Debug, Clone)]
pub struct Subject {
    /// The artifact path read on each poll.
    artifact: String,
    /// Forced to `[bot.fs]` by the constructor: reading the artifact is a
    /// filesystem operation and no constructor omits the capability.
    caps: Vec<Cap>,
    /// The bounds applied to the poll, including the byte read ceiling.
    budgets: Budgets,
}

impl Subject {
    /// Observe the artifact at `path`.
    #[must_use]
    pub fn at(path: impl Into<String>) -> Self {
        Self {
            artifact: path.into(),
            caps: vec![Cap::fs()],
            budgets: Budgets::default_budgets(),
        }
    }

    /// Build one from the `target` a spec names; the target is the artifact
    /// path, which must not be empty.
    ///
    /// # Errors
    ///
    /// [`BotError::IncompleteSpec`] when `target` is empty, so a spec that
    /// named no artifact is an attributed admission need rather than a poll
    /// that reads the working directory's empty name.
    pub fn from_target(target: &str) -> Result<crate::Source, BotError> {
        if target.is_empty() {
            let refusal = Err(BotError::IncompleteSpec { field: "target" });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "from_target: returning an error to the caller");
            return refusal;
        }
        Ok(crate::Source::new(Self::at(target)))
    }

    /// The artifact path this source reads.
    #[must_use]
    pub fn artifact(&self) -> &str {
        &self.artifact
    }

    /// Replace the budgets, including the byte read ceiling.
    #[must_use]
    pub fn with_budgets(mut self, budgets: Budgets) -> Self {
        self.budgets = budgets;
        self
    }
}

impl verb::Observe for Subject {
    type Output = Inspection;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn poll(&self, call: (Auth, ())) -> Result<Inspection, BotError> {
        call.0.check(self.required_caps())?;
        let path = self.artifact.clone();
        let limit = self.budgets.max_source_bytes;
        let subject = lgwks_std::task::spawn_blocking(move || read_subject(&path, limit)).await?;
        Ok(crate::inspect::inspect(
            &InspectRequest::new(&self.artifact, &subject).budgets(self.budgets),
        ))
    }

    fn domain_id(&self) -> &str {
        DOMAIN
    }
}

/// Read `path` as UTF-8, refusing before the read when it is past `limit`.
///
/// The byte budget is a read ceiling as well as an inspection ceiling: refusing
/// an oversize artifact *before* loading it is the difference between a bounded
/// refusal and loading a file only to refuse it. A non-UTF-8 artifact is refused
/// as a typed domain error rather than lossily decoded, because the operation
/// inspects a `&str` and a replacement character would be a subject the caller
/// did not hand over.
fn read_subject(path: &str, limit: usize) -> Result<String, BotError> {
    let metadata = std::fs::metadata(path).map_err(|error| io_error(path, error))?;
    let length = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    if length > limit {
        let refusal = Err(BotError::DomainError {
            domain: DOMAIN.into(),
            certainty: DispatchCertainty::Refused,
            cause: format!("{path} is {length} bytes; the inspection source budget is {limit}"),
        });
        lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), "read_subject: returning an error to the caller");
        return refusal;
    }
    std::fs::read_to_string(path).map_err(|error| io_error(path, error))
}

/// The typed refusal a failed filesystem read reports.
fn io_error(path: &str, error: std::io::Error) -> BotError {
    BotError::DomainError {
        domain: DOMAIN.into(),
        certainty: DispatchCertainty::NotDelivered,
        cause: format!("could not read artifact {path}: {error}"),
    }
}

/// The inspection report's identity for the change filter and admitted-input
/// digest.
///
/// The subject's content digest is the identity: two reports over the same bytes
/// name the same input, and a report over different bytes does not. The artifact
/// name is deliberately excluded, matching the operation's own content-addressed
/// digest, so renaming a file does not read as a changed observation.
impl InputIdentity for Inspection {
    const SCHEMA_ID: &'static [u8] = b"lgwks.bot.schema.v1.inspection";

    fn write_identity(&self, hasher: &mut lgwks_std::hash::Hasher) {
        hasher.write_framed(self.subject_digest().as_bytes());
    }
}

/// The body signature of [`inspection_task`].
#[cfg(feature = "script")]
pub type InspectionBody =
    fn(
        crate::script::Scope,
        InspectionJob,
    ) -> crate::BoxFuture<'static, Result<Inspection, crate::script::FlowError>>;

/// A [`Task`](crate::task::Task) that inspects its input and returns the
/// [`Inspection`].
///
/// The script-step entry point R8 asks for: a `Host` runs it inside a `task()`
/// body, and it reaches the same operation as the domain verbs. The body is a
/// plain function pointer (not a closure) so the `Task`'s future type is
/// nameable without erasing it.
///
/// # Errors
///
/// [`FlowError`](crate::script::FlowError) from [`task`](crate::task::task) when
/// `name` is not a valid task name.
#[cfg(feature = "script")]
pub fn inspection_task(
    name: &str,
) -> Result<crate::task::Task<InspectionBody>, crate::script::FlowError> {
    crate::task::task(name, inspection_body)
}

/// The body [`inspection_task`] runs.
#[cfg(feature = "script")]
fn inspection_body(
    _scope: crate::script::Scope,
    job: InspectionJob,
) -> crate::BoxFuture<'static, Result<Inspection, crate::script::FlowError>> {
    Box::pin(async move { Ok(job.inspect()) })
}
