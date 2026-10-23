# lgwks_bot

Pages of this README: [overview](README.md) · [execution model](README-execution.md) · [failure](README-failure.md) · [authority, domains and specs](README-authority.md) · [script!](README-script.md) · [async runtime](README-runtime.md) · [supervision](README-supervision.md)

[![crates.io](https://img.shields.io/crates/v/lgwks_bot.svg)](https://crates.io/crates/lgwks_bot)
[![docs.rs](https://img.shields.io/docsrs/lgwks_bot)](https://docs.rs/lgwks_bot)

Orchestration you cannot write wrongly: bounded fan-out, deadlines, retries under
a shared budget, tenant-keyed steps and a map of what a run declared.

```sh
cargo add lgwks_bot
```

The supported interface is `script!`, the `try_join!` macro and the `script`
module they expand into. [README-script.md](README-script.md) has a complete
flow and what every flow gets without asking, and
[docs.rs/lgwks_bot](https://docs.rs/lgwks_bot) has each primitive with a
runnable example.

## What a flow rules out

| Block | What it rules out |
|---|---|
| `each x in xs:` | unbounded fan-out; lost results; siblings left running after a failure |
| `within 2s:` | a wait with no deadline; a wait that ignores cancellation |
| `retry up to 3 times, waiting 100ms:` | retry storms; retrying a permanent failure; a new identity per attempt |
| `together:` | sequential awaits that should overlap; a failed branch that keeps running |
| `step name:` | anonymous work with no stable key or error location |
| `await ready(&db, 30s)` | a readiness guessed by sleeping; a poll loop |
| `admit the model's plan:` | an untrusted payload becoming an instruction |

## Features

| Feature | Default | Enables |
|---|---|---|
| `rt`, `time`, `sync`, `macros` | on | the orchestration runtime surface, driven through `lgwks_deps` |
| `script` | on | `script!` and the `script` module |
| `io`, `net`, `fs`, `process`, `signal` | off | the matching runtime drivers |
| `inspect` | off | structural code inspection through `lgwks_ast` |
| `ephemeral`, `profile` | off | run identities from `lgwks_std::random`; the per-stage profiler |
| `full` | off | every feature above |

## Relationship to the driver

It is the runtime. The driver behind it is owned through the
`lgwks_deps` storefront, so a consumer never names it directly. What the driver
does not give you is the layer above: what to bound, what to retry, which tenant
a step belongs to and where a failure happened.

This is **not a sandbox**. In-process code can always dial out directly, and
capability gating is auditable authority, not isolation.

## Stability

Everything outside `script!`, `try_join!` and the `script` module is
`#[doc(hidden)]` support code with no compatibility promise. Which release
carries a symbol is in [`CHANGELOG.md`](../../CHANGELOG.md), with the 3.0.0
migration notes under [`docs/changelog/`](../../docs/changelog). The minimum
supported Rust version is 1.98.

## License

MPL-2.0 — Copyright 2026 Logical Works Incorporated

Deliberately not the workspace's Apache-2.0. The bot is the artefact the rest of
the estate embeds, so it carries file-level copyleft while its siblings stay
permissive. A proprietary consumer is still permitted — MPL-2.0 §3.3 — and
only modification of the MPL-covered files carries an obligation. See
[`LICENSING.md`](../../LICENSING.md).
