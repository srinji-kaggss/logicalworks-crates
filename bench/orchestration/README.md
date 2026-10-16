# Orchestration, compared

`script!` makes claims about behaviour: bounded fan-out, first-failure stop,
cancellation that reaches every body, retries that cannot storm, deadlines,
idempotency keys and located errors. This directory checks those claims
against the tools a team would otherwise use for the same job. It ran one
workload through eight implementations in four languages and measured both
kinds of property:

- **Semantic invariants:** what the program guarantees.
- **Non-semantic costs:** throughput, latency, memory, and the code the author
  writes.

The measured record, from 2026-09-30, is in [`RESULTS.md`](RESULTS.md).

## What runs today

The Python runner (`run.py`) and the two Python ways (`asyncio_taskgroup.py`,
`trio_nursery.py`, with `site_model.py`) were deleted in `22a19af25` (#537),
when the repository went Rust and C++ only; the Trio pin they read
(`python/requirements.in` and `requirements.txt`) went with them. Nothing now
builds every way, repeats a cell five times or reads peak RSS, so no command
here reproduces the tables: they record the run that made them. The ways whose
sources remain each run one scenario per call:

```text
cargo run --release -p lgwks_bot --example compare_orchestration -- <way> <scenario>
(cd bench/orchestration/go && go run . <scenario>)
(cd bench/orchestration/node && node pool.mjs <scenario>)    # or effect.mjs
```

`<way>` for the Rust example is `script`, `join_all`, `joinset` or `host`
(`Host::run` per item); `host` has no row in the 2026-09-30 tables.
Third-party code is pinned by checksum file or lockfile:

- `go/go.sum` for errgroup.
- `node/package-lock.json` for Effect.

## The ways

| way | what it is | why it is here |
|---|---|---|
| `rust-script` | `script!` flow (`each` / `retry` / `within`) | the subject |
| `rust-join_all` | `lgwks_bot::rt::task::join_all_bounded` + a hand retry/timeout loop | the estate's own primitive, written by hand |
| `rust-joinset` | tokio `JoinSet` + `Semaphore` + hand retry/timeout, stop on first error | what most tokio code reaches for |
| `python-asyncio` | `asyncio.TaskGroup` + `Semaphore` + `asyncio.timeout` | stdlib structured concurrency (3.11+) |
| `python-trio` | Trio nursery + `Semaphore` + `fail_after` | the origin of structured concurrency ([Smith 2018][nursery]) |
| `go-errgroup` | `errgroup.WithContext` + `SetLimit` + `context.WithTimeout` | the canonical Go answer |
| `node-pool` | bounded worker pool + `AbortController` + `AbortSignal.timeout` | the stdlib Node answer |
| `node-effect` | Effect-TS `forEach({concurrency})` + `retry(Schedule)` + `timeoutFail` | the closest declarative sibling: typed errors, fibers, interruption |

The two `python-` ways no longer have a source; their rows in the results are
the record. Each hand-written way is written the way a careful author would
write it:
- it bounds admission (acquires a slot before starting a body);
- it retries only retryable failures;
- it applies the deadline per attempt;
- it stops at the first failure;
- it honours the caller's cancellation;
- it builds its idempotency key as `tenant/item`.

None of them is a strawman. Only the orchestration differs between ways. The
site model (`site_model.mjs`, the `Site` in `go/main.go` and the `Ctx` in the
Rust example, and the deleted `site_model.py`) is the same workload in each
language.

## The workload

| scenario | tenants x items | in flight | per attempt | attempts | failure |
|---|---|---|---|---|---|
| `throughput` | 2 x 10,000 | 64 per core | 1 ms | 3, 5 ms apart | one item in 97 fails once, transiently |
| `failfast` | 1 x 2,000 | 64 per core | 1-20 ms | 1 | item 500 fails permanently (`malformed record`) |
| `cancel` | 1 x 2,000 | 64 per core | 50 ms | 1 | the caller cancels after 10 ms |
| `storm` | 1 x 1,000 | 100 | 1 ms | 5, no wait | every attempt fails transiently |
| `deadline` | 1 x 1 | 1 | 2 s | 1 | a 100 ms deadline per attempt |

Every body is counted in on entry and out on any exit, including a drop or a
cancel. After the call returns, the harness records how many bodies are still
live, waits 200 ms, and records how many are live then and how many finished
after the return. The failfast error text deliberately does not name the item,
so any location in the reported error must come from the orchestration.

## What the results do not show

- It covers one machine and a synthetic site with sleeps for I/O. A real
  upstream adds its own latency distribution.
- The Node pool's tail (p99 409 ms, 208 MB) was not investigated. The pool
  builds an `AbortSignal.any` per attempt, as a careful stdlib author would;
  whether that is the cause is not measured.
- Trio's peak in flight stays under 100 in the throughput scenario although
  its bound is 960. The cause was not investigated.
- The retry budget changes an outcome on purpose. A caller who wanted five
  attempts per item does not get them when failures are systemic.
- Compile-time refusals (a typed concurrency number, `unwrap`, a detached
  spawn) are not measured here. `crates/lgwks-macros/src/tests.rs` proves them,
  and none of the other libraries builds one in.

[nursery]: https://vorpus.org/blog/notes-on-structured-concurrency-or-go-statement-considered-harmful/
