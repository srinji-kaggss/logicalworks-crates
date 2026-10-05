//! The source the observation rows share: a value, a cache declaration, and a
//! refusal, all under the test's control.
//!
//! One definition rather than one per test file, because every observation row
//! needs the same three facts and two hand-written copies would be free to
//! drift on which of them is read first: a declaration read before the poll
//! rather than after it is a different contract, and a test that passed against
//! one copy would say nothing about the other.
//!
//! Included by path:
//! `#[path = "support/declarable.rs"] mod declarable;` from a target at the
//! crate's `tests/` root, `#[path = "../support/declarable.rs"] mod declarable;`
//! from a module of `tests/it/`.

use std::cell::Cell;
use std::rc::Rc;

use lgwks_bot::{Auth, BotError, Cap, DispatchCertainty, Observe, RefreshReason};

/// A source whose value is the test's, and whose cache declaration and refusal
/// are both the test's.
///
/// The declaration is read through the public [`Observe::cache_state`] on the
/// tick after the poll that reached the failure, which is the ordering the trait
/// documents: the reason is a statement about state the poll already touched.
pub(crate) struct Declarable {
    /// The value this poll reports.
    value: Rc<Cell<u32>>,
    /// What `cache_state` answers, read fresh on every call.
    reason: Rc<Cell<Option<RefreshReason>>>,
    /// Whether the next poll refuses rather than reading.
    refusing: Rc<Cell<bool>>,
    /// How many times the body ran.
    polls: Rc<Cell<u32>>,
    /// The domain identity a report names this source by.
    domain: &'static str,
}

/// The handles a test drives a [`Declarable`] through.
///
/// Returned together rather than exposed as fields, so a caller moves a value, a
/// declaration, a refusal and a poll count by name at its call site rather than
/// by position.
pub(crate) type DeclarableHandles = (
    Rc<Cell<u32>>,
    Rc<Cell<Option<RefreshReason>>>,
    Rc<Cell<bool>>,
    Rc<Cell<u32>>,
);

impl Declarable {
    /// A source reporting `value`, declaring nothing, reading cleanly, under
    /// `domain`.
    pub(crate) fn new(value: u32, domain: &'static str) -> Self {
        Self::over(
            Rc::new(Cell::new(value)),
            Rc::new(Cell::new(None)),
            Rc::new(Cell::new(false)),
            Rc::new(Cell::new(0)),
            domain,
        )
    }

    /// A source reading the caller's own handles, so a scenario that moves the
    /// value or declares a reason through the handle it kept sees the source
    /// read the same cell.
    ///
    /// `new` and this are two constructors rather than one constructor taking
    /// four optional handles: a caller that has its own cells must not be able
    /// to forget one and silently observe a different source than the one its
    /// assertions are driving.
    pub(crate) fn over(
        value: Rc<Cell<u32>>,
        reason: Rc<Cell<Option<RefreshReason>>>,
        refusing: Rc<Cell<bool>>,
        polls: Rc<Cell<u32>>,
        domain: &'static str,
    ) -> Self {
        Self {
            value,
            reason,
            refusing,
            polls,
            domain,
        }
    }

    /// The handles this source reads from, cloned so the source keeps its own.
    pub(crate) fn handles(&self) -> DeclarableHandles {
        (
            Rc::clone(&self.value),
            Rc::clone(&self.reason),
            Rc::clone(&self.refusing),
            Rc::clone(&self.polls),
        )
    }
}

impl Observe for Declarable {
    type Output = u32;

    fn required_caps(&self) -> &[Cap] {
        &[]
    }

    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        crate::poll::admit_poll(&call.0, Observe::required_caps(self), &self.polls)?;
        if self.refusing.get() {
            return Err(BotError::DomainError {
                domain: self.domain.to_owned(),
                certainty: DispatchCertainty::NotDelivered,
                cause: "the source refused to read".to_owned(),
            });
        }
        Ok(self.value.get())
    }

    fn cache_state(&self) -> Option<RefreshReason> {
        self.reason.get()
    }

    fn domain_id(&self) -> &str {
        self.domain
    }
}
