# lgwks_std

Everyday Rust primitives with no async runtime required: hex, base64,
timestamps, UUIDs, hashing, glob matching, regex, JSON, HTTP, and structured
logging, as single-import calls, each behind one audited dependency stack.

Pick exactly what you need. The default build is `core` plus `trace`; every
other capability is one feature with one vetted stack beneath it.

`trace` is default-on. Structured logging is the replacement for terminal
output in library code, so it has to be reachable without selecting a feature
first. It includes the `tracing` facade and the default subscriber bootstrap:
`LGWKS_LOG`/`RUST_LOG` filtering, compact local output, pretty local output,
and JSON lines for machine ingestion. It still keeps `tracing-attributes` off,
so the foundation crate does not pull the `syn` instrumentation macro stack.

**Want a zero-dependency build?**

```sh
cargo add lgwks_std --no-default-features --features core
```

That is the only configuration in which this crate pulls nothing at all, and it
is verified by command rather than asserted. `cargo tree -p lgwks_std
--no-default-features --features core -e normal` prints one line: `lgwks_std`
itself.

Package `lgwks_std` (underscore) lives in directory `crates/lgwks-std`
(hyphen): `cargo add lgwks_std` then `use lgwks_std::...`.

## Install

```sh
cargo add lgwks_std                                    # core + trace
cargo add lgwks_std --features json,http               # pick what you need
cargo add lgwks_std --no-default-features --features core   # zero deps
cargo run -p lgwks_std --example quickstart            # 10-line tour (this repo)
```

## Usage

```rust
// Core — zero external deps, always available
use lgwks_std::{hex, time, encoding, glob};

let digest = hex::encode(b"hello");
let now = time::now_rfc3339();
let encoded = encoding::base64::encode(b"payload");
let matches = glob::matches("src/**/*.rs", "src/lib.rs");
```

```rust
// Debugging — default-on with the crate defaults
use lgwks_std::trace::{info, install_default};

install_default("my-service")?;
info!(tenant = "demo", "service started");
```

```rust
// Opt-in features — each adds exactly one vetted dependency
use lgwks_std::id::Uuid;       // feature = "random"
use lgwks_std::hash;            // feature = "hash"
use lgwks_std::pattern::Regex;  // feature = "pattern"
use lgwks_std::json;            // feature = "json"

let id = Uuid::new_v4().expect("OS entropy");
let blake3 = hash::blake3(b"content-addressable");
let re = Regex::new(r"\d+").expect("valid regex");

#[derive(json::Serialize, json::Deserialize)]
#[serde(crate = "lgwks_std::json::serde")]
struct Point { x: i32, y: i32 }

let point: Point = json::from_str(r#"{"x":1,"y":2}"#).expect("valid JSON");
```

> **The `#[serde(crate = ...)]` line is required.** The derive macro resolves a
> crate named `serde`, and the dependency policy forbids consumers from
> declaring `serde` directly, because `lgwks_std` owns that edge. The attribute
> points the expansion at the re-export instead. If the compiler reports
> `cannot find serde in this scope`, add the attribute rather than running
> `cargo add serde`; `lgwks-deps check` refuses that edge.

## Feature map

One manifest key, three alternative lines. Use exactly one of them:

```toml
[dependencies]
# Zero external deps: defaults off, core selected. The default build is core
# plus trace, so a bare `lgwks_std = "0.7"` is NOT zero-dependency.
lgwks_std = { version = "0.7", default-features = false, features = ["core"] }

# Pick what you need, on top of the default core + trace.
# lgwks_std = { version = "0.7", features = ["hash", "json"] }

# Everything.
# lgwks_std = { version = "0.7", features = ["full"] }
```

| Feature | Modules | What it adds | External deps |
|---------|---------|-------------|---------------|
| `core` (default) | encoding, fs, glob, hex, leb128, retry, similarity, task, time | — | **0** |
| `trace` (default) | trace | Structured logging plus default debugger install | tracing, tracing-subscriber stack |
| `random` | random, id | UUID v4, OS entropy | getrandom |
| `hash` | hash | BLAKE3 content-addressable hashing | blake3 |
| `pattern` | pattern | Linear-time compiled regex | regex |
| `json` | json | JSON serialization | serde, serde_json |
| `ron` | ron | Rusty Object Notation | serde, ron |
| `wire` | wire | Zero-copy binary serialization | rkyv |
| `http` | http | Blocking HTTPS client (rustls-only TLS) | ureq, iri-string |
| `online` | online | TCP reachability probing | none |
| `fs-raw` | fs | Bytes available on a filesystem, unprivileged; handle-relative `fs::capability::Dir` for trees being rewritten while you walk them | rustix |
| `process` | process | Signal a whole process group; observe a child's exit without releasing its pid | rustix |
| `full` | all of the above | — | all of the above |

## Module reference

| Module | What it does | Replaces |
|--------|-------------|----------|
| `encoding` | Base64 and percent-encoding | `base64`, `percent-encoding` |
| `fs` | Recursive directory walking for trusted trees, with omissions reported rather than hidden | `walkdir` |
| `glob` | Shell-style glob matching (DP algorithm, O(M*N)) | `glob` |
| `hex` | Hex encode and decode | `hex` |
| `leb128` | LEB128 variable-length integer encoding | — |
| `retry` | Retry budgets: attempts, exponential backoff with caller jitter, deadlines | — |
| `task` | Executor that drives futures on the current thread: `block_on`, interleaved `join_all`, off-thread `spawn_blocking` | — |
| `time` | RFC 3339 timestamps, calendar math | `chrono`, `time` |
| `random` | OS entropy via `getrandom` | `getrandom` |
| `id` | UUID v4 generation and parsing | `uuid` |
| `hash` | BLAKE3 content-addressable hashing | `blake3` |
| `pattern` | Compiled regex matching (linear-time guarantee) | `regex` |
| `json` | JSON encoding and decoding via serde | `serde_json` |
| `ron` | RON encoding and decoding via serde | `ron` |
| `wire` | Zero-copy binary wire serialization via rkyv | `rkyv` |
| `http` | Blocking HTTP GET/POST with strict URL validation | `ureq`, `iri-string` |
| `online` | TCP reachability probing, zero-dep | — |
| `similarity` | Edit-distance and cosine scorers, weighted composition, acceptance thresholds | — |
| `process` | Process-group signalling and unreaped exit observation (Unix-only) | — |
| `fs::capability` | Handle-relative `openat`/`statat`/`unlinkat`/`mkdirat` access (Unix-only) | — |

## Threat models: which walk to use

`fs::walk_dir` identifies files by path and says so: INV-FS-2 records that its
identity rechecks are best-effort, and the module header repeats it. That is the
right tradeoff for a tree you trust and the wrong one for a directory another
process can rewrite mid-walk.

For that, use `fs::capability::Dir`. It opens a directory once and resolves every
name beneath it from that one descriptor, so a name swapped underneath the walk
cannot redirect it — the failure is `ENOENT` on the entry that changed, which
`walk_dir_tolerant`'s omissions already have a place to record. Its
`entry_names` is Linux-only, because listing a descriptor needs `getdents64` and
the BSDs have no equivalent syscall; every other operation is `*at(2)`.

## Dependency philosophy

Every direct dependency is a vetted leaf or single-purpose stack registered by
semantic capability and owner. `lgwks-deps check` audits authored edges from
Cargo metadata; Cargo.lock preserves the exact transitive provenance.

- **blake3** — 3 zero-dep leaves (arrayvec, cfg-if, constant_time_eq)
- **regex** — 4 BurntSushi-internal crates, zero external deps
- **serde** — derive stack (proc-macro2, quote, syn)
- **serde_json** — 2 leaves beyond serde (itoa, ryu)
- **ron** — 1 leaf beyond serde (bitflags)
- **rkyv** — 5 djkoloski crates, zero external deps
- **getrandom** — zero deps in std-only mode
- **ureq** — blocking HTTP client, rustls-only TLS stack plus small leaves
- **iri-string** — zero-dep URI validation leaf at default features
- **rustix** — safe POSIX syscall surface for the `fs-raw` and `process` primitives; Unix-only, optional
- **tracing** — default-on structured event facade for library diagnostics.
- **tracing-subscriber** — default debugger bootstrap. `install_default("service")`
  installs env-filtered compact output, and `LGWKS_LOG_FORMAT=json` switches the
  same stream to JSON lines. `attributes` stays off, so `#[instrument]` and its
  proc-macro stack do not enter this crate.

`core` alone carries zero external dependencies; the default build is `core`
plus `trace`. You choose what you pull in; every other feature flag is one
capability, one stack, no surprises.

## Minimum supported Rust version

Rust **1.98.0**. The MSRV moves forward only when code or dependency
requirements demand it.

## The other crates

Four crates ship from this repository. They share a release process, not a
dependency graph: `lgwks_bot` and `lgwks_deps` depend on `lgwks_std`, and
`lgwks_ast` stands alone.

| Crate | What it gives you |
|---|---|
| [`lgwks_bot`](https://docs.rs/lgwks_bot) | A runtime for bots that run for weeks: four verbs, capability-gated authority, change-triggered execution, supervised background work |
| [`lgwks_ast`](https://docs.rs/lgwks_ast) | Parse many languages into one AST type, with bounded traversal and typed diagnostics |
| [`lgwks_deps`](https://docs.rs/lgwks_deps) | The audited storefront for third-party stacks, plus `lgwks-deps check` and `lgwks-deps debug` to prove dependency and debugger wiring |

The [repository README](https://github.com/srinji-kaggss/logicalworks-crates#readme)
indexes the design documents.

## License

Apache-2.0 — Copyright 2026 Logical Works Incorporated
