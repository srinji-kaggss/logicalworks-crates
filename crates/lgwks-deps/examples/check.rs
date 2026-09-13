//! Audit a checkout against its approval register, the way CI does.
//!
//! Library consumers take the storefront without the gate-tool default:
//!
//! ```toml
//! [dependencies]
//! lgwks_deps = { version = "0.1.6", default-features = false }
//! ```
//!
//! Run from a checkout with `cargo run -p lgwks_deps --example check`.
//! The example resolves the workspace root from `CARGO_MANIFEST_DIR`, so it
//! only runs inside the source tree — installed consumers call
//! [`lgwks_deps::check_dependencies`] against their own root instead.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .join("..")
        .join("..")
        .canonicalize()
        .expect("workspace root above crates/lgwks-deps");
    let contract = root.join(lgwks_deps::CONTRACT_PATH);
    let (_register, refusals) =
        lgwks_deps::check_dependencies_against(&root, &contract).expect("gate reads register");
    assert!(refusals.is_empty(), "unregistered edges: {refusals:?}");
    println!("lgwks_deps check ok at {}", root.display());
}
