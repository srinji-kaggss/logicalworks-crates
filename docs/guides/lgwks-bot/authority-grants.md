# Authority in lgwks_bot: proofs, credentials and grants

Pages of this guide: [admission](authority.md) · [proofs and grants](authority-grants.md) · [names and narrower bots](authority-names.md)

## A credential can lapse, and a receiver can refuse one

`GrantSet::grant_expiring(cap, ttl)` gives a capability a lifetime, and
`GrantSet::issue_at(required, clock)` mints a proof carrying the shortest
lifetime among the capabilities it covers. The lifetime is a duration on the
declared clock, never a timestamp. `Auth::check` refuses a lapsed proof
*before* it checks coverage, as `BotError::CredentialExpired`: the capabilities
are still held and name nothing, so the repair is a re-grant, not a grant.

An adapter whose receiver refuses the token it presented reports
`BotError::CredentialRejected`, built by `cap::upstream_credential_rejection`
for a status `cap::is_credential_status` accepts (401, 403, 404). It carries the
repair as a `NeedSet` whose `Need::CredentialExpired` names what to re-grant,
and `NeedSet::proposed_grants` turns it into a grant set. Both are
`RetryClass::Never`: the same credential against the same receiver is refused
again, so neither feeds a retry loop. The shipped `gh` adapter reports a
refused token this way on its `Observe` verb and in every flow.

## The grant set is a snapshot, narrowed only through `replace_grants`

This is the part that surprises people. `EcsBot::assemble` clones the set into
the world (`crates/lgwks-bot/src/ecs/builder.rs:630`):

```rust
world.insert_resource(Grants(grants.clone()));
```

`Grants` is a private resource holding that clone. `GrantSet`
(`crates/lgwks-bot/src/gate.rs:14`) has three methods that add or inspect:
`empty`, `all_shipped`, `grant`, plus `admit` and `issue`. There is no `revoke`,
and no method that removes a capability: the set is data, and revoking it is an
operation on the bot that holds it — `EcsBot::replace_grants` — not on the set.

The consequences, stated plainly:

- Dropping or changing the `GrantSet` you passed to `build` does not narrow the
  bot. It keeps the authority it was admitted with until you narrow it through
  `replace_grants`.
- An `Auth` you hold stays valid for the capabilities it covers. It is a
  capability-membership proof inside this process, not a signature and not an
  identity. A proof minted from an expiring grant lapses when its lifetime has
  passed (see "A credential can lapse" below), but that lifetime is fixed at the
  mint and a built bot mints afresh on every call, so it does not narrow a
  running bot.
- Narrowing a running bot is `replace_grants`: it swaps the retained set
  between ticks — atomic with respect to them, since both take `&mut self` —
  and reports what it withdrew and added. The next tick's calls for a withdrawn
  capability are denied; proofs already minted keep covering what they were
  minted for until their lifetimes lapse.

## It is not a sandbox

`crates/lgwks-bot/src/cap.rs:15` states the ceiling in the module documentation:

> This stops confused-deputy calls and accidental ungated use; it is not a
> sandbox, since in-process code can always dial out directly, so the guarantee
> is explicit, auditable authority, not confinement.

Concretely: the `Auth` check constrains code that calls through the verb traits.
A `Query` or `Observe` implementation you write can open a socket without
consulting its `Auth`, because it is ordinary Rust running in your process. The
crate does not intercept that, and nothing in the inspected source claims to.

What you get instead is an answer to "what is this bot permitted to do", readable
from its grant set rather than from a reading of every domain implementation.

## There is no run-time capability hold, because it would be unreachable

The obvious next step after a total refusal — an action denied while the bot is
running, the entry held, the caller supplies the capability, the tick carries on
from there — is **not implemented, and the reason is not effort. An action
cannot be denied for want of a capability at run time.** Two facts make that so:

- `EcsBot::assemble` admits every declared requirement before the world is built
  (`crates/lgwks-bot/src/ecs/builder.rs:610`), so a bot whose declared requirements are
  not granted does not exist to run.
- Every per-call proof is minted from **the same list**. `run_any` and
  `run_any_on` share `execute_erased`, which calls
  `grants.issue(self.0.required_caps())` (`crates/lgwks-bot/src/spec.rs:720`),
  and the action then checks `call.0.check(self.required_caps())`. The two cannot
  disagree, and the only writer of `Grants` besides `assemble` is
  `EcsBot::replace_grants`, which swaps the whole list rather than holding an
  entry.

So a run-time hold has nothing to resume from: `BotError::CapabilityDenied`
after admission means the retained list itself narrowed — through
`replace_grants` — and the repair is a wider `replace_grants`, not a held
entry. The `failure_state` arm that folds a denial into an abandoned entry
(`crates/lgwks-bot/src/ecs/admit.rs:344`) is the record of exactly that: a
denial now reachable at run time, reported rather than resumed.

The consequence worth knowing is the *reverse* of a hold, and it is a real gap:

> **An action that declares `&[]` and performs a side effect passes the gate
> silently.**

The gate checks the declared set, and `&[]` is trivially covered — so "this
action needs nothing" and "this action's author never said" are the same value,
and the crate cannot tell them apart. Everything above about authority is a
statement about the capabilities a domain **declares**. Making an undeclared
capability detectable, or making a run-time hold meaningful, is a design change
rather than a repair, and it is not made here.
