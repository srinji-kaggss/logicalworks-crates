# logicalworks-crates

Five independently versioned Rust crates: a zero-external-dependency core, a
runtime for long-running automation, a multi-language AST front end, a proc-macro
support crate, and an audited dependency storefront. Each is usable on its own.
Take one and ignore the rest.

What the crates are for is [`docs/overview.md`](docs/overview.md); which one to
reach for, and the dependency graph stated exactly, is
[`docs/choosing-a-crate.md`](docs/choosing-a-crate.md). Every document is indexed
in [`docs/README.md`](docs/README.md); to use a crate rather than change one,
start with the [consumer guides](docs/guides/index.md).

## Crates

| Crate | Version | Docs | What it gives you |
|---|---|---|---|
| `lgwks_std` | 2.2.0 | [docs.rs](https://docs.rs/lgwks_std) | Everyday primitives with no async runtime required: JSON/RON/wire codecs, a blocking HTTP client, retry policies, structured logging with default debugger install, time and calendars, hashing, ids, globs, process control |
| `lgwks_bot` | 2.2.0 | [docs.rs](https://docs.rs/lgwks_bot) | A runtime for bots that run for weeks: four verbs (Observe, Evaluate, Execute, Query), capability-gated authority, change-triggered execution, and supervision that bounds background work |
| `lgwks_ast` | 1.1.0 | [docs.rs](https://docs.rs/lgwks_ast) | Parse many languages into one AST type. tree-sitter grammars behind cargo features, bounded traversal, and typed diagnostics for tools that report on code |
| `lgwks_macros` | 1.1.1 | [docs.rs](https://docs.rs/lgwks_macros) | The syntax of `lgwks_bot::script!`, orchestration written as indented flows. Not a surface of its own: Rust requires a proc macro to live in its own crate, so use it through `lgwks_bot` |
| `lgwks_deps` | 3.0.1 | [docs.rs](https://docs.rs/lgwks_deps) | One audited place to opt into third-party stacks, plus `lgwks-deps check` and `lgwks-deps debug` to prove dependency and debugger wiring |

Versions move independently from one repository and one tag. The table lists the
versions in this tree's manifests. A manifest version is not an upload: the
newest tags are recorded in [`docs/guides/index.md`](docs/guides/index.md), and
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
use std::io::Write;

use lgwks_bot::broker::Broker;
use lgwks_bot::effect::{EnvironmentId, FlowRevision, RunId};
use lgwks_bot::journal::MemoryJournal;
use lgwks_bot::spec::{EffectIdentity, EffectScope};
use lgwks_bot::{Bot, GrantSet};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Output goes through an explicit locked handle, which turns a broken pipe
    // (`demo | head`) into an ordinary `Err` instead of a panic.
    let mut out = std::io::stdout().lock();

    // Core primitives: zero-config, zero external deps by default.
    let now = lgwks_std::time::now_rfc3339()?;
    writeln!(out, "now: {now} hex: {}", lgwks_std::hex::encode(b"hi"))?;

    // bot: capability-gated actors. Grants are required to build, not only to
    // run, and so is an effect scope: it names the run, the environment that run
    // acts on, and the journal a dispatch is written to before the effect leaves
    // the process. `build` returns a typed error; the caller decides what to do
    // with it.
    let environment = EnvironmentId::from_hex("2122232425262728292a2b2c2d2e2f30")?;
    let mut broker = Broker::new();
    broker.register(environment)?;
    let effects = EffectScope::new(
        EffectIdentity::new(
            RunId::from_hex("0102030405060708090a0b0c0d0e0f10")?,
            environment,
            FlowRevision::from_tagged(
                "blake3_256",
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            )?,
        ),
        broker,
        Box::new(MemoryJournal::new()),
    );

    let bot = Bot::builder("demo")
        .with_effects(effects)
        .build(&GrantSet::empty())?;
    let _ = bot;

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

## Building and testing

The gate is defined once in `scripts/gate-lanes.toml` and run with
`./scripts/ci-local.sh`; [`AGENTS.md`](AGENTS.md) lists every check that must pass.
See [`CONTRIBUTING.md`](CONTRIBUTING.md) to contribute, and report vulnerabilities
through [`SECURITY.md`](SECURITY.md), not as a public issue.

## License

Copyright 2026 Logical Works Incorporated; each crate ships its own `LICENSE`.
`lgwks_std`, `lgwks_ast`, and `lgwks_deps` are Apache-2.0, for example
[`crates/lgwks-std/LICENSE`](crates/lgwks-std/LICENSE). `lgwks_bot` is
[MPL-2.0](crates/lgwks-bot/LICENSE): file-level copyleft, so the artefact the
whole estate embeds cannot be taken, modified, and shipped closed. Both permit
proprietary *use*; [`LICENSING.md`](LICENSING.md) states the rule, why the bot
differs, and the commercial licence for the one case MPL-2.0 does not cover.
