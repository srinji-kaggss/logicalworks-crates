# lgwks_bot: the async orchestration runtime and the other crates

Pages of this README: [overview](README.md) · [execution model](README-execution.md) · [failure](README-failure.md) · [authority, domains and specs](README-authority.md) · [script!](README-script.md) · [async runtime](README-runtime.md) · [supervision](README-supervision.md)

## Async orchestration runtime

`lgwks_bot` is the asynchronous orchestration runtime (default feature
`rt`). The driver edge appears in `contract/APPROVED.toml` with
`owner = "lgwks_deps"`, and `lgwks-deps check` refuses any other workspace crate
that declares it directly (`INV-DEP-EDGE-OWNED`). `--no-default-features`
is a narrow test build with no driver at all, not the supported surface.

On `wasm32-wasip1`, the `rt` feature uses a current-thread runtime;
`Builder::worker_threads(Some(_))` returns `Unsupported`. The native-only
`net`/`process`/`fs`/`signal` driver features are outside the WASM domain.

| Module | Feature | Contents |
|---|---|---|
| `rt::runtime` | `rt` | `Runtime`, `Builder`, `Handle`, free `block_on` |
| `rt::task` | `rt` / `sync` | `JoinSet`, `JoinError`, `AbortHandle`, `yield_now`; `join_all_bounded` requires `sync`. No verb here starts a task and hands back a handle to it |
| `rt::time` | `time` | `sleep`, `sleep_until`, `timeout`, `timeout_at`, `interval`, `Instant`, `Elapsed` |
| `rt::sync` | `sync` | `CancellationToken`, `mpsc`, `oneshot`, `broadcast`, `watch`, `Mutex`, `RwLock`, `Semaphore`, `Notify`, `Barrier`, `OnceCell` |
| `rt::io` | `io` | `AsyncRead`/`AsyncWrite`/`AsyncBufRead` and their extensions, `BufReader`, `BufWriter`, `duplex`, `copy` |
| `rt::net` | `net` | `TcpListener`, `TcpStream`, `UdpSocket`, `lookup_host`; Unix `UnixListener`, `UnixStream`, `UnixDatagram`; the split halves at the driver's own paths, `rt::net::tcp::{OwnedReadHalf, OwnedWriteHalf, ReadHalf, WriteHalf, ReuniteError}` and the same five under `rt::net::unix` |
| `rt::process` | `process` | `Command` — describing what to run, with `StdioPolicy::Capture` for a bounded stream. Running it is `Supervisor::spawn_process` (reported later) or `Supervisor::run_process` (returns its output); `Child` and its pipes are not exported |
| `rt::fs` | `fs` | async filesystem (blocking-threadpool wrapper) |
| `rt::signal` | `signal` | OS signal streams (Unix/Windows) |

`select!`, `join!`, and `try_join!` are re-exported at the crate root. They are
the spelling for branches already owned by the awaiting task; work that must
outlive the call belongs on a `Supervisor` or in `join_all_bounded`, never in a
bare combinator. There is deliberately **no attribute macro**: a re-exported
proc-macro would expand to driver paths a consumer without a driver edge cannot
resolve. Enter through `Runtime::block_on` instead.

### Cancellation

`rt::sync::CancellationToken` is provided by this crate, built on
`tokio::sync::watch` rather than re-exported from `tokio-util`. A child token is cancelled when its
parent is; cancelling a child never touches the parent. The parent link points
*up*, so a token you hold can always be cancelled even if an intermediate token
in its chain has been dropped.

This completes the background-work contract the crate already enforced: every
task is tracked in a `JoinSet` and observes a `CancellationToken`.

`a_cancelled_token_stops_the_task_a_join_set_owns`
(`crates/lgwks-bot/tests/it/rt_front_door.rs`) runs it: a worker awaits
`cancelled_owned()` on a clone of the token, the token is cancelled, and
`JoinSet::join_next` drains the worker.

### Bounded fan-out

`a_bounded_fan_out_runs_every_input` (`crates/lgwks-bot/tests/it/rt_front_door.rs`)
runs sixteen inputs through `join_all_bounded(4, …)`, four at a time, and gets
sixteen results.


## The other crates

Five crates ship from this repository — the four in the table below plus
`lgwks_macros`, a proc-macro crate carrying the syntax of `lgwks_bot::script!`
which exists only because Rust requires a proc macro to live in its own crate.
It is not a surface of its own; use it through `lgwks_bot`. They share a
release process, not a dependency graph: `lgwks_bot` and `lgwks_deps` depend on
`lgwks_std` (unconditionally, plus an optional `lgwks_macros` behind `bot`'s
`script` feature), and `lgwks_ast` stands alone.

| Crate | What it gives you |
|---|---|
| [`lgwks_std`](https://docs.rs/lgwks_std) | Everyday primitives with no async runtime required: codecs, a blocking HTTP client, retry, default structured debugging, time, hashing, ids |
| [`lgwks_ast`](https://docs.rs/lgwks_ast) | Parse many languages into one AST type, with bounded traversal and typed diagnostics |
| [`lgwks_deps`](https://docs.rs/lgwks_deps) | The audited storefront for third-party stacks. The runtime's driver is selected through it, so this crate's driver edge is the workspace's only one |

The [repository README](https://github.com/srinji-kaggss/logicalworks-crates#readme)
indexes the design documents.
