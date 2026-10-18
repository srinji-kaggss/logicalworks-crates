# lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06 (16/20)

Release: lgwks_std 2.0.0 / lgwks_ast 1.1.0 / lgwks_deps 3.0.0 / lgwks_macros 1.1.1 / lgwks_bot 2.0.0 - 2026-10-06. Page 16 of 20. [Changelog index](../../CHANGELOG.md)

### lgwks_std — the blocking pool's ceiling and its shutdown (#264) (continued)

- `task::configure_blocking_pool(threads)` fixes the pool's thread ceiling
  once, before the pool first runs a job. The pool's own creation is the
  arbitration — a configure and a first use race to build it, so neither can
  miss the other's write — and a ceiling is therefore never silently ignored.
  A later attempt is refused with the new `PoolConfigError`: `InUse` once the
  pool has run work, `AlreadyConfigured` naming the ceiling in force,
  `InvalidCeiling` for a ceiling below one. Asking again for the ceiling
  already in force succeeds: the pool is at it.
- `task::shutdown_blocking_pool(within)` closes admission, lets the queued
  and running jobs finish, and joins every pool thread inside the deadline.
  A thread with no job leaves instead of parking, and a parked thread is woken
  to take a waiting job or leave, so the pool empties by finishing its work
  rather than by cancelling it. The new `PoolShutdown` reports what the wait
  found: `Drained { threads }` means every thread was joined and none outlives
  the call; `DeadlineExceeded { joined, running, queued }` names the threads
  still executing and the jobs still waiting, and their handles stay
  registered so a later shutdown joins them.
- `task::SpawnError::Shutdown` is the refusal both entry points give after a
  shutdown. `try_spawn_blocking` returns it; `spawn_blocking`, whose 1.0
  contract is that it never refuses, fails its awaiter with it as the
  payload, as it already did for an `Os` refusal. The closure never runs.
- A thread is no longer detached. Each start registers its handle under the
  same lock that counted the thread, so a shutdown can never observe a thread
  it cannot join.
- **A start now joins the threads that have already returned**, so a process
  whose load is bursty — a burst, an idle period, a burst — no longer keeps
  one handle per exited thread for ever. What the handle list holds is exactly
  `live + (threads that have left the accounting and not yet returned)`: every
  entry beyond `live` is a real thread still executing its last instructions,
  and that second group has no constant bound, because a departure frees its
  slot immediately. What is bounded is the accumulation: every thread that
  *has* returned is joined at the next start or at a shutdown.
- The pool is an `Arc`, so a thread owns its own reference and a caller can own
  and drop a pool; the tests' last `Box::leak` is gone.
- Two seeded simulation families cover the new paths: `sim_pool` drives the
  accounting through configure attempts that move nothing and a shutdown
  closing admission mid-schedule (2,000 seeds × 2,000 steps), and
  `sim_pool_lifetime` drives real OS threads through burst drains, an expired
  deadline, a parked thread, a refused ceiling, and burst/idle cycles that
  would grow the handle list if nothing reaped it. A process-owning test
  binary exercises the two public functions against the real process-wide
  pool, because a shutdown closes admission for the life of its process.

### lgwks_std — `random` reaches every target its backend does, and says why it failed (#276)

- `random` no longer refuses to compile on every target but Linux, macOS and
  Windows. The three-target `compile_error!` was a narrower claim than the
  backend it wraps, so it refused FreeBSD, the other BSDs, illumos, Solaris,
  Android, iOS, `wasm32-wasip1` and every other target `getrandom` already
  supported. The backend is the one authority on where an entropy source
  exists, so its own refusal is now the single compile-time gate and this crate
  holds no target list that could drift narrower.
- `EntropyError` carries the cause as data instead of a `String`: a
  `#[non_exhaustive]` `EntropyErrorKind`, `raw_os_error()` for the OS's own
  code, and `io_error_kind()` for its portable `std::io` classification. A UEFI
  status wider than `i32` is dropped rather than truncated into a code naming a
  different failure. Additive for 1.x: `backend()` and the `Display` rendering
  are unchanged, and the `String` was never in the public surface.
- `fill_bytes` documents that a refused draw leaves the buffer **unspecified**
  and must not be read; `bytes` has no such window, since a failed draw returns
  no array. A test drives the refused path through a crate-private seam, because
  `getrandom` offers no constructor for a backend failure carrying an OS code
  and an integration test cannot reach a private seam.
- New `tests/it/sim_random_error.rs`: seeded sweeps over draw lengths from empty
  to a mebibyte, concurrent drawers at 100, 1 000, 10 000 and 100 000, the UUID
  version and variant masks, and a seeded generator that must not be able to
  predict a draw.

### The gate: every `lgwks_std` feature alone, and the declared target matrix (#276)

- The `feat-std` lane now builds all twelve non-`full` features, each alone.
  `core`, `trace`, `random`, `ron` and `process` were built by nothing except
  the rustdoc lane, so a feature that quietly depended on a second feature was
  invisible to every build receipt.
- New required lanes `tests-std-per-feature-a`, `-b` and `-c`: one
  `cargo nextest run --no-default-features --features <f>` per feature, because
  a build receipt is not an execution receipt. Three shards of four features,
  each its own CI job: the twelve runs as one step took 228 s on GitHub and put
  the run at 307 s, past the five-minute budget.
- New required lane `target-matrix`, running the new
  `scripts/check-target-matrix.sh`. It installs each declared target with
  `rustup` and checks `lgwks_std`, `lgwks_ast` and `lgwks_deps` against it: the
  Rust-only surface is required everywhere, and a check that needs a C
  toolchain the runner lacks is recorded with its exact error and a named
  reason instead of being dropped. A failure that is not that reason fails the
  lane. It runs as its own CI job rather than on the critical path.
- Measured on aarch64-apple-darwin: 42 checks, 69 s from an empty target
  directory. `lgwks_std --features random` builds on all seven cross targets;
  nine `full`/`lgwks_ast` checks are exempt for a missing C cross-compiler.

### The gate: the saturation tiers spawn the fake the way a default binding does (#272)
