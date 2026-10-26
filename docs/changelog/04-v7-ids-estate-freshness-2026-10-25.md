# lgwks_std 3.1.0 · lgwks_deps 5.0.0 · lgwks_ast 3.0.0 · lgwks_bot 4.0.0 · lgwks_macros 1.1.6

2026-10-25

Every public item that existed in the 2026-10-21 releases still exists with the
same signature (`contract/PUBLIC_SURFACE.toml` only grew). Three crates still
take a major bump, because a type their public API names now comes from a
newer major of its upstream (`docs/releasing.md` §3):

- `lgwks_deps` re-exports Bevy, which moved 0.19 to 0.20 (#729).
- `lgwks_ast` re-exports `ast_grep_core::{AstGrep, Node}`, which moved 0.45 to
  0.51 (#729).
- `lgwks_bot`'s `inspect` API takes `lgwks_ast::Language`, so it moves with
  `lgwks_ast`.

## Every crate: defaults carry every capability (#732, ADR 0003)

A crate's default feature set now carries every capability it owns, except where
ADR 0003 records a measured cost or a target that cannot carry it. A build that
named no features now compiles more.

- `lgwks_std`: the default is `standard`, every feature but `http` (its rustls
  stack compiles C per target and failed on `x86_64-unknown-linux-musl`).
  `full` is `standard` plus `http`. `--no-default-features --features core` is
  still the zero-dependency build.
- `lgwks_ast`: the default is all 28 grammars, the `script!` reader, the
  `lgwks-ast` tool and the scan. `--no-default-features` is the derive alone.
- `lgwks_bot`: the default is `script`, which brings `rt`, `time`, `sync` and
  `macros` as before. `inspect` now builds every grammar (`lgwks_ast/full`).
- `lgwks_deps`: the default stays empty, so the no-flag build holds on
  `wasm32-wasip1`. `lgwks-deps catalog` prints every group, its feature and its
  cost.

## lgwks_std 3.1.0

- `Uuid::new_v7` issues RFC 9562 version 7 ids that are strictly increasing in
  value and text across every thread of the process: a backwards clock keeps
  the last stamp, a spent counter borrows the next millisecond, and a stamp past
  48 bits is refused (`V7Error`). `Uuid::unix_millis` reads the stamp (#715).
- `build` feature: a build script's input digest and checked cargo directives
  (#733).
- `EditDistance` scores through a kernel specialised to `char`; the scores do
  not change.
- `GlobPattern::is_match` reuses one scratch per thread and no longer allocates
  per call.
- `sign` moves to ed25519-dalek 3 with `zeroize`, so `SecretKey`'s wipe on drop
  is a compiled property. The key types stay opaque.

## lgwks_deps 5.0.0

- Bevy 0.20 behind `bevy-*`; every other storefront engine updated (#729).
- `time-jiff`: interim zoned time at `storefront::jiff` (#740).
- `lock::Resolved` gains `source()` and `dependencies()`; a `dependencies` array
  that is not a closed list of quoted strings is refused as
  `LockError::UnreadableDependencies`.
- `lgwks-deps freshness` runs `--jobs` lookups at once (default 32), ranks by
  SemVer precedence, never offers a yanked version, names the direct dependency
  each package comes through, reads every response through a 16 MiB cap, and
  exits 0 current, 1 behind, 2 unfinished (#702, #754, #757).
- New binaries: `catalog`, `check-docs`, `check-target-matrix`.

## lgwks_ast 3.0.0

- ast-grep 0.51 behind the re-exported `AstGrep` and `Node`.
- `lgwks-ast scan` exits 1 for findings on a tree it read, 2 for a file it could
  not read and 64 for a misuse (#698). ALLOW-SILENCE reads `expect`, `cfg_attr`,
  raw identifiers, macro bodies, unread includes and a package's `[lints]` table;
  a caller selects detectors by `Rule` and `--rule` (#699, #758).

## lgwks_bot 4.0.0

- `rt::estate::Estate`: one runtime, supervisor and host behind three tiers
  (`in_process`, `supervised`, `durable`), with a terminal `EstateReport`, and
  the `estate` binary as its deployment smoke (#735).
- `EcsBot::replace_grants` narrows a running bot between ticks and reports what
  it withdrew as a `GrantReplacement` (#716, #745).
- `Supervisor::in_flight` and `Supervisor::task_state` let an owner reconcile
  its ledger against the supervisor (#703, #745). `task_state` answers in one
  pass with no allocation.
- A `Host` dropped inside async no longer panics: its reactor ends in the
  background.

## lgwks_macros 1.1.6

- README publisher block; builds against the new `lgwks_ast` and `lgwks_deps`.
  No change to `script!`.

## Verifying the release

```sh
cargo run -p lgwks_deps --bin published-consumer
```

The release is done at `published consumers: all passed`.
