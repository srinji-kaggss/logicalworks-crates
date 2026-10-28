//! Compensation: undo finished steps when a later one fails (#654).
//!
//! Money must be refunded, a booking released, a reservation cancelled: real
//! structure with real ordering guarantees, not a line of passthrough Rust no
//! map can see. In `script!` this is `saga name:`, with a `compensate step:`
//! block after each step it undoes. A [`Compensations`] registrar is handed to
//! the guarded body; each finished step registers its undo, and when a later
//! step fails the stack runs in reverse order, once, before the failure is
//! returned. A run that succeeds runs no compensation.
//!
//! The registrar is an owned handle over a shared stack, so the body holds it
//! across awaits with no borrow of the caller and no lock held across an
//! await: each registration locks, pushes and unlocks synchronously. The stack
//! is bounded by [`MAX_COMPENSATIONS`]: a registration past it fails the saga,
//! never grows memory.
//!
//! Each undo runs in a scope of its own, `<saga>/undo:<step>`, entered and
//! settled like any other step, so the trail shows the refund the flow made and
//! not only the charge, and an undo never shares the key of the step it
//! reverses. Every undo runs even when an earlier one failed. When every undo
//! succeeds the caller gets the body's failure; when one fails the caller gets
//! the first undo failure, because the run's state is then the bigger problem,
//! and every failed undo is settled as failed on its own trail entry and
//! logged, so none is dropped silently. An undo that must survive a transient
//! fault says so itself: `retry up to N times:` inside the `compensate` block
//! spends the run's retry budget like any other retry. A resumed run replays
//! finished steps from the store, and the compensations of a run that failed
//! before a resume are the resume's own to register.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use crate::journal::frame::SaturatingFrom;

use super::trail::Site;
use super::{FlowError, Scope};

/// The most undos one saga registers.
///
/// A saga's steps are written, not generated, so a stack this deep is a loop
/// registering without end; refusing past it keeps the stack a bound, not a
/// leak.
pub const MAX_COMPENSATIONS: usize = 4_096;

/// An undo a finished step registers, as awaited.
///
/// `Send` like every other body the runtime drives: an undo held by one task
/// may be registered from any thread the flow runs on.
type UndoFut<'a> = Pin<Box<dyn Future<Output = Result<(), FlowError>> + Send + 'a>>;
/// The boxed undo itself: a scope in, run at most once.
type UndoFn<'a> = Box<dyn FnOnce(Scope) -> UndoFut<'a> + Send + 'a>;

/// An undo registered for one finished step.
struct Undo<'a> {
    /// The finished step's label; the undo runs as `undo:<step>`.
    step: &'static str,
    /// The undo itself.
    run: UndoFn<'a>,
}

/// The registrar one saga's body registers its undos on.
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
    ///
    /// # Errors
    ///
    /// [`FlowError::InvalidBound`] when [`MAX_COMPENSATIONS`] are already
    /// registered; the undo is not registered.
    pub fn add<F, Fut>(&self, step: &'static str, undo: F) -> Result<(), FlowError>
    where
        F: FnOnce(Scope) -> Fut + Send + 'a,
        Fut: Future<Output = Result<(), FlowError>> + Send + 'a,
    {
        let mut stack = self.locked();
        if stack.len() >= MAX_COMPENSATIONS {
            let refusal = Err(FlowError::InvalidBound {
                what: "saga: compensations registered",
                value: u64::saturating_from(stack.len().saturating_add(1)),
                max: u64::saturating_from(MAX_COMPENSATIONS),
            });
            lgwks_std::trace::debug!(error = ?refusal.as_ref().err(), step, "add: refusing an undo past the bound");
            return refusal;
        }
        stack.push(Undo {
            step,
            run: Box::new(move |scope| Box::pin(undo(scope))),
        });
        Ok(())
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

    /// Run the stack in reverse registration order, once, each undo in its own
    /// scope `undo:<step>` under `scope`.
    ///
    /// Every undo runs even when an earlier one failed. Returns the first undo
    /// failure, or `None` when every undo succeeded; every failure is settled
    /// on its own scope and logged.
    async fn run_all(self, scope: &Scope) -> Option<FlowError> {
        let undos: Vec<Undo<'a>> = std::mem::take(&mut *self.locked());
        let mut first: Option<FlowError> = None;
        for undo in undos.into_iter().rev() {
            let Undo { step, run } = undo;
            // `undo:<step>`, never `<step>`: the undo is its own effect, so it
            // carries its own key and never replays as the step it reverses.
            let outcome = match scope.enter(&format!("undo:{step}")) {
                Ok(undo_scope) => {
                    let finished = run(undo_scope.clone()).await;
                    undo_scope.settle(&finished);
                    finished.map_err(|error| error.located(&undo_scope))
                }
                Err(error) => Err(error),
            };
            if let Err(failed) = outcome {
                lgwks_std::trace::warn!(error = %failed, step, "run_all: an undo failed; the rest still run");
                if first.is_none() {
                    first = Some(failed);
                }
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
    saga_from(scope, step, None, body).await
}

#[doc(hidden)]
/// [`compensated`] for a `saga name:` that `script!` wrote at `site`, which
/// the saga's trail entry carries. Called by `script!`.
///
/// # Errors
///
/// As [`compensated`].
pub async fn compensated_at<'a, T, F, Fut>(
    scope: &Scope,
    step: &str,
    site: &'static Site,
    body: F,
) -> Result<T, FlowError>
where
    F: FnOnce(Scope, Compensations<'a>) -> Fut,
    Fut: Future<Output = Result<T, FlowError>>,
{
    saga_from(scope, step, Some(site), body).await
}

/// The body of [`compensated`] and [`compensated_at`].
async fn saga_from<'a, T, F, Fut>(
    scope: &Scope,
    step: &str,
    site: Option<&'static Site>,
    body: F,
) -> Result<T, FlowError>
where
    F: FnOnce(Scope, Compensations<'a>) -> Fut,
    Fut: Future<Output = Result<T, FlowError>>,
{
    let here = scope.enter_from(step, site)?;
    let stack = Compensations::new();
    let outcome = body(here.clone(), stack.clone()).await;
    let finished = match outcome {
        Ok(value) => Ok(value),
        Err(failed) => {
            let located = failed.located(&here);
            match stack.run_all(&here).await {
                Some(undo_failed) => Err(undo_failed),
                None => Err(located),
            }
        }
    };
    here.settle(&finished);
    finished
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{MAX_COMPENSATIONS, compensated};
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
                    compensations.add(step, move |_undo_scope| async move {
                        match order.lock() {
                            Ok(mut held) => held.push(step),
                            Err(_) => return Err(FlowError::failed("the undo order was poisoned")),
                        }
                        Ok(())
                    })?;
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

    /// An undo runs in `<saga>/undo:<step>`, never in the scope of the step it
    /// reverses, so the two never share a key.
    #[test]
    fn an_undo_runs_under_its_own_key() -> TestResult {
        let paths: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let scope = Scope::root(Tenant::new("acme").map_err(|error| error.to_string())?);
        let recorded = Arc::clone(&paths);
        let outcome = crate::block_on(compensated(
            &scope,
            "booking",
            |_scope, compensations| async move {
                compensations.add("charge", move |undo_scope: Scope| async move {
                    if let Ok(mut held) = recorded.lock() {
                        held.push(undo_scope.path().to_owned());
                    }
                    Ok(())
                })?;
                Err::<(), FlowError>(FlowError::failed("issue"))
            },
        ));
        assert!(outcome.is_err(), "the saga fails at its issue");
        let paths = paths
            .lock()
            .map_err(|error| format!("the path log was poisoned: {error}"))?;
        assert_eq!(
            *paths,
            vec![String::from("booking/undo:charge")],
            "the undo's scope is its own"
        );
        Ok(())
    }

    /// A saga that registers past the bound fails instead of growing.
    #[test]
    fn a_stack_past_its_bound_is_refused() -> TestResult {
        let scope = Scope::root(Tenant::new("acme").map_err(|error| error.to_string())?);
        let outcome = crate::block_on(compensated(
            &scope,
            "flood",
            |_scope, compensations| async move {
                for _ in 0..=MAX_COMPENSATIONS {
                    compensations.add("step", |_undo: Scope| async { Ok(()) })?;
                }
                Ok::<(), FlowError>(())
            },
        ));
        assert!(
            matches!(outcome, Err(FlowError::InvalidBound { .. })),
            "the registration past the bound fails the saga: {outcome:?}"
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
