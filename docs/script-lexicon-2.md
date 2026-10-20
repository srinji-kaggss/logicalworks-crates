# The `script!` lexicon, page 2

<!-- Generated from `LEXICON` in crates/lgwks-ast/src/script/lexicon.rs by the test `the_lexicon_page_is_rendered_from_the_lexicon` in lgwks_macros. Edit the table, not this page. -->

Pages: [page 1](script-lexicon.md) · page 2 · [page 3](script-lexicon-3.md).

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

Guarantees: an identifier with no `on <target> with <value>` is refused at the word, naming the form (idiomatic); the action runs once, under exactly the grants it declares; an external one settles a repeat by reading its receiver back (ephemeral); the same polls and actions, in the same order, as the `BotSpec` chain over the same registry (generalized); an identifier the registry lacks refuses the flow before its first step, naming the registry and the nearest identifier (decoupled).

```text
flow page(count: u16):
    if count > 2:
        act notify::page on "oncall" with count
```
