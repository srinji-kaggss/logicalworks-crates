# Orchestration, compared

`script!` makes claims about behaviour: bounded fan-out, first-failure stop,
cancellation that reaches every body, retries that cannot storm, deadlines,
idempotency keys and located errors. This directory checks those claims
against the tools a team would otherwise use for the same job. It runs one
workload through eight implementations in four languages and measures both
kinds of property:

- **Semantic invariants:** what the program guarantees.
- **Non-semantic costs:** throughput, latency, memory, and the code the author
  writes.

```sh
python3 bench/orchestration/run.py --runs 5 --json results.json
python3 bench/orchestration/run.py --render results.json   # the tables below
```

The runner builds every way into a temporary directory, which it removes at
the end. It runs each (way, scenario) cell five times, one process at a time,
reads peak RSS from `/usr/bin/time -l`, and prints a line per cell followed by
the verdicts. Needs `cargo`, `go`, `node`/`npm` and `uv`. Third-party code is
pinned by lockfile or hash:

- `go/go.sum` for errgroup.
- `node/package-lock.json` for Effect.
- `python/requirements.txt`, hashed, for Trio.

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

Each hand-written way is written the way a careful author would write it:
- it bounds admission (acquires a slot before starting a body);
- it retries only retryable failures;
- it applies the deadline per attempt;
- it stops at the first failure;
- it honours the caller's cancellation;
- it builds its idempotency key as `tenant/item`.

None of them is a strawman. Only the orchestration differs between ways. The
site model (`python/site_model.py`, `node/site_model.mjs`, the `Site` in
`go/main.go` and the `Ctx` in the Rust example) is the same workload in each
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

## Results

Measured 2026-09-30 on an Apple M5 Pro (15 cores), macOS 27.0: rustc 1.98.0,
Go 1.27.1, Node 24.14.1 with Effect 3.22.2, Python 3.14.7 with Trio 0.34.0.
Lines are the code the author writes for the orchestration, counted between the
`BEGIN`/`END` markers with comments and blanks left out.

Median (min-max) of 5 runs per cell.

**Throughput** (2 tenants x 10,000 items):

| way | items/s | p50 ms | p99 ms | peak RSS MB | lines |
|---|---:|---:|---:|---:|---:|
| `rust-script` | 266556 (213777-276602) | 2.936 | 35.909 | 13.8 | 11 |
| `rust-join_all` | 347517 (327235-380887) | 2.357 | 9.465 | 9.5 | 29 |
| `rust-joinset` | 348213 (317339-353456) | 2.419 | 9.385 | 7 | 46 |
| `python-asyncio` | 98119 (84890-107434) | 9.157 | 25.463 | 44.4 | 29 |
| `python-trio` | 27482 (23142-34518) | 1.072 | 7.127 | 37.8 | 29 |
| `go-errgroup` | 374847 (330824-425437) | 3.01 | 12.068 | 28.8 | 44 |
| `node-pool` | 29607 (22852-32799) | 11.821 | 543.33 | 208.8 | 41 |
| `node-effect` | 27926 (18608-28729) | 57.363 | 111.405 | 624.7 | 23 |

**Failure behaviour** (worst case over every run):

| way | failfast ms | failfast attempts | live at return (failfast / cancel / storm) | storm attempts | deadline ms | failfast error |
|---|---:|---:|---|---:|---:|---|
| `rust-script` | 3.854 | 977 | 0 / 0 / 0 | 130 | 103.31 | `run_items/each:item#500/retry: failed: "malformed record"` |
| `rust-join_all` | 38.888 | 2000 | 0 / 771 / 0 | 5000 | 106.164 | `: failed: "malformed record"` |
| `rust-joinset` | 3.231 | 983 | 844 / 828 / 70 | 500 | 105.964 | `: failed: "malformed record"` |
| `python-asyncio` | 7.79 | 960 | 0 / 0 / 0 | 500 | 101.342 | `Permanent: malformed record` |
| `python-trio` | 33.254 | 537 | 0 / 0 / 0 | 326 | 101.616 | `Permanent: malformed record` |
| `go-errgroup` | 3.815 | 1006 | 0 / 0 / 0 | 501 | 101.104 | `malformed record` |
| `node-pool` | 18.695 | 985 | 959 / 0 / 99 | 500 | 101.767 | `Error: malformed record` |
| `node-effect` | 81.464 | 985 | 1 / 0 / 1 | 500 | 106.013 | `(FiberFailure) Error: malformed record` |

**Semantic invariants:**

| way | bound held | stops at first failure | nothing live at return | nothing finishes after return | cancel reaches every body | storm attempts (1,000 items) | deadline honoured | no duplicate effect | error names the failing item |
|---|---|---|---|---|---|---|---|---|---|
| `rust-script` | yes | yes | yes | yes | yes | 130 | yes | yes | yes |
| `rust-join_all` | yes | **no** | **no** | yes | **no** | 5000 | yes | yes | **no** |
| `rust-joinset` | yes | yes | **no** | yes | **no** | 500 | yes | yes | **no** |
| `python-asyncio` | yes | yes | yes | yes | yes | 500 | yes | yes | **no** |
| `python-trio` | yes | yes | yes | yes | yes | 326 | yes | yes | **no** |
| `go-errgroup` | yes | yes | yes | yes | yes | 501 | yes | yes | **no** |
| `node-pool` | yes | yes | **no** | yes | yes | 500 | yes | yes | **no** |
| `node-effect` | yes | yes | **no** | yes | yes | 500 | yes | yes | **no** |

### What the numbers say

**On semantics, `script!` holds every invariant in the table and the author
writes none of the machinery.** It is the only way that does.

- *Nothing outlives the call.* Under failure, cancellation and storm alike, no
  body is left running when `script!` returns. asyncio, Trio and Go errgroup
  also hold this, because structured concurrency works. The others do not:
  - tokio `JoinSet` returns with 70-844 bodies still live, because its abort is
    asynchronous.
  - `join_all_bounded` returns with 771 still live after a cancel, and it never
    stops on a failure at all: all 2,000 items ran.
  - The Node pool returns with 959 still live, because `Promise.all` rejects
    before siblings settle. Effect returns with 1.
- *Retries cannot storm.* When every attempt fails, `script!` makes 130
  attempts in total: the run's retry budget of 10, plus one per five first
  attempts. It then stops with `FlowError::Throttled`. The other ways make
  326-5,000 attempts, because each call site retries on its own. That is the
  amplification that turns a partial outage into a total one
  ([arXiv:2608.25403], [arXiv:2510.03551]).
- *The error says where.* `run_items/each:item#500/retry` names the item and
  the block. Every other way reports `malformed record` and leaves the author
  to find which of 2,000 items failed.

**On cost, `script!` is slower than hand-written Rust.** Median throughput is
267,000 items/s against 348,000, a 23% gap. p99 is 35.9 ms against 9.4 ms, and
peak RSS is 13.8 MB against 7-9.5 MB. The cause is structural. An `each` drives
its bodies on the task that awaits it, which is what lets a body borrow the
flow's locals with no `Arc` and no `'static` bound. So one fan-out uses one
core, while `JoinSet` and `join_all_bounded` spawn every body as its own task
across all 15. Even so, `script!` is faster than every non-Rust way except Go
(375,000 items/s, 28.8 MB). It needs 11 lines where the others need 23-46.

This comparison already led to one fix. `script!` used to mint a cancellation
token per item and per retry; the stop is now shared with the step (see
`Scope`). In a 9-run A/B of the throughput scenario, that moved median
throughput from 244,000 to 278,000 items/s and RSS from 17 to 13.7 MB. The p99
fell from 41 to 18 ms in the A/B, but it did not hold in the full run above
(35.9 ms), so the tail is still open. The remaining per-item cost is two scope
allocations and two path strings, and it is the next thing to take out.

### What this does not show

- It covers one machine and a synthetic site with sleeps for I/O. A real
  upstream adds its own latency distribution.
- The Node pool's tail (p99 543 ms, 209 MB) was not investigated. The pool
  builds an `AbortSignal.any` per attempt, as a careful stdlib author would;
  whether that is the cause is not measured.
- Trio's peak in flight stays under 100 in the throughput scenario although
  its bound is 960. The cause was not investigated.
- The retry budget changes an outcome on purpose. A caller who wanted five
  attempts per item does not get them when failures are systemic.
- Compile-time refusals (a typed concurrency number, `unwrap`, a detached
  spawn) are not measured here. `crates/lgwks-macros/src/tests.rs` proves them,
  and none of the other libraries builds one in.

[arXiv:2608.25403]: https://arxiv.org/abs/2608.25403
[arXiv:2510.03551]: https://arxiv.org/abs/2510.03551

[nursery]: https://vorpus.org/blog/notes-on-structured-concurrency-or-go-statement-considered-harmful/
