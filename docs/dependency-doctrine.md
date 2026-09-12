# Dependency doctrine — how the estate avoids a million crates

This is the document an agent reads before typing `cargo add`. It exists
because the default failure of an AI coding agent in a Rust repo is to reach
for the crate it saw most often in training — `tokio`, `syn`, `serde_json`,
`regex`, `uuid`, `chrono`, `walkdir`, `reqwest` — even when the code in front
of it already has a first-party equivalent. Every added edge is supply-chain
surface, a build-time cost, and a license and advisory obligation. The estate
pays for a small number of those on purpose, and refuses the rest.

The rule, in one line: **if the capability is in `std`, `lgwks_std`,
`lgwks_bot`, or `lgwks_ast`, use it; a new third-party dependency is an
optional, feature-gated edge of `lgwks_deps`, registered with a reason and
never built unless the end user selects it.**

## 0. The deps law (Director, 2026-09-12)

Cognitive load is `std` + `bot` + `deps`, plus the standalone `ast`:

- `lgwks_std` — the std+ core.
- `lgwks_bot` — async, runners, actors.
- `lgwks_deps` — the **storefront**: install it, select features, get deps.
- `lgwks_ast` — standalone, grandfathered.

A new dependency is registered with `owner = "lgwks_deps"` and exposed as an
optional feature of `lgwks_deps` (for example `--features gpui`). Storefront
capability features are default-off; the reviewed `scan` gate-tool feature is
the explicit default-on exception. `lgwks_std`'s core feature stack and
`lgwks_ast`'s parser are grandfathered. `lgwks_std` cannot route through
`lgwks_deps` because `lgwks_deps` depends on `lgwks_std`; the reverse would be
a cycle.

## 1. The admission ladder

`cargo run -p lgwks_deps -- tiers` prints the rungs. Lowest rung first; each
step up is an escalation that needs a stated reason:

| # | Rung | Meaning |
|---|------|---------|
| 1 | `std` / `core` / `alloc` | Preferred unconditionally. Zero supply-chain cost. |
| 2 | `lgwks_std` | The estate substrate: hex, base64, time, id, hash, glob, pattern, json, ron, wire, fs, task, leb128, encoding, http, online. |
| 3 | `lgwks_bot` | Async, runners, and actor roles, including the curated runtime surface. |
| 4 | `lgwks_deps` storefront | Every other third-party capability, as an optional feature the end user selects. |
| 5 | ELIMINATE | The capability belongs in `lgwks_std`/std — write the minimal zero-dep implementation there. |
| 6 | CONSOLIDATE | Several crates solve the same problem — pick one, retire the rest. |
| 7 | VENDOR | Audited upstream source checked into the workspace, not a registry edge. |
| 8 | BOUNDARY | A third-party crate approved in `contract/APPROVED.toml` with a human sign-off and a reason naming what `std` cannot do. |

ELIMINATE and CONSOLIDATE crates never get a register entry — they become a
module in `lgwks_std` (or are removed). `tier` in the register admits only
`boundary` and `vendor`. A new BOUNDARY edge is normally an optional feature of
the `lgwks_deps` storefront (`owner = "lgwks_deps"`), so it is never built
unless the end user selects it.

## 2. Decision flow

Before adding any edge:

1. **Does `std` do it?** `OnceLock`/`LazyLock` (not `once_cell`/`lazy_static`),
   `available_parallelism` (not `num_cpus`), `std::sync::mpsc`,
   `std::thread`, `std::fs`, `std::net`, integer `from_le_bytes` (not
   `byteorder`). If yes, stop.
2. **Does an estate module do it?** Check the matrix in §3. If yes, import
   `lgwks_std`, `lgwks_bot`, or `lgwks_ast` — not the crate underneath.
3. **Is it a Rust parser or multi-language parser?** Rust source scanning is
   owned by `lgwks_deps::scan`; multi-language parsing by `lgwks_ast`. Do not
   add a second parser (§5).
4. **Is it async / actor work?** Async, runners, and actor roles are
   `lgwks_bot`; its tokio engine is selected through the `lgwks_deps` storefront.
   The zero-dependency synchronous executor is `lgwks_std::task`. See §4.
5. **Otherwise** it is a storefront BOUNDARY decision: add the crate as an
   optional feature of `lgwks_deps`, run
   `cargo run -p lgwks_deps -- request <crate> <version>`, fill the block with
   `owner = "lgwks_deps"`, and commit. The commit is the approval. See
   `skills/lgwks-dependency-admission/SKILL.md`.

A direct edge that skips this flow fails `lgwks-deps check` with the exact
crate, requirement, and source class named.

## 3. Replacement matrix

Import from the estate column. "Feature" is the `lgwks_std` cargo feature.

| Reaching for | Use instead | Feature | Notes |
|---|---|---|---|
| `hex` | `lgwks_std::hex` | core | encode/decode |
| `base64` | `lgwks_std::encoding::base64` | core | RFC 4648 standard |
| `percent-encoding` | `lgwks_std::encoding::percent` | core | strict decode |
| `uuid` | `lgwks_std::id::Uuid` | `random` | v4 generation + parse |
| `getrandom`, `rand` (entropy) | `lgwks_std::random` | `random` | OS CSPRNG only; no distributions |
| `chrono`, `time` | `lgwks_std::time` | core | UTC RFC 3339 + proleptic Gregorian calendar; no IANA time zones |
| `glob` | `lgwks_std::glob` | core | matching only; traversal is `fs` |
| `walkdir` | `lgwks_std::fs::walk_dir` | core | depth-bounded, symlink-sandboxed |
| `fs4`, raw `statvfs` | `lgwks_std::fs::available_space` | `fs-raw` | Unix; explicit `Unsupported` elsewhere |
| `regex` | `lgwks_std::pattern::Regex` | `pattern` | compile-once, linear-time |
| `blake3` | `lgwks_std::hash` | `hash` | BLAKE3 only; no SHA-2 |
| `serde` | `lgwks_std::json::{Serialize, Deserialize}` | `json` | serde is the owned stack |
| `serde_json` | `lgwks_std::json` | `json` | to/from str, slice, reader, Value |
| `ron` | `lgwks_std::ron` | `ron` | |
| `rkyv` | `lgwks_std::wire` | `wire` | deterministic internal binary |
| `ureq`, `reqwest` (blocking) | `lgwks_std::http` | `http` | blocking GET/POST; run on `spawn_blocking` from async |
| `url`, `iri-string` (validation) | `lgwks_std::http::validate_url` | `http` | absolute http(s) only |
| `tokio` | `lgwks_bot::rt` | `rt` | `lgwks_deps` owns the optional storefront edge; bot reaches it through its `tokio` feature |
| `gpui` | `lgwks_deps` feature `gpui` | — | storefront; default-off, user-selected |
| `futures`, `pollster`, `async-trait`, `tokio` (sync-only use) | `lgwks_std::task` | core | zero-dep synchronous executor; see §4 |
| `tokio::time`, `tokio::sync`, `tokio::net`, `tokio::process`, `tokio::fs`, `tokio::signal` | `lgwks_bot::rt::{time, sync, net, process, fs, signal}` | `time`, `sync`, `net`, `process`, `fs`, `signal` | feature-gated through `lgwks_deps` |
| actor/state frameworks | `lgwks_bot` | — | capability-gated four-verb bots |
| `tree-sitter`, `ast-grep-core`, `ast-grep-language` | `lgwks_ast` | — | one shared multi-language parser |
| `syn`, `proc-macro2` (source scanning) | `lgwks_deps::scan` | `scan` | owned exception; see §5 |
| `cargo-deny`-style admission | `lgwks_deps` | — | `check`, `tiers`, `request`, `vendor`, `freshness` |
| `rustix`, `libc` (OS syscalls) | extend `lgwks_std` behind a feature | — | `rustix` is the approved syscall layer (`fs-raw`); add the primitive to the facade, do not import rustix from a consumer |
| `once_cell`, `lazy_static` | `std::sync::{OnceLock, LazyLock}` | — | in `std` since 1.80 |
| `num_cpus` | `std::thread::available_parallelism` | — | in `std` |
| `tempfile` (tests only) | `tempfile` | — | dev-kind edge, already approved (`test.atomic-tempdir`) |

The measured source for each row is the module header in
`crates/lgwks-std/src/*.rs`; the register rows are `contract/APPROVED.toml`.

**serde derive, downstream.** `lgwks_std::json` re-exports the `serde` crate and
the `Serialize`/`Deserialize` derives, but serde's derive macro resolves a
`serde` crate path. The gate forbids a consumer declaring `serde` directly
(`allowed_consumers = lgwks_std`), so annotate the type:

```rust
use lgwks_std::json;

#[derive(json::Serialize, json::Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct Doc { n: u32 }
```

Verified downstream with no direct `serde` dependency.

## 4. Worked example — `tokio`

Two tiers share one owned engine.

**Synchronous tier — `lgwks_std::task` (feature `core`, zero dependencies).**
Its header states the scope: `block_on`, `join_all`, `join_all_boxed`, and
`spawn_blocking` replace `futures`, `pollster`, and `async-trait` **for
applications that only need to await futures, await a bounded set of them
together, and keep blocking work off the driving thread.** No reactor, no
worker pool.

```rust
use lgwks_std::task::{block_on, join_all, spawn_blocking};

let value = block_on(async { 1 + 1 });
let reads = block_on(join_all(vec![
    spawn_blocking(|| std::fs::read("a.txt")),
    spawn_blocking(|| std::fs::read("b.txt")),
]));
```

`lgwks_bot` builds on this: `Bot::tick` polls every source in bounded
concurrent waves (`MAX_IN_FLIGHT_POLLS = 32`) and fires actions sequentially.
The tick executor itself uses no `futures` or `async-trait`; its synchronous
path remains `lgwks_std::task`. The bot's separate runtime feature reaches
tokio through the `lgwks_deps` storefront.

**Async tier — `lgwks_bot` (the estate's async and runner surface).** Timers, wakeable
channels, async sockets, and a multi-threaded worker pool cannot be built on
`std` without an I/O reactor and `unsafe`, and `lgwks_std` forbids both.
`lgwks_bot` therefore wraps **tokio core** through the `lgwks_deps` storefront,
and `tokio` is registered in `contract/APPROVED.toml` with
`owner = "lgwks_deps"`. The gate refuses any other workspace crate that authors
a `tokio` edge, so this facade is the single entry.

```rust
use lgwks_bot::rt::task::join_all_bounded;
use lgwks_bot::rt::time::{Duration, sleep};

let runtime = lgwks_bot::Runtime::new().expect("runtime");
let results = runtime.block_on(async {
    join_all_bounded(4, (0..64).map(|i| async move {
        sleep(Duration::from_millis(1)).await;
        i
    }))
    .await
});
```

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
completion order is nondeterministic — only `join_all_bounded` result order is
guaranteed. `net` sockets do not pass the HTTP egress policy; a caller applies
policy before connecting. `fs` operations occupy a blocking thread (not
io_uring). A hard-realtime or io_uring workload is outside the estate's domain
and must be stated, not smuggled in as a dependency. On `wasm32-wasip1`, the
`rt` feature uses a current-thread runtime; supplying `worker_threads` is an
explicit `Unsupported` error, and Tokio's native-only driver features are
outside the WASM domain.

## 5. Worked example — `syn`

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

**Boundary:** there is no general-purpose Rust semantic API (types, trait
resolution, macro expansion) in the estate, because nothing here needs one.
Do not add a second Rust parser; if you need Rust semantics, route through
`lgwks_ast::Language::Rust` for structure, or state that `syn` is required and
register it with that reason.

## 6. Where no equivalent exists — do not fake it

These are real gaps. The doctrine is explicit about them so an agent neither
pretends coverage nor silently adds a crate:

| Capability | Path |
|---|---|
| `thiserror`, `anyhow` | Implement `std::error::Error` manually; keep typed error enums at library boundaries (the estate's own crates do this). |
| `log`, `tracing`, `env_logger` | `eprintln!` / `stderr`; there is no logging facade. Machine output must stay parseable (`experience/invariants/sdk.yaml`). |
| `clap`, `argh` | Parse `std::env::args` directly; CLI parsing is not an estate capability. |
| `toml`, `serde_yaml`, `csv` | Not provided. Use `json`/`ron` for data; the gate parses TOML line-wise on purpose to avoid a TOML dependency. |
| `sha2`, `hmac`, `aes-gcm`, `argon2`, `ed25519` | Not provided (BLAKE3 only). Crypto is a BOUNDARY tier — register it with the concrete reason. |
| `rand` distributions | `lgwks_std::random` is OS entropy only; derive the distribution you need or register `rand`. |
| `bytes`, `byteorder` | Use slices and `from_le_bytes`/`to_le_bytes`. |

When a gap forces a new edge, the edge is not the end of the conversation: the
register entry must say which owner carries it and what `std` cannot do. If the
capability is a general primitive, the correct answer is a new `lgwks_std`
module (ELIMINATE), not a consumer-local crate.

## 7. Adding a dependency

The short version; the exact procedure is
`skills/lgwks-dependency-admission/SKILL.md`.

```sh
cargo run -p lgwks_deps -- tiers            # find the rung
cargo run -p lgwks_deps -- request foo 1.2 # print a block to fill in
# edit contract/APPROVED.toml, add the Cargo.toml edge, update closure tests
cargo run -p lgwks_deps -- check .          # must print OK
```

An approval with no authored Cargo edge is refused as stale authority
(`UnusedApproval`); an authored edge with no approval is refused as
unregistered (`UnregisteredEdge`). Both directions are enforced.

## 8. Why this holds up

- `lgwks-deps check` runs as the first CI lane locally and remotely, and the
  same `check_dependencies` API is available to embedders — the verdict cannot
  drift between a developer's machine and CI.
- The gate is fail-closed: a missing register, unparseable metadata, or
  unreadable lock is a refusal, never a pass.
- `deps_are_approved_leaves` in `crates/lgwks-deps/src/lib.rs` pins the exact
  allowed dependency names for `lgwks_std` and for the gate itself, so a new
  edge cannot be added by editing a manifest alone.
