# lgwks_bot

Pages of this README: [overview](README.md) · [execution model](README-execution.md) · [failure](README-failure.md) · [authority, domains and specs](README-authority.md) · [script!](README-script.md) · [async runtime](README-runtime.md) · [supervision](README-supervision.md)

[![crates.io](https://img.shields.io/crates/v/lgwks_bot.svg)](https://crates.io/crates/lgwks_bot)
[![docs.rs](https://img.shields.io/docsrs/lgwks_bot)](https://docs.rs/lgwks_bot)

Orchestration with the common failure modes removed: bounded fan-out, deadlines, retries under
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

## Architecture: one ECS world, one driver

[ADR 0006](https://github.com/srinji-kaggss/logicalworks-crates/blob/HEAD/docs/adr/0006-one-ecs-world-one-tokio-axum-driver.md)
fixes the shape. One ECS world is the core: a source moving, a run admitted or
settled, a timer firing is an event written to it, and effects leave through
the journal. One driver on tokio and axum sits below the world, used only where
rebuilding the piece makes no sense, and a consumer never names it: it is owned
through the `lgwks_deps` storefront. `script!` is the framework above the world
that an agent or a developer writes in.

The boundary is a test, not a convention: `tests/it/core_boundary.rs` fails the
build if a core module names the driver or a file outside `ecs/` names bevy.
The `Estate`, `Host` and `Supervisor` still build their own runtimes in 4.0.0;
ADR 0006 R2 and R3 move them onto the one driver and into the world.

This is **not a sandbox**. In-process code can always dial out directly, and
capability gating is auditable authority, not isolation. A flow's `Tenant` is
its trusted host's partition key rather than an authenticated identity.

## Stability

Everything outside `script!`, `try_join!` and the `script` module is
`#[doc(hidden)]` support code with no compatibility promise. Which release
carries a symbol is in [`CHANGELOG.md`](../../CHANGELOG.md), with the 3.0.0
migration notes under [`docs/changelog/`](../../docs/changelog). The minimum
supported Rust version is 1.98.

## Boundaries and failure

Async-client limits are in [distributed-mesh boundaries](../../docs/distributed-boundaries.md); background bounds in [bounded background work](../../docs/guides/lgwks-bot/background-work.md); tick failures in [what a failed tick means](../../docs/guides/lgwks-bot/failures.md).

## Publisher

Logical Works Incorporated publishes `lgwks_bot` as one of five independently versioned crates in [logicalworks-crates](https://github.com/srinji-kaggss/logicalworks-crates#readme): `lgwks_std`, `lgwks_bot`, `lgwks_ast`, `lgwks_macros` and `lgwks_deps`.
[Should I use logicalworks-crates?](https://github.com/srinji-kaggss/logicalworks-crates/blob/HEAD/docs/should-i-use.md) says when to reach for each one and when to skip it.

## License

MPL-2.0 — Copyright 2026 Logical Works Incorporated

Deliberately not the workspace's Apache-2.0. The bot is the artefact the rest of
the estate embeds, so it carries file-level copyleft while its siblings stay
permissive. A proprietary consumer is still permitted — MPL-2.0 §3.3 — and
only modification of the MPL-covered files carries an obligation. See
[`LICENSING.md`](../../LICENSING.md).
