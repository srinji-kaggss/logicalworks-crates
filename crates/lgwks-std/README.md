# lgwks_std — zero-config primitives that replace a dozen crates

The standard library gets you 90% of the way. This crate is the last 10% —
hex, base64, timestamps, UUIDs, hashing, glob matching, regex, JSON, async — as
single-import, zero-config calls backed by a dependency stack that bottoms out
at zero external deps.

Pick exactly what you need. The default feature compiles with **zero external
dependencies**. Each optional feature unlocks one capability with one vetted
stack beneath it — no transitive surprises, no feature flag archaeology.

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
// Opt-in features — each adds exactly one vetted dependency
use lgwks_std::id::Uuid;       // feature = "random"
use lgwks_std::hash;            // feature = "hash"
use lgwks_std::pattern::Regex;  // feature = "pattern"
use lgwks_std::json;            // feature = "json"

let id = Uuid::new_v4().unwrap();
let blake3 = hash::blake3(b"content-addressable");
let re = Regex::new(r"\d+").unwrap();
let value: MyStruct = json::from_str(&data)?;
```

## Feature map

```toml
[dependencies]
lgwks_std = "0.5"                       # core only, zero deps
lgwks_std = { version = "0.5", features = ["hash", "json"] }  # pick what you need
lgwks_std = { version = "0.5", features = ["full"] }           # everything
```

| Feature | Modules | What it adds | External deps |
|---------|---------|-------------|---------------|
| `core` (default) | encoding, fs, glob, hex, leb128, task, time | — | **0** |
| `random` | random, id | UUID v4, OS entropy | getrandom |
| `hash` | hash | BLAKE3 content-addressable hashing | blake3 |
| `pattern` | pattern | Linear-time compiled regex | regex |
| `json` | json | JSON serialization | serde, serde_json |
| `ron` | ron | Rusty Object Notation | serde, ron |
| `wire` | wire | Zero-copy binary serialization | rkyv |
| `http` | http | Blocking HTTPS client (rustls-only TLS) | ureq, iri-string |
| `online` | online | TCP reachability probing | none |
| `full` | all of the above | — | all of the above |

## Module reference

| Module | What it does | Replaces |
|--------|-------------|----------|
| `encoding` | Base64 and percent-encoding | `base64`, `percent-encoding` |
| `fs` | Recursive directory walking with sandbox enforcement | `walkdir` |
| `glob` | Shell-style glob matching (DP algorithm, O(M*N)) | `glob` |
| `hex` | Hex encode and decode | `hex` |
| `leb128` | LEB128 variable-length integer encoding | — |
| `task` | Single-threaded executor: `block_on`, concurrent `join_all`, off-thread `spawn_blocking` | — |
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

The `core` feature carries zero external dependencies. You choose what you pull
in; every feature flag is one capability, one stack, no surprises.

## Minimum supported Rust version

Rust **1.98.0**. The MSRV moves forward only when code or dependency
requirements demand it.

## License

BSD-3-Clause — Copyright 2026 Logical Works Incorporated
