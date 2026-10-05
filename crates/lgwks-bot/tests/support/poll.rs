//! The opening every counted test source's poll performs.
//!
//! A source under test refuses a caller whose proof does not cover what it
//! requires, and counts every poll that got past that check, because the count
//! is what a liveness or refresh assertion reads. Written once so the sources
//! that share it cannot drift on which of the two comes first: a poll refused
//! for its capability is not a poll that ran.
//!
//! Included by path: `#[path = "support/poll.rs"] mod poll;` from a target at
//! the crate's `tests/` root, `#[path = "../support/poll.rs"]` from a module of
//! `tests/it/`.

use std::cell::Cell;

use lgwks_bot::{Auth, BotError, Cap};

/// Refuse `auth` unless it covers `required`, then count the poll in `polls`.
pub(crate) fn admit_poll(auth: &Auth, required: &[Cap], polls: &Cell<u32>) -> Result<(), BotError> {
    auth.check(required)?;
    polls.set(polls.get().saturating_add(1));
    Ok(())
}
