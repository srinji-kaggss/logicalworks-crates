//! The one-call front door to a bounded, fail-fast fan-out with your own errors.
//!
//! [`each`] is the primitive: it needs a [`Scope`], and its bodies can only fail
//! with a [`FlowError`], so an author who wants to say *which item failed, with
//! what error of their own* has to smuggle it out through a shared cell. That is
//! the `Arc<Mutex<..>>` the acceptance spec forbids in a common task body, and it
//! is what authors reach for the moment they want a typed error back.
//!
//! [`FanOut`] closes that gap without a second implementation: it opens a private
//! scope, runs [`each`] over `(index, item)` pairs, keeps the first error the
//! bodies return, and hands it back as [`FanOutError::Item`] with the item's
//! position. Bounded, ordered, fail-fast, owning and drop-safe are exactly
//! `each`'s guarantees, because it is `each` underneath.

use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use super::{FlowError, Scope, Tenant, at_most, each, within};

/// The tenant a [`FanOut`] runs under. A fan-out opened without a scope has no
/// caller identity to carry, and it writes no durable record, so its step keys
/// are never compared with another run's.
const TENANT: &str = "fan-out";

/// Why a [`FanOut`] did not produce its values.
///
/// `E` is the error your own body returned, unchanged: match on it instead of
/// parsing a message.
#[derive(Debug)]
#[non_exhaustive]
pub enum FanOutError<E> {
    /// The item at `index` (its position in the input, from 0) returned `error`.
    ///
    /// The first failure wins: no later item was started and every body still
    /// running was dropped before this was returned.
    Item {
        /// Position of the failing item in the input.
        index: usize,
        /// The error the body returned.
        error: E,
    },
    /// The deadline set with [`FanOut::within`] passed. Every running body was
    /// dropped and no further item was started.
    TimedOut {
        /// The deadline that was declared.
        after: Duration,
    },
    /// The fan-out could not run as asked, for example a bound of zero. No body
    /// was started.
    Flow(FlowError),
}

impl<E: fmt::Display> fmt::Display for FanOutError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Item { index, ref error } => write!(formatter, "item #{index} failed: {error}"),
            Self::TimedOut { after } => write!(formatter, "timed out after {after:?}"),
            Self::Flow(ref error) => write!(formatter, "{error}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for FanOutError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match *self {
            Self::Item { ref error, .. } => Some(error),
            Self::Flow(ref error) => Some(error),
            Self::TimedOut { .. } => None,
        }
    }
}

/// A bounded, fail-fast, drop-safe fan-out over `items`, with typed errors and
/// an optional deadline.
///
/// ```
/// use std::time::Duration;
///
/// use lgwks_bot::script::{FanOut, FanOutError};
///
/// #[derive(Debug, PartialEq)]
/// struct Refused(u32);
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let outcome = lgwks_bot::block_on(
///     FanOut::new(1_u32..=8)
///         .at_most(4)
///         .within(Duration::from_secs(5))
///         .run(|id| async move { if id == 6 { Err(Refused(id)) } else { Ok(id * 2) } }),
/// );
/// match outcome {
///     Err(FanOutError::Item { index, error }) => {
///         assert_eq!((index, error), (5, Refused(6)), "the sixth item, with its own error");
///     }
///     other => return Err(format!("expected the sixth item to fail, got {other:?}").into()),
/// }
/// # Ok(())
/// # }
/// ```
///
/// With no [`at_most`](Self::at_most) the bound is the runtime's: 64 bodies per
/// available core, the same as [`each`] with no limit.
#[must_use = "a fan-out does nothing until `run` is awaited"]
#[derive(Debug)]
pub struct FanOut<I> {
    /// What to run over.
    items: I,
    /// The written bound, or `None` for the runtime's.
    limit: Option<usize>,
    /// The overall deadline, or `None` for none.
    deadline: Option<Duration>,
}

impl<I: IntoIterator> FanOut<I> {
    /// Fan out over `items`. Nothing runs until [`run`](Self::run) is awaited.
    pub fn new(items: I) -> Self {
        Self {
            items,
            limit: None,
            deadline: None,
        }
    }

    /// Run at most `limit` bodies at once. A `limit` of zero, or one past
    /// [`MAX_IN_FLIGHT`](crate::script::MAX_IN_FLIGHT), is refused when the fan-out runs, as
    /// [`FanOutError::Flow`], rather than clamped to something nobody wrote.
    pub fn at_most(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Give the whole fan-out `deadline`. When it passes, every running body is
    /// dropped and the result is [`FanOutError::TimedOut`].
    ///
    /// The deadline is wall time from the call to [`run`](Self::run), not a count
    /// of polls, so it also bounds a body that has stopped making progress.
    pub fn within(mut self, deadline: Duration) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Run `body` once per item and return the values in input order.
    ///
    /// The first `Err` any body returns stops the fan-out and comes back as
    /// [`FanOutError::Item`]. Dropping the returned future drops every body.
    ///
    /// # Errors
    ///
    /// [`FanOutError::Item`] for a failing item, [`FanOutError::TimedOut`] when the
    /// deadline passes, and [`FanOutError::Flow`] when the bound is outside its range.
    pub async fn run<T, E, F, Fut>(self, body: F) -> Result<Vec<T>, FanOutError<E>>
    where
        F: Fn(I::Item) -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        let scope = Scope::root(Tenant::new(TENANT).map_err(FanOutError::Flow)?);
        self.drive(&scope, "run", body).await
    }

    /// Run `body` once per item under `scope`, and return the values in input
    /// order.
    ///
    /// The scoped form of [`run`](Self::run): the items run in scopes descended
    /// from `scope` — `<step>#i`, exactly as [`each`] names them — so every
    /// body reads the caller's [`Tenant`] from its own scope, keys its steps
    /// with it, and shares the caller's clock, policy and stop. A fan-out an
    /// `acme` flow runs is then `acme`'s work in every record it writes, rather
    /// than work under the standalone [`run`](Self::run)'s reserved tenant.
    /// Cancelling `scope` stops the fan-out, and the first `Err` any body
    /// returns still stops it as [`FanOutError::Item`].
    ///
    /// ```
    /// use lgwks_bot::script::{FanOut, Scope, Tenant};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let scope = Scope::root(Tenant::new("acme")?);
    /// let doubled = match lgwks_bot::block_on(
    ///     FanOut::new(1_u32..=8).at_most(4).run_in(&scope, "doubled", |id| async move {
    ///         Ok::<u32, String>(id.saturating_mul(2))
    ///     }),
    /// ) {
    ///     Ok(values) => values,
    ///     Err(error) => return Err(format!("the scoped fan-out failed: {error}").into()),
    /// };
    /// assert_eq!(
    ///     doubled,
    ///     vec![2, 4, 6, 8, 10, 12, 14, 16],
    ///     "every item ran under the caller's scope"
    /// );
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// As [`run`](Self::run), plus whatever [`Scope::enter`] refuses for `step`.
    pub async fn run_in<T, E, F, Fut>(
        self,
        scope: &Scope,
        step: &str,
        body: F,
    ) -> Result<Vec<T>, FanOutError<E>>
    where
        F: Fn(I::Item) -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        self.drive(scope, step, body).await
    }

    /// The one driver behind [`run`](Self::run) and
    /// [`run_in`](Self::run_in): [`each`] over `(index, item)` pairs under
    /// `scope`, keeping the first error the bodies return.
    async fn drive<T, E, F, Fut>(
        self,
        scope: &Scope,
        step: &str,
        body: F,
    ) -> Result<Vec<T>, FanOutError<E>>
    where
        F: Fn(I::Item) -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        let limit = match self.limit {
            Some(written) => Some(at_most(written).map_err(FanOutError::Flow)?),
            None => None,
        };
        let first: Arc<Mutex<Option<(usize, E)>>> = Arc::new(Mutex::new(None));
        let body = &body;
        let work = each(
            scope,
            step,
            limit,
            self.items.into_iter().enumerate(),
            |_step, (index, item)| {
                let first = Arc::clone(&first);
                let running = body(item);
                async move {
                    match running.await {
                        Ok(value) => Ok(value),
                        Err(error) => {
                            lock(&first).get_or_insert((index, error));
                            Err(FlowError::failed("a fan-out item failed"))
                        }
                    }
                }
            },
        );
        let outcome = match self.deadline {
            Some(deadline) => within(scope, "deadline", deadline, work).await,
            None => work.await,
        };
        match outcome {
            Ok(values) => Ok(values),
            Err(flow) => Err(lock(&first).take().map_or_else(
                || match flow {
                    FlowError::TimedOut { after, .. } => FanOutError::TimedOut { after },
                    other => FanOutError::Flow(other),
                },
                |(index, error)| FanOutError::Item { index, error },
            )),
        }
    }
}

/// Take the lock. A poisoned cell still holds a consistent value, because the
/// only writes are `get_or_insert` and `take`, which cannot be left half done.
fn lock<E>(cell: &Mutex<Option<(usize, E)>>) -> MutexGuard<'_, Option<(usize, E)>> {
    crate::journal::owner::lock(cell)
}
