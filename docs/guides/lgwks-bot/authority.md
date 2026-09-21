# Authority in lgwks_bot

Capability checks in this crate answer one question: was this call covered by
the grant set the bot was built with. They do not confine anything, and the
boundary is worth reading before you design around it.

## Admission happens at build

`GrantSet::admit` (`crates/lgwks-bot/src/gate.rs:50`) walks the required
capabilities and returns `BotError::CapabilityDenied { required }` for the first
one the set does not contain. `EcsBot::assemble` calls it for every source and
every action before the world is built (`crates/lgwks-bot/src/ecs.rs:592`):

```rust
for chain in &chains {
    grants.admit(chain.source.required_caps())?;
    for entry in &chain.entries {
        grants.admit(entry.action.required_caps())?;
    }
}
```

So a bot that declares `bot.net` and is built with `GrantSet::empty()` fails
before it ever ticks. The same gate runs on both terminal builder calls
(`EcsBuilder::build` and `EcsObserveBuilder::build`), because both route through
`assemble`.

## Every call presents a proof

`GrantSet::issue` (`crates/lgwks-bot/src/gate.rs:64`) is the only path that
constructs an `Auth`. Its constructor is crate-private
(`crates/lgwks-bot/src/cap.rs:102`), and `Auth` is not `Serialize`, so authority
cannot round-trip through JSON.

Each verb takes an `(Auth, input)` tuple. `poll`, `execute_action`, and `query`
call `Auth::check(&self.required_caps())?` before touching anything. `Evaluate`
does not, because it takes no `Auth` at all: it is a boolean over already-observed
state with no side effect to gate.

Coverage is exact set membership, not subsumption
(`crates/lgwks-bot/src/cap.rs:119`). A proof scoped to `bot.fs` presented to a
source requiring `bot.net` is denied. A proof covering nothing authorizes
nothing. Both cases have tests in `crates/lgwks-bot/src/spec.rs`
(`wrong_scope_proof_is_denied_confused_deputy`, `call_with_empty_proof_is_denied_at_the_callee`).

The framework mints a fresh proof per call from the grant set it retained, so you
never thread an `Auth` through your own call sites for the chained path.

## The grant set is a snapshot

This is the part that surprises people. `EcsBot::assemble` clones the set into
the world (`crates/lgwks-bot/src/ecs.rs:599`):

```rust
world.insert_resource(Grants(grants.clone()));
```

`Grants` is a private resource holding that clone. `GrantSet`
(`crates/lgwks-bot/src/gate.rs:12`) has three methods that add or inspect:
`empty`, `all_shipped`, `grant`, plus `admit` and `issue`. There is no `revoke`,
and no method that removes a capability.

The consequences, stated plainly:

- Dropping or changing the `GrantSet` you passed to `build` does not narrow the
  bot. It keeps the authority it was admitted with for its whole life.
- An `Auth` you hold stays valid for the capabilities it covers. It is a
  capability-membership proof inside this process, not a signature, not an
  identity, and not a lease with an expiry.
- Narrowing a running bot is a decision made outside this API. Build it from a
  narrower set, or stop calling `tick`.

If that is the property you need, it is not here yet. Live narrowing would need
a linearization point, a token generation, and a defined answer for effects
already in flight. None of those exist in the inspected source.

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

## Capability names

Four capabilities ship (`crates/lgwks-bot/src/cap.rs:35`):

| Constant | Name | Used for |
|---|---|---|
| `Cap::NET` | `bot.net` | HTTP, WebSocket, API calls |
| `Cap::FS` | `bot.fs` | reading, writing, watching paths |
| `Cap::SYS` | `bot.sys` | process control, environment |
| `Cap::NOTIFY` | `bot.notify` | Slack, email, webhook push |

`GrantSet::all_shipped()` grants all four. `Cap::new` accepts any name
(`crates/lgwks-bot/src/cap.rs:46`), because custom capabilities are data-driven:
`Cap::new("your.domain.cap")` is a valid capability that nothing grants unless
you grant it. Enforcement is equality at the gate, so a misspelled name is simply
a capability that never matches, not an error at construction.

## Building a narrower bot

Since there is no way to shrink a live bot, the lever is the grant set you pass:

```rust
use lgwks_bot::{Bot, Cap, GrantSet};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let read_only = GrantSet::empty().grant(Cap::net());

    let bot = Bot::builder("reporter").build(&read_only)?;
    assert_eq!(bot.name(), "reporter");
    Ok(())
}
```

`grant` is idempotent (`crates/lgwks-bot/src/gate.rs:44`), so granting a
capability twice is the same as granting it once and you need not track what an
earlier call added.
