# The `script!` lexicon, page 3

<!-- Generated from `LEXICON` in crates/lgwks-ast/src/script/lexicon.rs by the test `the_lexicon_page_is_rendered_from_the_lexicon` in lgwks_macros. Edit the table, not this page. -->

Pages: [page 1](script-lexicon.md) · [page 2](script-lexicon-2.md) · page 3 · [page 4](script-lexicon-4.md).

## `give back`

Starts a line of its own. Calls none: `return Ok`.

- `give back value` — return from the flow

Refuses: `give back` inside a block that has its own value.

Guarantees: leaves the flow, never a nested block (idiomatic).

```text
flow one() -> u8:
    give back 1
```

## `fail`

Starts a line of its own. Calls `FlowError::failed` / `FlowError::transient`.

- `fail with reason` / `fail transiently with reason` — stop with a permanent / retryable failure

Refuses: a reason missing after `with`.

Guarantees: a typed failure the caller can classify (decoupled).

```text
flow check(ready: bool):
    if !ready:
        fail with "not ready"
```

## `graph`

Opens a block: its line ends in `:`. Calls `graph`.

- `graph:` with `node name[ needs a, b]:` under it — every node once, each as soon as the nodes it needs have succeeded, longest remaining chain first, as many at once as the machine sustains; the value is the run's `GraphReport`
- `graph, at most (limit) at once:` — the same, under a limit that comes from somewhere named

Refuses: a line under it that is not a `node`; a node needing itself, a node no line declares, or a cycle; a name declared twice; a concurrency bound typed as a number.

Guarantees: a node starts only after every node it needs has succeeded; a node whose need failed never runs (generalized); at most the bound of nodes run at once, and each node runs once (hyperscale); each node runs in its own scope, `<graph>/<node>`, with its own key (ephemeral); a graph that cannot run is refused at the word, naming the node (idiomatic).

```text
flow ship(repo: &Repo) -> usize:
    let report = graph:
        node build:
            repo.build().await.or_fail()?
        node lint:
            repo.lint().await.or_fail()?
        node test needs build, lint:
            repo.test().await.or_fail()?
    give back report.succeeded().len()
```

## `node`

Opens a block: its line ends in `:`. Calls `GraphNode::new`.

- `node name[ needs a, b]:` — one node of the enclosing `graph`: its body runs once, after `a` and `b` have succeeded

Refuses: a `node` outside a `graph`; a name that is not one word.

Guarantees: a node starts only after every node it needs has succeeded; a node whose need failed never runs (generalized); only ever stands inside a `graph` (idiomatic).

```text
flow ship(repo: &Repo) -> usize:
    let report = graph:
        node build:
            repo.build().await.or_fail()?
        node lint:
            repo.lint().await.or_fail()?
        node test needs build, lint:
            repo.test().await.or_fail()?
    give back report.succeeded().len()
```

## `saga`

Opens a block: its line ends in `:`. Calls `compensated`.

- `saga name:` with `compensate step:` after the steps it undoes — a named scope whose finished steps are undone, newest first, when a later line fails; its value is the body's

Refuses: a name that is not one word.

Guarantees: when the saga fails, the finished steps' undos run in reverse order, once each; a saga that succeeds runs none (generalized); each undo runs under its own key, `<saga>/undo:<step>`, never the key of the step it reverses (ephemeral); an undo that fails is returned to the caller in place of the step's failure, and every undo still runs (decoupled); at most `MAX_COMPENSATIONS` undos registered; one past it fails the saga (hyperscale).

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
