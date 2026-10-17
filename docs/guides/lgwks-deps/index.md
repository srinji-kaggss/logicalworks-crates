# lgwks_deps: library and CLI

There are two ways to use this crate, and they have different dependency costs.

```sh
cargo install lgwks_deps    # the `lgwks-deps` binary: the gate
cargo add lgwks_deps        # the storefront library
```

The binary is the gate. The library is the storefront. Current version:
`3.0.1`, MSRV Rust 1.98.0, edition 2024.

No feature is on by default, so `cargo add lgwks_deps` pulls no third-party
crate: every storefront capability is selected by name, and the lists live in
[`storefront.md`](storefront.md). The gate's embedding API, `check_dependencies`,
is always present and is covered in [`embedding.md`](embedding.md).

The source detectors the CLI used to carry (`lgwks-deps scan`, behind a default
`scan` feature that pulled `syn`) moved to `lgwks_ast` with the one Rust reader
([ADR 0002](../../adr/0002-scan-routed-through-lgwks-ast.md), #408): run
`cargo run -p lgwks_ast --features tool,scan --bin lgwks-ast -- scan`.
`lgwks-deps scan` now says so and exits 2.

## The CLI

```sh
lgwks-deps check .              # audit this repository
lgwks-deps tiers                # the admission ladder, lowest rung first
lgwks-deps request <crate> <v>  # print an approval block to fill in
lgwks-deps init [PATH]          # fail-closed starting register
lgwks-deps freshness [PATH]     # resolved versus latest on crates.io
lgwks-deps vendor check [PATH]  # prove the lockfile is covered by the shared vendor tree
lgwks-deps debug [PATH]         # doctor the default lgwks_std debugger surface
```

`check` reads `cargo metadata --no-deps` rather than the transitive lockfile
closure, because only metadata preserves which package authored an edge
(`crates/lgwks-deps/src/lib.rs:22`). It refuses in both directions: an approval
with no authored Cargo edge is stale authority, and an authored edge with no
approval is unregistered.

`vendor check` binds the register to the bytes the offline build resolves, by
hash.

`debug` is the cargo-doctor path for the debugging lifecycle. It installs the
default `lgwks_std::trace` debugger in its own process, emits lifecycle events,
and inspects the repository manifest to prove the default `trace` surface still
includes `tracing-subscriber`. `--json` keeps the report on stdout and the
trace stream on stderr so agents can parse it without scraping terminal text.

The binary diagnoses. It never approves. Approval is a committed diff in
`contract/APPROVED.toml` with a human's name on it.

How to embed the gate in your own tool, and when adopting this policy is the right
call, are in [`embedding.md`](embedding.md).
