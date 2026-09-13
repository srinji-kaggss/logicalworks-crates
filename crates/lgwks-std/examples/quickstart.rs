//! `lgwks_std` in ten lines: core primitives need no features at all.
//!
//! ```sh
//! cargo add lgwks_std
//! ```
//!
//! Run with `cargo run -p lgwks_std --example quickstart`.

use lgwks_std::{encoding, glob, hex, retry, time};

fn main() {
    // Hex, base64, timestamps, glob — zero external deps on default features.
    let digest = hex::encode(b"hello");
    assert_eq!(digest, "68656c6c6f");

    let encoded = encoding::base64::encode(b"payload");
    assert_eq!(encoded, "cGF5bG9hZA==");

    let now = time::now_rfc3339();
    assert!(now.ends_with('Z'));

    assert!(glob::matches("src/**/*.rs", "src/lib.rs"));
    assert!(!glob::matches("src/**/*.rs", "README.md"));

    // Retry budgets are plain values: no I/O, no threads, no clock reads.
    let policy = retry::RetryPolicy::new(
        3,
        std::time::Duration::from_millis(100),
        std::time::Duration::from_secs(5),
    );
    assert!(policy.should_retry(0, std::time::Duration::ZERO));

    println!("lgwks_std quickstart ok at {now}");
}
