//! One recovery from a poisoned `Mutex`, shared by every test and example in
//! this crate.
//!
//! Included by path rather than compiled as its own target, because a lock
//! helper that two modules each carry is two definitions that may disagree
//! about what a panic leaves behind. It lives under `tests/support/` beside the
//! other shared fixtures and the measurement examples reach it across the
//! sibling directory, which is the price of there being exactly one of them.

use std::sync::{Mutex, MutexGuard};

/// Take `mutex`'s guard, recovering it from a poisoned lock.
///
/// # Why the guarded value is still usable
///
/// A `Mutex` is poisoned when a holder panicked *while holding it*, which says
/// something about the panicking thread and nothing about the value: a poisoned
/// lock means "some writer did not finish", not "the value is torn". Every lock
/// this crate guards in its tests holds either a `Vec` of samples appended after
/// the computation that was measured, a counter, or a set of finished handles,
/// and each is published by a single statement, so a half-written one cannot be
/// observed through this guard. Recovering is therefore the honest reading of
/// the state; refusing would discard measurements that were in fact complete and
/// report a measurement failure that never happened.
///
/// A `Mutex` whose invariant is a multi-statement update is **not** covered by
/// that argument: guard those with a type whose `Default` and mutation cannot
/// interleave, or recover inside the type that owns the invariant.
pub(crate) fn take_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
