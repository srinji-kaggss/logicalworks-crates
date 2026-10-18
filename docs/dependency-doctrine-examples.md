# Dependency doctrine: worked examples

Part of the [dependency doctrine](dependency-doctrine.md). This page is §4 and §5.

## 4. Worked example: `tokio`

Two tiers share one owned engine.

**Synchronous tier: `lgwks_std::task` (feature `core`, zero dependencies).**
Its header states the scope: `block_on`, `join_all`, `join_all_boxed`, and
`spawn_blocking` replace `futures`, `pollster`, and `async-trait` **for
applications that only need to await futures, await a bounded set of them
together, and keep blocking work off the driving thread.** No reactor, no
worker pool. The shape is `block_on` over a future, and `block_on(join_all(..))`
over a vector of `spawn_blocking` closures that each read one file.

`lgwks_bot` builds on this: `Bot::tick` polls every source in bounded
concurrent waves (`MAX_IN_FLIGHT_POLLS = 32`) and fires actions sequentially.
The tick executor itself uses no `futures` or `async-trait`; its synchronous
path remains `lgwks_std::task`. The bot's separate runtime feature reaches
tokio through the `lgwks_deps` storefront.

**Async tier: `lgwks_bot` (the async and runner surface of this workspace).** Timers, wakeable
channels, async sockets, and a multi-threaded worker pool cannot be built on
`std` without an I/O reactor and `unsafe`, and `lgwks_std` forbids both.
`lgwks_bot` therefore wraps **tokio core** through the `lgwks_deps` storefront,
and `tokio` is registered in `contract/APPROVED.toml` with
`owner = "lgwks_deps"`. The gate refuses any other workspace crate that authors
a `tokio` edge, so this facade is the single entry. The shape is a
`lgwks_bot::Runtime` whose `block_on` awaits
`lgwks_bot::rt::task::join_all_bounded(4, ..)` over 64 futures that each
`sleep` one millisecond (from `lgwks_bot::rt::time`) and return their index.

What `lgwks_bot` adds over importing `tokio` directly:

- **A curated surface.** tokio compiles `default-features = false` with
  `rt` always on and `rt-multi-thread` added only on native targets; `time`,
  `sync`, `net`, `process`, `fs`, and `signal` are opt-in features. No
  accidental feature pull-in.
- **Bounded fan-out.** `join_all_bounded(limit, futures)` (feature `sync`) runs every input,
  never exceeds `limit` in flight, returns results in input order, and surfaces
  a panicking input by resuming its panic on the awaiting task instead of
  aborting the process. A raw
  `JoinSet` gives completion order and no ceiling.
- **Timers safe to build early.** tokio's `sleep`/`timeout` capture the runtime
  at construction and panic before the runtime is entered; the
  `lgwks_bot::rt::time` wrappers defer construction to first poll.

**Boundary.** `lgwks_bot` is not a realtime scheduler, and worker-thread
completion order is nondeterministic, only `join_all_bounded` result order is
guaranteed. `net` sockets do not pass the HTTP egress policy; a caller applies
policy before connecting. `fs` operations occupy a blocking thread (not
io_uring). A hard-realtime or io_uring workload is outside the domain of these
crates and must be declared as a boundary rather than added as a dependency. On `wasm32-wasip1`, the
`rt` feature uses a current-thread runtime; supplying `worker_threads` is an
explicit `Unsupported` error, and Tokio's native-only driver features are
outside the WASM domain.

## 5. Worked example: `syn`

Two distinct jobs get confused under "parsing Rust":

- **Structural / multi-language parsing** (language ID, grammar selection,
  bounded AST walk): `lgwks_ast`. It re-exports the ast-grep types as
  `Parsed`/`AstNode` and compiles exactly the grammars you select. Consumers do
  not depend on `ast-grep` directly.
- **Rust source scanning inside the gate** (the zero-gate detectors):
  `lgwks_deps::scan`, which is the one reviewed exception that depends on
  `syn`/`proc-macro2` (optional, behind the default-on `scan` feature). A real
  Rust grammar is the only honest oracle for Rust source, and a parser is not a
  data format the gate polices, so it is not self-refuting.

**Boundary:** the workspace provides no general-purpose Rust semantic API
(types, trait resolution, macro expansion), because nothing here requires one. A
second Rust parser is not admitted. Structural queries route through
`lgwks_ast::Language::Rust`; a caller that genuinely requires `syn` for semantic
work states that reason and registers it.
