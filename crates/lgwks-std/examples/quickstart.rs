//! `lgwks_std` in ten lines: core primitives need no features at all.
//!
//! ```sh
//! cargo add lgwks_std
//! ```
//!
//! Run with `cargo run -p lgwks_std --example quickstart`.

use std::io::Write;

use lgwks_std::{encoding, glob, hex, retry, time};

/// `main` returns `Result` so a failing step reports its own `Debug` rather
/// than panicking, and output goes to an explicit handle because
/// `clippy::print_stdout` is forbidden workspace-wide with no example
/// carve-out.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Hex, base64, timestamps, glob — zero external deps on default features.
    let digest = hex::encode(b"hello");
    assert_eq!(
        digest, "68656c6c6f",
        "hex::encode of b\"hello\" must be lowercase hex"
    );

    let encoded = encoding::base64::encode(b"payload");
    assert_eq!(
        encoded, "cGF5bG9hZA==",
        "base64 of b\"payload\" must be the padded standard encoding"
    );

    let now = time::now_rfc3339();
    assert!(
        now.ends_with('Z'),
        "RFC 3339 UTC output must end in Z, got {now}"
    );

    assert!(
        glob::matches("src/**/*.rs", "src/lib.rs"),
        "`src/**/*.rs` must match a nested Rust source file"
    );
    assert!(
        !glob::matches("src/**/*.rs", "README.md"),
        "`src/**/*.rs` must not match a Markdown file"
    );

    // Retry budgets are plain values: no I/O, no threads, no clock reads.
    let policy = retry::RetryPolicy::new(
        3,
        std::time::Duration::from_millis(100),
        std::time::Duration::from_secs(5),
    );
    assert!(
        policy.should_retry(0, std::time::Duration::ZERO),
        "attempt 0 is under a budget of 3 and must retry"
    );

    let mut out = std::io::stdout();
    writeln!(out, "lgwks_std quickstart ok at {now}")?;
    Ok(())
}
