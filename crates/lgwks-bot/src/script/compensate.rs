//! Compensation: undo finished steps when a later one fails (#654).
//!
//! Money must be refunded, a booking released, a reservation cancelled: real
//! structure with real ordering guarantees, not a line of passthrough Rust no
//! map can see. A [`Compensations`] registrar is handed to the guarded body;
//! each finished step registers its undo, and when a later step fails the
//! stack runs in reverse order, once, before the failure is returned. A run
//! that succeeds runs no compensation.
//!
//! The registrar is an owned handle over a shared stack, so the body holds it
//! across awaits with no borrow of the caller and no lock held across an
//! await: each registration locks, pushes, and unlocks synchronously. The
//! trail shows every compensation as the step it is: each undo runs in a
//! scope of its own, entered and settled like any other, so a reader reviewing
//! the map sees the refund the flow performed, not only the charge. When every
//! undo succeeds the caller gets the body's failure; when an undo fails the
//! caller gets the undo's failure, because the run's state is then the bigger
//! problem. Cross-resume replay of compensations is not wired yet: a resumed
//! run replays finished steps from the store, and the compensations of a run
//! that failed before a resume are the resume's own to register.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use super::{FlowError, Scope};

/// An undo a finished step registers: a scope in, nothing or a failure out.
///
/// `Send` like every other body the runtime drives: an undo held by one task
/// may be registered from any thread the flow runs on.
type UndoFut<'a> = Pin<Box<dyn Future<Output = Result<(), FlowError>> + Send + 'a>>;
/// The boxed undo itself.
type UndoFn<'a> = Box<dyn FnOnce(Scope) -> UndoFut<'a> + Send + 'a>;

/// An undo registered for one finished step, run at most once.
struct Undo<'a> {
    /// The finished step's label, entered again when the undo runs.
    step: &'static str,
    /// The undo itself.
    run: UndoFn<'a>,
}

/// The registrar one guarded body registers its undos on.
///
/// Created by [`compensated`] and handed to the body by value: the body holds
/// it across awaits, and every registration is one synchronous lock, push and
/// unlock. Clone shares the same stack.
#[derive(Clone)]
pub struct Compensations<'a> {
    /// The registered undos, in registration order.
    stack: Arc<Mutex<Vec<Undo<'a>>>>,
}

impl<'a> Compensations<'a> {
    /// A registrar with nothing registered. [`compensated`] builds the one the
    /// body uses; this is the form a caller holds to inspect or drive one by
    /// hand.
    #[must_use]
    pub fn new() -> Self {
        Self {
            stack: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Register `undo` for the finished step `step`.
    pub fn add(&self, step: &'static str, undo: impl FnOnce(Scope) -> UndoFut<'a> + Send + 'a) {
        self.locked().push(Undo {
            step,
            run: Box::new(undo),
        });
    }

    /// How many undos are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.locked().len()
    }

    /// Whether nothing is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.locked().is_empty()
    }

    /// The stack, recovered from a poisoned lock.
    ///
    /// A panic while registered leaves no half-written entry — every mutation
    /// is one `push` — so the recovered stack is a stack of undos, and
    /// refusing on poison would strand them.
    fn locked(&self) -> std::sync::MutexGuard<'_, Vec<Undo<'a>>> {
        match self.stack.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Run the stack in reverse registration order, once, each undo in a scope
    /// of its own under `scope`.
    ///
    /// Every undo runs even when an earlier one failed: a half-compensated run
    /// reports every failure, and the first undo failure is the one returned.
    /// Returns the first undo failure, or `None` when every undo succeeded.
    async fn run_all(self, scope: &Scope) -> Option<FlowError> {
        let undos: Vec<Undo<'a>> = std::mem::take(&mut *self.locked());
        let mut first: Option<FlowError> = None;
        for undo in undos.into_iter().rev() {
            let Undo { step, run } = undo;
            let outcome = match scope.enter(step) {
                Ok(undo_scope) => {
                    let finished = run(undo_scope.clone()).await;
                    undo_scope.settle(&finished);
                    finished
                }
                Err(error) => Err(error),
            };
            if first.is_none()
                && let Err(failed) = outcome
            {
                first = Some(failed);
            }
        }
        first
    }
}

impl Default for Compensations<'_> {
    fn default() -> Self {
        Self::new()
    }
}

/// Run `body` with a compensation registrar, undoing in reverse order when it
/// fails.
///
/// The body registers one undo per finished step through the registrar it is
/// handed. On success the stack is dropped and nothing runs; on failure the
/// stack runs in reverse registration order, once, and the caller gets the
/// body's failure — or the first undo's failure when compensating itself
/// failed.
///
/// # Errors
///
/// The body's failure with its compensations run, or the first compensation
/// failure when an undo failed.
pub async fn compensated<'a, T, F, Fut>(scope: &Scope, step: &str, body: F) -> Result<T, FlowError>
where
    F: FnOnce(Scope, Compensations<'a>) -> Fut,
    Fut: Future<Output = Result<T, FlowError>>,
{
    let here = scope.enter(step)?;
    let stack = Compensations::new();
    let outcome = body(here.clone(), stack.clone()).await;
    let finished = match outcome {
        Ok(value) => Ok(value),
        Err(failed) => {
            let located = failed.located(&here);
            if let Some(undo_failed) = stack.run_all(&here).await {
                Err(undo_failed)
            } else {
                Err(located)
            }
        }
    };
    here.settle(&finished);
    finished
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::compensated;
    use crate::script::{FlowError, Scope, Tenant};

    /// A test's result: an error fails it with the error's text.
    type TestResult = Result<(), String>;

    /// Run `body` under `compensated` for tenant `acme`, recording the order
    /// undos ran in.
    async fn run_with(
        order: Arc<Mutex<Vec<&'static str>>>,
        fail_at: Option<&str>,
    ) -> Result<&'static str, FlowError> {
        let scope = Scope::root(Tenant::new("acme").map_err(FlowError::failed)?);
        compensated(&scope, "booking", |_scope, compensations| {
            let order = Arc::clone(&order);
            async move {
                for step in ["reserve", "charge", "issue"] {
                    if Some(step) == fail_at {
                        return Err(FlowError::failed(step));
                    }
                    let order = Arc::clone(&order);
                    compensations.add(step, move |undo_scope| {
                        let order = Arc::clone(&order);
                        Box::pin(async move {
                            let _ = undo_scope;
                            match order.lock() {
                                Ok(mut held) => held.push(step),
                                Err(_) => {
                                    return Err(FlowError::failed("the undo order was poisoned"));
                                }
                            }
                            Ok(())
                        })
                    });
                }
                Ok("ticket")
            }
        })
        .await
    }

    /// Read the undo order back, or fail the test on a poisoned lock.
    fn order_of(order: &Mutex<Vec<&'static str>>) -> Result<Vec<&'static str>, String> {
        match order.lock() {
            Ok(held) => Ok(held.clone()),
            Err(_) => Err(String::from("the undo order was poisoned")),
        }
    }

    /// When issuing fails after the charge, the charge and the reservation are
    /// undone in reverse order, once, and the caller gets the issue failure.
    #[test]
    fn undos_run_in_reverse_order_once_on_failure() -> TestResult {
        let order = Arc::new(Mutex::new(Vec::new()));
        let outcome = crate::block_on(run_with(Arc::clone(&order), Some("issue")));
        match outcome {
            Err(error) => assert!(
                error.to_string().contains("issue"),
                "the caller gets the body's failure: {error}"
            ),
            Ok(_) => return Err(String::from("issuing must fail this run")),
        }
        assert_eq!(
            order_of(&order)?,
            vec!["charge", "reserve"],
            "compensations run in reverse registration order, once"
        );
        Ok(())
    }

    /// A run that succeeds runs no compensation.
    #[test]
    fn success_runs_no_compensation() -> TestResult {
        let order = Arc::new(Mutex::new(Vec::new()));
        let ticket = crate::block_on(run_with(Arc::clone(&order), None))
            .map_err(|error| error.to_string())?;
        assert_eq!(ticket, "ticket", "the booking succeeds");
        assert!(order_of(&order)?.is_empty(), "success runs no compensation");
        Ok(())
    }
}
