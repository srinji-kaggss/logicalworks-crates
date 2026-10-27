# lgwks_std 3.2.0 · lgwks_deps 5.1.0 · lgwks_ast 3.1.0 · lgwks_bot 5.0.0

2026-10-27

Six branches land together (#764 to #769). One crate takes a major bump:
`lgwks_bot`, whose `net` endpoint's `Query` output is now the paginated listing
(`Vec<json::Value>`) rather than one body, with transport failures mapped to
`Refused` or `NotDelivered` by class. No public item of the other crates is
removed. `lgwks-deps check` refuses more (`MissingLifecycle`,
`DeferredAdmitted`, `PatchExpired`, `ProviderBlocked`, all on the
`#[non_exhaustive]` `Refusal`), which `docs/releasing.md` §3 makes a minor: a
register that passed 5.0.0 can be refused by 5.1.0. `lgwks_macros` 1.1.6 is
unchanged and builds against all of them.

## lgwks_std 3.2.0

- `http::Client`: one pooled agent per client with idle, per-host and age
  limits (`PoolConfig`), `get_with` / `post_with` under per-call `Options`, and
  `get_stream` returning a streamed body (`StreamedBody::read_chunk`) or a
  server-sent event reader (`into_sse`, `SseStream::next_event`) (#712).
- `http::paginate` walks cursor or RFC 8288 `Link` pages under
  `PaginationLimits` (pages, items, bytes), each page a typed `Page` (#713).
- `http::TlsOptions`: private roots and a PEM client identity for mTLS over
  rustls; a loader refuses a bundle it cannot read rather than falling back to
  the system roots (#714). Proved end to end against a loopback server that
  requires a client certificate (`tests/it/sim_http_tls.rs`).
- `fs::stream`: a bounded streaming walk (#692).
- `process`: a non-reaping waitid that tells a stopped child from an exited one
  (`INV-STD-PROCESS-1`, #691).

## lgwks_deps 5.1.0

- `sqlite` storefront feature: `lgwks_deps::sqlite` opens one pinned SQLite
  with WAL, synchronous FULL, a busy timeout and `user_version` migrations, and
  `lgwks-deps sqlite <PATH> [TARGET [SQL ...]]` reports or migrates a store (#696).
- `lgwks-deps gate`: the consumer gate (admission, citations, requirements,
  spec owners, script check, clippy, architecture diff) with one summary and
  one exit code; `requirements`, `spec_owners` and `doc_citations` are the
  library modules `logical_ci` calls (#695).
- The supply-chain ledger: `contract/LIFECYCLE.toml` (a replacement, removal
  trigger and review date per edge), `contract/DEFERRAL.toml` (crates deferred
  until a measured trigger), `contract/PATCHES.toml` (expiring upstream pins,
  never forks) and the TLS provider gate (#722 to #728).
- Storefront features `axum`, `tower-http` and `llm-guard`, default-off with
  their costs in ADR 0003 (#721).
- `rustls` leaves the deferral catalog: its trigger, TLS terminated in-process,
  is met by the mTLS loopback test. It stays a dev-only edge of `lgwks_std`
  with a lifecycle row, beside `rcgen`, `rsa` and `rand_core`.

## lgwks_ast 3.1.0

- The `script!` lexicon and refusals cover the new vocabulary words below.

## lgwks_bot 5.0.0

- `domain::net`: every endpoint holds one pooled client built from
  `LGWKS_NET_ROOTS`, `LGWKS_NET_CLIENT_CERT` and `LGWKS_NET_CLIENT_KEY`; `poll`
  previews a stream or an event, `query` walks the listing, and a new `Execute`
  posts a body (#712 to #714). Breaking: the `Query` output.
- `script` vocabulary: `justify`, `graph`, `compensate`, a `WorkQueue`-backed
  `stream` and typed domains (#649, #650, #651, #653, #654, #657, #694).
- Supervised processes: a gated start that records the child's identity before
  its payload runs, deny-one-directory confinement, UTF-8 sandbox profiles
  (#689 to #691), and piped stdin fed concurrently under its stated bound.
- `TenantStore`: tenant policies, run rows and lane rows with per-tenant keyed
  reads in one append-only, `sync_all`'d framed file under an OS file lock,
  which is what `logical_ci` needed without its direct `rusqlite` edge (#317).
- The `estate` smoke binary proves the process doors and the vocabulary on the
  shipped artifact.

## Verifying the release

```sh
cargo run -p lgwks_deps --bin published-consumer
```

The release is done at `published consumers: all passed`.
