# The grant set is a snapshot

Part of [Authority in lgwks_bot](../authority.md).


This is the part that surprises people. `EcsBot::assemble` clones the set into
the world (`crates/lgwks-bot/src/ecs/builder.rs:635`):

```rust
world.insert_resource(Grants(grants.clone()));
```

`Grants` is a private resource holding that clone. `GrantSet`
(`crates/lgwks-bot/src/gate.rs:14`) has three methods that add or inspect:
`empty`, `all_shipped`, `grant`, plus `admit` and `issue`. There is no `revoke`,
and no method that removes a capability.

The consequences, stated plainly:

- Dropping or changing the `GrantSet` you passed to `build` does not narrow the
  bot. It keeps the authority it was admitted with for its whole life.
- An `Auth` you hold stays valid for the capabilities it covers. It is a
  capability-membership proof inside this process, not a signature and not an
  identity. A proof minted from an expiring grant lapses when its lifetime has
  passed (see "A credential can lapse" below), but that lifetime is fixed at the
  mint and a built bot mints afresh on every call, so it does not narrow a
  running bot.
- Narrowing a running bot is a decision made outside this API. Build it from a
  narrower set, or stop calling `tick`.

If that is the property you need, it is not here yet. Live narrowing would need
a linearization point, a token generation, and a defined answer for effects
already in flight. None of those exist in the inspected source.
