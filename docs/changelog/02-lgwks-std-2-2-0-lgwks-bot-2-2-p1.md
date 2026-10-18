# lgwks_std 2.2.0 / lgwks_bot 2.2.0 / lgwks_deps 3.0.1 - 2026-10-07 (1/2)

Release: lgwks_std 2.2.0 / lgwks_bot 2.2.0 / lgwks_deps 3.0.1 - 2026-10-07. Page 1 of 2. [Changelog index](../../CHANGELOG.md)

### Upgrading

Two minor bumps and a patch; nothing a 2.1.0 or 3.0.0 caller wrote stops
compiling. `lgwks_ast` and `lgwks_macros` are unchanged (`lgwks_ast`'s only
change since 1.1.0 is inside its tests).

- **lgwks_std 2.2.0** — additive: the predicate walk `fs::walk_dir_entries`
  (#343), and `WalkReport` gains a defaulted type parameter. `serde_json` is
  built with `float_roundtrip`, so a JSON number reads back as the bits that
  wrote it.
- **lgwks_bot 2.2.0** — additive: `ProcessSpec::env_clear` and
  `EnvDelta::Clear` on the non-exhaustive `EnvDelta` (#337). **Behaviour
  change** a caller will see: a `Host::run` still queued for a permit when
  `Host::cancel` fires is always `Refused`, never `Cancelled` (#278 row 7).
- **lgwks_deps 3.0.1** — fixes: a declared `A/B` licence is read as `A OR B`
  (#354), so a package written in Cargo's deprecated spelling passes against its
  SPDX approval, and every alternative is still judged. The scan's
  `long-try-chain` rule no longer charges a `#[test]` function nested in
  production code to the function around it.

### lgwks_std — a request to an IP address starts no thread

ureq bounds a lookup by running it on a detached OS thread, and did so for every
request, including one whose host is an IP literal and needs no lookup. Each
`http::get` to `127.0.0.1` therefore paid a thread start and left a thread it
never joined. A hop to an IP literal now sets no resolve timeout, so ureq parses
it on the calling thread; a host name keeps the bound. The agent is kept across
redirect hops and rebuilt only when a hop crosses between a literal and a name,
so a same-origin redirect still reuses its pooled connection. Measured by
`tests/http_alloc.rs`'s interleaved loopback probe on macOS arm64 (501 pairs):
client p50/p95/p99 104/116/129 us before and 84/92/99 us after, against a bare
exchange's 68/75/81 us; the probe's 1,000-request concurrency sweep took the
whole test from 7.7 s to 1.0 s. On a loaded Linux runner the client median had
reached 119 us against a bare 11 us and failed the probe's 8x bound on main.
**Not claimed:** a call with `Options::deadline` set still starts the thread,
because ureq times the resolve phase against the hop's global timeout.

### lgwks_deps — the innermost function decides `long-try-chain`

The rule exempts test functions (`#[test]`, `#[tokio::test]`, any `#[test_*]`),
but only at the top level. An exempt function nested inside a production
function left the walk's state alone, so its chains were reported against the
enclosing function's line. The exempt function now marks the walk skipped until
it ends, and a production function nested inside a test is still inspected
under its own line. A trait's default method is now walked as a function too:
a top-level one was never judged, and one nested in a production function was
charged to that function's line. A new seeded family,
`tests/it/sim_scan_detectors.rs`, found the first defect, and its adversarial
review found the second. It generates whole files of `allow` lists, `?`
statements, nested functions and traits, and documented functions, and holds
ALLOW-SILENCE, `long-try-chain` and `tautological-doc` to a model written from
each rule's documented contract rather than from the scanner's code, matching
line, rule and evidence text, across 1,024 mixed files and 29 property families:
permutation, line shifts, 64 concurrent tenants, torn input, and disk against
text. With the old walk, all eight mixed bands fail.

### lgwks_std — a predicate walk with per-entry `lstat` (#343)

A caller that wanted to skip `.git` or `target` had to walk them and filter
afterwards, paying every read and every budget charge, and then stat each kept
path a second time to learn its size or mtime.

- **lgwks_std** — additive, under `fs` (no new feature, no dependency):
  `fs::walk_dir_entries` and `fs::walk_dir_entries_tolerant` take a predicate
  `FnMut(&Path, FileKind) -> Descend`; `Descend::{Enter, Skip, Stop}` decides
  per admitted entry whether the walk goes below it. A skipped subtree is never
  listed, charged or omitted; a stop is reported by the new
  `WalkReport::stopped`. Each `fs::WalkEntry` carries the walk's own `lstat`
  (`kind`, `metadata`) and `relative_path` beside `path`. INV-FS-7.
- `fs::FileKind` now lives in `fs` (always on); `fs::capability::FileKind` is a
  re-export of the same type, so existing code compiles unchanged.
- `WalkReport` gained a defaulted parameter, `WalkReport<E = PathBuf>`; every
  existing path walk returns `WalkReport<PathBuf>` as before. The path walks run
  the same engine with an always-enter predicate and stat nothing new.

### lgwks_std — the HTTP latency probe measures the client, not the host

- `tests/http_alloc.rs` held the loopback p99 under a fixed 50 ms, a number
  that measures the machine: with six CI runners on one host it read 198 ms
  with nothing wrong in the read path. The probe now pairs each request with a
  bare loopback exchange against the same server, interleaved and alternating
  which goes first, and holds the client's median within 8x the bare median
  and its p99 under the larger of 50 ms and 8x the bare p99 (measured ratios
  1.6 and 2.1). A 40 ms per-request stall now fails the median bound (47,008
  us against 8 x 190 us), where the old 50 ms ceiling passed it. The probe
  writes its measurements through a locked stdout and propagates a write
  error instead of `println!`.

### lgwks_bot — a supervised child can start from an empty environment (#337)

A supervised child inherited the supervisor's whole environment, less whatever
the caller remembered to `env_remove` one name at a time. A CI lane run this way
read every tenant's signing key (logical_ci#84).

- **lgwks_bot** — `ProcessSpec::env_clear` and the `EnvDelta::Clear` variant
  (`EnvDelta` is non-exhaustive). Deltas apply in recorded order, so the child's
  environment is exactly what is set after the last clear (INV-BOT-159).
- Not in this change: the after-fork containment hook #337 also asks for needs
  `unsafe`, which the crate forbids.

### Tests

- **lgwks_std's simulation fixtures draw `seeded::Seeded`.** `tests/support/rng.rs`
  and `tests/support/seeded_sweep.rs` carried a private xorshift64* generator, a
  second meaning for every seed; both now draw the crate's own `Seeded`
  (INV-STD-SEEDED-1), the stream every other crate's simulations draw, so one
  seed names one stream across the estate. `Rng::below` is now unbiased and
  answers a zero bound without moving the stream. The new draws reached an idle
  pool journey in `sim_task_pool_public` that the old stream never drew, whose
  assertion contradicted `PoolShutdown::Drained`'s contract (it counts the idle
  threads a shutdown joins); the assertion now states the contract.

### Fixed
