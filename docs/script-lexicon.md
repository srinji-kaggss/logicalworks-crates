# The `script!` lexicon

<!-- Generated from `LEXICON` in crates/lgwks-ast/src/script/lexicon.rs by the test `the_lexicon_page_is_rendered_from_the_lexicon` in lgwks_macros. Edit the table, not this page. -->

Everything needed to write `lgwks_bot::script!` flows. A script is one or more `flow`s inside the macro:

```rust
lgwks_bot::script! {
    flow count(items: Vec<u8>) -> usize:
        give back items.len()
}
```

Each line is one step: it starts with one of the words below, or it is a line of Rust. A line ending in `:` opens a block of the lines indented four spaces beneath it. Inside a flow, `scope` is the current step's scope. A line of Rust that could end or leak the program is refused when the macro expands, and the refusal names what to write instead.

## `flow`

Opens a flow at the top level. Calls `Scope::enter`.

- `[pub] flow name(inputs) [-> Output]:` — an `async fn` taking the tenant `Scope` first and returning `Result<Output, FlowError>`

Refuses: a promised output the last line never produces.

Guarantees: every step keyed by tenant and path (multi-tenant); the panicking calls refused by resolved path (idiomatic).

```text
flow count(items: Vec<u8>) -> usize:
    give back items.len()
```

## `each`

Opens a block: its line ends in `:`. Calls `each`.

- `each x in xs:` — every item, as many at once as the machine sustains, results in input order, first failure stops the rest
- `each x in xs, at most (limit) at once:` — the same, under a limit that comes from somewhere named (an upstream's quota)

Refuses: a concurrency bound typed as a number; a bound outside `1..=65536`.

Guarantees: bounded fan-out (hyperscale); results in input order (generalized); first failure cancels the rest (hyperscale).

```text
flow sizes(pages: Vec<String>) -> Vec<usize>:
    let sizes = each page in pages:
        page.len()
    give back sizes
```

## `within`

Opens a block: its line ends in `:`. Calls `within`.

- `within 2s:` — the block, or `TimedOut` when the deadline passes

Refuses: a zero duration; a duration with no unit.

Guarantees: a deadline, on the scope's clock (hyperscale).

```text
flow ping(host: &Host) -> u16:
    let code = within 2s:
        host.ping().await.or_retry()?
    give back code
```

## `retry`

Opens a block: its line ends in `:`. Calls `retry`.

- `retry up to 3 times[, waiting 100ms]:` — the block again while it fails transiently, same key each attempt, within the run's retry budget

Refuses: attempts outside `1..=1000`; a zero wait.

Guarantees: one idempotency key across attempts (ephemeral); attempts within the run's retry budget (hyperscale).

```text
flow fetch(site: &Site) -> Page:
    let page = retry up to 3 times, waiting 200ms:
        site.get(scope.key()).await.or_retry()?
    give back page
```

## `together`

Opens a block: its line ends in `:`. Calls `try_join!`.

- `together:` — each line underneath concurrently; `let x = ..` lines bind their result

Refuses: anything before its `:`; an indent under a line that opens no block.

Guarantees: every branch owned by this task (ephemeral); first failure cancels the rest (hyperscale).

```text
flow both(left: &Api, right: &Api):
    together:
        let up = run probe(left)
        let down = run probe(right)
```

## `step`

Opens a block: its line ends in `:`. Calls `Scope::enter`.

- `step name:` — a named scope: its own key and error location

Refuses: a name that is not one word.

Guarantees: its own step key (ephemeral); failures located at the step (decoupled).

```text
flow deploy(target: &Host):
    step upload:
        target.upload().await.or_fail()?
```

## `for`

Opens a block: its line ends in `:`. Calls `Scope::item`.

- `for x in xs:` — every item in turn, each in its own scope

Refuses: a missing pattern or missing items.

Guarantees: items in order, one at a time (generalized); one scope per item (ephemeral).

```text
flow notify(users: Vec<User>):
    for user in users:
        run email(user)
```

## `if`

Opens a block: its line ends in `:`. Calls none: Rust `if`.

- `if cond:` / `else if cond:` / `else:` — as written

Refuses: a missing condition.

Guarantees: a value only when an `else:` ends the chain (idiomatic).

```text
flow route(code: u16) -> u8:
    if code < 400:
        give back 0
    else if code < 500:
        give back 1
    else:
        give back 2
```

## `else`

Opens a block: its line ends in `:`. Calls none: Rust `else`.

Written as part of `if`'s forms.

Refuses: an `else` not after an `if`; an `else:` that is not the last branch.

Guarantees: only ever continues an `if` chain (idiomatic).

## `let`

Opens a block: its line ends in `:`. Calls the block's own.

- `let x = <block>:` — the block's last line becomes `x`

Refuses: a `let` with no `=` or nothing after it.

Guarantees: the block's failure propagates before `x` is bound (idiomatic).

```text
flow total(xs: Vec<u8>) -> u8:
    let sum = within 1s:
        xs.iter().sum()
    give back sum
```

## `run`

Stands inside a line of Rust. Calls the callee flow.

- `run other(args)` — call another flow in this scope, await it, propagate its failure

Refuses: nothing of its own.

Guarantees: the callee runs in this tenant's scope (multi-tenant).

```text
flow outer(site: &Site) -> usize:
    let pages = run crawl(site)
    give back pages
```

## `observe`

Stands inside a line of Rust. Calls `observe`.

- `observe domain::id of target` — poll the source the host's registry declares under `domain::id`, built from `target`, as a step of its own; bind it with a type, `let n: u16 = ..`

Refuses: an identifier with no `of <target>`.

Guarantees: an identifier with no `of <target>` is refused at the word, naming the form (idiomatic); an identifier the registry lacks refuses the flow before its first step, naming the registry and the nearest identifier (decoupled); the same polls and actions, in the same order, as the `BotSpec` chain over the same registry (generalized); the source's capabilities are checked against this run's grant at the step (multi-tenant).

```text
flow open(repo: &str) -> u16:
    let open: u16 = observe github::pr_status of repo
    give back open
```

## `act`

Stands inside a line of Rust. Calls `act`.

- `act domain::id on target with value` — run the action the host's registry declares under `domain::id`, built from `target`, on `value`, as a step of its own

Refuses: an identifier with no `on <target> with <value>`.

Guarantees: an identifier with no `on <target> with <value>` is refused at the word, naming the form (idiomatic); only an effect that stays in the process runs; an external one is refused before it runs, naming the effect ledger (ephemeral); the same polls and actions, in the same order, as the `BotSpec` chain over the same registry (generalized); an identifier the registry lacks refuses the flow before its first step, naming the registry and the nearest identifier (decoupled).

```text
flow page(count: u16):
    if count > 2:
        act notify::page on "oncall" with count
```

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
