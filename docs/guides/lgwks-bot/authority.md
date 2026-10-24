# Authority in lgwks_bot

Pages of this guide: [admission](authority.md) · [proofs and grants](authority-grants.md) · [names and narrower bots](authority-names.md)

Capability checks in this crate answer one question: was this call covered by
the grant set the bot was built with. They do not confine anything, and the
boundary is worth reading before you design around it.

## Admission happens at build, and reports the whole shortfall

`EcsBot::assemble` walks every source and every action before the world is built
(`crates/lgwks-bot/src/ecs/builder.rs:610`):

```rust
let mut shortages: Vec<Shortage> = Vec::new();
for chain in &chains {
    shortages.extend(grants.uncovered(
        chain.source.required_caps(),
        &Demand::new(chain.source.domain_id()),
    ));
    for entry in &chain.entries {
        shortages.extend(grants.uncovered(
            entry.action.required_caps(),
            &Demand::new(entry.action.domain_id()),
        ));
    }
}
if let Some(deficit) = Deficit::from_shortages(shortages) {
    return Err(BotError::CapabilityDenied { deficit });
}
```

So a bot that declares `bot.net` and is built with `GrantSet::empty()` fails
before it ever ticks. The same gate runs on both terminal builder calls
(`EcsBuilder::build` and `EcsObserveBuilder::build`), because both route through
`assemble`.

**One refusal carries every unmet requirement.** `BotError::CapabilityDenied`
holds a `Deficit` (`crates/lgwks-bot/src/cap.rs:205`) — the whole difference
between what the bot requires and what it was granted — rather than the first
element of it. The shape matters more than it looks: a check that returns one
missing capability at a time makes admission a loop where each pass reveals one
more word, so a bot short of four capabilities takes four refusals to diagnose
and the caller never holds the whole picture. Each `Shortage` also names the
domain that declared it, because `bot.net` says what is missing and
`gh::pr_status` says who is asking.
`GrantSet::admit` (`crates/lgwks-bot/src/gate.rs:116`) is the same gate for a
single requirement list, and `GrantSet::uncovered`
(`crates/lgwks-bot/src/gate.rs:106`) is the total form both are built on.

## The refusal derives its own repair

A `Deficit` already names every capability that would close it, so the caller
does not translate a diagnostic into a repair by hand:
`Deficit::to_grant_set` (`crates/lgwks-bot/src/cap.rs:291`) returns exactly the
set that closes the shortfall. `GrantSet::grant`
(`crates/lgwks-bot/src/gate.rs:59`) is consuming and de-duplicating, so folding
it into a set the caller already holds is `held.grant(..)` per element and
reaches a fixed point.

The repair is the *shortfall*, not a restatement of the requirement: a
capability that was already granted is not in it. Closing the requirement is the
two-step composition — the deficit's set folded into the held set — and a test
asserts both halves, because "the repair admits the requirement" and "the repair
admits the missing capabilities" are different claims and only the second is
true.

## Every call presents a proof

`GrantSet::issue` (`crates/lgwks-bot/src/gate.rs:131`) is the only path that
constructs an `Auth`. Its constructor is crate-private
(`crates/lgwks-bot/src/cap.rs:406`), and `Auth` is not `Serialize`, so authority
cannot round-trip through JSON.

Each verb takes an `(Auth, input)` tuple. `poll`, `execute_action`, and `query`
call `Auth::check(&self.required_caps())?` before touching anything. `Evaluate`
does not, because it takes no `Auth` at all: it is a boolean over already-observed
state with no side effect to gate.

Coverage is exact set membership, not subsumption
(`crates/lgwks-bot/src/cap.rs:526`). A proof scoped to `bot.fs` presented to a
source requiring `bot.net` is denied. A proof covering nothing authorizes
nothing. Both cases have tests in `crates/lgwks-bot/src/spec.rs`
(`wrong_scope_proof_is_denied_confused_deputy`, `call_with_empty_proof_is_denied_at_the_callee`).

The framework mints a fresh proof per call from the grant set it retained, so you
never thread an `Auth` through your own call sites for the chained path.

## Revocation is `replace_grants`

Withdrawing the caller's set never narrowed a built bot, and still does not.
Narrowing the bot itself is one operation: `EcsBot::replace_grants` swaps the
retained set the framework mints from and reports what it withdrew and added
(`GrantReplacement::withdrawn` / `added`, each sorted), so the next tick's
calls for a withdrawn capability are denied with `BotError::CapabilityDenied`.

Atomicity is the borrow checker's: the swap and every tick take `&mut self`,
and ticks are stepped by hand rather than on a background thread, so no tick
observes a half-swapped set. Proofs already minted keep covering what they
were minted for until their lifetimes lapse; only subsequent mints read the
new set. That is live revocation of future calls, not retroactive invalidation
of credentials in hand.
