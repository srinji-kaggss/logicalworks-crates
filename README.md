# logicalworks-crates

Five independently versioned Rust crates: a zero-external-dependency core, a
runtime for long-running automation, a multi-language AST front end, a proc-macro
support crate, and an audited dependency storefront. Each is usable on its own.
Take one and ignore the rest.

`lgwks_std`'s default features are not dependency-free; that caveat and the
exact dependency graph are in [Choosing a crate](docs/choosing-a-crate.md).

## Crates

| Crate | Version | Docs | What it gives you |
|---|---|---|---|
| `lgwks_std` | 3.0.0 | [docs.rs](https://docs.rs/lgwks_std) | Everyday primitives with no async runtime required: JSON/RON/wire codecs, a blocking HTTP client, retry policies, structured logging with default debugger install, time and calendars, hashing, ids, globs, process control |
| `lgwks_bot` | 3.0.0 | [docs.rs](https://docs.rs/lgwks_bot) | Orchestration you cannot write wrongly: `script!` flows with bounded fan-out, deadlines, retries under a shared budget, tenant-keyed steps and a map of what a run declared |
| `lgwks_ast` | 2.0.0 | [docs.rs](https://docs.rs/lgwks_ast) | Parse many languages into one AST type. tree-sitter grammars behind cargo features, bounded traversal, and typed diagnostics for tools that report on code |
| `lgwks_macros` | 1.1.2 | [docs.rs](https://docs.rs/lgwks_macros) | The syntax of `lgwks_bot::script!`, orchestration written as indented flows. Not a surface of its own: Rust requires a proc macro to live in its own crate, so use it through `lgwks_bot` |
| `lgwks_deps` | 4.0.0 | [docs.rs](https://docs.rs/lgwks_deps) | One audited place to opt into third-party stacks, plus `lgwks-deps check` and `lgwks-deps debug` to prove dependency and debugger wiring |

Versions move independently from one repository and one tag. The table lists the
versions in this tree's manifests. A manifest version is not an upload: the
registry and the repository's release tags say what is published, and
[`docs/releasing.md`](docs/releasing.md) keeps the two apart.

> **Names.** Package `lgwks_std` (underscore) lives in directory
> `crates/lgwks-std` (hyphen) and is imported as `use lgwks_std::...`. The one
> exception is the gate binary `lgwks-deps` (hyphen), built from the library
> `lgwks_deps` (underscore): `cargo install lgwks_deps` for the CLI,
> `cargo add lgwks_deps` for the storefront library.

## Quickstart

```sh
cargo new demo && cd demo
cargo add lgwks_std --features json,http
cargo add lgwks_bot
```

```rust
use std::error::Error;
use std::io::Write;

use lgwks_bot::script::{Scope, Tenant};

lgwks_bot::script! {
    /// Square every number, as many at once as the machine sustains; each
    /// attempt gets a second and a failure worth retrying is retried under
    /// the same key.
    flow squares(numbers: Vec<u64>) -> u64:
        let squared = each number in numbers:
            retry up to 3 times, waiting 10ms:
                within 1s:
                    number.saturating_mul(number)
        give back squared.iter().sum()
}

fn main() -> Result<(), Box<dyn Error>> {
    // Output goes through an explicit locked handle, which turns a broken pipe
    // (`demo | head`) into an ordinary `Err` instead of a panic.
    let mut out = std::io::stdout().lock();

    // Core primitives: zero-config, zero external deps by default.
    let now = lgwks_std::time::now_rfc3339()?;
    writeln!(out, "now: {now} hex: {}", lgwks_std::hex::encode(b"hi"))?;

    // bot: orchestration is a flow. Every flow takes the scope of the tenant it
    // runs for; the fan-out is bounded, every attempt has a deadline, and a
    // retry spends from a budget the whole run shares.
    let scope = Scope::root(Tenant::new("demo")?);
    let total = lgwks_bot::block_on(squares(&scope, vec![1, 2, 3]))?;
    writeln!(out, "sum of squares: {total}")?;
    assert_eq!(total, 14, "1 + 4 + 9");

    Ok(())
}
```

The snippet above is compiled by the workspace gate, from
[`crates/lgwks-bot/examples/quickstart.rs`](crates/lgwks-bot/examples/quickstart.rs).

The dependency gate reads a project's lockfile and its committed register, so it
runs as a command against a project rather than from a library call:

```sh
cargo generate-lockfile   # the gate reads Cargo.lock
cargo install lgwks_deps
lgwks-deps init .         # write a fail-closed register
lgwks-deps check .        # audit, and refuse any unowned external edge
```

## Where to go next

- [Use cases and boundaries](docs/use-cases.md): what each crate is for, and is not.
- [Choosing a crate](docs/choosing-a-crate.md): which crate and feature for a need.
- [Documentation](docs/README.md): every guide, spec and decision record.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md); the checks a change must pass are in
[`AGENTS.md`](AGENTS.md#checks-that-must-pass). Report vulnerabilities through
the process in [`SECURITY.md`](SECURITY.md), not as a public issue.

## License

Copyright 2026 Logical Works Incorporated. `lgwks_std`, `lgwks_ast` and
`lgwks_deps` are Apache-2.0; `lgwks_bot` is [MPL-2.0](crates/lgwks-bot/LICENSE),
file-level copyleft. Each crate ships its own `LICENSE`; [`LICENSING.md`](LICENSING.md)
states the rule, why the bot differs, and the commercial licence.
