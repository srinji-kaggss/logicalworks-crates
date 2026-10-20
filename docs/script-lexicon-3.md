# The `script!` lexicon, page 3

<!-- Generated from `LEXICON` in crates/lgwks-ast/src/script/lexicon.rs by the test `the_lexicon_page_is_rendered_from_the_lexicon` in lgwks_macros. Edit the table, not this page. -->

Pages: [page 1](script-lexicon.md) · [page 2](script-lexicon-2.md) · page 3.

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
