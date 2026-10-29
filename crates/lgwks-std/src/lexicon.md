This crate's rustdoc opens with the `script!` lexicon, the orchestration language this workspace ships. Each crate page also documents its own library front door; `script!` is the supported orchestration interface, not the only way to use every crate.

```text
lgwks_bot::script! {
    flow count(items: Vec<u8>) -> usize:
        give back items.len()
}
```

Each line is one step. It starts with one of the words below, or it is a line of Rust, passed through as written. What each word refuses and guarantees is on [the one-page lexicon](https://github.com/srinji-kaggss/logicalworks-crates/blob/main/docs/script-lexicon.md).

| Written | Means |
|---|---|
| `[pub] flow name(inputs) [-> Output]:` | an `async fn` taking the tenant [`Scope`] first and returning `Result<Output, FlowError>` |
| `each x in xs:` | every item, as many at once as the machine sustains, results in input order, first failure stops the rest |
| `each x in xs, at most (limit) at once:` | the same, under a limit that comes from somewhere named (an upstream's quota) |
| `within 2s:` | the block, or `TimedOut` when the deadline passes |
| `retry up to 3 times[, waiting 100ms]:` | the block again while it fails transiently, same key each attempt, within the run's retry budget |
| `together:` | each line underneath concurrently; `let x = ..` lines bind their result |
| `step name:` | a named scope: its own key and error location |
| `for x in xs:` | every item in turn, each in its own scope |
| `if cond:` / `else if cond:` / `else:` | as written |
| `let x = <block>:` | the block's last line becomes `x` |
| `run other(args)` | call another flow in this scope, await it, propagate its failure |
| `observe domain::id of target` | poll the source the host's registry declares under `domain::id`, built from `target`, as a step of its own; bind it with a type, `let n: u16 = ..` |
| `act domain::id on target with value` | run the action the host's registry declares under `domain::id`, built from `target`, on `value`, as a step of its own |
| `give back value` | return from the flow |
| `fail with reason` / `fail transiently with reason` | stop with a permanent / retryable failure |
| `graph:` with `node name[ needs a, b]:` under it | every node once, each as soon as the nodes it needs have succeeded, longest remaining chain first, as many at once as the machine sustains; the value is the run's `GraphReport` |
| `graph, at most (limit) at once:` | the same, under a limit that comes from somewhere named |
| `node name[ needs a, b]:` | one node of the enclosing `graph`: its body runs once, after `a` and `b` have succeeded |
| `saga name:` with `compensate step:` after the steps it undoes | a named scope whose finished steps are undone, newest first, when a later line fails; its value is the body's |
| `compensate step:` | the undo of `step`, registered once that step has finished, run only if the saga later fails |
| `whenever x in batches, buffering N, drop oldest:` (or `drop newest`, `refuse`) | each event of a live feed in arrival order, each in its own scope; `batches` is awaited for the next `Option<Vec<_>>` and `None` ends the feed; at most `N` events wait, and past that the policy decides; the value is the run's `PumpReport` |
| `justify with (rules):` | the block in a scope that answers to `rules`: every step, node and retry attempt inside asks them first, and is slowed, parked or stopped as they decide |
| `remember name:` | the block once per run; a resume reads its value back |
| `admit from model through (gate):` (or `tool`, `document`) | the block's bytes admitted as a `Plan`; a refusal fails the step |
| `ready x from (readiness) within 30s:` | the block once `readiness` is signalled, `x` bound to it |

[`Scope`]: https://docs.rs/lgwks_bot/latest/lgwks_bot/script/struct.Scope.html
