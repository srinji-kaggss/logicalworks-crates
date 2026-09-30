use std::io;

fn probe(pgid: i32) -> io::Result<bool> {
    lgwks_deps::process_group::exists(pgid)
}

fn main() {
    let _probe: fn(i32) -> io::Result<bool> = probe;
}
