# Authority in lgwks_bot

Capability checks in this crate answer one question: was this call covered by
the grant set the bot was built with. They do not confine anything, and the
boundary is worth reading before you design around it.

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

## In this guide

- [Admission happens at build, and reports the whole shortfall](authority/admission-happens-at-build-and-reports-the-whole-shortfall.md)
- [Every call presents a proof](authority/every-call-presents-a-proof.md)
- [The grant set is a snapshot](authority/the-grant-set-is-a-snapshot.md)
- [There is no run-time capability hold, because it would be unreachable](authority/there-is-no-run-time-capability-hold-because-it-would-be-unreachable.md)
