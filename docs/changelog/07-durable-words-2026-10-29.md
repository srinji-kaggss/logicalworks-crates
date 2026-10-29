# lgwks_std 3.2.2 · lgwks_deps 5.1.2 · lgwks_ast 4.1.0 · lgwks_macros 1.3.0 · lgwks_bot 6.1.0 · drishti 0.1.0

2026-10-29

ADR 0008 makes every runtime capability three things landed together: the SDK
implementation, the entrypoint the macro calls, and a `script!` word, or a row
in the ADR saying why it has none. Durability, admission and readiness were
exported by `lgwks_bot::script` with no word; they are words now. The release
also publishes what ADR 0006 R2–R6 landed since 6.0.0, and drishti's first
library release. Every change adds; none removes, so no crate takes a major.

## lgwks_ast 4.1.0

- Three words: `remember name:`, `admit from model through (gate):` (or
  `tool`, `document`) and `ready x from (readiness) within 30s:`. `LEXICON`
  holds 24 words.
- The tree carries them: `Construct::{Remember, Admit, Ready}` and `Admit`,
  `Ready` and `Producer`, each `#[non_exhaustive]` where it can grow.
- The parser refuses a nameless `remember`, an `admit` with no gate or an
  unknown producer, and a `ready` with no deadline. Each refusal names the
  form to write.
- The grammar simulation draws all three, so one band of seeds still says
  every word.

## lgwks_macros 1.3.0

- `remember` expands to `remembered_at`, `admit` to `admit_at` with the
  producer's `Source`, and `ready` to `Readiness::wait` under the written
  deadline, each in its own located scope that asks its justification first.
- Four refusal cases, asserted by exact message. The architecture map names
  each as `StepKind::{Remember, Admit, Ready}`.

## lgwks_bot 6.1.0

- `remembered_at` and `admit_at`, the located entrypoints the words call.
- `tests/it/sim_script_durable.rs`: a resume on a fresh host replays a
  remembered value and never reruns its block; the word admits exactly what
  `admit` admits, refusals located at the word's step with the provenance of
  the exact bytes; `ready` is released by a signal, fails with the readiness's
  own failure, and is `TimedOut` past its deadline on a virtual clock.
- From ADR 0006: one process driver, which host, estate and `block_on` share
  (R2); runs enter the world through the journal as `RunFeed` events (R3); the
  axum edge answers each tenant from its own state (R4); the world ticks on a
  `WakeHandle` wake or an `Observe::cadence` deadline, never a fixed poll (R5).
- The process driver's warm path takes no lock.

## drishti 0.1.0

- The first library release. 0.0.1 shipped documentation only; 0.1.0 ships
  the brand tokens (`design`), the eleven seeded ink effects (`ink_lab`), the
  GPU residency model (`runtime`) and a GPUI host behind feature `ui`
  (`native`), with the `drishti` command orchestrating them through `script!`.
  Its proof gates G01–G15 stay blocked, as `docs/status.md` records.

## lgwks_std 3.2.2 · lgwks_deps 5.1.2

- The front-page lexicon fragment every crate's rustdoc opens with carries the
  three words. `lgwks_deps` gains the six-crate project page and the
  `ci-local` wall-budget gate. No API change.

## Verifying the release

```sh
cargo run -p lgwks_deps --bin published-consumer
```

The release is done at `published consumers: all passed`.
