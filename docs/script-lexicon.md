# The `script!` lexicon

<!-- Generated from `LEXICON` in crates/lgwks-ast/src/script/lexicon.rs by the test `the_lexicon_page_is_rendered_from_the_lexicon` in lgwks_macros. Edit the table, not this page. -->

Pages: page 1 · [page 2](script-lexicon-2.md) · [page 3](script-lexicon-3.md).

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
