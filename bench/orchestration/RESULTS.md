# Orchestration, compared: results

The record of one run, 2026-09-30. Its runner (`run.py`) was deleted in `22a19af25` (#537), so no
command reproduces it. [`README.md`](README.md) describes the workload and the ways, says what
still runs, and lists what these results do not show.

Measured on an Apple M5 Pro (15 cores), macOS 27.0: rustc 1.99.0, Go 1.27.1, Node 24.14.1 with
Effect 3.22.2, Python 3.14.7 with Trio 0.34.0, on an idle host: the run started after three
readings 20 s apart below a one-minute load of 4, and its load was 3.3 at the start and 3.1 at the
end (the 5- and 15-minute loads were still 7.8 and 11.9, so the machine had just come off heavy
work). The Rust ways were built with this machine's `$CARGO_HOME` release profile, which overrides
the repository's: thin LTO, 8 codegen units, `panic = "abort"` and `target-cpu=native`. The Rust
ways compare fairly with each other; absolute numbers and comparisons across languages hold for
this host only.

An earlier run of the same harness was taken while other builds were running on the machine. Its
load was not recorded, and neither was the tree it built, except that it ran before this code
landed, so load and code changes cannot be fully separated in it. In that run most ways were slower
than they are here: `script!` ran 267,000 items/s with a 35.9 ms p99, asyncio 98,000, Trio 27,000,
the Node pool 30,000 and Go errgroup 375,000. The two hand-written Rust ways were the exception:
`join_all_bounded` ran 348,000 at 9.5 ms and `JoinSet` 348,000 at 9.4 ms, no slower than their
334,000 and 316,000 here. So `script!` is the only Rust way that slowed. An `each` drives all its
bodies on one task (see below), so one fan-out uses one core, as asyncio and Trio do on their
single-threaded loops, and those three lost the most (loaded throughput 0.61, 0.68 and 0.63 of
idle), while the Node ways, also on one thread, lost less (0.76 and 0.93). That is the likely
reason `script!` slowed where the hand-written Rust ways, which spread bodies across tokio's worker
threads, did not; no variant isolates that factor. Every yes/no verdict in the invariants table
was the same in both runs; some counts behind them moved, for example Trio's storm attempts (326
then 368) and the bodies left live by `JoinSet` and `join_all_bounded`. Lines are the code the
author writes for the orchestration, counted between the `BEGIN`/`END` markers with comments and
blanks left out.

Median (min-max) of 5 runs per cell.

**Throughput** (2 tenants x 10,000 items):

| way | items/s | p50 ms | p99 ms | peak RSS MB | lines |
|---|---:|---:|---:|---:|---:|
| `rust-script` | 435142 (425224-472199) | 2.235 | 12.017 | 13.6 | 11 |
| `rust-join_all` | 333655 (306748-338718) | 2.332 | 9.527 | 9.5 | 29 |
| `rust-joinset` | 316215 (300874-354880) | 2.577 | 9.477 | 7 | 46 |
| `python-asyncio` | 143589 (133064-144177) | 7.16 | 13.606 | 44.2 | 29 |
| `python-trio` | 43582 (43184-44052) | 1.046 | 7.092 | 37.3 | 29 |
| `go-errgroup` | 457622 (447740-471323) | 2.502 | 11.264 | 29.4 | 44 |
| `node-pool` | 38780 (37603-39460) | 9.634 | 409.09 | 208.1 | 41 |
| `node-effect` | 30154 (29272-30415) | 54.61 | 108.089 | 533.8 | 23 |

**Failure behaviour** (worst case over every run; the error text is from the first run):

| way | failfast ms | failfast attempts | live at return (failfast / cancel / storm) | storm attempts | deadline ms | failfast error |
|---|---:|---:|---|---:|---:|---|
| `rust-script` | 4.482 | 984 | 0 / 0 / 0 | 130 | 106.764 | `run_items/each:item#500/retry: failed: "malformed record"` |
| `rust-join_all` | 40.851 | 2000 | 0 / 825 / 0 | 5000 | 106.25 | `: failed: "malformed record"` |
| `rust-joinset` | 3.015 | 977 | 836 / 846 / 70 | 500 | 106.455 | `: failed: "malformed record"` |
| `python-asyncio` | 6.213 | 960 | 0 / 0 / 0 | 500 | 101.418 | `Permanent: malformed record` |
| `python-trio` | 18.984 | 543 | 0 / 0 / 0 | 368 | 101.466 | `Permanent: malformed record` |
| `go-errgroup` | 4.306 | 1012 | 0 / 0 / 0 | 501 | 101.21 | `malformed record` |
| `node-pool` | 13.039 | 985 | 959 / 0 / 99 | 500 | 101.936 | `Error: malformed record` |
| `node-effect` | 80.768 | 985 | 1 / 0 / 1 | 500 | 106.71 | `(FiberFailure) Error: malformed record` |

**Semantic invariants:**

| way | bound held | stops at first failure | nothing live at return | nothing finishes after return | cancel reaches every body | storm attempts (1,000 items) | deadline honoured | no duplicate effect | error names the failing item |
|---|---|---|---|---|---|---|---|---|---|
| `rust-script` | yes | yes | yes | yes | yes | 130 | yes | yes | yes |
| `rust-join_all` | yes | **no** | **no** | yes | **no** | 5000 | yes | yes | **no** |
| `rust-joinset` | yes | yes | **no** | yes | **no** | 500 | yes | yes | **no** |
| `python-asyncio` | yes | yes | yes | yes | yes | 500 | yes | yes | **no** |
| `python-trio` | yes | yes | yes | yes | yes | 368 | yes | yes | **no** |
| `go-errgroup` | yes | yes | yes | yes | yes | 501 | yes | yes | **no** |
| `node-pool` | yes | yes | **no** | yes | yes | 500 | yes | yes | **no** |
| `node-effect` | yes | yes | **no** | yes | yes | 500 | yes | yes | **no** |

### What the numbers say

**On semantics, `script!` holds every invariant in the table and the author writes none of the
machinery.** It is the only way that does.

- *Nothing outlives the call.* Under failure, cancellation and storm alike, no body is left
  running when `script!` returns. asyncio, Trio and Go errgroup also hold this, because structured
  concurrency works. The others do not:
  - tokio `JoinSet` returns with 70-846 bodies still live, because its abort is asynchronous.
  - `join_all_bounded` returns with 825 still live after a cancel, and it never stops on a failure
    at all: all 2,000 items ran.
  - The Node pool returns with 959 still live, because `Promise.all` rejects before siblings
    settle. Effect returns with 1.
- *Retries cannot storm.* When every attempt fails, `script!` makes 130 attempts in total: the
  run's retry budget of 10, plus one per five first attempts. It then stops with
  `FlowError::Throttled`. The other ways make 368-5,000 attempts, because each call site retries on
  its own. That is the amplification that turns a partial outage into a total one
  ([arXiv:2608.25403], [arXiv:2510.03551]).
- *The error says where.* `run_items/each:item#500/retry` names the item and the block. Every other
  way reports `malformed record` and leaves the author to find which of 2,000 items failed.

**On cost, on an idle host `script!` is not slower than the hand-written Rust.** Median throughput
is 435,000 items/s against 334,000 for `join_all_bounded` and 316,000 for `JoinSet`. Its slowest
run (425,000) beat the fastest run of each hand-written way (339,000 and 355,000). Its p99 is
higher and less steady: a median of 12.0 ms, 9.7-17.4 ms across runs, against 9.4-9.6 ms for
`join_all_bounded`. Its peak RSS is 13.6 MB against 7-9.5 MB. An `each` drives its bodies on the
task that awaits it rather than spawning each as a task, which is what lets a body borrow the
flow's locals with no `Arc` and no `'static` bound. It also means one fan-out uses one core, which
is the likely reason it slowed under load when the hand-written Rust ways did not (see above). No
variant isolates that factor, and why it comes out ahead when idle was not measured. Go errgroup
has the highest median, 458,000 items/s (448,000-471,000), with 29.4 MB; its range overlaps that of
`script!`. `script!` needs 11 lines where the others need 23-46.

This comparison already led to one fix. `script!` used to mint a cancellation token per item and
per retry; the stop is now shared with the step (see `Scope`). In a 9-run A/B of the throughput
scenario, that moved median throughput from 244,000 to 278,000 items/s and RSS from 17 to 13.7 MB.
The A/B ran on a loaded host, so its absolute numbers are not comparable with the table above; only
the difference between its two arms is. Its output was printed and not saved, so no file here
reproduces those figures. The p99 gap to hand-written Rust (12.0 against 9.5 ms) is still open. The
remaining per-item cost is two scope allocations and two path strings.

[arXiv:2608.25403]: https://arxiv.org/abs/2608.25403
[arXiv:2510.03551]: https://arxiv.org/abs/2510.03551
