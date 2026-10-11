# Admission happens at build, and reports the whole shortfall

Part of [Authority in lgwks_bot](../authority.md).


`EcsBot::assemble` walks every source and every action before the world is built
(`crates/lgwks-bot/src/ecs/builder.rs:658`):

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
