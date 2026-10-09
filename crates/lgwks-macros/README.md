# lgwks_macros

The `script!` orchestration language for `lgwks_bot`. Use it as
`lgwks_bot::script!`; this crate is the syntax, and `lgwks_bot::script` is the
runtime every block expands into.

```text
lgwks_bot::script! {
    pub flow crawl(site: &Site, paths: Vec<String>) -> Vec<usize>:
        let pages = each path in paths, at most (site.quota()) at once:
            retry up to 3 times, waiting 200ms:
                within 5s:
                    site.fetch(scope.key(), &path).await.or_retry()?
        give back pages.iter().map(|page| page.len()).collect()
}
```

Every fan-out is bounded, every wait has a deadline, every retry keeps one
idempotency key, every step is keyed by tenant and path, and every script emits
an `ARCHITECTURE` map compiled from the same tokens. See the crate
documentation for the full language and the list of what it refuses.

## The words

Every word of the language, one row each. The macro dispatches on the same
table this is rendered from (`src/lexicon.rs`), and a test refuses a README
whose table differs from it. A line that starts with none of these words is
plain Rust.

| Written | Where | Calls | Means | Refuses | Guarantees |
|---|---|---|---|---|---|
| `[pub] flow name(inputs) [-> Output]:` | flow | `Scope::enter` | an `async fn` taking the tenant `Scope` first and returning `Result<Output, FlowError>` | a promised output the last line never produces | every step keyed by tenant and path (multi-tenant; `tenants_isolated` in `sim_script.rs`); the panicking calls refused by resolved path (idiomatic; refusal *unwrap by path*) |
| `each x in xs:`<br>`each x in xs, at most (limit) at once:` | block | `each` | every item, as many at once as the machine sustains, results in input order, first failure stops the rest<br>the same, under a limit that comes from somewhere named (an upstream's quota) | a concurrency bound typed as a number; a bound outside `1..=65536` | bounded fan-out (hyperscale; `fan_bounded` in `sim_script.rs`); results in input order (generalized; `fan_machine_sized` in `sim_script.rs`); first failure cancels the rest (hyperscale; `fan_fail_fast` in `sim_script.rs`) |
| `within 2s:` | block | `within` | the block, or `TimedOut` when the deadline passes | a zero duration; a duration with no unit | a deadline, on the scope's clock (hyperscale; `a_within_budget_is_refused_by_an_advance_with_no_real_wait` in `sim_clock_wiring.rs`) |
| `retry up to 3 times[, waiting 100ms]:` | block | `retry` | the block again while it fails transiently, same key each attempt, within the run's retry budget | attempts outside `1..=1000`; a zero wait | one idempotency key across attempts (ephemeral; `retry_one_key` in `sim_script.rs`); attempts within the run's retry budget (hyperscale; `retry_budget_holds` in `sim_script.rs`) |
| `together:` | block | `try_join!` | each line underneath concurrently; `let x = ..` lines bind their result | anything before its `:`; an indent under a line that opens no block | every branch owned by this task (ephemeral; `together_owned` in `sim_script.rs`); first failure cancels the rest (hyperscale; `together_fail_fast` in `sim_script.rs`) |
| `step name:` | block | `Scope::enter` | a named scope: its own key and error location | a name that is not one word | its own step key (ephemeral; `step_own_key` in `sim_script.rs`); failures located at the step (decoupled; `step_failure_located` in `sim_script.rs`) |
| `for x in xs:` | block | `Scope::item` | every item in turn, each in its own scope | a missing pattern or missing items | items in order, one at a time (generalized; `for_in_order` in `sim_script.rs`); one scope per item (ephemeral; `for_scope_per_item` in `sim_script.rs`) |
| `if cond:` / `else if cond:` / `else:` | block | none: Rust `if` | as written | a missing condition | a value only when an `else:` ends the chain (idiomatic; refusal *if with no else as the value*) |
| `else` (see `if`) | block | none: Rust `else` | — | an `else` not after an `if`; an `else:` that is not the last branch | only ever continues an `if` chain (idiomatic; refusal *else without if*) |
| `let x = <block>:` | block | the block's own | the block's last line becomes `x` | a `let` with no `=` or nothing after it | the block's failure propagates before `x` is bound (idiomatic; `let_fails_before_binding` in `sim_script.rs`) |
| `run other(args)` | inline | the callee flow | call another flow in this scope, await it, propagate its failure | — | the callee runs in this tenant's scope (multi-tenant; `run_in_callers_tenant` in `sim_script.rs`) |
| `give back value` | line | none: `return Ok` | return from the flow | `give back` inside a block that has its own value | leaves the flow, never a nested block (idiomatic; refusal *give back from a nested block*) |
| `fail with reason` / `fail transiently with reason` | line | `FlowError::failed` / `FlowError::transient` | stop with a permanent / retryable failure | a reason missing after `with` | a typed failure the caller can classify (decoupled; `fail_is_classified` in `sim_script.rs`) |
