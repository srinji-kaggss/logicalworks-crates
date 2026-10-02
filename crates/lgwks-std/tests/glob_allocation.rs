//! Allocation and storage observations for `lgwks_std::glob` (#154).
//!
//! #154 asks for allocation observations rather than an RSS reading, with
//! pattern storage, path/scalar indexing, rolling rows and caller-owned storage
//! reported separately. A counting global allocator needs `unsafe`, which this
//! workspace forbids, so the probe is a standalone downstream package driven
//! through the shared helper.
//!
//! The oracle is the token count: a per-token row allocation would make a
//! warmed six-token match cost at least six allocations and a warmed
//! twelve-token match at least twelve. After warm capacity both are zero.

#[path = "support/consumer_probe.rs"]
mod consumer_probe;

use consumer_probe::{build_and_run, manifest, measurement};

/// A downstream binary with a counting global allocator observes the
/// allocation contract of the compiled glob matcher.
#[test]
fn consumer_counts_no_allocation_per_token_after_warm_capacity()
-> Result<(), Box<dyn std::error::Error>> {
    let stdout = build_and_run(&manifest("glob_alloc_probe", &[]), GLOB_ALLOCATION_PROBE)?;
    assert!(
        !stdout.trim().is_empty(),
        "the probe reported nothing:\n{stdout}"
    );

    // A warmed same-length match allocates nothing: the scalar index, both
    // rolling rows, and every per-token transition reuse existing memory.
    assert_eq!(
        measurement(&stdout, "warm-match"),
        0,
        "a warmed same-length match must allocate nothing per token:\n{stdout}"
    );
    // Returning to the warm size after a larger path also allocates nothing.
    assert_eq!(
        measurement(&stdout, "warm-match-same-length"),
        0,
        "returning to the warm size must reuse the retained capacity:\n{stdout}"
    );
    // Doubling the token count must not allocate per token either.
    assert_eq!(
        measurement(&stdout, "warm-match-12-token"),
        0,
        "doubling the token count must not allocate per token:\n{stdout}"
    );
    // Growing the scratch costs one resize per buffer — the scalar index and
    // both rolling rows — independent of the token count. A per-token row
    // allocation would cost at least six here.
    assert_eq!(
        measurement(&stdout, "warm-match-longer"),
        3,
        "growing the scratch must cost one resize per buffer, not a row per token:\n{stdout}"
    );
    // Compilation is charged separately from matching, and its cost is not
    // per token: it is the scalar table, the close-bracket table, the token
    // vector, and one range vector per compiled class. The doubled pattern has
    // twice the classes, so it costs one more allocation and nothing
    // proportional to its doubled token count.
    let compile_six = measurement(&stdout, "compile-6-token");
    let compile_twelve = measurement(&stdout, "compile-12-token");
    assert_eq!(
        compile_twelve,
        compile_six + 1,
        "compilation must cost one range vector per class, not one per token:\n{stdout}"
    );

    // Storage is reported as separate figures, never as one RSS number.
    assert_eq!(
        measurement(&stdout, "tokens-12"),
        2 * measurement(&stdout, "tokens-6"),
        "pattern storage must scale with the pattern:\n{stdout}"
    );
    assert_eq!(
        measurement(&stdout, "rows"),
        2,
        "the rolling rows are two, reported separately from the scalar index:\n{stdout}"
    );
    let scalar_slots = measurement(&stdout, "scalar-index");
    assert_eq!(
        measurement(&stdout, "scratch-after-warm"),
        scalar_slots * 4 + 2 * scalar_slots,
        "caller-owned scratch is the scalar index plus both rolling rows:\n{stdout}"
    );
    assert!(
        measurement(&stdout, "tokens-6") < measurement(&stdout, "scratch-after-warm"),
        "pattern storage must be reported apart from caller-owned scratch:\n{stdout}"
    );
    Ok(())
}

/// Counts every allocation in the probe process; each measured region reports
/// the delta across one call.
const GLOB_ALLOCATION_PROBE: &str = r#"
use lgwks_std::glob::{GlobDialect, GlobPattern, GlobScratch};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Runs one measured region and reports its allocation delta.
fn counted<T>(label: &str, work: impl FnOnce() -> T) -> T {
    let before = ALLOCATIONS.load(Ordering::SeqCst);
    let value = std::hint::black_box(work());
    let after = ALLOCATIONS.load(Ordering::SeqCst);
    println!("{label} {}", after - before);
    value
}

fn main() {
    // `*a**/b[0-9]?` compiles to six tokens; the doubled form to twelve.
    let six = counted("compile-6-token", || {
        GlobPattern::compile_with_dialect("*a**/b[0-9]?", GlobDialect::Legacy).ok()
    });
    let twelve = counted("compile-12-token", || {
        GlobPattern::compile_with_dialect("*a**b[0-9]?*a**b[0-9]?", GlobDialect::Legacy).ok()
    });

    let mut scratch = GlobScratch::new();

    // The first match warms the scratch's scalar index and both rolling rows.
    counted("first-match", || {
        if let Some(pattern) = six.as_ref() {
            pattern.is_match_with("a/x/b7z", &mut scratch);
        }
    });
    counted("warm-match", || {
        if let Some(pattern) = six.as_ref() {
            pattern.is_match_with("a/y/b7q", &mut scratch);
        }
    });
    // A longer path reallocates the scratch: one resize per buffer.
    counted("warm-match-longer", || {
        if let Some(pattern) = six.as_ref() {
            pattern.is_match_with("a/x/y/b7z/deep", &mut scratch);
        }
    });
    // Back at the warm size, the retained capacity serves it for free.
    counted("warm-match-same-length", || {
        if let Some(pattern) = six.as_ref() {
            pattern.is_match_with("a/x/b7z", &mut scratch);
        }
    });
    // The twelve-token pattern reuses the same scratch.
    counted("warm-match-12-token", || {
        if let Some(pattern) = twelve.as_ref() {
            pattern.is_match_with("a/x/b7z/a/y/b7q", &mut scratch);
        }
    });

    // Storage, reported separately rather than as one RSS figure.
    println!("tokens-6 {}", six.as_ref().map_or(0, GlobPattern::token_count));
    println!("tokens-12 {}", twelve.as_ref().map_or(0, GlobPattern::token_count));
    println!("rows {}", scratch.row_count());
    println!("scratch-after-warm {}", scratch.storage_bytes());
    println!("scalar-index {}", scratch.scalar_capacity());
}
"#;
