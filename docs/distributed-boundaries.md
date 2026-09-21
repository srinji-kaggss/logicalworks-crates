# Distributed-mesh boundaries

This document records what `lgwks_std` and `lgwks_bot` provide and refuse on a
mesh data path. Gaps are named explicitly, on the same principle as the
dependency doctrine (`docs/dependency-doctrine.md` §6), so that a caller neither
assumes coverage nor adds an unregistered dependency. Each row names the owner of
the capability: implemented here, caller policy, storefront BOUNDARY (requires
`lgwks_deps` admission with a concrete reason), or explicitly out of scope.

## Transport

| Capability | Status | Owner |
|---|---|---|
| Blocking GET/POST, rustls-only TLS, strict absolute-URL validation | Shipped | `lgwks_std::http` |
| One attempt per call; timeouts surface as `Timeout`, statuses never error | Shipped, by design | `lgwks_std::http` |
| Retries, exponential backoff, total deadlines | Caller policy | `lgwks_std::retry::RetryPolicy` (zero-dep values) + caller sleep |
| `Idempotency-Key` attachment | Shipped helper | `Options::idempotency_key` (caller generates the key) |
| Connection pooling / keep-alive reuse | Not implemented | Caller concern; the client builds one agent per call. Bursts stay bounded via `lgwks_bot::rt::task::join_all_bounded` |
| Per-phase timeouts (connect / TLS / TTFB / body) | Single total `timeout` only | `lgwks_std`: needs a stated use case before growing `Options` |
| mTLS, custom CA bundles, client identity material | Missing | Storefront BOUNDARY (`rustls-pki`/`pem`-class crate with reason); `std` has no PKI loader |
| Pagination cursors, streaming bodies, SSE | Missing | `lgwks_std` for sync-reader shapes; async streaming is a storefront transport decision |
| Raw-socket egress policy | Caller applies policy | `lgwks_bot::rt::net` explicitly does NOT pass the HTTP gate |

## Identity and keys

| Capability | Status | Owner |
|---|---|---|
| UUID v4 generation + parse | Shipped | `lgwks_std::id` (feature `random`) |
| Time-ordered IDs (UUIDv7-class) for WAL keys / dedup | Missing, recorded | `lgwks_std::id` (needs only `time` + `random`; ELIMINATE, not a new crate) |
| Keyed BLAKE3 MAC primitive | Shipped | `lgwks_std::hash::keyed` |
| Key sourcing, rotation, keyring loading | Out of scope | Host secret manager; a keyring crate would be a storefront BOUNDARY |
| Peer signatures (`ed25519`-class) | Missing | Storefront BOUNDARY, never `lgwks_std` directly |

## Time

| Capability | Status | Owner |
|---|---|---|
| UTC RFC 3339 + proleptic Gregorian calendar | Shipped | `lgwks_std::time`; no IANA zones |
| Monotonic clock for backoff/timeout authors | Split: wall in `lgwks_std::time`, `Instant` via `lgwks_bot::rt::time` | Documented friction; sync callers use `std::time::Instant` |
| Clock-skew bound / NTP discipline | Out of scope | Meshes carry a skew-bound config and refuse past it; an NTP client would be a storefront BOUNDARY |
| Leap-second round-trip | Defined as fold (not byte-preserving) | `lgwks_std::time` |

## Replication and coordination

Write-ahead log, snapshots, membership/gossip, failure detection, and
consensus are **explicitly out of scope** for `lgwks_std` / `lgwks_bot`.
`online::is_online` is a boolean TCP probe, not a detector (no latency, no
phi-accrual, no flap damping); `domain::net` is a single-endpoint poll. A
Raft-class edge would be a new `lgwks_deps` BOUNDARY plus a new bot-domain
surface, approved as a recorded decision, never a silent addition, and never a
faked consensus.

## Backpressure

- `join_all_bounded(limit, ...)` (bot, feature `sync`) never exceeds `limit`
  in flight, runs every input, returns input order. Raw `JoinSet` gives
  completion order and no ceiling. Prefer the bounded form.
- `lgwks_std::task::join_all` is unbounded O(n): callers chunk it themselves.
- Channel re-exports (`rt::sync`) carry no default bound: choose depth plus an
  overflow policy (drop-oldest / drop-newest / block-with-deadline) per queue.
- `Bot::tick` polls in waves of `MAX_IN_FLIGHT_POLLS = 32`; `BotSpec` JSON is
  capped at 1 MiB before validation.

## Observability

`log` / `tracing` / `env_logger` are not capabilities of this workspace
(doctrine §6), and an earlier revision of this section (which stated that
machine output stays parseable via `eprintln!` / `stderr`) is **superseded**
(doctrine §6.1). `print_stdout` and `print_stderr`
are `forbid` in the workspace lint table, so library code has no print path at
all: it returns information in its result and error types. Only a binary writes,
through an explicit locked handle, so a broken pipe is an ordinary `Err` rather
than a panic. Machine output must still stay parseable
(`experience/invariants/sdk.yaml`).

A mesh that needs spans, W3C `traceparent` propagation, or counters admits
`tracing` / `metrics` through the storefront as BOUNDARY edges. A hand-rolled
facade in `lgwks_std` is not the mechanism.

## Schema evolution

- `lgwks_std::wire` (rkyv) is deterministic internal binary with no envelope:
  version the envelope before putting it on a wire that outlives one deploy.
- `BotSpec` uses `deny_unknown_fields`, correct for strict manifests, wrong
  for rolling mesh channels. `BotSpec` has no `version` field yet; adding one
  plus a per-channel unknown-field policy is recorded future work.
- `ron` is config-format, not wire: same versioning absence, same rule.
