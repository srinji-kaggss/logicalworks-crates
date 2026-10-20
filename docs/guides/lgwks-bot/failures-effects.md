# What a failed tick means: uncertain effects and refusals

Pages of this guide: [polls and actions](failures.md) · [worked example](failures-example.md) · [uncertain effects](failures-effects.md) · [reporting order](failures-order.md)

## Case three: the effect may already have happened

`BotError` separates "the effect did not happen" from "the effect may have
happened" as two variants, not one variant with a flag.

| Variant | Means | A retry is |
|---|---|---|
| `BotError::DomainError` | The action did not take effect | a retry |
| `BotError::EffectIndeterminate` | The effect may be live | a possible duplicate |

`EffectIndeterminate` is documented as the case you reach "when a request timed
out, or a connection dropped, *after* the request was sent"
(`crates/lgwks-bot/src/error.rs:193`). Its `Display` renders the domain, the
cause, and the words "may have taken effect", so an operator reading a log line
sees the indeterminacy rather than having to infer it.

Decide a retry against the variant, never against the cause string. In a flow
you rarely write that decision at all: a `retry` block reads
`FlowError::is_retryable`, which is true for a bot error exactly when its class
is `RetryClass::Safe`, and `EffectIndeterminate` is `RetryClass::RequiresEvidence`,
so no `retry` block repeats it. This flow, run against an action that fails once
each way, ran the action once for the lost reply and three times for the refused
connection:

```rust
lgwks_bot::script! {
    /// Page on call, trying again while the page certainly was not sent.
    flow page(depth: u32):
        retry up to 3 times, waiting 1ms:
            act notify::page on "oncall" with depth
}
```

```text
indeterminate: ran 1 time(s): paging/page/retry/page: notify::page: may have taken effect — the reply never arrived
not delivered: ran 3 time(s): paging/page/retry: gave up after 3 attempts; last: paging/page/retry/page: notify::page: connection refused
```

Outside a flow, a classifier of your own is `matches!(*error,
BotError::EffectIndeterminate { .. })`, the variant and not the text.

`BotError` is `#[non_exhaustive]`, so a `match` on it needs a wildcard arm. That
is deliberate: a new variant becomes a compile-time prompt at your `match` rather
than a silent fallthrough.

An indeterminate effect is the one case the substrate refuses to decide on its
own. The entry is held — reported through `Bot::pending()` as
`TransitionHold::OutcomeUnknown`, and never attempted again by the tick loop,
whatever the budget says — until the caller says what happened:

```rust,ignore
// `key` comes from the `PendingWork` that `pending()` reported.
bot.resolve_effect(&key, EffectEvidence::Applied)?;    // it happened: recorded, not replayed
bot.resolve_effect(&key, EffectEvidence::NotApplied)?; // it did not: eligible again
```

The key binds the run, flow, environment, action, attempt, and admitted input.
It is required rather than inferred: a delayed report for a different attempt
must not make an unaccounted-for effect eligible to run again.

`resolve_effect` therefore distinguishes three refusals, and only the first of
them means what it always meant:

- `BotError::NoSuchWork` — there is no held effect at this address: it ran, its
  condition was false, or it has not been reached. Accepting evidence for an
  entry that has an answer would let a caller believe an effect was acknowledged
  when nothing was.
- `BotError::EvidenceContradicted` — this generation's entry is already settled
  and the new evidence says the opposite. Nothing changed. Repeating the *same*
  evidence is not this: it succeeds idempotently.

The live ledger is in memory, while recovery derives outstanding work from the
journal. An ephemeral scope cannot promise crash durability; an external effect
needs a journal acknowledgement that meets its declared durability grade.

`TransitionHold::RecordingFailed` is different from `OutcomeUnknown`: the
outcome is known, but its durable receipt could not be verified. Retrying that
hold retries receipt/recording work only; it never re-enters the action.

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
