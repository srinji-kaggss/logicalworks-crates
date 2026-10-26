# lgwks_bot: authority, shipped domains and serializable specs

Pages of this README: [overview](README.md) · [execution model](README-execution.md) · [failure](README-failure.md) · [authority, domains and specs](README-authority.md) · [script!](README-script.md) · [async runtime](README-runtime.md) · [supervision](README-supervision.md)

## Capability system

Every domain declares the capabilities it requires. The builder validates
`required ⊆ granted` before construction. A bot asking for `bot.net` without a
grant fails at build time, not at runtime.

Authority stays proof-carrying past build: every `poll`, `execute_action`, and
`query` takes an `(Auth, input)` tuple, and only `GrantSet::issue` can mint the
`Auth` half. The framework mints a fresh proof for each call, from the grant set
it retained at build, and each callee checks coverage before acting, so a
narrower proof presented to a broader domain is denied (no confused deputies).
`Evaluate` takes no proof, because it is pure boolean logic with no side effect
to gate.

### Authority is a snapshot, narrowed only through `replace_grants`

A built bot owns a *clone* of the grant set it was built with. `GrantSet` has no
`revoke`, and an `Auth` holds the capabilities it was issued with, so:

- Changing or dropping the `GrantSet` you passed to `build` does not narrow the
  bot. It keeps the authority it was admitted with until you narrow it.
- An `Auth` in hand stays valid for the capabilities it covers. It is a
  capability-membership proof inside this process — not a signature and not an
  identity. A proof minted from a grant that named a lifetime
  (`GrantSet::grant_expiring`) lapses when that lifetime, measured on the
  minting clock from the mint, has passed, and is then refused as
  `CredentialExpired` (INV-BOT-154). That lifetime is fixed at the mint and
  nothing shortens it afterwards, so it is not a live lease either.
- A lifetime bounds a proof *in hand*, not a built bot: the bot mints a fresh
  proof from the set it retained on every call, so each call's proof starts a
  fresh lifetime.
- Narrowing a running bot is `EcsBot::replace_grants`: it swaps the retained
  set between ticks — atomic with respect to them, since both take `&mut self`
  — and reports what it withdrew and added. The next tick's calls for a
  withdrawn capability are denied; proofs already minted keep covering what
  they were minted for.

The linearization point is the borrow checker: no tick observes a half-swapped
set, and effects already in flight keep the proofs they started with.

| Capability | Constant | Description |
|------------|----------|-------------|
| `bot.net` | `Cap::NET` | Network access: HTTP, WebSocket, API calls |
| `bot.fs` | `Cap::FS` | Filesystem access: read, write, watch paths |
| `bot.sys` | `Cap::SYS` | System access: process control, environment |
| `bot.notify` | `Cap::NOTIFY` | Notification delivery: Slack, email, webhooks |

Custom capabilities use `Cap::new("your.domain.cap")`.

### Tenant names are partition keys, not identities
A flow's `Tenant` is chosen by the trusted host that builds it: it keeps honest
tenants from sharing keys, not adversaries from choosing a name (`SECURITY.md`).
## Shipped domains

Nine domains ship with the crate, each implementing one or more verbs:

| Domain | Module | Capabilities | Verbs |
|--------|--------|-------------|-------|
| GitHub | `domain::gh` | `bot.net` | Observe, Execute, Query |
| Network | `domain::net` | `bot.net` | Observe, Query |
| Chat | `domain::chat` | `bot.net` | Observe, Query |
| Filesystem | `domain::fs` | `bot.fs` | Observe, Query |
| Data store | `domain::data` | `bot.fs` | Observe, Query |
| System | `domain::sys` | `bot.sys` | Observe, Execute, Query |
| Notifications | `domain::notify` | `bot.notify`, `bot.net` | Execute |
| Flow | `domain::flow` | inherited | Execute (pipeline) |
| Evaluators | `domain::eval` | — | Evaluate (changed, threshold) |

## Serializable specs

`BotSpec` is the serializable contract: what an AI emits, what a manifest
contains. It round-trips through JSON:

`a_bot_spec_round_trips_through_json`
(`crates/lgwks-bot/tests/it/readme_front_door.rs`) runs it: a spec naming one chain,
from `gh::pr_status` to `notify::slack` at `#deploys`, is read with
`BotSpec::from_json`, written with `to_json`, and read back as the same document.
The two halves have different error types, deliberately: `from_json` reports
typed field and size errors (`BotError`) because untrusted input can be wrong in
domain ways, while `to_json` can only fail in the serializer (`json::Error`),
because a `BotSpec` in hand is already valid.

### Materializing a spec

`BotSpec` is a document. `Bot::from_spec` turns one into a running bot against a
[`DomainRegistry`](https://docs.rs/lgwks_bot) — the `domain_id -> constructor`
list a binary declares with `domains!`:

`a_spec_naming_a_domain_the_registry_lacks_is_refused_with_a_need_set`
(`crates/lgwks-bot/tests/it/readme_front_door.rs`) makes the call:
`Bot::from_spec(&spec, &DomainRegistry::empty(), &GrantSet::empty(), effects)`
returns `Result<Bot, Admission>`, and a document that names domains the registry
does not hold comes back as `Admission::Needs`. Your binary's registry is the one
declared with `domains!`; the real, runnable example is on `Bot::from_spec`.

Materialization runs through the **same** `assemble`/`build` path a native bot
does — no second interpreter and no second execution path — so a bot built from
JSON and one built from `Bot::builder` with the same domains produce the same
operation trace. It is **all-or-nothing**: every presently knowable unmet need —
an unknown source or action domain, a constructor that rejects its target, a
missing capability, an unknown condition — is collected into one attributed
[`NeedSet`](https://docs.rs/lgwks_bot), and the materialization is refused with
that report; nothing is polled and nothing runs. A `NeedSet` is a report and a
repair *proposal*, never a grant.

Authority keeps coming from the `GrantSet` the caller holds and never from the
spec, so wire data cannot choose what a bot reaches. A condition is named in the
document from a closed vocabulary — `changed`, `always`, `threshold::above(<n>)`,
`threshold::below(<n>)`, the two thresholds only for a source registered with
`Source::ordered` — resolved against the source's own output type; an
identifier outside it is a reported need, never a silently always-true gate.

The builder chain remains the DSL for anything a document cannot express. There
is deliberately no `bot!` proc-macro: it would hide the per-call `Auth::check`
that auditors read. The orchestration *between* calls is a different matter, and
has one: [`script!`](README-script.md#orchestration-script).
