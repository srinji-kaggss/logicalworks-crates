use std::io;

#[expect(
    deprecated,
    reason = "this consumer checks the storefront feature keeps its 1.x path compiling"
)]
fn probe(pgid: i32) -> io::Result<bool> {
    lgwks_deps::process_group::exists(pgid)
}

fn main() {
    let _probe: fn(i32) -> io::Result<bool> = probe;
}
