//! `flow` owns composition domains, currently the pipeline. These are
//! Execute impls that chain other Execute actions. No new verb needed.

use crate::cap::{Auth, Cap};
use crate::error::{BotError, DispatchCertainty};
use crate::verb;

/// Execute actions in sequence. Each action's output feeds the next.
/// Capabilities are inherited from the contained actions.
pub struct Pipeline {
    /// The steps in execution order.
    steps: Vec<Box<dyn PipelineStep>>,
    /// The union of every step's `required_caps`, accumulated as steps are
    /// added. Retaining the union means the pipeline is admitted once at build
    /// against the authority it will actually present, rather than being
    /// admitted as cap-free and failing at the first step.
    caps: Vec<Cap>,
}

/// A type-erased pipeline step. Implement `Execute` on your domain and use
/// `Pipeline::step()` to add it. Steps receive the pipeline's `Auth` proof:
/// a step whose caps exceed the presented proof is denied, so a pipeline
/// can never smuggle broader authority into a narrower step.
pub trait PipelineStep {
    /// Capabilities this step requires.
    fn required_caps(&self) -> &[Cap];
    /// Run with a type-erased input, producing a type-erased output.
    fn run_any<'a>(
        &'a self,
        call: (Auth, &'a dyn std::any::Any),
    ) -> crate::BoxFuture<'a, Result<Box<dyn std::any::Any>, BotError>>;
}

impl<A> PipelineStep for A
where
    A: verb::Execute + 'static,
    A::Input: 'static,
    A::Output: 'static,
{
    fn required_caps(&self) -> &[Cap] {
        verb::Execute::required_caps(self)
    }

    fn run_any<'a>(
        &'a self,
        call: (Auth, &'a dyn std::any::Any),
    ) -> crate::BoxFuture<'a, Result<Box<dyn std::any::Any>, BotError>> {
        Box::pin(async move {
            let (auth, input) = call;
            match input.downcast_ref::<A::Input>() {
                Some(typed) => {
                    let value = self.execute_action((auth, typed)).await?;
                    let boxed: Box<dyn std::any::Any> = Box::new(value);
                    Ok(boxed)
                }
                None => Err(BotError::DomainError {
                    domain: verb::Execute::domain_id(self).into(),
                    certainty: DispatchCertainty::Refused,
                    cause: "type mismatch in pipeline step input".into(),
                }),
            }
        })
    }
}

impl Pipeline {
    /// Create an empty pipeline. An empty pipeline requires no capabilities and
    /// executes no steps, returning the unit payload it started with.
    #[must_use]
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            caps: Vec::new(),
        }
    }

    /// Add a step to the pipeline. Steps run in the order added, and the
    /// pipeline's required capabilities become the union of every step's, so a
    /// step that needs `bot.net` makes the whole pipeline need `bot.net`.
    #[must_use]
    pub fn step(mut self, step: impl PipelineStep + 'static) -> Self {
        self.caps.extend(step.required_caps().iter().cloned());
        self.steps.push(Box::new(step));
        self
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

/// Opaque pipeline output: wraps the final step's result.
///
/// `#[non_exhaustive]`: the payload is deliberately unnameable, so the only
/// legitimate way to read it is to downcast the inner `Any` to the type the
/// last step produced. The attribute keeps construction inside the crate, where
/// `execute_action` is the single place that builds one.
#[non_exhaustive]
pub struct PipelineOutput(pub Box<dyn std::any::Any>);

impl verb::Execute for Pipeline {
    type Input = ();
    type Output = PipelineOutput;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    async fn execute_action(&self, call: (Auth, &())) -> Result<PipelineOutput, BotError> {
        let (auth, _) = call;
        auth.check(verb::Execute::required_caps(self))?;
        let mut current: Box<dyn std::any::Any> = Box::new(());
        for step in &self.steps {
            let next = step.run_any((auth.clone(), current.as_ref())).await?;
            current = next;
        }
        Ok(PipelineOutput(current))
    }

    fn domain_id(&self) -> &str {
        "flow::pipeline"
    }
}
