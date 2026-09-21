# Async runtime parity

What `lgwks_bot::rt` offers against the three Rust runtimes a consumer would
otherwise reach for, measured against their current surfaces rather than
against a feature list either of them advertises.

Status: audited 2026-09-20 for the 0.4.0 release. Three gaps were found and
closed; four remain open and are named in §3.

## 1. Method

`lgwks_bot::rt` is a **facade over tokio**, so "parity with tokio" is the wrong
frame — the engine *is* tokio, reached through the `lgwks_deps` storefront so
that no other crate authors a `tokio` edge (`INV-DEP-EDGE-OWNED`). The question
worth asking is which parts of tokio's surface the facade **exposes**, because a
capability the facade does not re-export is one a consumer cannot use without
breaking the single-entry rule.

The audit therefore compared the exported surface, not the engines. `async-std`
and `smol` are listed for shape: both are alternatives a consumer might pick
instead of the facade entirely, so a capability they have and this does not is a
reason to leave.

## 2. Matrix

| Capability | tokio | async-std | smol | `lgwks_bot::rt` |
|---|---|---|---|---|
| Owned runtime + builder | ✅ | ✅ | ✅ | ✅ `Runtime`/`Builder`/`Handle` |
| `block_on` | ✅ | ✅ | ✅ | ✅ `rt::runtime::block_on` |
| `spawn` (`Send`) | ✅ | ✅ | ✅ | ✅ `rt::task::spawn` |
| Spawn non-`Send` | ✅ `LocalSet` | ✅ `spawn_local` | ✅ | ✅ **closed 0.4.0** |
| Structured task set | ✅ `JoinSet` | ✅ | ✅ | ✅ `rt::task::JoinSet` |
| `spawn_blocking` | ✅ | ✅ | ✅ | ✅ |
| `yield_now` | ✅ | ✅ | ✅ | ✅ |
| Cancellation token | ✅ `tokio-util` | ❌ | ✅ | ✅ **closed 0.4.0** |
| `sleep`/`timeout`/`interval` | ✅ | ✅ | ✅ | ✅ `rt::time` |
| Virtual / paused test clock | ✅ `pause`+`advance` | ❌ | ❌ | ❌ **open** |
| `mpsc`/`oneshot`/`broadcast`/`watch` | ✅ | ✅ | ✅ | ✅ `rt::sync` |
| Bounded `mpsc` | ✅ | ✅ | ✅ | ✅ |
| `Mutex`/`RwLock`/`Semaphore`/`Notify`/`Barrier`/`OnceCell` | ✅ | ✅ | ✅ | ✅ |
| Reader/writer traits, `BufReader`, `copy` | ✅ | ✅ | ✅ | ✅ **closed 0.4.0** |
| `fs` | ✅ | ✅ | ✅ | ✅ |
| `net` TCP/UDP/Unix + `lookup_host` | ✅ | ✅ | ✅ | ✅ |
| `process` | ✅ | ✅ | ✅ | ✅ |
| Signal streams | ✅ | ✅ | ✅ | ✅ |
| `select!`/`join!`/`try_join!` | ✅ | ✅ | ✅ (futures-lite) | ✅ crate root |
| Runtime shutdown with timeout | ✅ | ❌ | ❌ | ✅ `shutdown_timeout` |
| Shutdown without waiting | ✅ `shutdown_background` | ❌ | ❌ | ❌ minor |
| Move a blocking call off a worker | ✅ `block_in_place` | ❌ | ❌ | ❌ minor |
| Task id / introspection | ✅ `task::id` | ❌ | ❌ | ❌ minor |
| `#[main]` / `#[test]` attribute | ✅ | ✅ | ❌ | ❌ **by design** |
| `tracing` integration | ✅ feature | ❌ | ❌ | ✅ via `lgwks_std::trace` |

## 3. The three that were closed, and why each mattered

### Non-`Send` spawning — the crate's own thesis was unusable

Every verb in `lgwks_bot` is deliberately **not** `Send`, so a domain may hold
thread-local state. That is why `BoxFuture` is unconstrained and why the crate
carries its one `#[allow(async_fn_in_trait)]`. But `rt::task::spawn` requires
`F: Future + Send`, so there was no way to spawn a future with the property the
crate is built around — the thesis held for `tick` and evaporated the moment a
consumer spawned anything.

`LocalSet` and `spawn_local` are now exported, and
`tests/rt_async_tier.rs::a_local_set_runs_a_task_that_is_not_send` drives a
genuinely `!Send` future (`Rc<Cell<u8>>`) through it. Asserting the bound exists
would not have proved anything; the test compiles only because the future is
not `Send`.

### `CancellationToken` — the estate's rule named a type that did not exist

`AGENTS.md`: *"Every background task must be tracked in a `JoinSet` or
supervisor and listen to a `CancellationToken`."* The `JoinSet` half shipped.
The token was absent from the estate entirely, so the rule told a reader to use
something they could not obtain — enforceable against the wrong practice,
unenforceable in favour of the right one.

It is written in-crate on `tokio::sync::watch` rather than admitted from
`tokio-util`, because the storefront edge would exist to carry one type. `watch`
was chosen over `Notify` deliberately: `notify_waiters` wakes only the waiters
registered *at that instant*, so a `cancelled()` future created a microsecond
after `cancel()` would hang forever. `watch` carries the state as a value, so
there is no window.

The parent link points **up**. The first implementation linked down with `Weak`
children, and `a_descendant_survives_its_intermediate_parent_being_dropped`
caught it: dropping a mid-chain token orphaned its descendants, so a leaf held
by a spawned task silently became uncancellable — the one failure a cancellation
primitive must not have.

### `rt::io` — the drivers could not be read from

Absent entirely: no `AsyncRead`/`AsyncWrite`, no `BufReader`, no `copy`. A
consumer could open a socket or a pipe and still not read from it without naming
`tokio`, which is the thing the facade exists to prevent. This one needed a
storefront change, because `io-util` was reachable only by enabling the entire
networking stack: `tokio-io` is now its own rung and `tokio-net` builds on it.

## 4. The four that remain open

Stated rather than implied, because an agent that assumes one of these exists
will spend a day looking for it.

1. **Virtual time.** `tokio::time::pause`/`advance` let a test drive timers
   without wall-clock waits. `rt::time` does not expose them, so a test of a
   timed bot waits in real time. This is the one gap with a known estate path:
   `docs/bevy-admission.md` §5 already adopted `bevy_time` for exactly this, and
   `Time<Virtual>` is the intended clock root — `docs/bot-on-ecs.md` §10 step 4.
   Until then the gap is real.
2. **`block_in_place`.** For CPU-bound work inside an async task on a
   multi-thread runtime. `spawn_blocking` covers the common case; this covers
   the case where the future cannot be `'static`.
3. **`shutdown_background`.** `shutdown_timeout` exists; the non-waiting form
   does not.
4. **Task introspection** (`task::id`, per-task metrics). Nothing in the estate
   needs it yet.

## 5. Two deliberate divergences

- **No `#[main]`/`#[test]` attribute macro.** A re-exported proc-macro expands
  to `::tokio` paths a consumer without a `tokio` edge cannot resolve, so it
  would compile only for consumers who had already broken the single-entry rule.
  Entry is `Runtime::block_on`.
- **`rt::time::sleep` defers construction to first poll.** tokio's own `sleep`
  captures the reactor at construction and panics with *"there is no reactor
  running"* when built outside a runtime — the most common footgun in the
  library. The wrapper makes `sleep(d)` constructible anywhere, which is a
  divergence in behaviour and a strict improvement in ergonomics. `interval` is
  the exception, because it is a value rather than a future and must still be
  built inside a runtime; the module documents that.

## 6. What parity is not claimed

Parity with tokio's *engine* is not the claim and would be meaningless — the
engine is tokio. Parity with its *surface* is claimed for the matrix in §2 and
only there. The facade is narrower on purpose: `rt::fs` runs on the blocking
pool rather than a true async file API, and `rt::net` reaches the network
directly rather than through the estate's HTTP egress policy. Both are stated in
the module docs where a consumer will read them, not only here.
