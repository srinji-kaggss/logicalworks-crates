# Dependency doctrine: replacement matrix

Part of the [dependency doctrine](dependency-doctrine.md). This page is §3.

## 3. Replacement matrix

Import from the in-tree column. "Feature" is the `lgwks_std` cargo feature.

| Reaching for | Use instead | Feature | Notes |
|---|---|---|---|
| `hex` | `lgwks_std::hex` | core | encode/decode |
| `base64` | `lgwks_std::encoding::base64` | core | RFC 4648 standard |
| `percent-encoding` | `lgwks_std::encoding::percent` | core | strict decode |
| `uuid` | `lgwks_std::id::Uuid` | `random` | v4 generation + parse |
| `getrandom`, `rand` (entropy) | `lgwks_std::random` | `random` | OS CSPRNG only; no distributions |
| `chrono`, `time` | `lgwks_std::time` | core | UTC RFC 3339 + proleptic Gregorian calendar; no IANA time zones |
| `glob` | `lgwks_std::glob` | core | matching only; Unicode-scalar dialect with strict/legacy policies; not full upstream `glob` or POSIX parity; traversal is `fs` |
| `walkdir` | `lgwks_std::fs::walk_dir` | core | depth-bounded, best-effort trusted-tree listing; not a race-safe sandbox |
| `fs4`, raw `statvfs` | `lgwks_std::fs::available_space` | `fs-raw` | Unix; explicit `Unsupported` elsewhere |
| `regex` | `lgwks_std::pattern::Regex` | `pattern` | compile-once, linear-time |
| `blake3` | `lgwks_std::hash` | `hash` | BLAKE3 only; no SHA-2 |
| `serde` | `lgwks_std::json::{Serialize, Deserialize}` | `json` | serde is the owned stack |
| `serde_json` | `lgwks_std::json` | `json` | to/from str, slice, reader, Value |
| `ron` | `lgwks_std::ron` | `ron` | |
| `rkyv` | `lgwks_std::wire` | `wire` | feature-unified archive facade; not canonical semantic identity |
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
| `nix`, raw `killpg(pgid, 0)` (process-group probe) | `lgwks_std::process::process_group_exists` | `process` | rustix signal-zero beside `kill_process_group`, one backend for the kill and its confirmation (#263); `lgwks_deps::process_group::exists` is a deprecated forward |
| `once_cell`, `lazy_static` | `std::sync::{OnceLock, LazyLock}` | — | in `std` since 1.80 |
| `num_cpus` | `std::thread::available_parallelism` | — | in `std` |
| `tempfile` (tests only) | `tempfile` | — | dev-kind edge, already approved (`test.atomic-tempdir`) |

The measured source for each row is the module header in
`crates/lgwks-std/src/*.rs`; the register rows are `contract/APPROVED.toml`.

**serde derive, downstream.** `lgwks_std::json` and `lgwks_std::ron` each
re-export the `serde` crate and the `Serialize`/`Deserialize` derives, but
serde's derive macro resolves a `serde` crate path. The gate forbids a consumer
declaring `serde` directly (`allowed_consumers = lgwks_std`), so annotate the
type with the facade selected by the feature: derive `json::Serialize` and
`json::Deserialize` from `lgwks_std::json` and add
`#[serde(crate = "lgwks_std::json::serde")]` to the type.

For RON-only consumers, use `#[serde(crate = "lgwks_std::ron::serde")]`; that
path is available with `ron` alone and does not activate JSON. The text and
slice decode entry points borrow unescaped strings from their input where the
decoder supports it. Escaped strings need owned storage; JSON reader decoding
remains owned.

`lgwks_std::ron::to_writer` renders the value before touching the writer, then
streams the rendered bytes with `write_all`. A serialization failure writes
nothing; an I/O failure may leave a prefix, and `write_all` does not expose an
accepted-byte count. Its `ron::WriterError` distinguishes serialization from
I/O and preserves each original cause. Callers that previously matched
`ron::Error` must migrate to `WriterError::Serialize` or `WriterError::Write`.
The `Display` text also changes: the former fabricated `write_all failed after
{n} bytes` count is removed. These errors have no facade-defined serialized
representation, and no in-repository caller of `ron::to_writer` needs a source
migration.
