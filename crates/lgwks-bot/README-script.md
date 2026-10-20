# lgwks_bot: orchestration with script!

Pages of this README: [overview](README.md) · [execution model](README-execution.md) · [failure](README-failure.md) · [authority, domains and specs](README-authority.md) · [script!](README-script.md) · [async runtime](README-runtime.md) · [supervision](README-supervision.md)

## Orchestration: `script!`

`script!` (feature `script`, on by default) writes orchestration the way it is
said: indented flows of `each`, `within`, `retry`, `together`, `step`, `for` and
`if`, which expand to plain `async fn`s over `lgwks_bot::script`. It exists
because orchestration is where generated code fails most: fan-outs with no
bound, retries with no budget, waits with no deadline, work that loses track of
which tenant it is for, and a system nobody can see the shape of. The language
has no way to write the first three, and makes the last two automatic.

```rust
use lgwks_bot::script::{FlowError, Scope, Tenant};

lgwks_bot::script! {
    /// Square every number, as many at once as the machine sustains; each
    /// attempt gets a second and a failure worth retrying is retried under
    /// the same key.
    pub flow squares(numbers: Vec<u64>) -> u64:
        let squared = each number in numbers:
            retry up to 3 times, waiting 10ms:
                within 1s:
                    number.saturating_mul(number)
        give back squared.iter().sum()
}

fn main() -> Result<(), FlowError> {
    let scope = Scope::root(Tenant::new("acme")?);
    let total = lgwks_bot::block_on(squares(&scope, vec![1, 2, 3]))?;
    assert_eq!(total, 14);
    assert!(ARCHITECTURE.to_string().contains("each number in numbers"));
    Ok(())
}
```

The code reads top to bottom like a synchronous script; what runs is
asynchronous, concurrent and bounded. What every flow gets without asking:

- **The computer picks the numbers people get wrong.** `each` with no bound
  runs 64 bodies per available core, capped at 65,536; a concurrency number
  typed as a literal is a compile error, because it is right on one host and
  wrong on the next. A limit that comes from outside (an upstream's quota) is
  written `at most (quota) at once`.
- **Retries cannot storm.** `retry up to N times` is also bounded by a budget
  shared by every `retry` under the root scope: 10 retries, plus one for every
  five first attempts (Finagle's `RetryBudget` defaults). One flaky step keeps
  its full `N`; a thousand items failing together spend about 210 retries,
  not thousands, and the refusal is a typed `Throttled`. Per-call-site retry
  counts are how retry storms happen: under correlated failure a naive policy
  cut success from 55.4% to 41.5% against not retrying at all, while a budget
  held the baseline ([arXiv:2608.25403](https://arxiv.org/abs/2608.25403));
  the same loop sustains metastable outages
  ([arXiv:2510.03551](https://arxiv.org/abs/2510.03551)).
- **One tenant, one scope.** Every flow takes a `Scope` first. A step's
  `scope.key()` is BLAKE3 over its tenant, its run and its *structural* path
  (`crawl/each:page#41/retry`), never a line number, so the key an effect is
  deduplicated by survives an edit, a retry and a restart, two tenants never
  share one, and two runs of one flow never share one either. Work that is one
  effect across runs — one charge per order — names itself with
  `scope.business_key(name, &input)` instead.
- **Stop reaches everything.** The scope carries a `CancellationToken`; a
  supervisor's shutdown, a failed sibling or a caller's cancel stops every
  body beneath it, and no new step starts after a stop.
- **Located failures.** A `FlowError` names the path it happened at and
  whether it is worth retrying (`is_retryable`).
- **A map.** Every script emits `ARCHITECTURE`: its flows, their signatures,
  which flows each one `run`s and the tree of blocks, as text or JSON.
- **Refusals.** `.unwrap()`, `.expect()`, `panic!` and friends, `loop`,
  `while`, `spawn`, unbounded channels, `block_on`, `thread::sleep`, `unsafe`
  and machine-specific absolute paths are compile errors naming what to write
  instead.

`each` runs its bodies on the calling task, not as spawned tasks, so a body may
borrow the flow's locals: no `Arc`, no `clone`, no `'static`. It polls at most
128 bodies before it yields to the executor. A flow is still `Send` whenever
what it holds is, so each tenant's flow can be spawned onto its own worker.
One fan-out uses one core, which is the price of the borrowing. An `each` body
and a `retry` attempt share their step's stop, so `scope.cancel()` inside a
body ends the whole fan-out, like `break`.

[`bench/orchestration`](../../bench/orchestration/README.md) measures this
against `join_all_bounded`, tokio `JoinSet`, asyncio, Trio, Go `errgroup`, a
Node pool and Effect-TS. `script!` holds every invariant measured there,
including no body left running at return and 130 attempts in a retry storm
where the others make 368-5,000. On an idle host it ran 435,000 items/s
against 316,000-334,000 for hand-written Rust, with a p99 of 12.0 ms against
9.5 ms. An earlier run on a loaded host, whose tree was not recorded,
measured 267,000 while they held about 348,000, likely because an `each`
drives its bodies on one task. The language reference is the
[`lgwks_macros`](https://docs.rs/lgwks_macros) crate documentation;
`examples/script_tenants.rs` crawls 10,000 pages for two tenants at once and
prints the counts, the timing against a hand-written `join_all_bounded`, and
the map.
