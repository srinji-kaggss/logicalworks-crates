//! The allocation counter every rig in this directory shares.
//!
//! One copy, included by path from both roots, for the same reason
//! `crates/lgwks-bot/tests/sim/seed.rs` exists once: two copies of an instrument
//! that produces published numbers is two instruments, and a reader comparing
//! two rigs would be comparing two different measurements under one name.
//!
//! # What it is for, and what it is not
//!
//! Timing alone says a tick is slow; it does not say what the tick spent its
//! time on. This counts allocations, so the hot path's allocation model is a
//! measured number rather than an inference from reading `Box::new` in a source
//! file.
//!
//! Two counters, because they answer different questions: the count is what the
//! work pays, and the bytes say whether those allocations are small handovers or
//! large buffers.
//!
//! # Why counting is switched, not always-on
//!
//! The counters are behind a flag. Counting on every allocation path is a
//! relaxed atomic add, and it is **not free**: leaving it on during a timed run
//! would attribute the counter's own cost to the code under test, which is how a
//! measurement instrument ends up reporting its overhead as a result. Both rigs
//! therefore count in a window separate from every timed round, and the flag is
//! only ever flipped between phases, never inside one.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

/// Allocations counted since the last [`reset`].
static ALLOCS: AtomicU64 = AtomicU64::new(0);

/// Bytes requested across those allocations.
static BYTES: AtomicU64 = AtomicU64::new(0);

/// Whether the counters are live. Relaxed: the flag is flipped between
/// measurement phases only.
static COUNTING: AtomicU64 = AtomicU64::new(0);

/// The counting allocator.
pub struct Counting;

impl Counting {
    /// Whether the counters are live.
    fn on() -> bool {
        COUNTING.load(Ordering::Relaxed) == 1
    }
}

// SAFETY: every method forwards to `System` unchanged, so the allocator
// contract (`alloc`/`dealloc`/`realloc` paired on the same `Layout`) is the
// system allocator's. The only addition is a counter increment, which allocates
// nothing and touches no memory the caller owns.
unsafe impl GlobalAlloc for Counting {
    /// # Safety
    /// `layout` is forwarded verbatim from the caller.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if Self::on() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        // SAFETY: `layout` is forwarded verbatim.
        unsafe { System.alloc(layout) }
    }

    /// # Safety
    /// `ptr` and `layout` are forwarded verbatim from the caller, which obtained
    /// them from this allocator's `alloc`/`realloc`.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded verbatim.
        unsafe { System.dealloc(ptr, layout) }
    }

    /// # Safety
    /// `ptr`, `layout` and `new_size` are forwarded verbatim.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if Self::on() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            // The *new* size, not the delta: an allocator that grew reports the
            // larger figure, and a rig that reported the delta would understate
            // a growing workload. The approximation is stated here rather than
            // left for a reader to discover in a number.
            BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        }
        // SAFETY: forwarded verbatim.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Install this allocator as the process's global one.
///
/// A caller invokes it once. It is a function rather than a `#[global_allocator]`
/// item because a `global_allocator` static may appear once per crate, and the
/// two rigs are separate crates that include this file by path.
#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Turn counting on.
pub fn start() {
    COUNTING.store(1, Ordering::Relaxed);
}

/// Turn counting off.
pub fn stop() {
    COUNTING.store(0, Ordering::Relaxed);
}

/// Zero both counters, so a rig can read a window of its own.
pub fn reset() {
    ALLOCS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
}

/// `(allocations, bytes)` counted so far.
#[must_use]
pub fn snapshot() -> (u64, u64) {
    (ALLOCS.load(Ordering::Relaxed), BYTES.load(Ordering::Relaxed))
}