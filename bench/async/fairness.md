# The fairness gate

Why the rig is its own workspace, the gate that refuses a ratio on unequal work, and the negative control that proves it can refuse. Part of the [async measurement](README.md).

## Its own workspace root, and why

The comparison needs a raw `tokio` edge the four published crates must never
author. `lgwks-deps check` discovers packages through Cargo's own
`workspace_members`, so a separate root is invisible to the gate — the same
arrangement `bench/` uses.

It is a *separate* root from `bench/`, not a second binary in it, because
`bench/` deliberately builds `lgwks_bot` with `rt` withdrawn so its measured path
is exactly the ECS schedule. Cargo features are per-package and not per-binary,
so a second binary there would have forced the async surface into that build and
made its headline "no tokio edge is compiled" claim false.

## The fairness gate, and what it caught

Every round compares the two sides' **terminal counts field by field**: tasks
placed, tasks that completed, cancelled, aborted, spawns refused, and the sum of
each body's own work counter. Any difference aborts the run and prints the field
that diverged. A ratio computed on unequal work is not a slower engine, it is a
different program.

Four real asymmetries were found and fixed *in the harness*, not in the crate:

- **`Supervisor` reports a task cancelled if the token was cancelled when the
  body returned**, even if the body finished its work first. That reading is
  correct and conservative. But `shutdown` cancels first, so draining through
  `shutdown` measures "who cancelled" rather than "who completed". The facade
  side is therefore drained with its token intact (`reap` until the completion
  counter reaches the placed count) — the drain a caller who cares about
  completion performs.
- **`Supervisor` retains terminal outcomes in a buffer capped at the in-flight
  bound** and counts the loss in `reports_dropped`. The baseline has no
  reporting layer and so bounds it at nothing. Comparing retained-list lengths
  would compare reporting granularity, not execution. The retention asymmetry is
  **reported separately**, as the asymmetry it is, and is not part of the gate.
- **A time-bounded sweep point is not the same offered work on both sides.** The
  gate refused this, naming `placed`: offering "for one second" lets the faster
  side place more arrivals, and the offered work becomes a property of each
  engine's speed. A sweep point now offers the **arrival count** its rate and
  window define, and both sides are offered exactly that.
- **The facade had no blocking join, and the drain's timer was the gap (#269).**
  `Supervisor::reap` joins only what has already finished, so a drain through it
  had to poll, and every poll slept on Tokio's timer. Tokio's timer wheel has a
  1 ms granularity, so each drain paid a fixed 1.3–1.6 ms that raw Tokio, which
  awaits `JoinSet::join_next`, never paid: that was the whole 3–5x p99 gap the
  gate reported on the quiet rows. `Supervisor::wait_idle` now joins on the task
  set's own wakeup, and every drain here awaits it, so both sides wait on the same
  primitive and no timer is left in the drain path. A scheduler round-trip in
  place of the timer was built and measured earlier and lost (3.49x / 5.09x /
  3.50x), because it reschedules the draining task behind the workers it waits on.

The gate is not decoration: it refused during development on each of the first
three, each time on a real difference between the two sides.

## The mutant baseline — proving the gate can refuse

A gate that has only ever been shown agreeing runs cannot be told apart from one
that always says yes. `--mutant-check` runs the negative control: a side that
places every task exactly as the honest facade does and then **stops draining**,
so the tally it hands the gate claims 512 placed tasks while fewer bodies were
observed to finish. The gate must refuse it, and must refuse it *for a
work-count reason*.

```sh
CARGO_TARGET_DIR=/tmp/lgwks-bench-async \
  cargo run --release --manifest-path bench/async/Cargo.toml -- --mutant-check
```

Recorded output, Apple M5 Pro, macOS 27.0, rustc 1.98.0, 512 tasks at bound 8:

```
refused, as required: completed: facade 505 vs baseline 512
the refusal names a work-count field, so the gate discriminates on work

mutant tally:  placed 512 completed 505 cancelled 0 aborted 0 work_units 509
honest tally:  placed 512 completed 512 cancelled 0 aborted 0 work_units 512
```

The mutant is the honest facade body with one parameter flipped — `Drain::GiveUp`
instead of `Drain::Complete` — so the defect is readable in a diff of one line
rather than by comparing two functions. Which field the gate *names* depends on
the order it checks them and on how far the workers got before the early reap, so
the check accepts any work-count field and prints which one fired rather than
pinning one; a slower host would name `work_units` first.

The mutant is deliberately *unfair* and not merely slow: a slower-but-identical
mutant would pass the gate and print a meaningless ratio, which is the failure
this control exists to rule out. The mutant check runs alone and exits non-zero
if the gate ever accepts it.
