# What a failed tick means

Both entry points return `Result<usize, BotError>`: `Bot::tick` is the
synchronous adapter (`crates/lgwks-bot/src/ecs.rs:3546`) and `Bot::tick_async` is
the one to `await` from inside a runtime (`crates/lgwks-bot/src/ecs.rs:3435`).
Four different things can produce an `Err`. Three are failures that mean
different things for your data — the distinction is the difference between a
retry and a duplicate — and the fourth is the adapter refusing to run a tick at
all.

Cases one and two are the two the `observe_fold` and `fire_plan` systems produce,
and the program under case two is compiled and run as
`crates/lgwks-bot/examples/failed_tick.rs`. Case three is what the error variant
behind an indeterminate effect means.

## Case one: a poll failed

Nothing ran. The `observe_fold` system returns before committing anything
(`crates/lgwks-bot/src/ecs.rs:2813`):

```rust,ignore
// Pass one: the first error, in declaration order.
let mut first_error = None;
for result in polled.iter_mut() {
    if result.is_err() {
        first_error = std::mem::replace(result, Ok(None)).err();
        break;
    }
}
if let Some(error) = first_error {
    world.resource_mut::<TickError>().0 = Some(error);
    put_polled(world, polled);
    return;
}
```

The first error in declaration order is the one `tick` reports, and it is moved
out of the slot it was found in rather than copied, because `BotError` is
deliberately not `Clone` — a copy would let a caller settle an effect against a
duplicate of the failure instead of the failure. The early return happens before
the observed values are written and before any `Revision` is bumped, so a source
that held still on the previous tick still holds still, and a source that had
moved still shows its previous revision. No condition is evaluated, so no action
runs.

`tick` returns that error and the tick had no effect.

## Case two: an action failed

Actions run in declaration order, but deciding and doing are two systems. The
`fire_plan` system walks the eligible work — a chain whose `Revision` moved opens
a transition, and a chain with a transition outstanding is walked whether or not
it moved — and records one ordered `Step` per condition that held
(`crates/lgwks-bot/src/ecs.rs:3216`); the `run_steps` pass then awaits those steps
on the caller's executor in exactly that order
(`crates/lgwks-bot/src/ecs.rs:3738`). It breaks on the first failure and records
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
`fire_plan` decides (`crates/lgwks-bot/src/ecs.rs:2993`), so selecting work by
`Changed<Revision>` alone means the next tick polls an unchanged source, finds
nothing eligible, and never attempts the actions after the failure again. The
work is lost, not queued.

Eligible work is now recorded in its own structure, keyed by `(chain, entry)`,
and `fire_plan` walks *that* rather than the change set: `Changed<Revision>` only
opens a transition. Three consequences follow, and the example below asserts all
three.

- **The failed entry is retried while it has budget, even if the source never
  moves again.** `RetryPolicy` sets the budget (three attempts by default;
  `RetryPolicy::ONE_ATTEMPT` for a domain whose failures are always terminal).
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

```rust,ignore
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lgwks_bot::spec::{AbandonReason, TransitionHold};
use lgwks_bot::{Auth, Bot, BotError, Cap, Execute, GrantSet, Observe};

struct Reading(Arc<AtomicU32>);

impl Observe for Reading {
    type Output = u32;
    fn required_caps(&self) -> &[Cap] { &[] }
    async fn poll(&self, call: (Auth, ())) -> Result<u32, BotError> {
        call.0.check(self.required_caps())?;
        Ok(self.0.load(Ordering::SeqCst))
    }
    fn domain_id(&self) -> &str { "test::reading" }
}

/// Counts its own successful runs.
struct Count(Arc<AtomicU32>);

impl Execute for Count {
    type Input = u32;
    type Output = ();
    fn required_caps(&self) -> &[Cap] { &[] }
    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(self.required_caps())?;
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn domain_id(&self) -> &str { "test::count" }
}

/// Fails with `DomainError`: the effect did not happen.
struct Fail;

impl Execute for Fail {
    type Input = u32;
    type Output = ();
    fn required_caps(&self) -> &[Cap] { &[] }
    async fn execute_action(&self, call: (Auth, &u32)) -> Result<(), BotError> {
        call.0.check(self.required_caps())?;
        Err(BotError::DomainError {
            domain: "test::fail".into(),
            cause: "the second action refused".into(),
        })
    }
    fn domain_id(&self) -> &str { "test::fail" }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let value = Arc::new(AtomicU32::new(0));
    let counted = Arc::new(AtomicU32::new(0));
    let mut bot = Bot::builder("two-actions")
        .observe(Reading(Arc::clone(&value)))
        .on(|_: &u32| true, Count(Arc::clone(&counted)))
        .on(|_: &u32| true, Fail)
        .build(&GrantSet::empty())?;

    let error = match bot.tick() {
        Ok(fired) => return Err(format!("the second action was reported as {fired} fired").into()),
        Err(error) => error,
    };
    assert!(matches!(error, BotError::DomainError { .. }), "got {error:?}");
    assert_eq!(counted.load(Ordering::SeqCst), 1, "the action before the failure already ran");

    // The source holds still, and the refused entry is attempted again anyway.
    // The action ahead of it has already succeeded and is not replayed.
    assert!(matches!(bot.tick(), Err(BotError::DomainError { .. })));
    assert_eq!(counted.load(Ordering::SeqCst), 1, "the acknowledged effect is not replayed");

    // Third attempt: the budget is spent. The tick reports the refusal, and
    // `pending` names the entry it gave up on rather than dropping it.
    assert!(matches!(bot.tick(), Err(BotError::DomainError { .. })));
    let pending = bot.pending();
    assert_eq!(pending.len(), 1, "one entry is left owing an answer: {pending:?}");
    assert!(matches!(
        pending[0].hold(),
        TransitionHold::Abandoned { reason: AbandonReason::AttemptsExhausted { .. }, .. }
    ));
    // The chain is not clean while the abandonment stands: it asks the tick for
    // nothing, but it is work nobody resolved, and the entry behind a
    // prerequisite that was given up on is not work that may proceed.
    assert!(matches!(bot.tick(), Err(BotError::PendingTransition { .. })));

    // A new source value is new work for the entries that were not given up on.
    // The abandoned entry is a barrier: it is not retried because the source
    // moved, nothing behind it is attempted either, and the tick still reports
    // the chain as unresolved even though the first entry ran again.
    value.store(1, Ordering::SeqCst);
    assert!(matches!(bot.tick(), Err(BotError::PendingTransition { .. })));
    assert_eq!(counted.load(Ordering::SeqCst), 2);
    Ok(())
}
```

## Case three: the effect may already have happened

`BotError` separates "the effect did not happen" from "the effect may have
happened" as two variants, not one variant with a flag.

| Variant | Means | A retry is |
|---|---|---|
| `BotError::DomainError` | The action did not take effect | a retry |
| `BotError::EffectIndeterminate` | The effect may be live | a possible duplicate |

`EffectIndeterminate` is documented as the case you reach "when a request timed
out, or a connection dropped, *after* the request was sent"
(`crates/lgwks-bot/src/error.rs:106`). Its `Display` renders the domain, the
cause, and the words "may have taken effect", so an operator reading a log line
sees the indeterminacy rather than having to infer it.

Write your retry classifier against the variant, never against the cause string:

```rust
fn may_have_taken_effect(error: &lgwks_bot::BotError) -> bool {
    matches!(*error, lgwks_bot::BotError::EffectIndeterminate { .. })
}
```

`BotError` is `#[non_exhaustive]`, so a `match` on it needs a wildcard arm. That
is deliberate: a new variant becomes a compile-time prompt at your `match` rather
than a silent fallthrough.

An indeterminate effect is the one case the substrate refuses to decide on its
own. The entry is held — reported through `Bot::pending()` as
`TransitionHold::OutcomeUnknown`, and never attempted again by the tick loop,
whatever the budget says — until the caller says what happened:

```rust,ignore
// `work` and `revision` come from the `PendingWork` that `pending()` reported.
bot.resolve_effect(work, revision, EffectEvidence::Applied)?;   // it happened: recorded, not replayed
bot.resolve_effect(work, revision, EffectEvidence::NotApplied)?; // it did not: eligible again
```

The `revision` is the generation the evidence is about, and it is required rather
than inferred. One chain holds one transition at a time, and a slot is reused by
every generation over it, so `(chain, entry)` alone cannot distinguish a report
about the attempt the caller was shown from one about the attempt that replaced
it. A delayed report is what makes that dangerous rather than pedantic:
`NotApplied` against the wrong generation makes an attempt nobody has accounted
for eligible to run again.

`resolve_effect` therefore distinguishes three refusals, and only the first of
them means what it always meant:

- `BotError::NoSuchWork` — there is no held effect at this address: it ran, its
  condition was false, or it has not been reached. Accepting evidence for an
  entry that has an answer would let a caller believe an effect was acknowledged
  when nothing was.
- `BotError::EvidenceSuperseded` — the chain holds a transition at a different
  revision, so the work this evidence is about is gone. Nothing changed; re-read
  `pending()` and report against the generation there now.
- `BotError::EvidenceContradicted` — this generation's entry is already settled
  and the new evidence says the opposite. Nothing changed. Repeating the *same*
  evidence is not this: it succeeds idempotently.

The ledger is in memory, so this is a report to the caller that owns the run, not
durability: an effect left in doubt is owed an answer by whoever holds the bot,
including across a restart. Delivering exactly-once across an external effect
needs that intent stored outside the process.

## Not a failure: the adapter refused

`BotError::TickInsideRuntime` is an `Err` from `tick`, and it is not a failed
tick. Nothing was observed, no condition was evaluated and no effect ran,
because the synchronous adapter refused to park the thread before it started.
It is returned whenever `tick` is called while an async runtime is running on
that thread, on a current-thread runtime and on a multi-worker one alike: a
parked thread starves the reactor the bot's own verbs need, so the deadlock it
would otherwise cause is not something the adapter can wait out. Await
`bot.tick_async()` instead, or drive the tick from outside the runtime. The
README's "Running a tick from async code" section has both forms.

## Confusion inside a chain

Two failures come from the wiring rather than from a domain, and both are typed
so they cannot be mistaken for a condition that simply did not fire.

- A condition whose `Evaluate<T>` implementation returns `Err` stops the chain
  with that error (`crates/lgwks-bot/src/ecs.rs:3223`). A structural failure in a
  condition is `BotError::EvaluateError`, not `false`. The stop is ordered rather
  than absolute: the steps `fire_plan` recorded before the failing condition are
  still run, so an effect the walk had already cleared does take effect, while
  the entry behind the failing condition is left exactly as it was — nothing was
  attempted, so nothing about its effect is claimed.
  `a_condition_failure_stops_the_walk_after_the_effects_it_cleared` pins the
  ordered half and
  `a_condition_that_cannot_be_evaluated_holds_the_chain_without_claiming_anything`
  pins the other.
- The erased chain wrappers downcast the observed value back to the type the
  condition was registered with. A mismatch in the condition is
  `EvaluateError`; a mismatch in the action's input is
  `BotError::TypeMismatch`, naming the site that caught it and both types
  (`crates/lgwks-bot/src/spec.rs`). It names no domain, because a mismatch
  means no domain was reached — and it is `Terminal`, so it costs one attempt
  rather than a whole retry budget. The `Auth` is issued before the downcast in
  the action path, so a type mismatch fails without a side effect.
- The witness is checked before either downcast. Each chain records the type its
  source produces (`TypeId`, taken where the type was still a parameter) and
  every polled value carries the type it was erased from; `observe_fold` compares
  them at the rendezvous and reports `BotError::TypeMismatch` from site
  `observe_fold rendezvous` if they disagree, committing nothing. This is the
  half a typed builder cannot prove: the builder fixes the pairing at
  construction, and the witness checks it still holds across erasure.

## The order errors are reported in

A poll failure stops the tick before any effect: all sources are polled before
any `Revision` is written, so a failing poll cannot leave one source updated and
another not, and `tick` returns the first error in declaration order.

An action failure does not stop the later chains. `fire_plan` walks every chain
in declaration order and `run_steps` runs them in that same order, parking the
first failure it saw and reporting that one, while the chains behind the failing
one still run and still record their work. `TickError` is a resource rather than
a return value because an exclusive system returns `()` and cannot propagate
(`crates/lgwks-bot/src/ecs.rs:890`); `tick` takes it after the schedule runs, and
it takes precedence over the `PendingTransition` report because it carries the
typed variant a retry classifier matches on.
