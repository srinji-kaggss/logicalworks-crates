# lgwks_std 0.8.0 / lgwks_ast 0.3.0 / lgwks_deps 0.2.0 / lgwks_bot 0.6.0 / lgwks_macros 0.1.0 - 2026-09-30 (2/2)

Release: lgwks_std 0.8.0 / lgwks_ast 0.3.0 / lgwks_deps 0.2.0 / lgwks_bot 0.6.0 / lgwks_macros 0.1.0 - 2026-09-30. Page 2 of 2. [Changelog index](../../CHANGELOG.md)

### lgwks_bot Added (continued)

- `script!` and the `script` module (feature `script`, default on):
  orchestration written as indented flows (`each x in xs:`, `within 2s:`,
  `retry up to 3 times, waiting 100ms:`, `together:`, `step`, `for`,
  `if`/`else`, `run`, `give back`, `fail with`). Every flow takes a tenant
  `Scope`; every step has a stable `StepKey` over its tenant and structural
  path; failures are `FlowError`s located at a path and classed as retryable
  or not; every script emits an `ARCHITECTURE` map as text or JSON. `each`
  runs bodies on the calling task, so they may borrow locals.
- The root scope decides what an author should not: `each` with no bound runs
  64 bodies per available core (a typed concurrency literal is refused at
  compile time), and every `retry` under one root shares a retry budget of 10
  plus one per five first attempts (Finagle's `RetryBudget` defaults), ending
  in the new `FlowError::Throttled` instead of a retry storm
  (arXiv:2608.25403, arXiv:2510.03551).
- A flow is `Send` whenever what it holds is: `each` and `retry` store their
  bodies as named future types, never `dyn`, so a tenant's flow can be spawned
  onto the multi-threaded runtime. `retry` bodies take their `Scope` by value.
- An `each` item's scope is `<step>#i` (for example `sync/each:page#39`), and
  item and retry scopes share the stop of their step instead of minting a
  token each: cancelling inside a body ends the whole fan-out, like `break`.
- `bench/orchestration` compares `script!` with `join_all_bounded`, tokio
  `JoinSet`, asyncio `TaskGroup`, Trio, Go `errgroup`, a Node pool and
  Effect-TS on one workload, five runs per cell. `script!` is the only way to
  hold every invariant measured: no body live at return under failure, cancel
  and storm; 130 attempts in a retry storm where the others make 368-5,000;
  and an error that names the failing item. On an idle host it ran 435,000
  items/s median against 334,000 for `join_all_bounded` and 316,000 for
  `JoinSet`, with a higher p99 (12.0 ms against 9.5 ms) and more peak RSS
  (13.6 MB against 7-9.5 MB). An earlier run on a loaded host, whose
  tree was not recorded, measured 267,000 while the hand-written Rust ways held
  about 348,000. It needs 11 lines to their 23-46.

### lgwks_macros 0.1.0 Added

- New crate: the syntax of `lgwks_bot::script!`. Parses indentation from token
  spans, expands each block to one call into `lgwks_bot::script`, and refuses
  unbounded, panicking, blocking or machine-specific code with a compile error
  naming the replacement.

### lgwks_deps Added

- Feature `macro`: `syn`, `proc-macro2` and `quote` (new edge, registered in
  `contract/APPROVED.toml`) re-exported for first-party proc-macro crates.

### lgwks_std Breaking

- `time::format::from_unix_parts`, `unix_parts`, `to_rfc3339`, and
  `time::now_rfc3339` now return `Result` so clock-range failures and
  out-of-range RFC 3339 years are visible. Migrate by handling `?` or matching
  `UnixTimeError` / `FormatError`; the old lossy behavior is available only as
  explicitly named, deprecated `*_lossy` functions.
  Replace `try_from_unix_parts` with checked `from_unix_parts`; replace calls
  that intentionally relied on the old infallible epoch fallback with
  `from_unix_parts_lossy`.
- `time::parse_rfc3339` rejects offset hours above 23, offset minutes above 59,
  leap-second labels, and platform-unrepresentable instants. Its SystemTime
  profile is UTC-normalized, nanosecond-limited, and does not retain original
  offset spelling or `-00:00` provenance.
- `glob::matches` now interprets `?` and character classes as Unicode scalar
  values rather than UTF-8 bytes. ASCII results and literals are unchanged;
  callers that relied on multiple wildcards consuming one multibyte scalar
  should update their pattern. `GlobPattern::compile` offers checked strict
  syntax, while `GlobDialect::Legacy` preserves permissive bracket and `**`
  forms during migration.
- `http` failures are one structured `Error::Failure { stage, kind, cause }`
  with `FailureStage` and `FailureKind` (including `Interrupted` for `EINTR`,
  at every stage); the `Error::Timeout`, `Error::Interrupted` and
  `Error::Transport(String)` variants are removed. Match on `kind` instead of
  display text. The `Options::user_agent` and `Options::headers` fields and
  the `Response::headers` and `Response::body` fields are private: set them
  with `Options::user_agent(value)` and `Options::header(name, value)`, read
  them with `headers()` and `body()`. `Options::redirect_policy`,
  `Response::header_values`, `final_target` and `redirect_chain` are added.
- `http` redirects are explicit (`RedirectPolicy::{NoFollow, Follow,
  FollowAtMost}`, at most ten hops) and are followed by this crate, not ureq: a
  hop to another origin carries none of the caller's headers (ureq forwarded
  every header but `Authorization` and `Cookie`, so an `X-Api-Key` reached any
  host a server redirected to), a same-origin hop drops `Authorization`,
  `Cookie` and `Proxy-Authorization`, and a 307/308 of a POST is refused
  rather than replaying its body.
