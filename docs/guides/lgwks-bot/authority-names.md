# Authority in lgwks_bot: capability names and narrower bots

Pages of this guide: [admission](authority.md) · [proofs and grants](authority-grants.md) · [names and narrower bots](authority-names.md)

## Capability names

Four capabilities ship (`crates/lgwks-bot/src/cap.rs:56`):

| Constant | Name | Used for |
|---|---|---|
| `Cap::NET` | `bot.net` | HTTP, WebSocket, API calls |
| `Cap::FS` | `bot.fs` | reading, writing, watching paths |
| `Cap::SYS` | `bot.sys` | process control, environment |
| `Cap::NOTIFY` | `bot.notify` | Slack, email, webhook push |

`GrantSet::all_shipped()` grants all four. `Cap::new` accepts any name
(`crates/lgwks-bot/src/cap.rs:67`), because custom capabilities are data-driven:
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

`grant` is idempotent (`crates/lgwks-bot/src/gate.rs:59`), so granting a
capability twice is the same as granting it once and you need not track what an
earlier call added.
