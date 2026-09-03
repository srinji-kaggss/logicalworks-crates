//! `flow` owns composition domains — pipeline, branch, fan-out. These are
//! Execute impls that chain other Execute actions. No new verb needed.

use crate::cap::Cap;
use crate::error::BotError;
use crate::verb;

/// Execute actions in sequence. Each action's output feeds the next.
/// Capabilities are inherited from the contained actions.
pub struct Pipeline {
    steps: Vec<Box<dyn PipelineStep>>,
    caps: Vec<Cap>,
}

/// A type-erased pipeline step. Implement `Execute` on your domain and use
/// `Pipeline::step()` to add it.
pub trait PipelineStep {
    /// Capabilities this step requires.
    fn required_caps(&self) -> &[Cap];
    /// Run with a type-erased input, producing a type-erased output.
    fn run_any(&self, input: &dyn std::any::Any) -> Result<Box<dyn std::any::Any>, BotError>;
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

    fn run_any(&self, input: &dyn std::any::Any) -> Result<Box<dyn std::any::Any>, BotError> {
        match input.downcast_ref::<A::Input>() {
            Some(typed) => self
                .run(typed)
                .map(|v| Box::new(v) as Box<dyn std::any::Any>),
            None => Err(BotError::DomainError {
                domain: verb::Execute::domain_id(self).into(),
                cause: "type mismatch in pipeline step input".into(),
            }),
        }
    }
}

impl Pipeline {
    /// Create an empty pipeline.
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            caps: Vec::new(),
        }
    }

    /// Add a step to the pipeline.
    pub fn step(mut self, s: impl PipelineStep + 'static) -> Self {
        self.caps.extend(s.required_caps().iter().cloned());
        self.steps.push(Box::new(s));
        self
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

/// Opaque pipeline output — wraps the final step's result.
pub struct PipelineOutput(pub Box<dyn std::any::Any>);

impl verb::Execute for Pipeline {
    type Input = ();
    type Output = PipelineOutput;

    fn required_caps(&self) -> &[Cap] {
        &self.caps
    }

    fn run(&self, _input: &()) -> Result<PipelineOutput, BotError> {
        let mut current: Box<dyn std::any::Any> = Box::new(());
        for step in &self.steps {
            current = step.run_any(current.as_ref())?;
        }
        Ok(PipelineOutput(current))
    }

    fn domain_id(&self) -> &str {
        "flow::pipeline"
    }
}
