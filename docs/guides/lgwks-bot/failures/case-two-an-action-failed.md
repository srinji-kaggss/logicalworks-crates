# Case two: an action failed

Part of [What a failed tick means](../failures.md).


Actions run in declaration order, but deciding and doing are two systems. The
`fire_plan` system walks the eligible work — a chain whose `Revision` moved opens
a transition, and a chain with a transition outstanding is walked whether or not
it moved — and records one ordered `Step` per condition that held
(`crates/lgwks-bot/src/ecs/systems.rs:321`); the `run_steps` pass then awaits those steps
on the caller's executor in exactly that order
(`crates/lgwks-bot/src/ecs/bot.rs:445`). It breaks on the first failure and records
it, and there is no rollback:

```rust,ignore
match entry.action.run_any(&grants.0, value).await {
```

`value` is the payload the transition is bound to — moved into it when it opened
or resumed, and read from there by every entry of that transition, so a retry
after a failure runs against the same input the failed attempt did.

Every action before the failure already ran, and its effect is live. `Err` here
means "this run did not finish", not "nothing happened".

One consequence is easy to miss, and it is the reason the ledger exists.
`observe_fold` commits the new values and bumps the revisions *before*
`fire_plan` decides (`crates/lgwks-bot/src/ecs/systems.rs:321`), so selecting work by
`Changed<Revision>` alone means the next tick polls an unchanged source, finds
nothing eligible, and never attempts the actions after the failure again. The
work is lost, not queued.

Eligible work is now recorded in its own structure, keyed by `(chain, entry)`,
and `fire_plan` walks *that* rather than the change set: `Changed<Revision>` only
opens a transition. Three consequences follow, and the example below asserts all
three.

- **The failed entry is retried while it has budget, even if the source never
  moves again.** `lgwks_std::retry::RetryPolicy` sets the budget (three
  attempts by default, retried on the next tick; `RetryPolicy::new(1, ..)` for
  a domain whose failures are always terminal). Its backoff holds a retry, and
  the entries behind it, until the delay has elapsed on the bot's `Clock`
  (`with_clock`), and its deadline abandons the entry with
  `AbandonReason::DeadlineExceeded` once the next backoff would reach it.
- **Every entry of a transition runs against the one payload the transition was
  opened under.** The observed value is *moved* into the transition when it opens
  or resumes, so a retry after a failure reads the same input the entry that
  failed read — not whatever the source has since become. A newer value is
  admitted only once the transition has nothing open, and it then becomes the
  next transition's payload for every entry. Without this, one transition would
  produce effects of two different inputs: an entry acknowledged against the old
  value would be retried against the new one, and the tick would report the pair
  as a single finished unit of work. Nothing in the crate asks a source's
  `Output` to be `Clone`; the value is moved.
- **The entries after it wait.** The walk stops at the first entry it cannot
  settle, so an acknowledged effect is never replayed to reach a successor. That
  includes an entry it has *given up* on: declaration order is not success
  dependency, and an abandonment is the strongest statement that the entry behind
  it must not run — a draft that was never reserved has nothing to send.
  Successors stay reported behind it, and the way past is evidence
  (`EffectEvidence::NotApplied`), which revives the entry and them with it.
- **Giving up is reported, not silent.** When the budget is spent the entry is
  abandoned, the tick still returns the action's own typed error, and
  `Bot::pending()` names the entry, the reason (`AbandonReason`), and the source
  revision. A `pending()` that lists anything is work that is still owed, and a
  clean `Ok` tick — with `Err(BotError::PendingTransition)` in place of silence
  whenever work is held and nothing failed — cannot be read as "the transition
  was handled".
