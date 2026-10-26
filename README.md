# logicalworks-crates

**logicalworks-crates is the Rust crate estate behind Logical Works: five
independently versioned crates — primitives with no async runtime required,
automation that runs unattended, one AST across many languages, the `script!`
proc macro, and an audited dependency storefront.**

[![CI](https://github.com/srinji-kaggss/logicalworks-crates/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/srinji-kaggss/logicalworks-crates/actions/workflows/ci.yml) ![status: unstable](https://img.shields.io/badge/status-unstable-orange) ![license: Apache-2.0 and MPL-2.0](https://img.shields.io/badge/license-Apache--2.0%20%7C%20MPL--2.0-blue)
[![lgwks_std](https://img.shields.io/crates/v/lgwks_std?label=lgwks_std)](https://crates.io/crates/lgwks_std) [![lgwks_bot](https://img.shields.io/crates/v/lgwks_bot?label=lgwks_bot)](https://crates.io/crates/lgwks_bot) [![lgwks_ast](https://img.shields.io/crates/v/lgwks_ast?label=lgwks_ast)](https://crates.io/crates/lgwks_ast) [![lgwks_macros](https://img.shields.io/crates/v/lgwks_macros?label=lgwks_macros)](https://crates.io/crates/lgwks_macros) [![lgwks_deps](https://img.shields.io/crates/v/lgwks_deps?label=lgwks_deps)](https://crates.io/crates/lgwks_deps)

> **Unstable.** Every interface here changes without notice until we stop breaking something every other day. A breaking change still bumps the major version, but nothing outside Logical Works is promised a stable surface yet. This note comes off when that is true.

[Should I use logicalworks-crates?](docs/should-i-use.md) — where each crate is right, where it is wrong, and what to use instead; the estate is not part of Braid, which pins three of these crates and depends on them, never the reverse.

`lgwks_std`'s default features are not dependency-free; that caveat and the
exact dependency graph are in [Choosing a crate](docs/choosing-a-crate.md).

## Crates

| Crate | Version | Docs | What it gives you |
|---|---|---|---|
| `lgwks_std` | 3.1.0 | [docs.rs](https://docs.rs/lgwks_std) | Everyday primitives with no async runtime required: JSON/RON/wire codecs, a blocking HTTP client, retry policies, structured logging with default debugger install, time and calendars, hashing, ids, globs, process control |
| `lgwks_bot` | 4.0.0 | [docs.rs](https://docs.rs/lgwks_bot) | Orchestration you cannot write wrongly: `script!` flows with bounded fan-out, deadlines, retries under a shared budget, tenant-keyed steps and a map of what a run declared |
| `lgwks_ast` | 3.0.0 | [docs.rs](https://docs.rs/lgwks_ast) | Parse many languages into one AST type. tree-sitter grammars behind cargo features, bounded traversal, and typed diagnostics for tools that report on code |
| `lgwks_macros` | 1.1.6 | [docs.rs](https://docs.rs/lgwks_macros) | The syntax of `lgwks_bot::script!`, orchestration written as indented flows. Not a surface of its own: Rust requires a proc macro to live in its own crate, so use it through `lgwks_bot` |
| `lgwks_deps` | 5.0.0 | [docs.rs](https://docs.rs/lgwks_deps) | One audited place to opt into third-party stacks, plus `lgwks-deps check` and `lgwks-deps debug` to prove dependency and debugger wiring |

Versions move independently from one repository and one tag; the table lists this tree's
manifests, not what is published ([`docs/releasing.md`](docs/releasing.md) keeps the two apart).

> **Names.** `lgwks_std` (underscore) lives in `crates/lgwks-std` (hyphen) and is
> imported `use lgwks_std::...`; `cargo install lgwks_deps` builds `lgwks-deps`.

## Quickstart

```sh
cargo new demo && cd demo
cargo add lgwks_std --features json,http
cargo add lgwks_bot
```

```rust
use lgwks_bot::script::{FlowError, Scope, Tenant};
use std::error::Error;
use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};

/// A stand-in for a backend having a bad minute: the first two calls answer
/// "busy", the third succeeds. The count is shared state the flow reaches
/// through a reference, the way a real client handle rides in.
struct Flaky(AtomicU8);

impl Flaky {
    /// Square `number`, or refuse transiently twice before succeeding.
    async fn call(&self, number: u64) -> Result<u64, FlowError> {
        if self.0.fetch_add(1, Ordering::Relaxed) < 2 {
            lgwks_std::trace::debug!("the backend was busy; the flow will retry");
            return Err(FlowError::transient("the backend was busy"));
        }
        Ok(number.saturating_mul(number))
    }
}

lgwks_bot::script! {
    /// Square every number; a failure worth retrying is retried under the
    /// same key, every attempt holding a one-second budget.
    flow squares(flaky: &Flaky, numbers: Vec<u64>) -> u64:
        let squared = each number in numbers:
            retry up to 3 times, waiting 10ms:
                within 1s:
                    flaky.call(number).await?
        give back squared.iter().sum()
}

fn main() -> Result<(), Box<dyn Error>> {
    // Output goes through an explicit locked handle, which turns a broken pipe
    // (`demo | head`) into an ordinary `Err` instead of a panic.
    let mut out = std::io::stdout().lock();

    let flaky = Flaky(AtomicU8::new(0));
    let scope = Scope::root(Tenant::new("demo")?);
    let total = lgwks_bot::block_on(squares(&scope, &flaky, vec![3]))?;
    writeln!(out, "sum of squares: {total}")?;
    assert_eq!(total, 9, "3 squared after two transient refusals");
    assert_eq!(flaky.0.load(Ordering::Relaxed), 3, "three attempts");

    Ok(())
}
```

The snippet is compiled by the workspace gate, from [`crates/lgwks-bot/examples/quickstart.rs`](crates/lgwks-bot/examples/quickstart.rs);
the other guarantees fire in [`examples/guarantees.rs`](crates/lgwks-bot/examples/guarantees.rs) with `--features ephemeral`.

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
- [Documentation](docs/README.md): every guide, spec and decision record. Agent-readable index: [`llms.txt`](llms.txt).
- [Boundaries and failure](docs/distributed-boundaries.md): what async clients do not get, plus [bounded background work](docs/guides/lgwks-bot/background-work.md) and [what a failed tick means](docs/guides/lgwks-bot/failures.md).

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md); the checks a change must pass are in [`AGENTS.md`](AGENTS.md#checks-that-must-pass).
Report vulnerabilities through the process in [`SECURITY.md`](SECURITY.md), not as a public issue.

## License

Copyright 2026 Logical Works Incorporated. `lgwks_std`, `lgwks_ast` and `lgwks_deps` are Apache-2.0;
`lgwks_bot` is [MPL-2.0](crates/lgwks-bot/LICENSE), file-level copyleft. Each crate ships its own `LICENSE`;
[`LICENSING.md`](LICENSING.md) states the rule, why the bot differs, and the commercial licence.
