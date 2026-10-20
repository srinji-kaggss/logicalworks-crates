# The measurement rigs

Three instruments live here. The first two are each their own Cargo workspace
root, so neither is seen by the estate's dependency contract; the third is an
example of `lgwks_ast`.

- **The `lgwks_bot` rig** (this directory), which measures the bot against a
  hand-rolled baseline doing the same work. Pages: [headline](bot-headline.md),
  [per-stage profile and allocations](bot-profile.md), [method](bot-method.md),
  [analysis](bot-analysis.md), [defects, exclusions and what the multiplier
  buys](bot-defects.md).
- **[`std-measure/`](std-measure/)**, the `lgwks_std` before/after harness for
  issues #153, #154, #160 and #164. It reproduces the #154 G2 table and the
  #164 retry flat-latency rows from raw per-call samples, with its committed
  output in [`std-measure/results.txt`](std-measure/results.txt).
- **The `lgwks_ast` parse-budget rig**, produced by
  `crates/lgwks-ast/examples/parse_budget.rs`, with its committed evidence in
  [`ast-budget.tsv`](ast-budget.tsv). Pages: [throughput and peak
  RSS](ast-budget.md), [adversarial shapes, markdown and fan-out](ast-adversarial.md).

---

# The `lgwks_bot` benchmark rig

This directory measures `lgwks_bot` against a hand-rolled baseline that performs
the *same work*, and reports where the bot is faster, where it is slower, and by
how much. It is a measurement instrument, not a consumer of the crate: it is its
own Cargo workspace root so that the estate's dependency contract never sees it,
and it is not published.

## Two rigs, two claims

| directory | measures | build |
|---|---|---|
| `bench/` (this one) | the **synchronous** ECS schedule against a hand-rolled loop | `lgwks_bot` with `rt` withdrawn — no tokio edge compiled at all |
| `bench/async/` | `lgwks_bot::rt` against **pinned raw Tokio**, matched semantics | `lgwks_bot` with `rt`, plus a raw `tokio` edge the estate never authors |

They are separate roots rather than two binaries here because Cargo features are
per-package and not per-binary: a second binary in this workspace would have
forced the async surface into the build below and made its "no tokio edge"
headline false. Both roots are invisible to `lgwks-deps check`, which discovers
packages through Cargo's own `workspace_members`.

`bench/async/` carries its own fairness gate — it compares terminal counts field
by field and aborts on any mismatch — and its own results, in
[`async/README.md`](async/README.md).

## Running it

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-target \
  cargo run --release --locked --manifest-path bench/Cargo.toml -- --json=bench/results.json
```

`--capcheck-only` skips the timing scenarios. The rig writes a JSON copy of its
results and prints a human table; `results.json` in this directory is the
committed record of the run in the headline. Two more doors:

```sh
# where a tick goes, stage by stage, measured on the tick path itself
cargo run --release --locked --manifest-path bench/Cargo.toml -- --profile

# heap allocations per steady-state tick, both engines, over the same window
cargo run --release --locked --manifest-path bench/Cargo.toml -- --alloc-report

# one row, both doors, the width #640 added
cargo run --release --locked --manifest-path bench/Cargo.toml -- \
  --scenario wide-4096x10 --alloc-report --profile
```

`--scenario NAME` (or `--scenario=NAME`) restricts the timing run and both
doors to that row; both doors run when both are asked for, in that order.

Both are separate from the timing run and neither number is taken from a
counting or an instrumented window: the counters and the clock reads are real
costs, and a ratio taken from a window that pays for them would be a ratio
about the instrument.

## The result in one table

One run on main `940e253cd` with #279's changes. The reading, the run-to-run
spread, the earlier run it is compared with and what each row is made of are in
[the headline](bot-headline.md).

| scenario | bot ns/tick | baseline ns/tick | ratio (bot/baseline) | 95% CI |
|---|---:|---:|---:|---|
| `poll-only-64x100` | 896.0 | 32.3 | 27.58x | [27.40, 28.05] |
| `steady-64x100` | 2 055.4 | 32.4 | 63.65x | [63.09, 64.20] |
| `churn-64x1` | 133 986.7 | 51.5 | 2 605.27x | [2 547.15, 2 662.27] |
| `fanout-1x64` | 110 259.8 | 42.7 | 2 574.46x | [2 527.44, 2 588.59] |
| `wide-256x10` | 54 248.3 | 135.6 | 401.05x | [397.17, 410.04] |

**The wide family, measured again for #640** (2026-10-20, load 2.02 to 2.27
across the three doors). The four rows hold the change-tick fraction at one in
ten and vary only the source count; the run fits them at **slope 1.206** (2.31x
per doubling) and prints that line itself, and `--profile` puts the superlinear
term in `act` ([§4.9](../../docs/production-readiness/4-9-performance.md)):

| scenario | bot ns/tick | baseline ns/tick | ratio | 95% CI |
|---|---:|---:|---:|---|
| `wide-1024x10` | 238 705.6 | 530.6 | 451.45x | [440.97, 458.55] |
| `wide-4096x10` | 1 167 018.7 | 2 053.1 | 568.06x | [550.42, 575.49] |
| `wide-16384x10` | 8 471 299.9 | 8 490.6 | 996.37x | [988.15, 1 004.88] |

**Machine and toolchain.** Apple M5 Pro, 15 cores, macOS 27.0, rustc 1.99.0,
`opt-level = 3`, `lto = true`, `codegen-units = 1`. One machine: these are
absolute numbers for this host, not a cross-platform claim.
