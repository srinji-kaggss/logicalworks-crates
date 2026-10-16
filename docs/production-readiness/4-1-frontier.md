# 4.1 Frontier — does the design beat credible alternatives?

Part of [production readiness](../production-readiness.md), §4, nine axes.

**✅ — argued from a survey, with the winning axis named.**

[`framework-comparison.md`](../framework-comparison.md) covers eight systems across
crawler, Discord-bot, browser-agent, scraping and commercial-guidance families.
[`frontier.md`](../frontier.md) covers nine research areas with fetched leaderboard
and paper numbers (Similo 88.0% vs 75.6% on real drifted pages; ScreenSpot-Pro
18.9% → 82.7% in eighteen months; the false-heal study's overlapping score
ranges that no threshold separates).

The design position is one axis nobody in the sample occupies: **authority
bounded at the invocation, with decision provenance a third party can verify.**
`GrantSet` mints the only `Auth`, `Auth` travels with the call, and `T4` is the
gate-soundness theorem.

**Scored against the ecosystem standard on authoring, the facade loses.** The
2026-10-04 fixed-model runs (`bench/ai-authoring/README.md`) put the facade, the
facade plus `FanOut`, the old `rt` surface and the `futures` crate on the same
two oracle tasks, 40 trials per arm:

| arm | full pass | sheet API used | hand-rolled `std::thread` | mean repairs | mean lines |
|---|---:|---:|---:|---:|---:|
| `futures` combinators | 39/40 | 40/40 | 0/40 | 0.15 | 37 |
| old `lgwks_bot::rt` | 38/40 | 38/40 | 1/40 | 0.85 | 68 |
| `script`/`task` facade | 37/40 | 12/40 | 24/40 | 0.75 | 140 |
| facade + `FanOut` | 37/40 | 31/40 | 8/40 | 1.07 | 70 |

Pass rate does not separate the arms at ten trials per cell; what does is that
only 12 of 40 facade solutions used the facade, and `futures` did the same work in
about a quarter of the facade's lines.

The oracle does not ask for what `lgwks_bot` adds over `futures` (durable steps,
authority, repair), so this does not say the crate has no case; it says the case
has not been measured on tasks that ask for it
([#270](https://github.com/srinji-kaggss/logicalworks-crates/issues/270)). Until
it is, the frontier claim rests on the authority axis above and not on authoring.

That measurement now exists for reference solutions, and it falls the honest
way in both directions (`bench/ai-authoring/README.md`, runs
`20261008T003455Z-dry` and `20261008T005932Z-dry`, 140 dry trials, ten per
cell, zero variance everywhere). On three held-out tasks whose oracles fail any
solution lacking the guarantee — bounded capture of a forking child past 64
MiB, tenant isolation at a thousand in flight with typed refusals, and
exactly-once effect application across `SIGKILL` — the `script!` arm, the
facade, the facade plus `FanOut` and the hand-rolled old surface pass every
clause on every trial, and the `futures` comparator fails exactly the
guarantee clauses on every trial (the flood, the orphan and the promptness of
the cancel; the isolation, both refusals and the mixed thousand; the
drop-duplicate and the kill-duplicate). Each oracle's negative control is a
committed reference `futures` solution failing it with a committed `lgwks_bot`
solution passing it, quoted in the README.

What this does and does not move. The `script!` arm is measured for the first
time and holds: 60 of 60 full passes across the three tasks. The `futures`
lead on the old tasks stands unchallenged — nothing here re-runs models, and
no interval is claimed over deterministic references. The frontier closed
model, the other models and the human authors have not run: the matrix calls
paid models and nobody has ordered that spend, so #270 stays open on it. Its
runner is back, in Rust (`ai-authoring`, a `lgwks_deps` binary driven by
`bench/ai-authoring/bench.toml`; the Python runner stays deleted), and the
README gives its exact command and its planned scale (990 trials). What ran,
at HEAD, for free: a dry run of all 33 cells (`runs/20261016T225212Z-dry`;
every `lgwks_bot` and `lexicon` cell passes every clause, `futures` fails
exactly the guarantee clauses) and the six mutant controls
(`runs/20261016T225558Z-mutants`; every mutant fails its intended clauses,
none survives), plus five journeys through the shipped binary under real
cargo (a mutant the oracle cannot catch exits 1, an oracle past its bound is
recorded as `timeout`, a `kill -9` and `--resume` leave no process behind).
The README's per-task table says which arm wins where it is measured. So the
frontier claim now rests on two legs — the authority axis above, and a
measured authoring comparison on guarantee tasks where every `lgwks_bot` arm
beats the ecosystem standard — with the model half still open.

*Not covered:* the survey sample is eight systems and one patent reading, not
the whole market. A rival that occupies the same axis would falsify the
position, and no one has looked for one since the survey ran.
