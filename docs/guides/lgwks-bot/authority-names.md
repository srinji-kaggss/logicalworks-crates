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

A flow carries no authority of its own: the host that runs it holds the
`GrantSet`, so the lever is the grant set you hand the host.
`crates/lgwks-bot/examples/queue_watch.rs` runs one flow on two hosts, one built
with `GrantSet::empty().grant(Cap::notify())` and one with `GrantSet::empty()`.
The second is refused at the step that needs the grant, and the action never runs:

```text
no grant: watching/watch/page: blocked: 1 ungranted: bot.notify
```

`grant` is idempotent (`crates/lgwks-bot/src/gate.rs:59`), so granting a
capability twice is the same as granting it once and you need not track what an
earlier call added.
