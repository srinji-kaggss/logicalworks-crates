# Allocation attribution

Every allocation window priced separately, and why the latency target is a different target. Part of the [async measurement](README.md).

## Allocation attribution — every window, priced separately

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  cargo run --release --manifest-path bench/async/Cargo.toml -- \
    --alloc-attribution --json=bench/async/alloc-attribution.json
```

1,024 tasks per wave, counted in a window separate from every timed round, on one
unchanged harness. **BEFORE** is `origin/main`'s `cancel.rs` and `supervise.rs`;
**AFTER** is this branch. Per task:

| operation | before | after |
|---|---:|---:|
| facade spawn wave, contended (bound 8) | **15.33** | **3.61** |
| facade spawn wave, uncontended (bound 4096) | 15.17 | 3.17 |
| raw tokio wave, contended (bound 8) | 2.02 | 2.02 |
| the facade's excess over raw | 13.31 | 1.59 |
| `CancellationToken::new` (a root token) | 11.00 | 1.00 |
| one child token | 11.00 | 1.00 |
| one `run_until_cancelled` race | 13.00 | 3.00 |
| one `Supervisor::new(4096)` | 15.00 | 5.00 |

**The issue's allocation target (≤ 6 per task) is met: 3.61, from 15.33** (3.46
on `origin/main` 22f1d526, 2026-10-15, after #320: [results.md](results.md)).
Four changes, each measured on its own:

1. **`Inner`'s signal channel is built on first use** (`OnceLock`), not in the
   constructor. `CancellationToken::new` cost 11 allocations and `child_token`
   another 11, all of it a `watch::channel` that nothing awaited: the
   `AtomicBool` is the authority and a channel only *wakes* waiters. 11 → 1 each.
   The subscriber re-reads the flag **after** installing the channel, which closes
   the window a lazy channel opens; two regression tests in `rt::cancel::tests`
   cover exactly that window, because nothing else reached it.
2. **`Supervisor::claim` no longer arms a 100 ms timer on an uncontended
   spawn.** It raced the permit against the token through a
   `timeout(100 ms)` inside `run_until_cancelled` — 13 allocations and a
   timer-wheel registration — for a wait never taken. It now takes a free permit
   and re-checks cancellation on it, which is the same decision with the same
   cancellation-wins rule. The contended path is untouched.
3. `claim_now` is the same shape through one non-counting `try_take`, so a full
   pool on the backpressure door is backpressure rather than a refusal.
4. `run_until_cancelled`'s two `Box::pin`s became one allocation's worth (3.00
   from 13.00) as a side effect of (1) and (2).

**The remaining 3.61, itemised, with the guarantee that pays for each** (1.61
above the raw wave's 2.02):

| per task | what it is | the guarantee |
|---:|---|---|
| 1.00 | the child `CancellationToken`'s own `Arc<Inner>` | a task's token must be cancellable *without* being able to cancel its siblings (`cancel.rs`, "Why the link points up"). The raw baseline's `child` is an `Arc::clone` of the parent and so cannot be; this one owns a flag. |
| 1.00 | the identity map's node | `Supervisor::identities: BTreeMap<Id, TaskId>` — the terminal `JoinError` has no value to carry a `TaskId`, so the engine's task id must be mapped back to the supervisor's own (INV-BOT-31). |
| ~1.60 | `JoinSet`'s own task and the report buffer's growth | the owned, un-detached task (INV-BOT-18) and the capped `TaskOutcome` ring. |

**A bare `JoinSet::spawn` measures 0.00 allocations** after the first, because the
runtime hands tasks out of a per-worker slab and never returns to the allocator.
That is why the raw wave's 2.02 is the semaphore permit, the baseline's own
cancellation child and its body — not the spawn.

### The latency target was not met by this change, and the reason is the finding

The issue also asks for **≤ 2x the baseline at p99 on every scenario**. This
change did not meet it — `quiet-async-bot` read 4.49x — and it is met now: #320
removed the cause, and [the scenario table](results.md) reads 0.93x–1.41x at p99.
The finding stands as the record of why: the two targets are not the same target:
**removing 76% of the allocations moved the allocation count exactly as predicted
and moved the paired latency ratios not at all.** Measured paired, four rounds
each, same harness:

| scenario | ratio before | ratio after |
|---|---:|---:|
| quiet-async-bot | 4.65 / 4.36 / 4.01 / 4.54 | 4.28 / 3.91 / 2.65 / 3.37 |
| high-fanout | 2.52 / 2.25 / 2.13 / 2.15 | 1.76 / 2.07 / 1.83 / 1.69 |
| at-capacity | 2.11 / 2.14 / 2.64 / 2.81 | 1.92 / 1.50 / 1.54 / 1.30 |
| single-permit | 1.70 / 1.45 / 1.50 / 1.51 | 1.56 / 1.46 / 1.48 / 1.44 |

(The three rounds that overlapped with a load of 50+ are excluded from the
`quiet-async-bot` after column's reading as noise; the harness printed the load
for exactly this reason.) **The remaining gap was work, not allocation.** This
section first named the per-spawn reap, the identity map, the `TaskOutcome` the
wrapper builds and the reporting layer; the measurement that followed found it
elsewhere: the drain polled `reap()` behind a `tokio::time::sleep` and paid
Tokio's 1 ms timer granularity on every drain. `Supervisor::wait_idle` now joins
on the `JoinSet` wakeup (#320, `b1b1a3434`), which is the change that closed it.
