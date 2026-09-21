# lgwks_bot features

Every feature here is declared in `crates/lgwks-bot/Cargo.toml`. Read the
section that matches the build you want, because two of these features have a
consequence the name does not carry.

## Defaults

```toml
default = ["rt", "time", "sync", "macros"]
```

That is the `cargo add lgwks_bot` build. It pulls tokio through the `lgwks_deps`
storefront, and `lgwks_deps` is the only crate in the workspace that authors a
`tokio` edge.

## The full table

| Feature | Requires | Adds |
|---|---|---|
| `rt` | | `rt::runtime` (`Runtime`, `Builder`, `Handle`, `block_on`), `rt::task` |
| `time` | `rt` | `rt::time`: `sleep`, `sleep_until`, `timeout`, `timeout_at`, `interval`, `Instant`, `Elapsed` |
| `sync` | `rt` | `rt::sync`: `CancellationToken`, `mpsc`, `oneshot`, `broadcast`, `watch`, `Mutex`, `RwLock`, `Semaphore`, `Notify`, `Barrier`, `OnceCell`; and `rt::supervise` |
| `macros` | `rt` | the `join!`, `select!`, and `try_join!` re-exports at the crate root |
| `io` | `rt` | `rt::io`: `AsyncRead`, `AsyncWrite`, `AsyncBufRead`, `BufReader`, `BufWriter`, `duplex`, `copy` |
| `net` | `io` | `rt::net`: `TcpListener`, `TcpStream`, `UdpSocket`, `lookup_host` |
| `process` | `io`, `sync` | `rt::process`: `Command` — how to describe a child. Running one is `Supervisor::spawn_process` (`sync`), and it is the only way: `Child` and its pipes are deliberately not exported |
| `fs` | `io` | `rt::fs`: an async filesystem, a blocking-threadpool wrapper |
| `signal` | `rt` | `rt::signal`: OS signal streams. Compiled only on `unix` or `windows` (`crates/lgwks-bot/src/rt/mod.rs:96`) |
| `full` | | `rt`, `time`, `sync`, `macros`, `io`, `net`, `process`, `fs`, `signal` |

The module gates are in `crates/lgwks-bot/src/rt/mod.rs:85`. `rt::supervise` and
`rt::sync` are both behind `sync`, so disabling `sync` removes the supervisor as
well as the channels.

## `--no-default-features` does not remove the ECS substrate

`crates/lgwks-bot/Cargo.toml` carries `bevy_ecs` as an unconditional edge through
the storefront:

```toml
lgwks_deps = { workspace = true, default-features = false, features = ["bevy-ecs"] }
```

The manifest comment next to it gives the reason: the ECS substrate is how a bot
executes, "so it is not a candidate the caller may decline." A no-default build
still compiles the schedule, `Bot`, and all four verbs. What it removes is tokio
and every `rt` driver, leaving `lgwks_std::task` as the executor that drives the
verbs. If you were reaching for `--no-default-features` to keep bevy out of your
tree, this crate cannot do that, and no feature flag changes it.

`lgwks_bot` also depends on `lgwks_std` with the `json` and `http` features
enabled, because `BotSpec` serializes through the JSON facade and `domain::net`
reads through the HTTP client. Those two edges are not optional either.

## `io` is not implied by `rt`

A consumer reading from a pipe or a file needs `AsyncRead` without a socket
layer, so `io` is its own feature. `net`, `process`, and `fs` each require `io`
rather than the reverse. Selecting `rt` alone gives you `JoinSet`,
`join_all_bounded` (with `sync`), and `block_on`, and no reader or writer traits.
`process` additionally requires `sync`, because `Command` describes a child and
`Supervisor::spawn_process` is what runs it: a `process` build without `sync`
would ship the description with its only remaining runner being `Command::spawn`,
which this workspace bans.

## Limits by feature

**`rt` is not a scheduler with realtime guarantees.** `crates/lgwks-bot/src/rt/mod.rs:75`
states the bound: future completion order across worker threads is not
deterministic, and only the result order of `join_all_bounded` is.

**Worker-thread count is bounded.** `rt::runtime::MAX_WORKER_THREADS` is 1024,
and `Builder::worker_threads` rejects a count above it with an error rather than
clamping silently (`crates/lgwks-bot/src/rt/runtime.rs:105`).

**WASM has one thread.** On `target_family = "wasm"` the runtime is built with
`new_current_thread`, and `Builder::worker_threads(Some(_))` returns
`io::ErrorKind::Unsupported` (`crates/lgwks-bot/src/rt/runtime.rs:102`).

**Signals need an OS.** `rt::signal` compiles only where `unix` or
`windows` holds, so a `full` build on another target silently lacks it.

## Features on the storefront side

`lgwks_bot`'s tokio edge is a storefront feature on `lgwks_deps`, not a
dependency it declares itself. If you need raw tokio without the bot facade, the
[deps guide](../lgwks-deps/index.md) covers the `tokio*` features and the
`default-features = false` step that keeps the gate tool's Rust parser out of
your runtime graph.
