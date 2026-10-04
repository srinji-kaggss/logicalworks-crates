//! Audit a checkout against its approval register, the way CI does.
//!
//! Library consumers take the storefront without the gate-tool default:
//!
//! ```toml
//! [dependencies]
//! lgwks_deps = { version = "1", default-features = false }
//! ```
//!
//! Run from a checkout with `cargo run -p lgwks_deps --example check`.
//! The example resolves the workspace root from `CARGO_MANIFEST_DIR`, so it
//! only runs inside the source tree; installed consumers call
//! [`lgwks_deps::check_dependencies`] against their own root instead.
//!
//! Output goes through an explicit stdout handle rather than `println!` so the
//! example is lint-clean under the workspace's `print_stdout` forbid, and so
//! `| head -1` ends the run cleanly instead of panicking on a closed pipe.

use std::error::Error;
use std::io::{self, Write as _};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir.join("..").join("..").canonicalize()?;
    let contract = root.join(lgwks_deps::CONTRACT_PATH);
    let ((_register, refusals), unresolved) =
        lgwks_deps::check_dependencies_against(&root, &contract)?.into_parts();
    assert!(refusals.is_empty(), "unregistered edges: {refusals:?}");
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if let Some(cleanup) = unresolved {
        writeln!(out, "warning: {cleanup}")?;
    }
    writeln!(out, "lgwks_deps check ok at {}", root.display())?;
    Ok(())
}
