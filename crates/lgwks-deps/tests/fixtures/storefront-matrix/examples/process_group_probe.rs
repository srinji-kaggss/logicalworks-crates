//! The one consumer of `lgwks_deps::process_group::exists`, the deprecated
//! forward kept for the 1.x API.
//!
//! This example compiles so the fixture matrix proves the forward still builds
//! for a consumer that has not migrated. It compiles *with* the deprecation
//! warning it is supposed to produce: the warning is what a consumer on the old
//! path sees, and an example that hid it would prove nothing about what they get.
//! The supported path is `lgwks_std::process::process_group_exists`, which this
//! fixture cannot name — its only dependency is `lgwks_deps`, and reaching the
//! owner through the deprecated forward is what this example is for.

use std::io;

fn probe(pgid: i32) -> io::Result<bool> {
    lgwks_deps::process_group::exists(pgid)
}

fn main() {
    let _probe: fn(i32) -> io::Result<bool> = probe;
}