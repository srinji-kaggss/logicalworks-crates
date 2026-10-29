# The `script!` lexicon, page 4

<!-- Generated from `LEXICON` in crates/lgwks-ast/src/script/lexicon.rs by the test `the_lexicon_page_is_rendered_from_the_lexicon` in lgwks_macros. Edit the table, not this page. -->

Pages: [page 1](script-lexicon.md) · [page 2](script-lexicon-2.md) · [page 3](script-lexicon-3.md) · page 4.

## `compensate`

Opens a block: its line ends in `:`. Calls `Compensations::add`.

- `compensate step:` — the undo of `step`, registered once that step has finished, run only if the saga later fails

Refuses: a `compensate` outside a `saga`; a step no earlier line of the saga names.

Guarantees: when the saga fails, the finished steps' undos run in reverse order, once each; a saga that succeeds runs none (generalized); only ever undoes a step written before it in the same saga (idiomatic); a step the saga never ran is refused at the word, naming the steps it has (generalized).

```text
flow book(trip: &Trip) -> Ticket:
    let ticket = saga booking:
        step charge:
            trip.charge().await.or_fail()?
        compensate charge:
            trip.refund().await.or_fail()?
        step issue:
            trip.issue().await.or_fail()?
    give back ticket
```

## `whenever`

Opens a block: its line ends in `:`. Calls `pump`.

- `whenever x in batches, buffering N, drop oldest:` (or `drop newest`, `refuse`) — each event of a live feed in arrival order, each in its own scope; `batches` is awaited for the next `Option<Vec<_>>` and `None` ends the feed; at most `N` events wait, and past that the policy decides; the value is the run's `PumpReport`

Refuses: a feed with no `buffering N`; a buffer outside `1..=65536`; an overflow policy other than `drop oldest`, `drop newest` or `refuse`.

Guarantees: never more than `N` events buffered, whatever the producer's rate; past it the declared policy, counted (hyperscale); events handled in arrival order, each in its own scope with its own key (generalized); a feed with no overflow policy is refused at the word, naming the three (idiomatic).

```text
flow watch(feed: &Feed) -> u64:
    let report = whenever tick in feed.next().await, buffering 1024, drop oldest:
        tick.record()
    give back report.handled
```

## `justify`

Opens a block: its line ends in `:`. Calls `Scope::with_justification`.

- `justify with (rules):` — the block in a scope that answers to `rules`: every step, node and retry attempt inside asks them first, and is slowed, parked or stopped as they decide

Refuses: a `justify` with no `with (rules)`.

Guarantees: a step the rules stop fails with `Justified`, naming the rule, before its body runs (decoupled); the rules govern only the block's own scope, never a sibling's (multi-tenant); a `justify` with no rules is refused at the word, naming the form (idiomatic).

```text
flow sync(api: &Api, rules: &Justify) -> u16:
    let code = justify with (rules):
        step call:
            api.call().await.or_fail()?
    give back code
```

## `remember`

Opens a block: its line ends in `:`. Calls `remembered_at`.

- `remember name:` — the block once per run; a resume reads its value back

Refuses: a name that is not one word.

Guarantees: a resume replays the value, never the block (ephemeral); a nameless `remember` is refused (idiomatic).

```text
flow ask(model: &Model) -> String:
    let reply = remember ask:
        model.ask().await.or_retry()?
    give back reply
```

## `admit`

Opens a block: its line ends in `:`. Calls `admit_at`.

- `admit from model through (gate):` (or `tool`, `document`) — the block's bytes admitted as a `Plan`; a refusal fails the step

Refuses: a producer other than `model`, `tool`, `document`.

Guarantees: a refusal is typed, at the step, with its provenance (decoupled); an `admit` with no gate is refused (idiomatic).

```text
flow plan(reply: &[u8], gate: &Gate) -> usize:
    let plan = admit from model through (gate):
        reply
    give back plan.wanted().len()
```

## `ready`

Opens a block: its line ends in `:`. Calls `Readiness::wait`.

- `ready x from (readiness) within 30s:` — the block once `readiness` is signalled, `x` bound to it

Refuses: a `ready` with no `within`.

Guarantees: released by the signal, never a poll; `TimedOut` past the deadline (hyperscale); a `ready` with no deadline is refused (idiomatic).

```text
flow rows(db: &Readiness<Pool>) -> u64:
    let n = ready pool from (db) within 30s:
        pool.awaited().count()
    give back n
```
