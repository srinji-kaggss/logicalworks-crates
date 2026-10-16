# The `lgwks_bot` rig: a defect it found, what it excludes, and what the multiplier buys

Part of the [measurement rigs](README.md).

## A defect this rig found, and the fix it was measured against

**The table below is the pre-fix measurement, kept because it is the evidence
the fix was made on. The measurement of the fixed code follows it.**

| required caps | ns/call | ns per cap |
|---:|---:|---:|
| 1 | 3.40 | 3.40 |
| 8 | 135.59 | 16.95 |
| 32 | 1 029.85 | 32.18 |
| 128 | 12 886.19 | 100.67 |

`Auth::check` iterated the required capabilities and asked, for each, whether a
`Vec<Cap>` contains it, a linear scan inside a linear scan. The cost grew
**3 794x for a 128x increase in capabilities**, and a single check reached
**12.9 microseconds** at 128 capabilities.

The growth was not proportional to the capability count, and at large counts it
fitted a quadratic term closely: at 128 capabilities the measured 12 886 ns is
within 1% of 128² scaled by the ~0.79 ns a single `Cap` string comparison costs
in that loop. The small-count numbers are dominated by fixed call overhead — at
one capability the whole check is 3.40 ns — which is why the headline growth
figure (3 794x) is *below* the 16 384x a pure quadratic would give. The
quadratic term is what governs once the count is large, which is the regime the
shipped configuration never reaches and a custom domain can.

With the four shipped capabilities this was invisible (3.4 ns). It was a real
scalability limit for a custom domain that requires many, and `Cap::new` accepts
any dotted name by convention, so nothing prevented one.

**The fix has since landed.** Capability lists are a set, so the sorted
representation is now the only one: `Auth::new` sorts and de-duplicates at mint
(`crates/lgwks-bot/src/cap.rs:365`), and both the membership question
(`covers_cap`) and the coverage question (`uncovered`) are binary searches over
that sorted slice (`crates/lgwks-bot/src/cap.rs:382`). The product this table
measures moves from `required.len() * granted.len()` to
`required.len() * log2(granted.len())`, paid once per mint rather than once per
check. The derivation is stated on the type itself
(`crates/lgwks-bot/src/cap.rs:345`).

Measured after the fix, in the same rig:

| required caps | ns/call | ns per cap |
|---:|---:|---:|
| 1 | 4.85 | 4.85 |
| 8 | 91.47 | 11.43 |
| 32 | 546.95 | 17.09 |
| 128 | 3 638.08 | 28.42 |

The growth is **750.5x for a 128x increase in capabilities**, against 16 384x
for a pure quadratic and 896x for the `n * log2 n` the fix implements. At 128
capabilities a check fell from 12.9 microseconds to 3.6, a 3.5x improvement. The
rig's latest run reads 4.36, 91.81, 547.75 and 3 620.88 ns at 1, 8, 32 and 128
capabilities (830.9x for 128x), so the fix holds. The remaining cost at 128 is
the `n * log2 n` term plus the per-call overhead; a `HashSet` would flatten it to
constant and costs a hash on every mint, which is the trade this crate did not
take.

## What is deliberately excluded

**No raw-`bevy_ecs` comparator.** The bot is built on `bevy_ecs`, so a
raw-bevy comparison looks like the obvious one. It is not: a raw-bevy
reimplementation of this schedule would be *this rig's* code, measuring the
author's fluency with bevy's API rather than the bot's cost. The profile answers
the same question — where does the time go — using the bot's own code paths and
the counted-work gate.

**No CEL / zen-engine / rhai comparator.** Those are real engines, but they are
expression evaluators, and this crate's `Evaluate` verb is a thin wrapper over a
Rust closure: the comparison would measure a closure call against an interpreter
and would say nothing about the schedule, which is where the cost is. They are
excluded as off-axis, not as unflattering.

**No throughput claim in either direction against Home Assistant, n8n, Node-RED
or Temporal.** Those are separate processes with separate I/O models. An
in-process microbenchmark against them would be a category error dressed as a
result.

## What the multiplier buys

The bot is slower than a loop. What the loop does not have:

- **No effect without a grant.** Every side effect passes a capability check,
  and the guarantee is stated formally and proved (`proofs/`, T4).
- **Replay exactness.** The effects that ran can be re-derived from what was
  recorded, and this is proved (T6).
- **A total condition language.** Every condition terminates within a bound
  determined by its own structure, which a language with iteration does not
  have (T1, T7).
- **Determinism by construction**, exercised here and stated formally.

Whether that is a good trade depends on the workload. For an automation bot
whose stated design point is running for weeks — the crate's own framing — a
tick rate of ~490 000/s on the steady scenario is not a binding constraint, and
the guarantees are the reason to choose this crate over a for-loop. For a hot
path, the loop wins and this rig says so with a number.
