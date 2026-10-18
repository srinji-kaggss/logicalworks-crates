# lgwks_std 1.1.0 / lgwks_deps 1.1.0 / lgwks_macros 1.1.0 / lgwks_bot 1.1.0 - 2026-10-05 (1/2)

Release: lgwks_std 1.1.0 / lgwks_deps 1.1.0 / lgwks_macros 1.1.0 / lgwks_bot 1.1.0 - 2026-10-05. Page 1 of 2. [Changelog index](../../CHANGELOG.md)

`lgwks_ast` stays at 1.0.0: its source is unchanged since that tag. Every
other change below is additive, and every enum that gained a variant is
`#[non_exhaustive]`. The exception is `lgwks_macros`: a script that relied on
a refusal now added (#265) no longer compiles. That is a correction to the
guard, recorded here as a minor bump rather than hidden in a patch.

### lgwks_std — Digest equality measured constant-time (#275)

- `Digest`'s `==` now delegates to `blake3::Hash`'s equality, the
  `constant_time_eq` routine behind an optimisation barrier, instead of a
  hand-written XOR/OR fold that the compiler was free to turn into an
  early exit. No new dependency: `constant_time_eq` was already in the graph
  through `blake3`.
- New `Digest::ct_eq`, the same comparison by name, for checks against a
  secret-derived or adversary-written value. `lgwks_bot`'s `verify_chain`
  compares recorded chain heads through it.
- `Digest`'s `Ord`/`PartialOrd` are documented as variable-time, for
  collections only. They are kept, because removing them would break 1.x.
- `examples/digest_timing.rs` is a dudect-style Welch's t test over two
  classes at 1,000,000 samples each, with an early-exit negative control it
  must detect. The `digest-timing` gate lane runs it on the release build,
  locally (aarch64-apple-darwin) and in CI (x86_64-unknown-linux-gnu).

### One process-group backend (#263)

- `lgwks_std::process::process_group_exists` (feature `process`) is the
  signal-zero group probe, on rustix beside `kill_process_group`, so the
  supervisor's kill and the check that confirms it share one syscall binding
  and one error mapping. Zero and negative ids are refused as `InvalidInput`;
  off Unix it reports `Unsupported`.
- `lgwks_deps::process_group::exists` is deprecated and forwards to it. The
  `process-group-probe` feature now enables `lgwks_std/process` and no longer
  pulls in `nix`. That edge is withdrawn from `contract/APPROVED.toml`, and
  the workspace no longer builds `nix`.
- `lgwks_bot`'s `process` feature no longer enables
  `lgwks_deps/process-group-probe`.

### lgwks_bot — fan-out, journal status, process stdio (#257, #259, #260)

- `script::FanOut` and `script::FanOutError`: a one-call fan-out over `script::each`, bounded
  by `at_most(limit)` and `within(deadline)`, whose error names the failing
  item or the timeout.
- `journal::AttemptStatus::VerificationFailed`: a failed verification
  recovers as its own status instead of collapsing into `Applied` (#257).
  `Verified` is no longer terminal, so a later verdict revises it (#260).
  Recovered status exposes the latest verification and a per-attempt history
  of `Transition`s, each with its position in the journal (#259).
- `rt::process::ProcessSpec::stdout_to_file` and `stderr_to_file`.
- `rt::runtime::Builder::thread_stack_size`, bounded by
  `MAX_THREAD_STACK_SIZE` (256 MiB).

### lgwks_deps — attestation bound to the tree under review (#258)

- `invariants::Status::Attested` is refused unless the recorded revision is
  in the history of `HEAD` and the enforcer is unchanged since then. The
  refusal is `InvariantError::EvidenceNotBound`, carrying an `EvidenceGap`.
  Before this, any well-formed hex revision certified any commit.

### Property tests with shrinking (#273)

- `proptest` 1.11 is admitted as a dev-only edge (`contract/APPROVED.toml`,
  default features off). No shipped artifact links it.
- New property targets, each from one fixed seed with failures persisted
  under `proptest-regressions/`, and each paired with a mutant it must catch
  and shrink: `lgwks_std` `prop_codecs` (hex, base64, percent, leb128,
  RFC 3339, wire, and glob against a regex oracle); `lgwks_bot` `prop_journal`
  (`recover()` over arbitrary proposal histories, refused appends, reopen)
  and `prop_each` (order, in-flight bound, fail-fast); `lgwks_deps`
  `prop_parsers` (register and lockfile round trips, duplicate keys); and
  `lgwks_macros`' line splitter and indentation tree.
- `lgwks_deps::lock::parse` refused nothing when a `[[package]]` block
  assigned `name`, `version`, `source` or `checksum` twice: the last
  assignment won. It now refuses with the new `LockError::DuplicateKey`, at
  the line of the second assignment.

### lgwks_std — bounded blocking pool (#264)

- `task::spawn_blocking` runs on one process-wide pool of at most 512 threads
  instead of one new OS thread per call. A job submitted while every thread is
  busy waits its turn; a thread idle for ten seconds exits. Its signature and
  its never-refuses contract are unchanged. Jobs that wait on each other must
  now number fewer than 512.
- New `task::try_spawn_blocking` and `task::SpawnError`: the same pool with a
  wait queue bounded at 16,384, refusing past it as `SpawnError::AtCapacity`,
  and as `SpawnError::Os` when no thread can be started. A refused job never
  runs. Before this, an OS refusal to start a thread surfaced only as a panic.
- `task::join_all` gives each child its own waker: a wake re-polls only the
  child that woke, so `n` children waking `k` times cost `n·(k+1)` polls
  rather than a scan of every pending child per wake.
- A job that starts a pool thread is handed to it directly instead of through
  the queue, so under the ceiling a job starts as fast as a thread of its own
  (p50 11 µs, p99 20–60 µs at 500 concurrent jobs, against 10–11 µs and
  19–20 µs before the pool). Past the ceiling a job waits for a thread: at
  10,000 × 20 ms jobs, p50 238 ms and p99 484 ms to start.
- The pool's accounting (queued, live, idle, claimed wakeups) is a set of
  transitions a seeded simulation drives through every interleaving of
  submit, thread start and start failure, lost notify, spurious wake and
  keep-alive expiry: every admitted job runs exactly once, a refused one
  never, and the pool drains to nothing.

### lgwks_bot

- `domain::net::Endpoint`'s poll uses `try_spawn_blocking`, so a burst of
  concurrent polls cannot start a thread per poll; a refusal is a
  `DomainError` with `DispatchCertainty::Refused`. Ten thousand concurrent
  polls against a full pool are all refused this way and send nothing; once
  the pool frees, the same ten thousand all run.

### lgwks_macros — refusals by path, and lints the consumer enforces (#265)
