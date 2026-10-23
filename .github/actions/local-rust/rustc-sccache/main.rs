//! cargo's `RUSTC_WRAPPER` on the runners: rustc through sccache, without the
//! variable that names the instance.
//!
//! sccache keys a compile by the `CARGO_` variables rustc is given, and cargo
//! gives rustc its whole environment. The `local-rust` action exports
//! `CARGO_TARGET_DIR`, a different directory for each of the thirteen instances,
//! so no instance could read a registry crate another had compiled: one checkout
//! built into a second target directory hit 0 of 2 with the variable set and 2 of
//! 2 with the directory passed as a flag (sccache 0.18.0). No compile reads it.
//!
//! The action builds this file with the pinned `rustc` alone: no cargo, no
//! dependencies, under a second.

use std::io::{Error, Result};
use std::os::unix::process::CommandExt;
use std::process::Command;

fn main() -> Result<()> {
    // `exec` replaces this process and returns only when it could not; the
    // runtime prints that error and exits 1, which cargo reads as a failed rustc.
    let cause = Command::new("sccache")
        .args(std::env::args_os().skip(1))
        .env_remove("CARGO_TARGET_DIR")
        .exec();
    Err(Error::new(
        cause.kind(),
        format!("rustc-sccache: cannot run sccache: {cause}"),
    ))
}
