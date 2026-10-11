# There is no run-time capability hold, because it would be unreachable

Part of [Authority in lgwks_bot](../authority.md).


The obvious next step after a total refusal — an action denied while the bot is
running, the entry held, the caller supplies the capability, the tick carries on
from there — is **not implemented, and the reason is not effort. An action
cannot be denied for want of a capability at run time.** Two facts make that so:

- `EcsBot::assemble` admits every declared requirement before the world is built
  (`crates/lgwks-bot/src/ecs/builder.rs:658`), so a bot whose declared requirements are
  not granted does not exist to run.
- Every per-call proof is minted from **the same list**. `run_any` calls
  `grants.issue(self.0.required_caps())` (`crates/lgwks-bot/src/spec.rs:797`),
  and the action then checks `call.0.check(self.required_caps())`. The two cannot
  disagree, and nothing narrows `Grants` after `assemble` — the only writer is
  `assemble` itself.

So `Auth::check` cannot fail inside a running bot.
`BotError::CapabilityDenied` is a build-time failure, and the `failure_state` arm
that folds it into an abandoned entry (`crates/lgwks-bot/src/ecs/plan.rs:342`) is
defensive rather than live. A hold was built on top of that arm and then removed,
because machinery whose only caller is a contrived test is not a feature.

The consequence worth knowing is the *reverse* of a hold, and it is a real gap:

> **An action that declares `&[]` and performs a side effect passes the gate
> silently.**

The gate checks the declared set, and `&[]` is trivially covered — so "this
action needs nothing" and "this action's author never said" are the same value,
and the crate cannot tell them apart. Everything above about authority is a
statement about the capabilities a domain **declares**. Making an undeclared
capability detectable, or making a run-time hold meaningful, is a design change
rather than a repair, and it is not made here.
