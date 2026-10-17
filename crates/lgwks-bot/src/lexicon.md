The supported way to use this crate is `script!` (`lgwks_bot::script!`): orchestration written the way it is said.

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

[`Scope`]: https://docs.rs/lgwks_bot/latest/lgwks_bot/script/struct.Scope.html
