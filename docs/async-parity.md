# Async runtime parity

This document compares the surface of `lgwks_bot::rt` against the third-party
runtime crates a consumer would otherwise reach for, measured against their
current exported surfaces rather than against advertised feature lists.

Status: audited 2026-09-20 for the 0.4.0 release; the third-party surface was
re-pinned on 2026-10-02. Three gaps were found and closed; two more were closed by
#152 (virtual time and task introspection, both in a deliberately different shape
— §4.1), and the two that remain open are named in §4.

## 1. Method

`lgwks_bot::rt` is a **facade over tokio**, so "parity with tokio" is the wrong
frame: the engine *is* tokio, reached through the `lgwks_deps` storefront so
that no other crate authors a `tokio` edge. The question worth asking is which
parts of tokio's surface the facade **exposes**, because a capability the facade
does not re-export is one a consumer cannot use without breaking the
single-entry rule.

The audit therefore compared the exported surface, not the engines. `async-std`
and `smol` are listed for shape: both are alternatives a consumer might pick
instead of the facade entirely, so a capability they have and this does not is a
reason to leave.

The matrix is pinned to **tokio 1.53.1**, **tokio-util 0.7.19**, **async-std
1.13.2** and **smol 2.0.2** (published crate versions, accessed 2026-10-02), and
compares surfaces only — it is not a benchmark. `async-std` is deprecated by its
own crate metadata in favour of `smol`; it is listed here only as shape. The
virtual-clock path named in §4 leans on `bevy_time`; Bevy's latest stable release
at that date is **0.19.1** (`0.20.0-rc.2` is a pre-release).

## 2. Matrix

| Capability | tokio | async-std | smol | `lgwks_bot::rt` |
|---|---|---|---|---|
| Owned runtime + builder | ✅ | ✅ | ✅ | ✅ `Runtime`/`Builder`/`Handle` |
| `block_on` | ✅ | ✅ | ✅ | ✅ `rt::runtime::block_on` |
| Spawn a `Send` task | ✅ | ✅ | ✅ | ❌ **by design** — `Supervisor::spawn`: bounded by an in-flight ceiling and reported, and it returns no handle to drop |
| Spawn non-`Send` | ✅ `LocalSet` | ✅ `spawn_local` | ✅ | ❌ **by design**, and neither escape route is a workaround: a non-`Send` future is awaited inline, or handed to a **caller-owned single-threaded driver**, not to `join_all_bounded` or `Supervisor::spawn` (both require `Send + 'static`) |
| Structured task set | ✅ `JoinSet` | ◐ `spawn`/`JoinHandle`, no set type | ◐ `spawn`/`Task`, no set type | ✅ `rt::task::JoinSet` |
| Off-thread blocking call | ✅ `spawn_blocking` | ✅ | ✅ | ✅ `lgwks_std::task::spawn_blocking` — not in `rt`, and it returns a future for the result rather than a handle |
| `yield_now` | ✅ | ✅ | ✅ | ✅ |
| Cancellation token | ✅ `tokio-util` | ❌ | ❌ | ✅ **closed 0.4.0** |
| `sleep`/`timeout`/`interval` | ✅ | ✅ | ✅ | ✅ `rt::time` |
| Virtual / paused test clock | ✅ `pause`+`advance` | ❌ | ❌ | ✅ **closed 0.9.0** — `rt::clock`, a *declared* clock; see §4.1 |
| `mpsc`/`oneshot`/`broadcast`/`watch` | ✅ | ✅ | ✅ | ✅ `rt::sync` |
| Bounded `mpsc` | ✅ | ✅ | ✅ | ✅ |
| `Mutex`/`RwLock`/`Semaphore`/`Notify`/`Barrier`/`OnceCell` | ✅ | ✅ | ✅ | ✅ |
| Reader/writer traits, `BufReader`, `copy` | ✅ | ✅ | ✅ | ✅ **closed 0.4.0** |
| `fs` | ✅ | ✅ | ✅ | ✅ |
| `net` TCP/UDP/Unix + `lookup_host` | ✅ | ✅ | ✅ | ✅ |
| `process` | ✅ | ✅ | ✅ | ✅ `Command` to describe, `Supervisor::spawn_process` to run — bounded, killed as a process group, reported |
| Signal streams | ✅ | ✅ | ✅ | ✅ |
| `select!`/`join!`/`try_join!` | ✅ | ✅ | ✅ (futures-lite) | ✅ crate root |
| Runtime shutdown with timeout | ✅ | ❌ | ❌ | ✅ `shutdown_timeout` |
| Shutdown without waiting | ✅ `shutdown_background` | ❌ | ❌ | ❌ minor |
| Move a blocking call off a worker | ✅ `block_in_place` | ❌ | ❌ | ❌ minor |
| Task id / introspection | ✅ `task::id` | ❌ | ❌ | ✅ **closed 0.9.0** — `Supervisor::snapshot`; see §4.1 |
| `#[main]` / `#[test]` attribute | ✅ | ✅ | ❌ | ❌ **by design** |
| `tracing` integration | ✅ feature | ❌ | ❌ | ✅ via `lgwks_std::trace` |

◐ means the capability is present only in the weaker shape named. `async-std`
and `smol` return a spawned, awaitable handle (`task::spawn`; `Task`) but export
no `JoinSet` that owns and aborts the whole set, so their "structured task set"
cells are not full matches (`docs.rs/async-std/1.13.2`,
`docs.rs/smol/2.0.2`). `smol`'s public modules at 2.0.2 are `channel`, `fs`,
`future`, `io`, `lock`, `net`, `process` and `stream` — no cancellation token —
so its `CancellationToken` cell was corrected from ✅ to ❌; only `tokio-util`
ships one.

## 3. The capabilities that were closed, and why each mattered

### Spawning at all: the crate's own thesis decided it, twice

Every verb in `lgwks_bot` is deliberately **not** `Send`, so a domain may hold
thread-local state. That is why `BoxFuture` is unconstrained and why the crate
carries its one `#[allow(async_fn_in_trait)]`. `rt::task::spawn` required
`F: Future + Send`, so a future with the property the crate is built around had
nowhere to go; 0.4.0 answered that by exporting `LocalSet` and `spawn_local`.

That answer is withdrawn, and the reason generalises past it: a single-threaded
spawn returns the *same droppable handle* as the multi-threaded one. A caller
could start a local task and forget it exactly as easily, with the added trap
that the handle's type could not be named at the call site, so it could not even
be stored. The thesis — no verb here starts work and hands back a handle to it —
decides both.

What serves the thesis is *driving* instead of spawning: await the future
directly. `tests/rt_async_tier.rs::a_non_send_future_is_driven_rather_than_spawned`
polls a genuinely `!Send` future (`Rc<Cell<u8>>`) to completion on the calling
thread. Asserting the bound exists would not have proved anything; the test
compiles only because the future is not `Send`.

**The two escape routes this page used to recommend do not exist, and naming
them was a false claim about the API.** Both
[`rt::task::join_all_bounded`](https://docs.rs/lgwks_bot) and
`Supervisor::spawn` are declared `F: Future + Send + 'static` in this revision:
`join_all_bounded` internally spawns onto a `JoinSet`, and a `Supervisor` owns
its work as `Send` tasks. Neither accepts a borrowed or `!Send` future, so "run
it through `join_all_bounded`, or place it on a `Supervisor`" was advice that
does not compile for exactly the futures a `!Send` domain holds.

What actually remains today is: **await it on the task that owns it.** A
borrowed or non-`Send` future is driven inline, and any bounded fan-out over
such futures is a caller's own single-threaded driver. That driver has no
bounded-admission ceiling, no supervision and no structured reporting in this
crate — those are the properties `join_all_bounded` and `Supervisor` provide for
`Send` work, and they do not transfer. A composed borrowed/non-`Send` API with
those properties is the open front door under #87
(`docs/declarative-orchestration.spec.md`); it is **proposed, not shipped**, and
this table does not claim its behaviour. Until it lands, a consumer needing
bounded non-`Send` fan-out owns that machinery themselves.

### `CancellationToken`: the supervision rule named a type that did not exist

The supervision rule requires that every background task be tracked in a
`JoinSet` or supervisor and listen to a `CancellationToken`. The `JoinSet` half
shipped; the token was absent from the workspace entirely. A rule that names a
type the caller cannot obtain is enforceable against the wrong practice and
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
by a spawned task silently became uncancellable, the one failure a cancellation
primitive must not have.

### `rt::io`: the drivers could not be read from

Absent entirely: no `AsyncRead`/`AsyncWrite`, no `BufReader`, no `copy`. A
consumer could open a socket or a pipe and still not read from it without naming
`tokio`, which is the thing the facade exists to prevent. This one needed a
storefront change, because `io-util` was reachable only by enabling the entire
networking stack: `tokio-io` is now its own rung and `tokio-net` builds on it.

## 4. The two that remain open

These are stated rather than left implied, because a capability assumed to
exist costs a search that cannot succeed.

1. **`block_in_place`.** For CPU-bound work inside an async task on a
   multi-thread runtime. `lgwks_std::task::spawn_blocking` covers the common
   case; this covers the case where the future cannot be `'static`.
2. **`shutdown_background`.** `shutdown_timeout` exists; the non-waiting form
   does not.

## 4.1 What "closed" means for virtual time and introspection (#152)

`tokio::time::pause`/`advance` work by replacing the engine's timer driver.
This crate does not, and the difference is the point rather than a gap.

**What shipped.** `rt::clock::Clock` is a *declared* clock: `Clock::wall`
follows real time, `Clock::virtual_at` is driven by the caller, and
`rt::time::Deadline` names the clock that governs a deadline instead of an
opaque `Instant`. `Supervisor::snapshot` reads the supervisor's own admission
and reporting fields for a bounded view of live capacity and the next eligible
action.

**Why not the engine's `pause`.** It would require enabling `tokio/test-util`,
which is a new feature edge on the `tokio` edge this workspace admits by exact
allowlist (`contract/APPROVED.toml`, INV-DEP-3) — not a decision a parity review
makes on its own. And it would be the *wrong* shape for this crate: `pause`
swaps the engine's timer and leaves every `std::time::Instant` reading in the
program still on real time. This crate already has wall-clock budgets
(`Supervisor`'s cooperative drain grace, a process deadline), so a "paused"
runtime would still hang on any of them. A declared clock names which clock
governs each deadline and keeps the watchdog separate — the property
`INV-BOT-30` states and `tests/sim_clock.rs` proves.

**What this does not claim.** A declared clock determinizes *which deadline is
eligible*. It does not make poll order across workers deterministic, it does not
make an external system deterministic, and it does not measure cross-host clock
skew. Those are named in `rt::clock`'s module docs as explicitly out of scope.

**The remaining gap, stated.** There is no way to advance the *engine's* timer
from the public surface. A test that needs a real `sleep` to resolve without a
real wait still waits. That is the honest residual of not taking a new
dependency edge, and it is the one thing a future admission of `tokio/test-util`
would buy.

## 5. Three deliberate divergences

- **No `#[main]`/`#[test]` attribute macro.** A re-exported proc-macro expands
  to `::tokio` paths a consumer without a `tokio` edge cannot resolve, so it
  would compile only for consumers who had already broken the single-entry rule.
  Entry is `Runtime::block_on`.
- **`rt::time::sleep` defers construction to first poll.** tokio's own `sleep`
  captures the reactor at construction and panics with *"there is no reactor
  running"* when built outside a runtime, the most common footgun in the
  library. The wrapper makes `sleep(d)` constructible anywhere, which is a
  divergence in behaviour and a strict improvement in ergonomics. `interval` is
  the exception, because it is a value rather than a future and must still be
  built inside a runtime; the module documents that.
- **Supervised execution is stricter than the ecosystem's, not wider.**
  `rt::supervise::Supervisor` has no direct counterpart in §2. `tokio_util`'s
  `TaskTracker` tracks tasks and lets a caller wait for them, but it does not
  bound how many run at once, does not refuse when full, and carries no loop
  budget. Requiring a `Budget` before a loop can be written is a tighter
  contract than any runtime here imposes, and that is the point: the rule in
  this workspace is that no background task is untracked and no loop is
  unbounded. A consumer who wants the looser model still has a bare `JoinSet`,
  which tracks what it starts and aborts on drop, but imposes no ceiling of its
  own.

## 5a. What "supervised" guarantees, and the five things it does not

The phrase "cannot express an untracked task or an unbounded loop" is about the
**API surface**, and it is true there. Read as a runtime claim it would be false,
so the boundary is stated explicitly. Five distinct facts are easy to collapse
and are kept apart:

1. **Tracked lifetime ≠ termination.** Every supervised task is registered and
   reported. Registration is a fact about bookkeeping, not about the task
   having stopped. `TaskId` tells you what was tracked.
2. **A cancellation *request* ≠ actual termination.** A cancellation token is a
   signal a task observes at a poll boundary. Nothing forces a task to observe
   it.
3. **A non-yielding callback is not preemptible.** A `Future` that never
   returns `Pending`, or a closure that never yields, holds its thread until it
   finishes on its own terms. This runtime is cooperative; there is no
   preemption here, and shutdown **waits** for such work rather than
   interrupting it. `shutdown_timeout` bounds how long shutdown *waits*, and a
   timed-out shutdown leaves the task running rather than terminating it.
4. **Bounded APIs are bounded where they say they are.** `join_all_bounded`
   bounds concurrent tasks by `limit`, and the `Supervisor` refuses past its
   in-flight ceiling. The **low-level `rt::task::JoinSet` has no capacity
   ceiling at all** — it tracks and aborts on drop, and a consumer can insert
   unboundedly many tasks into it. A bound on one API is not a property of the
   runtime.
5. **Progress detail can be dropped; terminal state is authoritative.** The
   per-task progress buffer is bounded and overflows *by design*: counters
   increment and the retained detail is the most recent window, while the
   terminal/Unknown state of a task stays retrievable. "The progress log is
   bounded" is not "the task's outcome is bounded".

So the accurate sentence is: *this crate offers a supervised API whose declared
ceilings are real, and which cannot express an untracked task; it does not
guarantee that a supervised task terminates when asked, and it makes no
preemption claim.* Any stronger reading — that a bounded API cannot leak, that
cancellation always wins, that shutdown implies termination — is not what the
code does.

## 6. What parity is not claimed

Parity with tokio's *engine* is not the claim and would be meaningless. The
engine is tokio. Parity with its *surface* is claimed for the matrix in §2 and
only there. The facade is narrower on purpose: `rt::fs` runs on the blocking
pool rather than a true async file API, and `rt::net` reaches the network
directly rather than through the workspace's HTTP egress policy. Both are stated
in the module docs, where a consumer will read them, and not only here.
