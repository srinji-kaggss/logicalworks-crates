# lgwks_std 3.2.1 · lgwks_deps 5.1.1 · lgwks_ast 4.0.0 · lgwks_macros 1.2.0 · lgwks_bot 6.0.0

2026-10-28

The 2026-10-27 release shipped `justify`, `graph`, `compensate` and `stream` as
runtime calls a flow had to write by hand, which left `script!` as one way in
among several. This release makes them words of the language, so `script!` is
the one entry point for every orchestration shape, and republishes all five
crates together, `lgwks_macros` included.

Two crates take a major bump. `lgwks_ast`, because `LEXICON` grows from 15
words to 21 and its type is `[Word; 21]`. `lgwks_bot`, because
`Compensations::add` now returns `Result<(), FlowError>` (one past
`MAX_COMPENSATIONS` is refused instead of growing without bound) and
`GraphReport::waves` now lists the nodes started at each scheduling point
rather than a level-by-level wave.

## lgwks_ast 4.0.0

- Six words, each a `LEXICON` row with its forms, refusals, guarantees and the
  test that proves each guarantee: `graph`, `node`, `saga`, `compensate`,
  `whenever` and `justify`.
- The tree carries them: `Construct::{Graph, Saga, Whenever, Justify}`,
  `Statement::Compensate`, and `Graph`, `Node`, `Compensate`, `Overflow`,
  `Whenever` and `Justify`.
- The parser refuses a graph whose needs repeat, name no node, name the node
  itself or form a cycle; a `node` outside a `graph`; a `compensate` outside a
  `saga` or naming no step before it; a `whenever` with no policy or a buffer
  outside `1..=65536`; and a `justify` with no rules. Each refusal names what
  to write instead.
- The grammar simulation draws the new words, so one band of seeds says every
  word of the lexicon.

## lgwks_macros 1.2.0

- Each new word expands to one call: `graph_at`, `compensated_at` with the
  registrar its `compensate` lines add to, `stream_at`, and a justified scope
  for `justify`. Every `step` and `node` asks its scope's justification before
  its body runs.
- Six refusal cases, one per new refusal, asserted by exact message.

## lgwks_bot 6.0.0

- `graph` schedules greedily by critical-path height: a node starts the moment
  its needs have succeeded and a slot is free, longest remaining chain first,
  so a run finishes within W/m + C of the best schedule (Graham's bound). The
  simulation `graph_schedule` checks every seeded run against a hand model of
  that schedule.
- `saga` undoes every finished step, newest first, once, each under its own
  `undo:<step>` key; an undo that fails is what the caller gets, and every
  other undo still runs.
- `whenever` handles a live feed in arrival order, each event in its own scope,
  shedding or refusing past its buffer exactly as its policy says.
- `justify` governs only its own block.
- `tests/it/sim_script_words.rs` proves each guarantee through flows the real
  macro expanded, two sweeps per band with identical trace hashes.
- The `estate` smoke binary and `examples/script_vocabulary.rs` run the words
  as `script!` flows.

## lgwks_std 3.2.1 · lgwks_deps 5.1.1

- The front-page lexicon fragment every crate's rustdoc opens with carries the
  new words. No code change.

## Verifying the release

```sh
cargo run -p lgwks_deps --bin published-consumer
```

The release is done at `published consumers: all passed`.
